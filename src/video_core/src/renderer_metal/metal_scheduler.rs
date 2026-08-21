// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Metal command-buffer submission and completion ordering.

use std::collections::VecDeque;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLBlitCommandEncoder, MTLCommandBuffer, MTLCommandBufferStatus, MTLCommandEncoder,
    MTLCommandQueue, MTLComputeCommandEncoder, MTLRenderCommandEncoder, MTLRenderPassDescriptor,
};
use thiserror::Error;

use super::metal_device::MetalDevice;

#[derive(Debug, Error)]
pub enum MetalSchedulerError {
    #[error("Metal command queue failed to allocate a command buffer")]
    NoCommandBuffer,
    #[error("Metal command buffer failed to allocate a blit encoder")]
    NoBlitEncoder,
    #[error("Metal command buffer failed to allocate a compute encoder")]
    NoComputeEncoder,
    #[error("Metal command buffer failed to allocate a render encoder")]
    NoRenderEncoder,
    #[error("no Metal render encoder is active")]
    NoActiveRenderEncoder,
    #[error("Metal command buffer completed with status {0:?}")]
    CommandBufferFailed(MTLCommandBufferStatus),
}

/// Serial Metal queue owner.
///
/// Metal command buffers submitted to one queue execute in commit order. The
/// backend therefore records guest uploads, render/compute passes and copies
/// in their original Maxwell order without translating Vulkan barriers or
/// layouts into Metal concepts.
pub struct MetalScheduler {
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    active: Option<Retained<ProtocolObject<dyn MTLCommandBuffer>>>,
    active_encoder: Option<ActiveEncoder>,
    in_flight: VecDeque<InFlightCommandBuffer>,
    next_tick: u64,
    known_gpu_tick: u64,
}

enum ActiveEncoder {
    Blit(Retained<ProtocolObject<dyn MTLBlitCommandEncoder>>),
    Compute(Retained<ProtocolObject<dyn MTLComputeCommandEncoder>>),
    Render(Retained<ProtocolObject<dyn MTLRenderCommandEncoder>>),
}

impl ActiveEncoder {
    fn end_encoding(self) {
        match self {
            Self::Blit(encoder) => encoder.endEncoding(),
            Self::Compute(encoder) => encoder.endEncoding(),
            Self::Render(encoder) => encoder.endEncoding(),
        }
    }
}

struct InFlightCommandBuffer {
    tick: u64,
    command_buffer: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
}

impl MetalScheduler {
    pub fn new(device: &MetalDevice) -> Self {
        Self {
            queue: device.retained_command_queue(),
            active: None,
            active_encoder: None,
            in_flight: VecDeque::new(),
            next_tick: 1,
            known_gpu_tick: 0,
        }
    }

    pub fn begin(
        &self,
    ) -> Result<Retained<ProtocolObject<dyn MTLCommandBuffer>>, MetalSchedulerError> {
        self.queue
            .commandBuffer()
            .ok_or(MetalSchedulerError::NoCommandBuffer)
    }

    /// Return the command buffer that records the current guest batch.
    ///
    /// This is the Metal counterpart of Eden's scheduler chunk: buffer/image
    /// copies, compute passes and render encoders are appended in guest order,
    /// then `flush` commits the batch once. Returning a retained reference lets
    /// a short-lived encoder borrow the command buffer without exposing the
    /// scheduler's active-slot ownership.
    pub fn active_command_buffer(
        &mut self,
    ) -> Result<Retained<ProtocolObject<dyn MTLCommandBuffer>>, MetalSchedulerError> {
        if self.active.is_none() {
            self.active = Some(self.begin()?);
        }
        Ok(self.active.as_ref().unwrap().clone())
    }

    /// Encode a transfer while preserving Metal's one-active-encoder rule.
    /// Consecutive copies share one blit encoder and are therefore batched.
    pub fn with_blit_encoder<R>(
        &mut self,
        record: impl FnOnce(&ProtocolObject<dyn MTLBlitCommandEncoder>) -> R,
    ) -> Result<R, MetalSchedulerError> {
        if !matches!(self.active_encoder.as_ref(), Some(ActiveEncoder::Blit(_))) {
            self.end_active_encoder();
            let command_buffer = self.active_command_buffer()?;
            let encoder = command_buffer
                .blitCommandEncoder()
                .ok_or(MetalSchedulerError::NoBlitEncoder)?;
            self.active_encoder = Some(ActiveEncoder::Blit(encoder));
        }
        let Some(ActiveEncoder::Blit(encoder)) = self.active_encoder.as_ref() else {
            unreachable!("blit encoder was installed above")
        };
        Ok(record(encoder))
    }

    /// Encode compute work, ending a render or blit pass first when needed.
    pub fn with_compute_encoder<R>(
        &mut self,
        record: impl FnOnce(&ProtocolObject<dyn MTLComputeCommandEncoder>) -> R,
    ) -> Result<R, MetalSchedulerError> {
        if !matches!(
            self.active_encoder.as_ref(),
            Some(ActiveEncoder::Compute(_))
        ) {
            self.end_active_encoder();
            let command_buffer = self.active_command_buffer()?;
            let encoder = command_buffer
                .computeCommandEncoder()
                .ok_or(MetalSchedulerError::NoComputeEncoder)?;
            self.active_encoder = Some(ActiveEncoder::Compute(encoder));
        }
        let Some(ActiveEncoder::Compute(encoder)) = self.active_encoder.as_ref() else {
            unreachable!("compute encoder was installed above")
        };
        Ok(record(encoder))
    }

    /// Start a native render pass. Render-pass compatibility is owned by the
    /// framebuffer runtime, so a new descriptor always closes the prior pass.
    pub fn begin_render_pass(
        &mut self,
        descriptor: &MTLRenderPassDescriptor,
    ) -> Result<(), MetalSchedulerError> {
        self.end_active_encoder();
        let command_buffer = self.active_command_buffer()?;
        let encoder = command_buffer
            .renderCommandEncoderWithDescriptor(descriptor)
            .ok_or(MetalSchedulerError::NoRenderEncoder)?;
        self.active_encoder = Some(ActiveEncoder::Render(encoder));
        Ok(())
    }

    pub fn with_render_encoder<R>(
        &mut self,
        record: impl FnOnce(&ProtocolObject<dyn MTLRenderCommandEncoder>) -> R,
    ) -> Result<R, MetalSchedulerError> {
        let Some(ActiveEncoder::Render(encoder)) = self.active_encoder.as_ref() else {
            return Err(MetalSchedulerError::NoActiveRenderEncoder);
        };
        Ok(record(encoder))
    }

    pub fn end_render_pass(&mut self) {
        if matches!(self.active_encoder.as_ref(), Some(ActiveEncoder::Render(_))) {
            self.end_active_encoder();
        }
    }

    pub fn request_outside_render_pass_operation_context(&mut self) {
        self.end_render_pass();
    }

    fn end_active_encoder(&mut self) {
        if let Some(encoder) = self.active_encoder.take() {
            encoder.end_encoding();
        }
    }

    /// Submit the current guest batch, if any.
    pub fn flush(&mut self) -> Result<Option<u64>, MetalSchedulerError> {
        self.end_active_encoder();
        let Some(command_buffer) = self.active.take() else {
            return Ok(None);
        };
        self.commit(command_buffer).map(Some)
    }

    pub fn commit(
        &mut self,
        command_buffer: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
    ) -> Result<u64, MetalSchedulerError> {
        self.poll_completed()?;
        let tick = self.next_tick;
        self.next_tick = self.next_tick.wrapping_add(1);
        command_buffer.commit();
        self.in_flight.push_back(InFlightCommandBuffer {
            tick,
            command_buffer,
        });
        Ok(tick)
    }

    pub fn finish(
        &mut self,
        command_buffer: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
    ) -> Result<u64, MetalSchedulerError> {
        // Metal executes command buffers in commit order, not allocation
        // order. Preserve the guest order by committing the current batch
        // before the caller-owned buffer that must complete synchronously.
        self.flush()?;
        let tick = self.next_tick;
        self.next_tick = self.next_tick.wrapping_add(1);
        command_buffer.commit();
        command_buffer.waitUntilCompleted();
        Self::check_completed(&command_buffer)?;
        self.known_gpu_tick = self.known_gpu_tick.max(tick);
        self.poll_completed()?;
        Ok(tick)
    }

    pub fn current_tick(&self) -> u64 {
        self.next_tick
    }

    pub fn known_gpu_tick(&mut self) -> Result<u64, MetalSchedulerError> {
        self.poll_completed()?;
        Ok(self.known_gpu_tick)
    }

    pub fn is_free(&mut self, tick: u64) -> Result<bool, MetalSchedulerError> {
        Ok(tick == 0 || tick <= self.known_gpu_tick()?)
    }

    pub fn wait(&mut self, tick: u64) -> Result<(), MetalSchedulerError> {
        if tick >= self.next_tick && self.active.is_some() {
            self.flush()?;
        }
        if tick == 0 || tick <= self.known_gpu_tick()? {
            return Ok(());
        }
        while self
            .in_flight
            .front()
            .is_some_and(|front| front.tick <= tick)
        {
            let completed = self.in_flight.pop_front().unwrap();
            completed.command_buffer.waitUntilCompleted();
            Self::check_completed(&completed.command_buffer)?;
            self.known_gpu_tick = self.known_gpu_tick.max(completed.tick);
        }
        Ok(())
    }

    pub fn finish_all(&mut self) -> Result<(), MetalSchedulerError> {
        self.flush()?;
        while let Some(in_flight) = self.in_flight.pop_front() {
            in_flight.command_buffer.waitUntilCompleted();
            Self::check_completed(&in_flight.command_buffer)?;
            self.known_gpu_tick = self.known_gpu_tick.max(in_flight.tick);
        }
        Ok(())
    }

    fn poll_completed(&mut self) -> Result<(), MetalSchedulerError> {
        while self
            .in_flight
            .front()
            .is_some_and(|entry| is_terminal_status(entry.command_buffer.status()))
        {
            let completed = self.in_flight.pop_front().unwrap();
            Self::check_completed(&completed.command_buffer)?;
            self.known_gpu_tick = self.known_gpu_tick.max(completed.tick);
        }
        Ok(())
    }

    fn check_completed(
        command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
    ) -> Result<(), MetalSchedulerError> {
        let status = command_buffer.status();
        if status == MTLCommandBufferStatus::Completed {
            Ok(())
        } else {
            Err(MetalSchedulerError::CommandBufferFailed(status))
        }
    }
}

fn is_terminal_status(status: MTLCommandBufferStatus) -> bool {
    status == MTLCommandBufferStatus::Completed || status == MTLCommandBufferStatus::Error
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commits_and_waits_for_an_empty_native_command_buffer() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut scheduler = MetalScheduler::new(&device);
        let command_buffer = scheduler.begin().expect("command buffer must be allocated");
        scheduler
            .finish(command_buffer)
            .expect("empty command buffer must complete");
        assert_eq!(scheduler.known_gpu_tick().unwrap(), 1);
    }

    #[test]
    fn tracks_asynchronous_submission_ticks_in_commit_order() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut scheduler = MetalScheduler::new(&device);
        let first = scheduler.begin().unwrap();
        let second = scheduler.begin().unwrap();
        assert_eq!(scheduler.commit(first).unwrap(), 1);
        assert_eq!(scheduler.commit(second).unwrap(), 2);
        scheduler.wait(2).unwrap();
        assert_eq!(scheduler.known_gpu_tick().unwrap(), 2);
    }

    #[test]
    fn batches_active_work_until_flush() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut scheduler = MetalScheduler::new(&device);
        let first = scheduler.active_command_buffer().unwrap();
        let second = scheduler.active_command_buffer().unwrap();
        assert_eq!(Retained::as_ptr(&first), Retained::as_ptr(&second));
        assert_eq!(scheduler.flush().unwrap(), Some(1));
        assert_eq!(scheduler.flush().unwrap(), None);
        scheduler.wait(1).unwrap();
    }

    #[test]
    fn command_buffer_error_is_a_terminal_status() {
        assert!(is_terminal_status(MTLCommandBufferStatus::Completed));
        assert!(is_terminal_status(MTLCommandBufferStatus::Error));
        assert!(!is_terminal_status(MTLCommandBufferStatus::Scheduled));
    }

    #[test]
    fn finish_preserves_active_batch_order_and_monotonic_ticks() {
        let device = MetalDevice::new().expect("Metal device");
        let mut scheduler = MetalScheduler::new(&device);
        scheduler.active_command_buffer().unwrap();
        let synchronous = scheduler.begin().unwrap();
        assert_eq!(scheduler.finish(synchronous).unwrap(), 2);
        assert_eq!(scheduler.known_gpu_tick().unwrap(), 2);
    }
}

// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal buffer allocation and copy operations.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLBlitCommandEncoder, MTLBuffer, MTLDevice, MTLResourceOptions};
use thiserror::Error;

use super::metal_device::MetalDevice;
use super::metal_scheduler::{MetalScheduler, MetalSchedulerError};

#[derive(Debug, Error)]
pub enum MetalBufferError {
    #[error("Metal failed to allocate a {0}-byte buffer")]
    AllocationFailed(usize),
    #[error("buffer range {offset}..{end} exceeds allocation size {length}")]
    RangeOutOfBounds {
        offset: usize,
        end: usize,
        length: usize,
    },
    #[error(transparent)]
    Scheduler(#[from] MetalSchedulerError),
}

/// A shared-storage allocation usable by both the guest upload path and Metal.
///
/// Apple Silicon has unified memory, so `MTLStorageModeShared` is the native
/// zero-copy representation for guest buffer cache allocations. Metal's
/// tracked hazards preserve command-buffer ordering without Vulkan-style
/// access masks.
pub struct MetalBuffer {
    buffer: Retained<ProtocolObject<dyn MTLBuffer>>,
    length: usize,
}

impl MetalBuffer {
    pub fn new(device: &MetalDevice, length: usize) -> Result<Self, MetalBufferError> {
        Self::new_with_options(
            device,
            length,
            MTLResourceOptions::StorageModeShared
                | MTLResourceOptions::CPUCacheModeDefaultCache
                | MTLResourceOptions::HazardTrackingModeTracked,
        )
    }

    pub fn new_stream(device: &MetalDevice, length: usize) -> Result<Self, MetalBufferError> {
        Self::new_with_options(
            device,
            length,
            MTLResourceOptions::StorageModeShared
                | MTLResourceOptions::CPUCacheModeWriteCombined
                | MTLResourceOptions::HazardTrackingModeUntracked,
        )
    }

    pub fn new_private(device: &MetalDevice, length: usize) -> Result<Self, MetalBufferError> {
        Self::new_with_options(
            device,
            length,
            MTLResourceOptions::StorageModePrivate | MTLResourceOptions::HazardTrackingModeTracked,
        )
    }

    fn new_with_options(
        device: &MetalDevice,
        length: usize,
        options: MTLResourceOptions,
    ) -> Result<Self, MetalBufferError> {
        // Metal rejects zero-byte buffers. The common cache's null object still
        // needs a bindable native resource, matching Eden's reserved null
        // buffer behavior.
        let allocation_length = length.max(4);
        let buffer = device
            .device()
            .newBufferWithLength_options(allocation_length, options)
            .ok_or(MetalBufferError::AllocationFailed(allocation_length))?;
        Ok(Self {
            buffer,
            length: allocation_length,
        })
    }

    pub fn handle(&self) -> &ProtocolObject<dyn MTLBuffer> {
        &self.buffer
    }

    pub fn length(&self) -> usize {
        self.length
    }

    pub fn raw_handle(&self) -> u64 {
        Retained::as_ptr(&self.buffer) as usize as u64
    }

    pub(crate) fn contents_ptr(&self) -> *mut u8 {
        self.buffer.contents().as_ptr().cast::<u8>()
    }

    pub(crate) fn retained_handle(&self) -> Retained<ProtocolObject<dyn MTLBuffer>> {
        self.buffer.clone()
    }

    pub fn write(&self, offset: usize, data: &[u8]) -> Result<(), MetalBufferError> {
        let range = self.checked_range(offset, data.len())?;
        unsafe {
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                self.buffer
                    .contents()
                    .as_ptr()
                    .cast::<u8>()
                    .add(range.start),
                data.len(),
            );
        }
        Ok(())
    }

    pub fn read(&self, offset: usize, data: &mut [u8]) -> Result<(), MetalBufferError> {
        let range = self.checked_range(offset, data.len())?;
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.buffer
                    .contents()
                    .as_ptr()
                    .cast::<u8>()
                    .add(range.start),
                data.as_mut_ptr(),
                data.len(),
            );
        }
        Ok(())
    }

    pub fn encode_copy(
        &self,
        scheduler: &mut MetalScheduler,
        destination: &MetalBuffer,
        source_offset: usize,
        destination_offset: usize,
        size: usize,
    ) -> Result<(), MetalBufferError> {
        self.checked_range(source_offset, size)?;
        destination.checked_range(destination_offset, size)?;
        scheduler.with_blit_encoder(|encoder| unsafe {
            encoder.copyFromBuffer_sourceOffset_toBuffer_destinationOffset_size(
                self.handle(),
                source_offset,
                destination.handle(),
                destination_offset,
                size,
            )
        })?;
        Ok(())
    }

    fn checked_range(
        &self,
        offset: usize,
        size: usize,
    ) -> Result<std::ops::Range<usize>, MetalBufferError> {
        let end = offset
            .checked_add(size)
            .ok_or(MetalBufferError::RangeOutOfBounds {
                offset,
                end: usize::MAX,
                length: self.length,
            })?;
        if end > self.length {
            return Err(MetalBufferError::RangeOutOfBounds {
                offset,
                end,
                length: self.length,
            });
        }
        Ok(offset..end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer_metal::metal_scheduler::MetalScheduler;

    #[test]
    fn copies_shared_buffers_on_the_native_metal_queue() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut scheduler = MetalScheduler::new(&device);
        let source = MetalBuffer::new(&device, 32).expect("source buffer allocation");
        let destination = MetalBuffer::new(&device, 32).expect("destination buffer allocation");
        source.write(4, &[1, 2, 3, 4, 5]).unwrap();

        source
            .encode_copy(&mut scheduler, &destination, 4, 12, 5)
            .unwrap();
        source
            .encode_copy(&mut scheduler, &destination, 5, 20, 4)
            .unwrap();
        // Switching encoder kinds must end the shared blit encoder before
        // opening compute on the same command buffer.
        scheduler.with_compute_encoder(|_| {}).unwrap();
        scheduler.finish_all().unwrap();

        let mut copied = [0; 5];
        destination.read(12, &mut copied).unwrap();
        assert_eq!(copied, [1, 2, 3, 4, 5]);
        destination.read(20, &mut copied[..4]).unwrap();
        assert_eq!(&copied[..4], [2, 3, 4, 5]);
    }

    #[test]
    fn rejects_out_of_bounds_cpu_and_gpu_ranges() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let buffer = MetalBuffer::new(&device, 16).unwrap();
        assert!(matches!(
            buffer.write(15, &[1, 2]),
            Err(MetalBufferError::RangeOutOfBounds { .. })
        ));
    }
}

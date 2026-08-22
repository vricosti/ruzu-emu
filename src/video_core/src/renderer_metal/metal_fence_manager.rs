// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal fence object used by the common fence manager.
//!
//! A retained `MTLCommandBuffer` is Metal's completion primitive. Multiple
//! guest fences may refer to the same command buffer; Objective-C retention
//! keeps it alive until every delayed operation has observed completion.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLCommandBuffer, MTLCommandBufferStatus};

use crate::fence_manager::{FenceBase, FenceManager};

pub type MetalFenceManager = FenceManager<MetalFence>;

pub struct MetalFence {
    command_buffer: Option<Retained<ProtocolObject<dyn MTLCommandBuffer>>>,
}

// SAFETY: Metal command buffers are thread-safe Objective-C objects. This
// wrapper only retains them, reads their status, and calls
// `waitUntilCompleted`; it never mutates an encoder or records commands from
// the fence-manager thread. objc2 cannot express that guarantee on protocol
// objects, so the backend supplies the same cross-thread ownership contract
// used by Eden's fence manager.
unsafe impl Send for MetalFence {}

impl MetalFence {
    pub fn stubbed() -> Self {
        Self {
            command_buffer: None,
        }
    }

    pub fn from_command_buffer(
        command_buffer: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
    ) -> Self {
        Self {
            command_buffer: Some(command_buffer),
        }
    }

    pub fn is_signaled(&self) -> bool {
        self.command_buffer.as_ref().is_none_or(|command_buffer| {
            matches!(
                command_buffer.status(),
                MTLCommandBufferStatus::Completed | MTLCommandBufferStatus::Error
            )
        })
    }
}

impl FenceBase for MetalFence {
    fn is_stubbed(&self) -> bool {
        self.command_buffer.is_none()
    }

    fn wait_for_fence(&self) {
        let Some(command_buffer) = self.command_buffer.as_ref() else {
            return;
        };
        command_buffer.waitUntilCompleted();
        if command_buffer.status() != MTLCommandBufferStatus::Completed {
            log::error!(
                "Metal fence command buffer completed with status {:?}",
                command_buffer.status()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer_metal::metal_device::MetalDevice;
    use crate::renderer_metal::metal_scheduler::MetalScheduler;

    #[test]
    fn retained_command_buffer_is_a_real_fence() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut scheduler = MetalScheduler::new(&device);
        let command_buffer = scheduler.active_command_buffer().unwrap();
        let fence = MetalFence::from_command_buffer(command_buffer);

        assert!(!fence.is_stubbed());
        assert!(!fence.is_signaled());
        scheduler.flush().unwrap();
        fence.wait_for_fence();
        assert!(fence.is_signaled());
    }

    #[test]
    fn stubbed_fence_is_immediately_signaled() {
        let fence = MetalFence::stubbed();
        assert!(fence.is_stubbed());
        assert!(fence.is_signaled());
        fence.wait_for_fence();
    }
}

// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GPU engine trait and subchannel definitions.
//!
//! The Tegra X1 GPU has several engines, each responsible for a class of
//! operations. Engines are addressed by subchannel in GPFIFO commands.

pub mod const_buffer_info;
pub mod draw_manager;
pub mod engine_interface;
pub mod engine_upload;
pub mod fermi_2d;
pub mod inline_to_memory;
pub mod kepler_compute;
pub mod kepler_memory;
pub mod maxwell_3d;
pub mod maxwell_dma;
pub mod nv01_timer;
pub mod puller;
pub mod sw_blitter;

/// GPU engine class IDs (NV device class numbers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ClassId {
    Twod = 0x902D,
    Threed = 0xB197,
    Compute = 0xB1C0,
    InlineToMemory = 0xA140,
    Dma = 0xB0B5,
}

/// GPFIFO subchannel assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SubChannel {
    Maxwell3D = 0,
    Compute = 1,
    InlineToMemory = 2,
    Fermi2D = 3,
    MaxwellDMA = 4,
}

impl SubChannel {
    pub fn from_raw(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Maxwell3D),
            1 => Some(Self::Compute),
            2 => Some(Self::InlineToMemory),
            3 => Some(Self::Fermi2D),
            4 => Some(Self::MaxwellDMA),
            _ => None,
        }
    }
}

/// Describes a pending write from an engine to GPU VA space.
pub struct PendingWrite {
    /// Destination GPU virtual address.
    pub gpu_va: u64,
    /// Data to write.
    pub data: Vec<u8>,
}

/// Trait for a GPU engine that accepts register writes.
pub trait Engine: Send {
    /// The class ID of this engine.
    fn class_id(&self) -> ClassId;

    /// Write a value to a method register.
    fn write_reg(&mut self, method: u32, value: u32);

    /// Collect any rendered framebuffer output. Returns `None` if nothing new.
    fn take_framebuffer(&mut self) -> Option<Framebuffer> {
        None
    }

    /// Execute pending operations that need memory access (blit, DMA copy).
    /// Reads source data via `read_gpu`, returns writes to be applied.
    fn execute_pending(&mut self, _read_gpu: &dyn Fn(u64, &mut [u8])) -> Vec<PendingWrite> {
        vec![]
    }

    /// Drain accumulated draw calls. Only Maxwell3D produces draw calls.
    fn take_draw_calls(&mut self) -> Vec<maxwell_3d::DrawCall> {
        vec![]
    }
}

/// Number of registers per engine.
/// Upstream: `Regs::NUM_REGS = 0xE00`. Register array indexed by word (u32) offset.
/// GPU methods are word indices: method M writes reg_array[M].
pub const ENGINE_REG_COUNT: usize = 0xE00;

/// Rendered framebuffer output from an engine.
pub struct Framebuffer {
    /// Render target GPU virtual address.
    pub gpu_va: u64,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// RGBA8 pixel data, row-major, width*height*4 bytes.
    pub pixels: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subchannel_from_raw() {
        assert_eq!(SubChannel::from_raw(0), Some(SubChannel::Maxwell3D));
        assert_eq!(SubChannel::from_raw(4), Some(SubChannel::MaxwellDMA));
        assert_eq!(SubChannel::from_raw(5), None);
    }

    #[test]
    fn test_class_ids() {
        assert_eq!(ClassId::Threed as u32, 0xB197);
        assert_eq!(ClassId::Compute as u32, 0xB1C0);
        assert_eq!(ClassId::Dma as u32, 0xB0B5);
    }
}

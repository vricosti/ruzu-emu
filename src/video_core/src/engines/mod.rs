// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GPU engine module declarations and shared compatibility payloads.
//!
//! The Tegra X1 GPU has several engines, each responsible for a class of
//! operations. Engine identifiers and pushbuffer dispatch remain owned by
//! `puller`, matching Eden.

pub mod const_buffer_info;
pub mod draw_manager;
pub mod engine_interface;
pub mod engine_upload;
pub mod fermi_2d;
#[cfg(test)]
pub mod inline_to_memory;
pub mod kepler_compute;
pub mod kepler_memory;
pub mod maxwell_3d;
pub mod maxwell_dma;
pub mod nv01_timer;
pub mod puller;
pub mod sw_blitter;

/// Describes a pending write from an engine to GPU VA space.
pub struct PendingWrite {
    /// Destination GPU virtual address.
    pub gpu_va: u64,
    /// Data to write.
    pub data: Vec<u8>,
}

/// Number of registers per engine.
/// Upstream: `Regs::NUM_REGS = 0xE00`. Register array indexed by word (u32) offset.
/// GPU methods are word indices: method M writes reg_array[M].
pub const ENGINE_REG_COUNT: usize = 0xE00;

// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/engines/engine_upload.h and engine_upload.cpp
//!
//! Implements the GPU inline-to-memory upload mechanism (P2MF / I2M).
//! Engines that support inline uploads (KeplerMemory, Maxwell3D) embed
//! an `upload::State` that accumulates data words and flushes them to
//! GPU virtual memory when the transfer completes.

use crate::rasterizer_interface::{RasterizerHandle, RasterizerInterface};
use crate::textures::decoders;
use parking_lot::Mutex;
use std::sync::Arc;

/// GPU virtual address type.
pub type GPUVAddr = u64;

/// Upload destination registers, corresponding to the C++ `Upload::Registers` struct.
///
/// Layout matches upstream at offset 0x60 within the engine register file.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct Registers {
    pub line_length_in: u32,
    pub line_count: u32,
    pub dest: DestRegisters,
}

/// Destination surface descriptor embedded in `Registers`.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct DestRegisters {
    pub address_high: u32,
    pub address_low: u32,
    pub pitch: u32,
    /// Packed block dimensions: bits [0:3] = width, [4:7] = height, [8:11] = depth.
    pub block_dims: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub layer: u32,
    pub x: u32,
    pub y: u32,
}

impl DestRegisters {
    /// Compute the full GPU virtual address from high and low halves.
    pub fn address(&self) -> GPUVAddr {
        ((self.address_high as u64) << 32) | (self.address_low as u64)
    }

    /// Block width exponent (bits [0:3]).
    pub fn block_width(&self) -> u32 {
        self.block_dims & 0xF
    }

    /// Block height exponent (bits [4:7]).
    pub fn block_height(&self) -> u32 {
        (self.block_dims >> 4) & 0xF
    }

    /// Block depth exponent (bits [8:11]).
    pub fn block_depth(&self) -> u32 {
        (self.block_dims >> 8) & 0xF
    }
}

/// Upload state machine, corresponding to the C++ `Upload::State` class.
///
/// Accumulates inline data words and writes them to GPU memory when
/// a transfer is completed.
///
/// Upstream holds `MemoryManager&` and a rasterizer pointer bound through
/// `BindRasterizer`. Rust stores the same owner edges through an `Arc<Mutex<_>>`
/// memory-manager handle and a non-owning `RasterizerHandle`.
///
/// Eden also stores `Registers&` in this object. The Rust engine owners keep
/// their register files in the owning engine, so each entry point receives the
/// corresponding register view explicitly. This avoids a self-referential
/// engine while preserving Eden's call-boundary register snapshot.
pub struct State {
    /// Current write offset within `inner_buffer`.
    write_offset: u32,
    /// Total number of bytes to copy for this transfer.
    copy_size: u32,
    /// Accumulation buffer for incoming data words.
    inner_buffer: Vec<u8>,
    /// Temporary buffer for block-linear swizzle operations.
    tmp_buffer: Vec<u8>,
    /// Whether the current transfer target is pitch-linear (vs block-linear).
    is_linear: bool,
    /// Upstream `MemoryManager& memory_manager`.
    memory_manager: Option<Arc<Mutex<crate::memory_manager::MemoryManager>>>,
    /// Upstream `VideoCore::RasterizerInterface* rasterizer`.
    rasterizer: Option<RasterizerHandle>,
}

impl State {
    /// Create a reduced upload state without its upstream `MemoryManager&`.
    ///
    /// Runtime owners call `new_with_memory_manager`. This reduced constructor
    /// stays crate-local so ownerless upload state cannot be introduced through
    /// the public runtime API by accident.
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self {
            write_offset: 0,
            copy_size: 0,
            inner_buffer: Vec::new(),
            tmp_buffer: Vec::new(),
            is_linear: false,
            memory_manager: None,
            rasterizer: None,
        }
    }

    /// Create a new upload state with the upstream-owned `MemoryManager`.
    pub fn new_with_memory_manager(
        memory_manager: Arc<Mutex<crate::memory_manager::MemoryManager>>,
    ) -> Self {
        Self {
            write_offset: 0,
            copy_size: 0,
            inner_buffer: Vec::new(),
            tmp_buffer: Vec::new(),
            is_linear: false,
            memory_manager: Some(memory_manager),
            rasterizer: None,
        }
    }

    /// Bind the upstream `MemoryManager&` owner after reduced construction.
    #[cfg(test)]
    pub(crate) fn bind_memory_manager(
        &mut self,
        memory_manager: Arc<Mutex<crate::memory_manager::MemoryManager>>,
    ) {
        self.memory_manager = Some(memory_manager);
    }

    /// Binds a rasterizer to this engine.
    ///
    /// Corresponds to upstream `State::BindRasterizer`.
    pub fn bind_rasterizer(&mut self, rasterizer: &dyn RasterizerInterface) {
        self.rasterizer = Some(RasterizerHandle::from_ref(rasterizer));
    }

    /// Begin a new transfer. Called when the engine's exec register is written.
    ///
    /// Corresponds to `State::ProcessExec`.
    pub fn process_exec(&mut self, regs: &Registers, is_linear: bool) {
        self.write_offset = 0;
        self.copy_size = regs.line_length_in.wrapping_mul(regs.line_count);
        self.inner_buffer.resize(self.copy_size as usize, 0);
        self.is_linear = is_linear;
    }

    /// Append a single data word to the transfer buffer.
    ///
    /// Corresponds to `State::ProcessData(u32, bool)`.
    pub fn process_data_word(&mut self, regs: &Registers, data: u32, is_last_call: bool) {
        self.accumulate_word(data);
        if is_last_call {
            Self::process_data_bytes(
                self.is_linear,
                self.memory_manager.as_ref(),
                self.rasterizer,
                &mut self.tmp_buffer,
                regs,
                &self.inner_buffer,
            );
        }
    }

    /// Append multiple data words to the transfer buffer.
    ///
    /// Corresponds to `State::ProcessData(const u32*, size_t)`.
    pub fn process_data_multi(&mut self, regs: &Registers, data: &[u32]) {
        // Safe conversion: reinterpret &[u32] as &[u8] matching C++ reinterpret_cast.
        let byte_view: &[u8] =
            unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4) };
        Self::process_data_bytes(
            self.is_linear,
            self.memory_manager.as_ref(),
            self.rasterizer,
            &mut self.tmp_buffer,
            regs,
            byte_view,
        );
    }

    /// Target GPU virtual address for the current transfer.
    pub fn exec_target_address(&self, regs: &Registers) -> GPUVAddr {
        regs.dest.address()
    }

    /// Total upload size in bytes.
    pub fn get_upload_size(&self) -> u32 {
        self.copy_size
    }

    // ── Internal helpers ─────────────────────────────────────────────────

    /// Accumulate a single u32 data word into the inner buffer.
    fn accumulate_word(&mut self, data: u32) {
        let sub_copy_size =
            std::cmp::min(4, self.copy_size.wrapping_sub(self.write_offset)) as usize;
        let bytes = data.to_ne_bytes();
        let offset = self.write_offset as usize;
        self.inner_buffer[offset..offset + sub_copy_size].copy_from_slice(&bytes[..sub_copy_size]);
        self.write_offset = self.write_offset.wrapping_add(sub_copy_size as u32);
    }

    /// Flush data to GPU memory.
    ///
    /// Corresponds to `State::ProcessData(span<const u8>)`.
    /// Upstream logic:
    ///   - Linear: iterate lines, call rasterizer->AccelerateInlineToMemory per line.
    ///   - Block-linear: compute BPP shift, read GPU memory, swizzle subrect, write back.
    fn process_data_bytes(
        is_linear: bool,
        memory_manager: Option<&Arc<Mutex<crate::memory_manager::MemoryManager>>>,
        rasterizer: Option<RasterizerHandle>,
        tmp_buffer: &mut Vec<u8>,
        regs: &Registers,
        read_buffer: &[u8],
    ) {
        let address = regs.dest.address();
        if is_linear {
            // Linear copy: iterate lines, call rasterizer->AccelerateInlineToMemory
            // for each line. Upstream:
            //   for (line = 0; line < line_count; ++line) {
            //       dest_line = address + line * dest.pitch;
            //       buffer = read_buffer[line * line_length_in .. +line_length_in];
            //       rasterizer->AccelerateInlineToMemory(dest_line, line_length_in, buffer);
            //   }
            let rasterizer =
                rasterizer.expect("linear inline upload requires the upstream-bound rasterizer");
            let rasterizer = unsafe { rasterizer.as_mut() };
            for line in 0..regs.line_count as usize {
                let dest_line =
                    address.wrapping_add((line as u64).wrapping_mul(regs.dest.pitch as u64));
                let start = line.wrapping_mul(regs.line_length_in as usize);
                let end = start.wrapping_add(regs.line_length_in as usize);
                let buffer = &read_buffer[start..end];
                rasterizer.accelerate_inline_to_memory(
                    dest_line,
                    regs.line_length_in as usize,
                    buffer,
                );
            }
        } else {
            let memory_manager = memory_manager
                .expect("block-linear inline upload requires the upstream MemoryManager owner");
            // Block-linear copy: calculate BPP shift, swizzle subrect.
            // Upstream uses Common::FoldRight to compute bpp_shift as:
            //   min(4, min of countr_zero(width), countr_zero(x_elements),
            //       countr_zero(x_offset), countr_zero(address))
            let mut width = regs.dest.width;
            let mut x_elements = regs.line_length_in;
            let mut x_offset = regs.dest.x;

            // Compute bpp_shift matching upstream FoldRight(4, min(x, countr_zero(y)), ...)
            let bpp_shift = [width, x_elements, x_offset, address as u32]
                .iter()
                .fold(4u32, |acc, &val| acc.min(val.trailing_zeros()));

            width >>= bpp_shift;
            x_elements >>= bpp_shift;
            x_offset >>= bpp_shift;
            let bytes_per_pixel = 1u32 << bpp_shift;

            let dst_size = decoders::calculate_size(
                true,
                bytes_per_pixel,
                width,
                regs.dest.height,
                regs.dest.depth,
                regs.dest.block_height(),
                regs.dest.block_depth(),
            );

            // Read existing GPU memory into tmp_buffer. Upstream uses
            // GpuGuestMemoryScoped<SafeReadCachedWrite>.
            tmp_buffer.resize(dst_size, 0);
            let mut memory_manager = memory_manager.lock();
            memory_manager.read_block(address, tmp_buffer);

            // Swizzle the upload data into the tiled buffer.
            decoders::swizzle_subrect(
                tmp_buffer,
                read_buffer,
                bytes_per_pixel,
                width,
                regs.dest.height,
                regs.dest.depth,
                x_offset,
                regs.dest.y,
                x_elements,
                regs.line_count,
                regs.dest.block_height(),
                regs.dest.block_depth(),
                regs.line_length_in,
            );

            // Write the swizzled buffer back to GPU memory with the upstream cached-write path.
            memory_manager.write_block_cached(address, tmp_buffer);
        }
    }
}

#[cfg(test)]
impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{DestRegisters, Registers, State};

    #[test]
    fn register_layout_matches_eden() {
        assert_eq!(std::mem::size_of::<DestRegisters>(), 0x28);
        assert_eq!(std::mem::align_of::<DestRegisters>(), 4);
        assert_eq!(std::mem::offset_of!(DestRegisters, address_high), 0x00);
        assert_eq!(std::mem::offset_of!(DestRegisters, address_low), 0x04);
        assert_eq!(std::mem::offset_of!(DestRegisters, pitch), 0x08);
        assert_eq!(std::mem::offset_of!(DestRegisters, block_dims), 0x0c);
        assert_eq!(std::mem::offset_of!(DestRegisters, width), 0x10);
        assert_eq!(std::mem::offset_of!(DestRegisters, height), 0x14);
        assert_eq!(std::mem::offset_of!(DestRegisters, depth), 0x18);
        assert_eq!(std::mem::offset_of!(DestRegisters, layer), 0x1c);
        assert_eq!(std::mem::offset_of!(DestRegisters, x), 0x20);
        assert_eq!(std::mem::offset_of!(DestRegisters, y), 0x24);

        assert_eq!(std::mem::size_of::<Registers>(), 0x30);
        assert_eq!(std::mem::align_of::<Registers>(), 4);
        assert_eq!(std::mem::offset_of!(Registers, line_length_in), 0x00);
        assert_eq!(std::mem::offset_of!(Registers, line_count), 0x04);
        assert_eq!(std::mem::offset_of!(Registers, dest), 0x08);
    }

    #[test]
    fn destination_helpers_match_eden_bitfields() {
        let dest = DestRegisters {
            address_high: 0x0123_4567,
            address_low: 0x89ab_cdef,
            block_dims: 0xffff_fa95,
            ..Default::default()
        };

        assert_eq!(dest.address(), 0x0123_4567_89ab_cdef);
        assert_eq!(dest.block_width(), 5);
        assert_eq!(dest.block_height(), 9);
        assert_eq!(dest.block_depth(), 10);
    }

    #[test]
    fn data_word_uses_native_memcpy_byte_order() {
        let mut state = State::new();
        let regs = Registers {
            line_length_in: 4,
            line_count: 1,
            ..Default::default()
        };
        state.process_exec(&regs, true);
        state.accumulate_word(0x0123_4567);

        assert_eq!(state.inner_buffer, 0x0123_4567u32.to_ne_bytes());
        assert_eq!(state.write_offset, 4);
    }
}

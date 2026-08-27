// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Maxwell DMA engine (NV class B0B5).
//!
//! Handles the pitch-linear DMA copy subset used by early renderer paths.

use std::sync::Arc;

use parking_lot::Mutex;

use super::engine_interface::{EngineInterface, EngineInterfaceState};
use crate::memory_manager::MemoryManager;
use crate::pte_kind::{is_pitch_kind, PteKind};
use crate::query_cache::types::{QueryPropertiesFlags, QueryType};
use crate::rasterizer_interface::{RasterizerHandle, RasterizerInterface};
use crate::textures::decoders::{calculate_size, swizzle_subrect, unswizzle_subrect};

/// Number of MaxwellDMA method registers (`MaxwellDMA::NUM_REGS`).
const NUM_REGS: usize = 0x800;

/// Deferred guest-memory write used by the Rust engine integration.
pub(crate) struct PendingWrite {
    pub gpu_va: u64,
    pub data: Vec<u8>,
}

/// Port of upstream `Tegra::DMA` helper structs in `engines/maxwell_dma.h`.
pub mod dma {
    pub type GPUVAddr = u64;

    /// Port of `Tegra::DMA::Origin`.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Origin {
        pub raw: u32,
    }

    impl Origin {
        pub fn x(self) -> u32 {
            self.raw & 0xffff
        }

        pub fn y(self) -> u32 {
            self.raw >> 16
        }
    }

    /// Port of `Tegra::DMA::BlockSize`.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct BlockSize {
        pub raw: u32,
    }

    impl BlockSize {
        pub fn width(self) -> u32 {
            self.raw & 0xf
        }

        pub fn height(self) -> u32 {
            (self.raw >> 4) & 0xf
        }

        pub fn depth(self) -> u32 {
            (self.raw >> 8) & 0xf
        }

        pub fn gob_height(self) -> u32 {
            (self.raw >> 12) & 0xf
        }
    }

    /// Port of `Tegra::DMA::Parameters`.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Parameters {
        pub block_size: BlockSize,
        pub width: u32,
        pub height: u32,
        pub depth: u32,
        pub layer: u32,
        pub origin: Origin,
    }

    /// Port of `Tegra::DMA::ImageOperand`.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct ImageOperand {
        pub bytes_per_pixel: u32,
        pub params: Parameters,
        pub address: GPUVAddr,
    }

    /// Port of `Tegra::DMA::ImageCopy`.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct ImageCopy {
        pub length_x: u32,
        pub length_y: u32,
    }

    /// Port of `Tegra::DMA::BufferOperand`.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct BufferOperand {
        pub pitch: u32,
        pub width: u32,
        pub height: u32,
        pub address: GPUVAddr,
    }
}

/// Backend-owned DMA acceleration interface.
///
/// This is the Rust counterpart of upstream
/// `Tegra::Engines::AccelerateDMAInterface` in `maxwell_dma.h`.  The concrete
/// object belongs to the rasterizer backend; `MaxwellDMA` only obtains it
/// through `RasterizerInterface::access_accelerate_dma`.
pub trait AccelerateDMAInterface {
    fn buffer_copy(&mut self, src_address: u64, dest_address: u64, amount: u64) -> bool;

    fn buffer_clear(&mut self, dst_address: u64, amount: u64, value: u32) -> bool;

    fn image_to_buffer(
        &mut self,
        copy_info: &dma::ImageCopy,
        src: &dma::ImageOperand,
        dst: &dma::BufferOperand,
    ) -> bool;

    fn buffer_to_image(
        &mut self,
        copy_info: &dma::ImageCopy,
        src: &dma::BufferOperand,
        dst: &dma::ImageOperand,
    ) -> bool;
}

// ── Register constants (method = byte_offset / 4) ──────────────────────────

const LAUNCH_DMA: u32 = 0xC0;

const SEMAPHORE_ADDR_HIGH: u32 = 0x90;
const SEMAPHORE_ADDR_LOW: u32 = 0x91;
const SEMAPHORE_PAYLOAD: u32 = 0x92;

const SRC_ADDR_HIGH: u32 = 0x100;
const SRC_ADDR_LOW: u32 = 0x101;
const DST_ADDR_HIGH: u32 = 0x102;
const DST_ADDR_LOW: u32 = 0x103;

const PITCH_IN: u32 = 0x104;
const PITCH_OUT: u32 = 0x105;
const LINE_LENGTH: u32 = 0x106;
const LINE_COUNT: u32 = 0x107;
const REMAP_CONSTA_VALUE: u32 = 0x1C0;
const REMAP_COMPONENTS: u32 = 0x1C2;
const DST_PARAMS: u32 = 0x1C3;
const SRC_PARAMS: u32 = 0x1CA;

#[cfg(test)]
const LAUNCH_DATA_TRANSFER_TYPE_MASK: u32 = 0x3;
#[cfg(test)]
const LAUNCH_DATA_TRANSFER_NON_PIPELINED: u32 = 2;
const LAUNCH_SEMAPHORE_TYPE_SHIFT: u32 = 3;
const LAUNCH_SEMAPHORE_TYPE_MASK: u32 = 0x3;
const LAUNCH_SEMAPHORE_TYPE_NONE: u32 = 0;
const LAUNCH_SEMAPHORE_TYPE_RELEASE_ONE_WORD: u32 = 1;
const LAUNCH_SEMAPHORE_TYPE_RELEASE_FOUR_WORD: u32 = 2;
const LAUNCH_INTERRUPT_TYPE_SHIFT: u32 = 5;
const LAUNCH_INTERRUPT_TYPE_MASK: u32 = 0x3;
const LAUNCH_INTERRUPT_TYPE_NONE: u32 = 0;
const LAUNCH_SRC_MEMORY_LAYOUT_PITCH: u32 = 1 << 7;
const LAUNCH_DST_MEMORY_LAYOUT_PITCH: u32 = 1 << 8;
const LAUNCH_MULTI_LINE_ENABLE: u32 = 1 << 9;
const LAUNCH_REMAP_ENABLE: u32 = 1 << 10;
const REMAP_SWIZZLE_CONST_A: u32 = 4;

fn convert_linear_2_blocklinear_addr(address: u64) -> u64 {
    (address & !0x1f0)
        | ((address & 0x40) >> 2)
        | ((address & 0x10) << 1)
        | ((address & 0x180) >> 1)
        | ((address & 0x20) << 3)
}

pub struct MaxwellDMA {
    regs: Box<[u32; NUM_REGS]>,
    interface_state: EngineInterfaceState,
    memory_manager: Arc<Mutex<MemoryManager>>,
    /// Set when a DMA launch trigger is detected; consumed by tests / future logic.
    pub pending_launch: bool,
    /// Upstream stores `VideoCore::RasterizerInterface* rasterizer`.
    rasterizer: Option<RasterizerHandle>,
}

impl MaxwellDMA {
    /// Corresponds to upstream `MaxwellDMA(Core::System&, MemoryManager&)`.
    /// Rust stores the upstream `MemoryManager&` owner directly; the broader
    /// `System&` constructor dependency remains outside this bounded slice.
    pub fn new(memory_manager: Arc<Mutex<MemoryManager>>) -> Self {
        Self {
            regs: Box::new([0u32; NUM_REGS]),
            interface_state: {
                let mut state = EngineInterfaceState::new();
                state.execution_mask[LAUNCH_DMA as usize] = true;
                state
            },
            memory_manager,
            pending_launch: false,
            rasterizer: None,
        }
    }

    /// Corresponds to upstream `MaxwellDMA::CallMethod`.
    pub fn call_method(&mut self, method: u32, argument: u32, _is_last_call: bool) {
        let idx = method as usize;
        assert!(idx < NUM_REGS, "Invalid MaxwellDMA register");
        self.regs[idx] = argument;
        if method == LAUNCH_DMA {
            self.log_launch();
            self.launch_immediate();
        }
    }

    /// Corresponds to upstream `MaxwellDMA::CallMultiMethod`.
    pub fn call_multi_method(
        &mut self,
        method: u32,
        args: &[u32],
        amount: u32,
        methods_pending: u32,
    ) {
        assert!(
            args.len() >= amount as usize,
            "MaxwellDMA::call_multi_method needs {amount} arguments, got {}",
            args.len()
        );
        for i in 0..amount {
            self.call_method(
                method,
                args[i as usize],
                methods_pending.wrapping_sub(i) <= 1,
            );
        }
    }

    /// Corresponds to `MaxwellDMA::BindRasterizer`.
    pub fn bind_rasterizer(&mut self, rasterizer: &dyn RasterizerInterface) {
        self.rasterizer = Some(RasterizerHandle::from_ref(rasterizer));
    }

    // ── Typed accessors ────────────────────────────────────────────────

    pub fn src_addr(&self) -> u64 {
        ((self.regs[SRC_ADDR_HIGH as usize] as u64) << 32)
            | (self.regs[SRC_ADDR_LOW as usize] as u64)
    }

    pub fn dst_addr(&self) -> u64 {
        ((self.regs[DST_ADDR_HIGH as usize] as u64) << 32)
            | (self.regs[DST_ADDR_LOW as usize] as u64)
    }

    pub fn pitch_in(&self) -> u32 {
        self.regs[PITCH_IN as usize]
    }

    pub fn pitch_out(&self) -> u32 {
        self.regs[PITCH_OUT as usize]
    }

    pub fn line_length(&self) -> u32 {
        self.regs[LINE_LENGTH as usize]
    }

    pub fn line_count(&self) -> u32 {
        self.regs[LINE_COUNT as usize]
    }

    fn semaphore_addr(&self) -> u64 {
        (((self.regs[SEMAPHORE_ADDR_HIGH as usize] & 0xff) as u64) << 32)
            | self.regs[SEMAPHORE_ADDR_LOW as usize] as u64
    }

    fn semaphore_payload(&self) -> u32 {
        self.regs[SEMAPHORE_PAYLOAD as usize]
    }

    fn remap_consta_value(&self) -> u32 {
        self.regs[REMAP_CONSTA_VALUE as usize]
    }

    fn remap_components(&self) -> u32 {
        self.regs[REMAP_COMPONENTS as usize]
    }

    fn launch_dma(&self) -> u32 {
        self.regs[LAUNCH_DMA as usize]
    }

    #[cfg(test)]
    fn launch_data_transfer_type(&self) -> u32 {
        self.launch_dma() & LAUNCH_DATA_TRANSFER_TYPE_MASK
    }

    fn launch_semaphore_type(&self) -> u32 {
        (self.launch_dma() >> LAUNCH_SEMAPHORE_TYPE_SHIFT) & LAUNCH_SEMAPHORE_TYPE_MASK
    }

    fn launch_interrupt_type(&self) -> u32 {
        (self.launch_dma() >> LAUNCH_INTERRUPT_TYPE_SHIFT) & LAUNCH_INTERRUPT_TYPE_MASK
    }

    fn launch_multi_line_enable(&self) -> bool {
        (self.launch_dma() & LAUNCH_MULTI_LINE_ENABLE) != 0
    }

    fn launch_src_is_pitch(&self) -> bool {
        (self.launch_dma() & LAUNCH_SRC_MEMORY_LAYOUT_PITCH) != 0
    }

    fn launch_dst_is_pitch(&self) -> bool {
        (self.launch_dma() & LAUNCH_DST_MEMORY_LAYOUT_PITCH) != 0
    }

    fn launch_remap_enable(&self) -> bool {
        (self.launch_dma() & LAUNCH_REMAP_ENABLE) != 0
    }

    fn remap_dst_x(&self) -> u32 {
        self.remap_components() & 0x7
    }

    fn remap_component_size_minus_one(&self) -> u32 {
        (self.remap_components() >> 16) & 0x3
    }

    fn remap_num_dst_components_minus_one(&self) -> u32 {
        (self.remap_components() >> 24) & 0x3
    }

    fn dst_params(&self) -> dma::Parameters {
        self.parameters_at(DST_PARAMS)
    }

    fn src_params(&self) -> dma::Parameters {
        self.parameters_at(SRC_PARAMS)
    }

    fn parameters_at(&self, base: u32) -> dma::Parameters {
        let base = base as usize;
        dma::Parameters {
            block_size: dma::BlockSize {
                raw: self.regs[base],
            },
            width: self.regs[base + 1],
            height: self.regs[base + 2],
            depth: self.regs[base + 3],
            layer: self.regs[base + 4],
            origin: dma::Origin {
                raw: self.regs[base + 5],
            },
        }
    }

    fn page_kind_is_pitch(&self, gpu_addr: u64) -> bool {
        let kind = PteKind::from_raw(self.memory_manager.lock().get_page_kind_raw(gpu_addr) as u8);
        is_pitch_kind(kind)
    }

    fn report_unimplemented_dma_path(&self, reason: &str) {
        // Eden's UNIMPLEMENTED_IF is an ASSERT and therefore reports through
        // the configured fail-soft policy before continuing.  Do not turn
        // guest input into a host-side file write or an unconditional panic.
        log::error!(
            "MaxwellDMA unsupported path: {} (launch=0x{:08X} src=0x{:X} dst=0x{:X})",
            reason,
            self.launch_dma(),
            self.src_addr(),
            self.dst_addr(),
        );
    }

    // ── Launch handling ────────────────────────────────────────────────

    fn with_rasterizer_mut<R>(
        &mut self,
        f: impl FnOnce(&mut dyn RasterizerInterface) -> R,
    ) -> Option<R> {
        let handle = self.rasterizer?;
        Some(unsafe { handle.with_mut(f) })
    }

    /// Notify the rasterizer about a GPU-virtual source range.
    ///
    /// Upstream performs this through `GpuGuestMemory`, which delegates to
    /// `MemoryManager::FlushRegion` and translates the range to `DAddr`
    /// segments before reaching the rasterizer.
    fn flush_gpu_region(&self, gpu_addr: u64, size: u64) {
        self.memory_manager.lock().flush_region(gpu_addr, size);
    }

    /// Notify the rasterizer about a GPU-virtual destination range.
    ///
    /// This is the explicit Rust counterpart of the cached-write
    /// `GpuGuestMemoryScoped` destructor used by upstream DMA fallbacks.
    fn invalidate_gpu_region(&self, gpu_addr: u64, size: u64) {
        if std::env::var_os("RUZU_TRACE_TEXTURE_ALIAS").is_some()
            && gpu_addr < 0x4040_0000_0
            && gpu_addr.saturating_add(size) > 0x4030_0000_0
        {
            let device_addr = self.memory_manager.lock().gpu_to_cpu_address(gpu_addr);
            eprintln!(
                "[MAXWELL_DMA_INVALIDATE_SHADER_RANGE] gpu=0x{gpu_addr:X} size=0x{size:X} device={device_addr:?}"
            );
        }
        self.memory_manager.lock().invalidate_region(gpu_addr, size);
    }

    fn release_semaphore(&mut self) {
        let semaphore_type = self.launch_semaphore_type();
        match semaphore_type {
            LAUNCH_SEMAPHORE_TYPE_NONE => {}
            LAUNCH_SEMAPHORE_TYPE_RELEASE_ONE_WORD => {
                let address = self.semaphore_addr();
                let payload = self.semaphore_payload();
                self.with_rasterizer_mut(|rasterizer| {
                    rasterizer.query(
                        address,
                        QueryType::Payload as u32,
                        QueryPropertiesFlags::IS_A_FENCE,
                        payload,
                        0,
                    );
                });
            }
            LAUNCH_SEMAPHORE_TYPE_RELEASE_FOUR_WORD => {
                let address = self.semaphore_addr();
                let payload = self.semaphore_payload();
                self.with_rasterizer_mut(|rasterizer| {
                    rasterizer.query(
                        address,
                        QueryType::Payload as u32,
                        QueryPropertiesFlags::IS_A_FENCE | QueryPropertiesFlags::HAS_TIMEOUT,
                        payload,
                        0,
                    );
                });
            }
            _ => panic!("MaxwellDMA: unknown semaphore type={semaphore_type}"),
        }
    }

    fn fold_min_trailing_zeroes(&self, values: &[u32]) -> u32 {
        values
            .iter()
            .copied()
            .fold(4, |acc, value| acc.min(value.trailing_zeros()))
    }

    fn base_bytes_per_pixel(&self) -> u32 {
        if !self.launch_remap_enable() {
            1
        } else {
            (self.remap_num_dst_components_minus_one() + 1)
                * (self.remap_component_size_minus_one() + 1)
        }
    }

    fn read_gpu_range(read_gpu: &dyn Fn(u64, &mut [u8]), gpu_addr: u64, size: usize) -> Vec<u8> {
        let mut data = vec![0u8; size];
        if size != 0 {
            read_gpu(gpu_addr, &mut data);
        }
        data
    }

    fn copy_blocklinear_to_pitch(
        &mut self,
        read_gpu: &dyn Fn(u64, &mut [u8]),
    ) -> Option<Vec<PendingWrite>> {
        let mut bytes_per_pixel = 1;
        let src_params = self.src_params();
        let dst_pitch = (self.pitch_out() as i32).unsigned_abs();
        let copy_info = dma::ImageCopy {
            length_x: self.line_length(),
            length_y: self.line_count(),
        };
        let src_operand = dma::ImageOperand {
            bytes_per_pixel,
            params: src_params,
            address: self.src_addr(),
        };
        let dst_operand = dma::BufferOperand {
            pitch: dst_pitch,
            width: self.line_length(),
            height: self.line_count(),
            address: self.dst_addr(),
        };
        let accelerated = self
            .with_rasterizer_mut(|rasterizer| {
                rasterizer.access_accelerate_dma().image_to_buffer(
                    &copy_info,
                    &src_operand,
                    &dst_operand,
                )
            })
            .unwrap_or(false);
        if accelerated {
            return Some(vec![]);
        }

        if src_params.block_size.width() != 0 {
            self.report_unimplemented_dma_path(
                "blocklinear->pitch source block_size.width is not zero",
            );
        }
        if src_params.block_size.depth() != 0 {
            self.report_unimplemented_dma_path(
                "blocklinear->pitch source block_size.depth is not zero",
            );
        }
        if src_params.block_size.depth() == 0 && src_params.depth != 1 {
            self.report_unimplemented_dma_path(
                "blocklinear->pitch source depth must be one when block depth is zero",
            );
        }

        let is_remapping = self.launch_remap_enable();
        let base_bpp = self.base_bytes_per_pixel();
        let mut width = src_params.width;
        let mut x_elements = self.line_length();
        let mut x_offset = src_params.origin.x();
        let bpp_shift = if !is_remapping {
            self.fold_min_trailing_zeroes(&[width, x_elements, x_offset, self.src_addr() as u32])
        } else {
            0
        };
        if !is_remapping {
            width >>= bpp_shift;
            x_elements >>= bpp_shift;
            x_offset >>= bpp_shift;
        }
        bytes_per_pixel = base_bpp << bpp_shift;

        let height = src_params.height;
        let depth = src_params.depth;
        let block_height = src_params.block_size.height();
        let block_depth = src_params.block_size.depth();
        let src_size = calculate_size(
            true,
            bytes_per_pixel,
            width,
            height,
            depth,
            block_height,
            block_depth,
        );
        let dst_size = dst_pitch as usize * self.line_count() as usize;
        let src_addr = self.src_addr();
        let dst_addr = self.dst_addr();

        self.flush_gpu_region(src_addr, src_size as u64);
        self.invalidate_gpu_region(dst_addr, dst_size as u64);

        let src = Self::read_gpu_range(read_gpu, src_addr, src_size);
        // Upstream uses GpuGuestMemoryScoped<..., *ReadCachedWrite> for the
        // destination. Preserve pixels outside the copied subrectangle.
        let mut dst = Self::read_gpu_range(read_gpu, dst_addr, dst_size);
        unswizzle_subrect(
            &mut dst,
            &src,
            bytes_per_pixel,
            width,
            height,
            depth,
            x_offset,
            src_params.origin.y(),
            x_elements,
            self.line_count(),
            block_height,
            block_depth,
            dst_pitch,
        );

        Some(vec![PendingWrite {
            gpu_va: dst_addr,
            data: dst,
        }])
    }

    fn copy_pitch_to_blocklinear(
        &mut self,
        read_gpu: &dyn Fn(u64, &mut [u8]),
    ) -> Option<Vec<PendingWrite>> {
        let dst_params = self.dst_params();
        if dst_params.block_size.width() != 0 {
            self.report_unimplemented_dma_path(
                "pitch->blocklinear destination block_size.width is not zero",
            );
        }
        if dst_params.layer != 0 {
            self.report_unimplemented_dma_path("pitch->blocklinear destination layer is not zero");
        }

        let base_bpp = self.base_bytes_per_pixel();
        let copy_info = dma::ImageCopy {
            length_x: self.line_length(),
            length_y: self.line_count(),
        };
        let src_operand = dma::BufferOperand {
            pitch: self.pitch_in(),
            width: self.line_length(),
            height: self.line_count(),
            address: self.src_addr(),
        };
        let dst_operand = dma::ImageOperand {
            bytes_per_pixel: 1,
            params: dst_params,
            address: self.dst_addr(),
        };
        let accelerated = self
            .with_rasterizer_mut(|rasterizer| {
                rasterizer.access_accelerate_dma().buffer_to_image(
                    &copy_info,
                    &src_operand,
                    &dst_operand,
                )
            })
            .unwrap_or(false);
        if accelerated {
            return Some(vec![]);
        }

        let mut width = dst_params.width;
        let mut x_elements = self.line_length();
        let mut x_offset = dst_params.origin.x();
        let bpp_shift = if !self.launch_remap_enable() {
            self.fold_min_trailing_zeroes(&[width, x_elements, x_offset, self.dst_addr() as u32])
        } else {
            0
        };
        width >>= bpp_shift;
        x_elements >>= bpp_shift;
        x_offset >>= bpp_shift;

        let bytes_per_pixel = base_bpp << bpp_shift;
        let height = dst_params.height;
        let depth = dst_params.depth;
        let block_height = dst_params.block_size.height();
        let block_depth = dst_params.block_size.depth();
        let dst_size = calculate_size(
            true,
            bytes_per_pixel,
            width,
            height,
            depth,
            block_height,
            block_depth,
        );
        let src_size = self.pitch_in() as usize * self.line_count() as usize;
        let src_addr = self.src_addr();
        let dst_addr = self.dst_addr();

        self.flush_gpu_region(src_addr, src_size as u64);
        self.invalidate_gpu_region(dst_addr, dst_size as u64);

        let src = Self::read_gpu_range(read_gpu, src_addr, src_size);
        // Upstream uses GpuGuestMemoryScoped<..., *ReadCachedWrite> for the
        // destination. Preserve pixels outside the copied subrectangle.
        let mut dst = Self::read_gpu_range(read_gpu, dst_addr, dst_size);
        swizzle_subrect(
            &mut dst,
            &src,
            bytes_per_pixel,
            width,
            height,
            depth,
            x_offset,
            dst_params.origin.y(),
            x_elements,
            self.line_count(),
            block_height,
            block_depth,
            self.pitch_in(),
        );

        Some(vec![PendingWrite {
            gpu_va: dst_addr,
            data: dst,
        }])
    }

    fn copy_blocklinear_to_blocklinear(
        &mut self,
        read_gpu: &dyn Fn(u64, &mut [u8]),
    ) -> Option<Vec<PendingWrite>> {
        let src_params = self.src_params();
        if src_params.block_size.width() != 0 {
            self.report_unimplemented_dma_path(
                "blocklinear->blocklinear source block_size.width is not zero",
            );
        }

        let dst_params = self.dst_params();
        let base_bpp = self.base_bytes_per_pixel();
        let mut src_width = src_params.width;
        let mut dst_width = dst_params.width;
        let mut x_elements = self.line_length();
        let mut src_x_offset = src_params.origin.x();
        let mut dst_x_offset = dst_params.origin.x();
        let bpp_shift = if !self.launch_remap_enable() {
            self.fold_min_trailing_zeroes(&[
                src_width,
                dst_width,
                x_elements,
                src_x_offset,
                dst_x_offset,
                self.src_addr() as u32,
                self.dst_addr() as u32,
            ])
        } else {
            0
        };
        src_width >>= bpp_shift;
        dst_width >>= bpp_shift;
        x_elements >>= bpp_shift;
        src_x_offset >>= bpp_shift;
        dst_x_offset >>= bpp_shift;

        let bytes_per_pixel = base_bpp << bpp_shift;
        let src_size = calculate_size(
            true,
            bytes_per_pixel,
            src_width,
            src_params.height,
            src_params.depth,
            src_params.block_size.height(),
            src_params.block_size.depth(),
        );
        let dst_size = calculate_size(
            true,
            bytes_per_pixel,
            dst_width,
            dst_params.height,
            dst_params.depth,
            dst_params.block_size.height(),
            dst_params.block_size.depth(),
        );
        let pitch = x_elements * bytes_per_pixel;
        let mid_size = pitch as usize * self.line_count() as usize;
        let src_addr = self.src_addr();
        let dst_addr = self.dst_addr();

        self.flush_gpu_region(src_addr, src_size as u64);
        self.invalidate_gpu_region(dst_addr, dst_size as u64);

        let src = Self::read_gpu_range(read_gpu, src_addr, src_size);
        let mut intermediate = vec![0u8; mid_size];
        // Upstream uses GpuGuestMemoryScoped<..., *ReadCachedWrite> for the
        // destination. Preserve pixels outside the copied subrectangle.
        let mut dst = Self::read_gpu_range(read_gpu, dst_addr, dst_size);
        unswizzle_subrect(
            &mut intermediate,
            &src,
            bytes_per_pixel,
            src_width,
            src_params.height,
            src_params.depth,
            src_x_offset,
            src_params.origin.y(),
            x_elements,
            self.line_count(),
            src_params.block_size.height(),
            src_params.block_size.depth(),
            pitch,
        );
        swizzle_subrect(
            &mut dst,
            &intermediate,
            bytes_per_pixel,
            dst_width,
            dst_params.height,
            dst_params.depth,
            dst_x_offset,
            dst_params.origin.y(),
            x_elements,
            self.line_count(),
            dst_params.block_size.height(),
            dst_params.block_size.depth(),
            pitch,
        );

        Some(vec![PendingWrite {
            gpu_va: dst_addr,
            data: dst,
        }])
    }

    fn log_launch(&self) {
        log::debug!(
            "MaxwellDMA: LAUNCH src=0x{:X} dst=0x{:X} pitch_in={} pitch_out={} {}x{}",
            self.src_addr(),
            self.dst_addr(),
            self.pitch_in(),
            self.pitch_out(),
            self.line_length(),
            self.line_count(),
        );
    }

    #[cfg(test)]
    fn handle_deferred_launch(&mut self) {
        self.log_launch();
        self.pending_launch = true;
    }

    fn execute_multi_line_pitch_to_pitch(&mut self) -> bool {
        if !self.launch_multi_line_enable()
            || !self.launch_src_is_pitch()
            || !self.launch_dst_is_pitch()
        {
            return false;
        }

        let lines = self.line_count();
        let line_length = self.line_length() as u64;
        let pitch_in = self.pitch_in() as u64;
        let pitch_out = self.pitch_out() as u64;
        let src_addr = self.src_addr();
        let dst_addr = self.dst_addr();
        {
            let mut memory_manager = self.memory_manager.lock();
            memory_manager.flush_caching();
            for line in 0..lines {
                let source_line = src_addr + u64::from(line) * pitch_in;
                let dest_line = dst_addr + u64::from(line) * pitch_out;
                memory_manager.copy_block(dest_line, source_line, line_length);
            }
        }
        self.release_semaphore();
        true
    }

    fn launch_immediate(&mut self) {
        assert_eq!(
            self.launch_interrupt_type(),
            LAUNCH_INTERRUPT_TYPE_NONE,
            "MaxwellDMA launch interrupt type must be NONE"
        );
        if self.execute_multi_line_pitch_to_pitch() {
            return;
        }
        if self.launch_multi_line_enable() {
            self.memory_manager.lock().flush_caching();
        }
        let memory_manager = Arc::clone(&self.memory_manager);
        let read_gpu = move |addr: u64, buf: &mut [u8]| {
            let _ = memory_manager.lock().read_block(addr, buf);
        };
        let writes = self.collect_launch_writes(&read_gpu);
        if writes.is_empty() {
            return;
        }

        let memory_manager = self.memory_manager.lock();
        for write in writes {
            if std::env::var_os("RUZU_TRACE_TEXTURE_ALIAS").is_some()
                && write.gpu_va < 0x4040_0000_0
                && write.gpu_va.saturating_add(write.data.len() as u64) > 0x4030_0000_0
            {
                let device_addr = memory_manager.gpu_to_cpu_address(write.gpu_va);
                eprintln!(
                    "[MAXWELL_DMA_WRITE_SHADER_RANGE] gpu=0x{:X} size=0x{:X} device={device_addr:?} head={:02X?}",
                    write.gpu_va,
                    write.data.len(),
                    &write.data[..write.data.len().min(16)]
                );
            }
            let _ = memory_manager.write_block_unsafe(write.gpu_va, &write.data);
        }
    }

    fn collect_launch_writes(&mut self, read_gpu: &dyn Fn(u64, &mut [u8])) -> Vec<PendingWrite> {
        let lines = self.line_count();
        let ll = self.line_length();
        if ll == 0 {
            self.release_semaphore();
            return vec![];
        }

        if !self.launch_multi_line_enable() {
            let src_addr = self.src_addr();
            let dst_addr = self.dst_addr();
            if self.launch_remap_enable() && self.remap_dst_x() == REMAP_SWIZZLE_CONST_A {
                let component_size = self.remap_component_size_minus_one().wrapping_add(1);
                if !matches!(component_size, 1 | 2 | 4) {
                    self.report_unimplemented_dma_path(
                        "single-line remap CONST_A component size is not 1, 2, or 4",
                    );
                }
                let value = self.remap_consta_value();
                if component_size == 4 {
                    self.with_rasterizer_mut(|rasterizer| {
                        rasterizer
                            .access_accelerate_dma()
                            .buffer_clear(dst_addr, ll as u64, value);
                    });
                }
                let mut data = Vec::with_capacity(ll as usize * std::mem::size_of::<u32>());
                for _ in 0..ll {
                    data.extend_from_slice(&value.to_le_bytes());
                }
                data.truncate(ll as usize * component_size as usize);
                self.invalidate_gpu_region(dst_addr, data.len() as u64);
                log::debug!(
                    "MaxwellDMA: single-line remap CONST_A clear executed {} words value=0x{:X} dst=0x{:X}",
                    ll,
                    value,
                    dst_addr
                );
                self.release_semaphore();
                return vec![PendingWrite {
                    gpu_va: dst_addr,
                    data,
                }];
            }

            let is_src_pitch = self.page_kind_is_pitch(src_addr);
            let is_dst_pitch = self.page_kind_is_pitch(dst_addr);
            if !is_src_pitch || !is_dst_pitch {
                if ll % 16 != 0 || src_addr % 16 != 0 || dst_addr % 16 != 0 {
                    self.report_unimplemented_dma_path(
                        "single-line MaxwellDMA pitch/blocklinear copy requires 16-byte alignment",
                    );
                }

                if !is_src_pitch && is_dst_pitch {
                    let mut data = Vec::with_capacity(ll as usize);
                    for offset in (0..ll).step_by(16) {
                        let source = convert_linear_2_blocklinear_addr(src_addr + offset as u64);
                        self.flush_gpu_region(source, 16);
                        let mut chunk = [0u8; 16];
                        read_gpu(source, &mut chunk);
                        data.extend_from_slice(&chunk);
                    }
                    self.invalidate_gpu_region(dst_addr, ll as u64);
                    log::debug!(
                        "MaxwellDMA: single-line blocklinear->pitch copy executed {} bytes src=0x{:X} -> dst=0x{:X}",
                        ll,
                        src_addr,
                        dst_addr
                    );
                    self.release_semaphore();
                    return vec![PendingWrite {
                        gpu_va: dst_addr,
                        data,
                    }];
                }

                if is_src_pitch && !is_dst_pitch {
                    self.flush_gpu_region(src_addr, ll as u64);
                    let mut writes = Vec::with_capacity((ll / 16) as usize);
                    for offset in (0..ll).step_by(16) {
                        let source = src_addr + offset as u64;
                        let dest = convert_linear_2_blocklinear_addr(dst_addr + offset as u64);
                        let mut data = vec![0u8; 16];
                        read_gpu(source, &mut data);
                        self.invalidate_gpu_region(dest, 16);
                        writes.push(PendingWrite { gpu_va: dest, data });
                    }
                    log::debug!(
                        "MaxwellDMA: single-line pitch->blocklinear copy executed {} bytes src=0x{:X} -> dst=0x{:X}",
                        ll,
                        src_addr,
                        dst_addr
                    );
                    self.release_semaphore();
                    return writes;
                }
            }

            if self
                .with_rasterizer_mut(|rasterizer| {
                    rasterizer
                        .access_accelerate_dma()
                        .buffer_copy(src_addr, dst_addr, ll as u64)
                })
                .unwrap_or(false)
            {
                self.release_semaphore();
                return vec![];
            }

            self.flush_gpu_region(src_addr, ll as u64);
            self.invalidate_gpu_region(dst_addr, ll as u64);

            let mut data = vec![0u8; ll as usize];
            read_gpu(src_addr, &mut data);
            log::debug!(
                "MaxwellDMA: single-line pitch copy executed {} bytes src=0x{:X} -> dst=0x{:X}",
                ll,
                src_addr,
                dst_addr
            );
            self.release_semaphore();
            return vec![PendingWrite {
                gpu_va: dst_addr,
                data,
            }];
        }

        if lines == 0 {
            self.release_semaphore();
            return vec![];
        }

        if !self.launch_src_is_pitch() && !self.launch_dst_is_pitch() {
            if let Some(writes) = self.copy_blocklinear_to_blocklinear(read_gpu) {
                self.release_semaphore();
                return writes;
            }
            return vec![];
        }

        if !self.launch_src_is_pitch() && self.launch_dst_is_pitch() {
            if let Some(writes) = self.copy_blocklinear_to_pitch(read_gpu) {
                self.release_semaphore();
                return writes;
            }
            return vec![];
        }

        if self.launch_src_is_pitch() && !self.launch_dst_is_pitch() {
            if let Some(writes) = self.copy_pitch_to_blocklinear(read_gpu) {
                self.release_semaphore();
                return writes;
            }
            return vec![];
        }

        let pi = self.pitch_in();
        let po = self.pitch_out();
        let src_span = (pi as u64)
            .saturating_mul(lines.saturating_sub(1) as u64)
            .saturating_add(ll as u64);
        let dst_span = (po as u64)
            .saturating_mul(lines.saturating_sub(1) as u64)
            .saturating_add(ll as u64);
        let src_addr = self.src_addr();
        let dst_addr = self.dst_addr();

        // Upstream `MaxwellDMA::Launch` calls `memory_manager.FlushCaching()`
        // before reading from the DMA source. In the Rust OpenGL path, render
        // target contents may still live only in the texture cache, so flush
        // the source range through the rasterizer before the CPU fallback copy.
        if src_span != 0 {
            self.flush_gpu_region(src_addr, src_span);
        }
        if dst_span != 0 {
            self.invalidate_gpu_region(dst_addr, dst_span);
        }

        let dst_size = dst_span as usize;
        // Upstream copies only line_length bytes per row. Reading the
        // destination first preserves pitch padding between copied rows.
        let mut dst_buf = Self::read_gpu_range(read_gpu, dst_addr, dst_size);
        let mut line_buf = vec![0u8; ll as usize];

        for line in 0..lines {
            let src_off = self.src_addr() + (line as u64 * pi as u64);
            read_gpu(src_off, &mut line_buf);
            let dst_off = (line * po) as usize;
            let w = ll as usize;
            if dst_off + w <= dst_buf.len() {
                dst_buf[dst_off..dst_off + w].copy_from_slice(&line_buf);
            }
        }

        log::debug!(
            "MaxwellDMA: copy executed {}x{} (pi={} po={}) src=0x{:X} -> dst=0x{:X}",
            ll,
            lines,
            pi,
            po,
            self.src_addr(),
            self.dst_addr()
        );

        self.release_semaphore();
        vec![PendingWrite {
            gpu_va: self.dst_addr(),
            data: dst_buf,
        }]
    }
}

impl EngineInterface for MaxwellDMA {
    fn call_method(&mut self, method: u32, method_argument: u32, is_last_call: bool) {
        MaxwellDMA::call_method(self, method, method_argument, is_last_call);
    }

    fn call_multi_method(
        &mut self,
        method: u32,
        base_start: &[u32],
        amount: u32,
        methods_pending: u32,
    ) {
        MaxwellDMA::call_multi_method(self, method, base_start, amount, methods_pending);
    }

    fn consume_sink_impl(&mut self) {
        let sink = std::mem::take(&mut self.interface_state.method_sink);
        for (method, value) in sink {
            let idx = method as usize;
            if idx < NUM_REGS {
                self.regs[idx] = value;
            }
        }
    }

    fn has_pending_methods(&self) -> bool {
        !self.interface_state.method_sink.is_empty()
    }

    fn execution_mask(&self) -> &[bool] {
        &self.interface_state.execution_mask
    }

    fn push_method_sink(&mut self, method: u32, value: u32) {
        self.interface_state.method_sink.push((method, value));
    }

    fn set_current_dma_segment(&mut self, segment: u64) {
        self.interface_state.current_dma_segment = segment;
    }

    fn current_dirty(&self) -> bool {
        self.interface_state.current_dirty
    }

    fn set_current_dirty(&mut self, dirty: bool) {
        self.interface_state.current_dirty = dirty;
    }
}

#[cfg(test)]
impl Default for MaxwellDMA {
    fn default() -> Self {
        Self::new(Arc::new(Mutex::new(MemoryManager::new(0))))
    }
}

#[cfg(test)]
impl MaxwellDMA {
    fn write_reg(&mut self, method: u32, value: u32) {
        log::trace!("MaxwellDMA: reg[0x{:X}] = 0x{:X}", method, value);
        let idx = method as usize;
        assert!(idx < NUM_REGS, "Invalid MaxwellDMA register");
        self.regs[idx] = value;
        if method == LAUNCH_DMA {
            self.handle_deferred_launch();
        }
    }

    fn execute_pending(&mut self, read_gpu: &dyn Fn(u64, &mut [u8])) -> Vec<PendingWrite> {
        if !self.pending_launch {
            return vec![];
        }
        self.pending_launch = false;
        self.collect_launch_writes(read_gpu)
    }
}

#[cfg(test)]
#[path = "maxwell_dma_test.rs"]
mod tests;

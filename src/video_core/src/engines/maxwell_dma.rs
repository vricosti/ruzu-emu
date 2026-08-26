// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Maxwell DMA engine (NV class B0B5).
//!
//! Handles the pitch-linear DMA copy subset used by early renderer paths.

use std::sync::Arc;

use parking_lot::Mutex;

use super::engine_interface::{EngineInterface, EngineInterfaceState};
use super::{PendingWrite, ENGINE_REG_COUNT};
use crate::memory_manager::MemoryManager;
use crate::pte_kind::{is_pitch_kind, PteKind};
use crate::query_cache::types::{QueryPropertiesFlags, QueryType};
use crate::rasterizer_interface::{RasterizerHandle, RasterizerInterface};
use crate::textures::decoders::{calculate_size, swizzle_subrect, unswizzle_subrect};

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
    regs: Box<[u32; ENGINE_REG_COUNT]>,
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
            regs: Box::new([0u32; ENGINE_REG_COUNT]),
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
        assert!(idx < ENGINE_REG_COUNT, "Invalid MaxwellDMA register");
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
        _amount: u32,
        _methods_pending: u32,
    ) {
        for &arg in args {
            self.call_method(method, arg, false);
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
            if idx < ENGINE_REG_COUNT {
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
        assert!(idx < ENGINE_REG_COUNT, "Invalid MaxwellDMA register");
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
mod tests {
    use super::*;
    use crate::engines::draw_manager::Maxwell3DClearView;
    use crate::rasterizer_interface::RasterizerDownloadArea;

    #[derive(Default)]
    struct RasterizerCalls {
        flushes: Vec<(u64, u64)>,
        invalidations: Vec<(u64, u64)>,
        queries: Vec<(u64, u32, QueryPropertiesFlags, u32, u32)>,
        dma_buffer_copies: Vec<(u64, u64, u64)>,
        dma_buffer_clears: Vec<(u64, u64, u32)>,
        dma_image_to_buffers: Vec<(dma::ImageCopy, dma::ImageOperand, dma::BufferOperand)>,
        dma_buffer_to_images: Vec<(dma::ImageCopy, dma::BufferOperand, dma::ImageOperand)>,
    }

    struct TestRasterizer {
        calls: Arc<Mutex<RasterizerCalls>>,
        accelerate_buffer_copy: bool,
        accelerate_buffer_clear: bool,
        accelerate_image_to_buffer: bool,
        accelerate_buffer_to_image: bool,
    }

    impl TestRasterizer {
        fn new(calls: Arc<Mutex<RasterizerCalls>>) -> Self {
            Self {
                calls,
                accelerate_buffer_copy: false,
                accelerate_buffer_clear: false,
                accelerate_image_to_buffer: false,
                accelerate_buffer_to_image: false,
            }
        }
    }

    impl AccelerateDMAInterface for TestRasterizer {
        fn buffer_copy(&mut self, src_address: u64, dest_address: u64, amount: u64) -> bool {
            self.calls
                .lock()
                .dma_buffer_copies
                .push((src_address, dest_address, amount));
            self.accelerate_buffer_copy
        }

        fn buffer_clear(&mut self, dst_address: u64, amount: u64, value: u32) -> bool {
            self.calls
                .lock()
                .dma_buffer_clears
                .push((dst_address, amount, value));
            self.accelerate_buffer_clear
        }

        fn image_to_buffer(
            &mut self,
            copy_info: &dma::ImageCopy,
            src: &dma::ImageOperand,
            dst: &dma::BufferOperand,
        ) -> bool {
            self.calls
                .lock()
                .dma_image_to_buffers
                .push((*copy_info, *src, *dst));
            self.accelerate_image_to_buffer
        }

        fn buffer_to_image(
            &mut self,
            copy_info: &dma::ImageCopy,
            src: &dma::BufferOperand,
            dst: &dma::ImageOperand,
        ) -> bool {
            self.calls
                .lock()
                .dma_buffer_to_images
                .push((*copy_info, *src, *dst));
            self.accelerate_buffer_to_image
        }
    }

    impl RasterizerInterface for TestRasterizer {
        fn draw(
            &mut self,
            _draw_view: crate::engines::draw_manager::Maxwell3DDrawView<'_>,
            _instance_count: u32,
        ) {
        }
        fn draw_texture(
            &mut self,
            _draw_texture_view: crate::engines::draw_manager::Maxwell3DDrawTextureView<'_>,
        ) {
        }
        fn clear(&mut self, _clear_view: Maxwell3DClearView<'_>, _layer_count: u32) {}
        fn dispatch_compute(&mut self, _dispatch: &crate::engines::kepler_compute::DispatchCall) {}
        fn reset_counter(&mut self, _query_type: u32) {}
        fn query(
            &mut self,
            gpu_addr: u64,
            query_type: u32,
            flags: QueryPropertiesFlags,
            payload: u32,
            subreport: u32,
        ) {
            self.calls
                .lock()
                .queries
                .push((gpu_addr, query_type, flags, payload, subreport));
        }
        fn bind_graphics_uniform_buffer(
            &mut self,
            _stage: usize,
            _index: u32,
            _gpu_addr: u64,
            _size: u32,
        ) {
        }
        fn disable_graphics_uniform_buffer(&mut self, _stage: usize, _index: u32) {}
        fn signal_fence(&mut self, _func: Box<dyn FnOnce() + Send>) {}
        fn sync_operation(&mut self, _func: Box<dyn FnOnce() + Send>) {}
        fn signal_sync_point(&mut self, _value: u32) {}
        fn signal_reference(&mut self) {}
        fn release_fences(&mut self, _force: bool) {}
        fn flush_all(&mut self) {}
        fn flush_region(&mut self, addr: u64, size: u64, _which: crate::cache_types::CacheType) {
            self.calls.lock().flushes.push((addr, size));
        }
        fn must_flush_region(
            &self,
            _addr: u64,
            _size: u64,
            _which: crate::cache_types::CacheType,
        ) -> bool {
            false
        }
        fn get_flush_area(&self, addr: u64, size: u64) -> RasterizerDownloadArea {
            RasterizerDownloadArea {
                start_address: addr,
                end_address: addr + size,
                preemptive: false,
            }
        }
        fn invalidate_region(
            &mut self,
            addr: u64,
            size: u64,
            _which: crate::cache_types::CacheType,
        ) {
            self.calls.lock().invalidations.push((addr, size));
        }
        fn on_cache_invalidation(&mut self, _addr: u64, _size: u64) {}
        fn on_cpu_write(&mut self, _addr: u64, _size: u64) -> bool {
            false
        }
        fn invalidate_gpu_cache(&mut self) {}
        fn unmap_memory(&mut self, _addr: u64, _size: u64) {}
        fn modify_gpu_memory(&mut self, _as_id: usize, _addr: u64, _size: u64) {}
        fn flush_and_invalidate_region(
            &mut self,
            _addr: u64,
            _size: u64,
            _which: crate::cache_types::CacheType,
        ) {
        }
        fn wait_for_idle(&mut self) {}
        fn fragment_barrier(&mut self) {}
        fn tiled_cache_barrier(&mut self) {}
        fn flush_commands(&mut self) {}
        fn tick_frame(&mut self) {}
        fn accelerate_inline_to_memory(
            &mut self,
            _address: u64,
            _copy_size: usize,
            _memory: &[u8],
        ) {
        }
        fn access_accelerate_dma(&mut self) -> &mut dyn AccelerateDMAInterface {
            self
        }
    }

    fn new_test_engine() -> MaxwellDMA {
        MaxwellDMA::new(Arc::new(Mutex::new(MemoryManager::new(0))))
    }

    fn bind_memory_rasterizer(
        eng: &mut MaxwellDMA,
        rasterizer: &dyn RasterizerInterface,
        mappings: &[(u64, u64)],
    ) {
        eng.bind_rasterizer(rasterizer);
        let mut memory_manager = eng.memory_manager.lock();
        memory_manager.bind_rasterizer(rasterizer);
        for &(gpu_addr, device_addr) in mappings {
            memory_manager.map(
                gpu_addr,
                device_addr,
                0x1000,
                PteKind::PITCH.raw() as u32,
                false,
            );
        }
    }

    fn write_dma_params(eng: &mut MaxwellDMA, base: u32, params: dma::Parameters) {
        eng.write_reg(base, params.block_size.raw);
        eng.write_reg(base + 1, params.width);
        eng.write_reg(base + 2, params.height);
        eng.write_reg(base + 3, params.depth);
        eng.write_reg(base + 4, params.layer);
        eng.write_reg(base + 5, params.origin.raw);
    }

    const MULTI_LINE_PITCH_TO_PITCH_LAUNCH: u32 = LAUNCH_DATA_TRANSFER_NON_PIPELINED
        | LAUNCH_SRC_MEMORY_LAYOUT_PITCH
        | LAUNCH_DST_MEMORY_LAYOUT_PITCH
        | LAUNCH_MULTI_LINE_ENABLE;
    const MULTI_LINE_BLOCKLINEAR_TO_PITCH_LAUNCH: u32 = LAUNCH_DATA_TRANSFER_NON_PIPELINED
        | LAUNCH_DST_MEMORY_LAYOUT_PITCH
        | LAUNCH_MULTI_LINE_ENABLE;
    const MULTI_LINE_PITCH_TO_BLOCKLINEAR_LAUNCH: u32 = LAUNCH_DATA_TRANSFER_NON_PIPELINED
        | LAUNCH_SRC_MEMORY_LAYOUT_PITCH
        | LAUNCH_MULTI_LINE_ENABLE;
    const MULTI_LINE_BLOCKLINEAR_TO_BLOCKLINEAR_LAUNCH: u32 =
        LAUNCH_DATA_TRANSFER_NON_PIPELINED | LAUNCH_MULTI_LINE_ENABLE;
    const SINGLE_LINE_LAUNCH: u32 = LAUNCH_DATA_TRANSFER_NON_PIPELINED;
    const RELEASE_ONE_WORD_SEMAPHORE_LAUNCH: u32 =
        MULTI_LINE_PITCH_TO_PITCH_LAUNCH | (LAUNCH_SEMAPHORE_TYPE_RELEASE_ONE_WORD << 3);
    const RELEASE_FOUR_WORD_SEMAPHORE_LAUNCH: u32 =
        MULTI_LINE_PITCH_TO_PITCH_LAUNCH | (LAUNCH_SEMAPHORE_TYPE_RELEASE_FOUR_WORD << 3);

    #[test]
    fn test_address_accessors() {
        let mut eng = new_test_engine();
        eng.write_reg(SRC_ADDR_HIGH, 0xAB);
        eng.write_reg(SRC_ADDR_LOW, 0xCDEF_0000);
        assert_eq!(eng.src_addr(), 0xAB_CDEF_0000);

        eng.write_reg(DST_ADDR_HIGH, 0x12);
        eng.write_reg(DST_ADDR_LOW, 0x3456_7890);
        assert_eq!(eng.dst_addr(), 0x12_3456_7890);
    }

    #[test]
    fn test_pitch_and_size_accessors() {
        let mut eng = new_test_engine();
        eng.write_reg(PITCH_IN, 5120);
        eng.write_reg(PITCH_OUT, 5120);
        eng.write_reg(LINE_LENGTH, 5120);
        eng.write_reg(LINE_COUNT, 720);
        assert_eq!(eng.pitch_in(), 5120);
        assert_eq!(eng.pitch_out(), 5120);
        assert_eq!(eng.line_length(), 5120);
        assert_eq!(eng.line_count(), 720);
    }

    #[test]
    fn test_launch_trigger_sets_pending() {
        let mut eng = new_test_engine();
        assert!(!eng.pending_launch);

        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x1000);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x2000);
        eng.write_reg(PITCH_IN, 5120);
        eng.write_reg(PITCH_OUT, 5120);
        eng.write_reg(LINE_LENGTH, 5120);
        eng.write_reg(LINE_COUNT, 720);

        // Trigger DMA launch
        eng.write_reg(LAUNCH_DMA, MULTI_LINE_PITCH_TO_PITCH_LAUNCH);
        assert!(eng.pending_launch);
        assert_eq!(
            eng.launch_data_transfer_type(),
            LAUNCH_DATA_TRANSFER_NON_PIPELINED
        );
    }

    #[test]
    fn test_no_trigger_without_launch_method() {
        let mut eng = new_test_engine();
        eng.write_reg(0x200, 42); // Random register
        assert!(!eng.pending_launch);
    }

    #[test]
    fn test_bind_rasterizer_stores_reference() {
        let syncpoints =
            std::sync::Arc::new(crate::host1x::syncpoint_manager::SyncpointManager::new());
        let rasterizer = crate::renderer_null::null_rasterizer::RasterizerNull::new(syncpoints);
        let mut eng = new_test_engine();
        assert!(eng.rasterizer.is_none());
        eng.bind_rasterizer(&rasterizer);
        assert!(eng.rasterizer.is_some());
    }

    #[test]
    fn test_dma_copies_lines() {
        let mut eng = new_test_engine();

        // 2 lines of 8 bytes each, same pitch.
        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x1000);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x2000);
        eng.write_reg(PITCH_IN, 8);
        eng.write_reg(PITCH_OUT, 8);
        eng.write_reg(LINE_LENGTH, 8);
        eng.write_reg(LINE_COUNT, 2);

        eng.write_reg(LAUNCH_DMA, MULTI_LINE_PITCH_TO_PITCH_LAUNCH);
        assert!(eng.pending_launch);

        // Source data.
        let src: Vec<u8> = (0..16).collect();

        let writes = eng.execute_pending(&|addr, buf| {
            if addr == 0x2000 {
                buf.fill(0);
                return;
            }
            let offset = (addr - 0x1000) as usize;
            let len = buf.len();
            buf.copy_from_slice(&src[offset..offset + len]);
        });

        assert!(!eng.pending_launch);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].gpu_va, 0x2000);
        assert_eq!(writes[0].data, src);
    }

    #[test]
    fn test_dma_different_pitches() {
        let mut eng = new_test_engine();

        // Copy 4 bytes per line, 2 lines. pitch_in=8, pitch_out=16.
        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x1000);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x2000);
        eng.write_reg(PITCH_IN, 8);
        eng.write_reg(PITCH_OUT, 16);
        eng.write_reg(LINE_LENGTH, 4);
        eng.write_reg(LINE_COUNT, 2);

        eng.write_reg(LAUNCH_DMA, MULTI_LINE_PITCH_TO_PITCH_LAUNCH);

        // Source memory: pitch_in=8 per line.
        let src = vec![
            1, 2, 3, 4, 0xAA, 0xBB, 0xCC, 0xDD, // line 0 (4 useful + 4 padding)
            5, 6, 7, 8, 0xEE, 0xFF, 0x11, 0x22, // line 1 (4 useful + 4 padding)
        ];

        let writes = eng.execute_pending(&|addr, buf| {
            if addr == 0x2000 {
                buf.fill(0x7E);
                return;
            }
            let offset = (addr - 0x1000) as usize;
            let len = buf.len();
            buf.copy_from_slice(&src[offset..offset + len]);
        });

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].gpu_va, 0x2000);
        let dst = &writes[0].data;
        assert_eq!(dst.len(), 20); // pitch_out * (line_count - 1) + line_length
                                   // Line 0: 4 bytes copied + 12 untouched bytes.
        assert_eq!(&dst[0..4], &[1, 2, 3, 4]);
        assert_eq!(&dst[4..16], &[0x7E; 12]);
        // Line 1 starts at pitch_out and contributes only its copied bytes.
        assert_eq!(&dst[16..20], &[5, 6, 7, 8]);
    }

    #[test]
    fn test_immediate_pitch_copy_executes_lines_sequentially() {
        let mut eng = new_test_engine();
        let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
        let rasterizer = TestRasterizer::new(Arc::clone(&calls));
        bind_memory_rasterizer(
            &mut eng,
            &rasterizer,
            &[(0x1000, 0x1_1000), (0x8000, 0x1_8000)],
        );

        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x1000);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x8000);
        eng.write_reg(PITCH_IN, 8);
        eng.write_reg(PITCH_OUT, 16);
        eng.write_reg(LINE_LENGTH, 4);
        eng.write_reg(LINE_COUNT, 2);

        eng.call_method(LAUNCH_DMA, MULTI_LINE_PITCH_TO_PITCH_LAUNCH, true);

        let calls = calls.lock();
        assert_eq!(
            calls.flushes,
            vec![(0x1_1000, 4), (0x1_8000, 4), (0x1_1008, 4), (0x1_8010, 4),]
        );
        assert_eq!(calls.invalidations, vec![(0x1_8000, 4), (0x1_8010, 4)]);
    }

    #[test]
    fn test_dma_pitch_to_pitch_preserves_overlapping_upstream_pitch() {
        let mut eng = new_test_engine();

        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x1000);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x2000);
        eng.write_reg(PITCH_IN, 2);
        eng.write_reg(PITCH_OUT, 2);
        eng.write_reg(LINE_LENGTH, 4);
        eng.write_reg(LINE_COUNT, 3);
        eng.write_reg(LAUNCH_DMA, MULTI_LINE_PITCH_TO_PITCH_LAUNCH);

        let src: Vec<u8> = (0x10..0x20).collect();
        let writes = eng.execute_pending(&|addr, buf| {
            if addr == 0x2000 {
                buf.fill(0);
                return;
            }
            let offset = (addr - 0x1000) as usize;
            buf.copy_from_slice(&src[offset..offset + buf.len()]);
        });

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].gpu_va, 0x2000);
        assert_eq!(
            writes[0].data,
            vec![0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17]
        );
    }

    #[test]
    fn test_single_line_pitch_copy_tries_accelerated_buffer_copy_before_fallback() {
        let mut eng = new_test_engine();
        {
            let mut mm = eng.memory_manager.lock();
            mm.map(0x1000, 0x1000, 0x1000, PteKind::PITCH.raw() as u32, false);
            mm.map(0x8000, 0x8000, 0x1000, PteKind::PITCH.raw() as u32, false);
        }
        let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
        let mut rasterizer = TestRasterizer::new(Arc::clone(&calls));
        rasterizer.accelerate_buffer_copy = true;
        eng.bind_rasterizer(&rasterizer);

        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x1000);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x8000);
        eng.write_reg(LINE_LENGTH, 12);
        eng.write_reg(LINE_COUNT, 0);
        eng.write_reg(LAUNCH_DMA, SINGLE_LINE_LAUNCH);

        let writes = eng.execute_pending(&|_, buf| buf.fill(0xAA));

        assert!(writes.is_empty());
        let calls = calls.lock();
        assert_eq!(calls.dma_buffer_copies, vec![(0x1000, 0x8000, 12)]);
        assert!(calls.flushes.is_empty());
        assert!(calls.invalidations.is_empty());
    }

    #[test]
    fn test_single_line_const_a_clear_calls_accelerated_buffer_clear_then_fallback() {
        let mut eng = new_test_engine();
        let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
        let mut rasterizer = TestRasterizer::new(Arc::clone(&calls));
        rasterizer.accelerate_buffer_clear = true;
        bind_memory_rasterizer(&mut eng, &rasterizer, &[(0x8000, 0x1_8000)]);

        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x8000);
        eng.write_reg(LINE_LENGTH, 4);
        eng.write_reg(LINE_COUNT, 0);
        eng.write_reg(REMAP_CONSTA_VALUE, 0x1122_3344);
        eng.write_reg(REMAP_COMPONENTS, REMAP_SWIZZLE_CONST_A | (3 << 16));
        eng.write_reg(LAUNCH_DMA, SINGLE_LINE_LAUNCH | LAUNCH_REMAP_ENABLE);

        let writes = eng.execute_pending(&|_, buf| buf.fill(0xAA));

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].gpu_va, 0x8000);
        assert_eq!(
            writes[0].data,
            [
                0x44, 0x33, 0x22, 0x11, 0x44, 0x33, 0x22, 0x11, 0x44, 0x33, 0x22, 0x11, 0x44, 0x33,
                0x22, 0x11
            ]
        );
        let calls = calls.lock();
        assert_eq!(calls.dma_buffer_clears, vec![(0x8000, 4, 0x1122_3344)]);
        assert_eq!(calls.invalidations, vec![(0x1_8000, 16)]);
    }

    #[test]
    fn test_multi_line_blocklinear_to_pitch_tries_image_to_buffer_before_fallback() {
        let mut eng = new_test_engine();
        let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
        let mut rasterizer = TestRasterizer::new(Arc::clone(&calls));
        rasterizer.accelerate_image_to_buffer = true;
        eng.bind_rasterizer(&rasterizer);

        let params = dma::Parameters {
            block_size: dma::BlockSize { raw: 0 },
            width: 16,
            height: 4,
            depth: 1,
            layer: 0,
            origin: dma::Origin { raw: 0 },
        };
        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x1000);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x8000);
        eng.write_reg(PITCH_OUT, 16);
        eng.write_reg(LINE_LENGTH, 16);
        eng.write_reg(LINE_COUNT, 4);
        write_dma_params(&mut eng, SRC_PARAMS, params);
        eng.write_reg(LAUNCH_DMA, MULTI_LINE_BLOCKLINEAR_TO_PITCH_LAUNCH);

        let writes = eng.execute_pending(&|_, buf| buf.fill(0xAA));

        assert!(writes.is_empty());
        let calls = calls.lock();
        assert_eq!(calls.dma_image_to_buffers.len(), 1);
        let (copy, src, dst) = calls.dma_image_to_buffers[0];
        assert_eq!(
            copy,
            dma::ImageCopy {
                length_x: 16,
                length_y: 4
            }
        );
        assert_eq!(
            src,
            dma::ImageOperand {
                bytes_per_pixel: 1,
                params,
                address: 0x1000
            }
        );
        assert_eq!(
            dst,
            dma::BufferOperand {
                pitch: 16,
                width: 16,
                height: 4,
                address: 0x8000
            }
        );
    }

    #[test]
    fn test_multi_line_pitch_to_blocklinear_tries_buffer_to_image_before_fallback() {
        let mut eng = new_test_engine();
        let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
        let mut rasterizer = TestRasterizer::new(Arc::clone(&calls));
        rasterizer.accelerate_buffer_to_image = true;
        eng.bind_rasterizer(&rasterizer);

        let params = dma::Parameters {
            block_size: dma::BlockSize { raw: 0 },
            width: 16,
            height: 4,
            depth: 1,
            layer: 0,
            origin: dma::Origin { raw: 0 },
        };
        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x1000);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x8000);
        eng.write_reg(PITCH_IN, 16);
        eng.write_reg(LINE_LENGTH, 16);
        eng.write_reg(LINE_COUNT, 4);
        write_dma_params(&mut eng, DST_PARAMS, params);
        eng.write_reg(LAUNCH_DMA, MULTI_LINE_PITCH_TO_BLOCKLINEAR_LAUNCH);

        let writes = eng.execute_pending(&|_, buf| buf.fill(0xAA));

        assert!(writes.is_empty());
        let calls = calls.lock();
        assert_eq!(calls.dma_buffer_to_images.len(), 1);
        let (copy, src, dst) = calls.dma_buffer_to_images[0];
        assert_eq!(
            copy,
            dma::ImageCopy {
                length_x: 16,
                length_y: 4
            }
        );
        assert_eq!(
            src,
            dma::BufferOperand {
                pitch: 16,
                width: 16,
                height: 4,
                address: 0x1000
            }
        );
        assert_eq!(
            dst,
            dma::ImageOperand {
                bytes_per_pixel: 1,
                params,
                address: 0x8000
            }
        );
    }

    #[test]
    fn test_multi_line_blocklinear_to_pitch_unswizzles_subrect() {
        let mut eng = new_test_engine();
        let src_addr = 0x1000;
        let dst_addr = 0x8000;
        let width = 16;
        let height = 4;
        let depth = 1;
        let block_height = 0;
        let block_depth = 0;
        let line_length = 16;
        let line_count = 4;
        let linear: Vec<u8> = (0..64).collect();
        let mut tiled =
            vec![0u8; calculate_size(true, 1, width, height, depth, block_height, block_depth)];
        swizzle_subrect(
            &mut tiled,
            &linear,
            1,
            width,
            height,
            depth,
            0,
            0,
            line_length,
            line_count,
            block_height,
            block_depth,
            line_length,
        );

        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, src_addr);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, dst_addr);
        eng.write_reg(PITCH_OUT, line_length);
        eng.write_reg(LINE_LENGTH, line_length);
        eng.write_reg(LINE_COUNT, line_count);
        write_dma_params(
            &mut eng,
            SRC_PARAMS,
            dma::Parameters {
                block_size: dma::BlockSize {
                    raw: block_height << 4 | block_depth << 8,
                },
                width,
                height,
                depth,
                layer: 0,
                origin: dma::Origin { raw: 0 },
            },
        );
        eng.write_reg(LAUNCH_DMA, MULTI_LINE_BLOCKLINEAR_TO_PITCH_LAUNCH);

        let writes = eng.execute_pending(&|addr, buf| {
            if addr == dst_addr as u64 {
                buf.fill(0);
                return;
            }
            let offset = (addr - src_addr as u64) as usize;
            buf.copy_from_slice(&tiled[offset..offset + buf.len()]);
        });

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].gpu_va, dst_addr as u64);
        assert_eq!(writes[0].data, linear);
    }

    #[test]
    fn test_multi_line_blocklinear_to_pitch_remap_uses_component_size() {
        let mut eng = new_test_engine();
        let src_addr = 0x1000;
        let dst_addr = 0x8000;
        let width = 16;
        let height = 4;
        let depth = 1;
        let bytes_per_pixel = 4;
        let line_count = height;
        let pitch = width * bytes_per_pixel;
        let linear: Vec<u8> = (0..pitch * line_count).map(|value| value as u8).collect();
        let mut tiled =
            vec![0u8; calculate_size(true, bytes_per_pixel, width, height, depth, 0, 0,)];
        swizzle_subrect(
            &mut tiled,
            &linear,
            bytes_per_pixel,
            width,
            height,
            depth,
            0,
            0,
            width,
            line_count,
            0,
            0,
            pitch,
        );

        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, src_addr);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, dst_addr);
        eng.write_reg(PITCH_OUT, pitch);
        eng.write_reg(LINE_LENGTH, width);
        eng.write_reg(LINE_COUNT, line_count);
        eng.write_reg(REMAP_COMPONENTS, 3 << 16);
        write_dma_params(
            &mut eng,
            SRC_PARAMS,
            dma::Parameters {
                block_size: dma::BlockSize { raw: 0 },
                width,
                height,
                depth,
                layer: 0,
                origin: dma::Origin { raw: 0 },
            },
        );
        eng.write_reg(
            LAUNCH_DMA,
            MULTI_LINE_BLOCKLINEAR_TO_PITCH_LAUNCH | LAUNCH_REMAP_ENABLE,
        );

        let writes = eng.execute_pending(&|addr, buf| {
            if addr == dst_addr as u64 {
                buf.fill(0);
                return;
            }
            let offset = (addr - src_addr as u64) as usize;
            buf.copy_from_slice(&tiled[offset..offset + buf.len()]);
        });

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].gpu_va, dst_addr as u64);
        assert_eq!(writes[0].data, linear);
    }

    #[test]
    fn test_multi_line_pitch_to_blocklinear_swizzles_subrect() {
        let mut eng = new_test_engine();
        let src_addr = 0x1000;
        let dst_addr = 0x8000;
        let width = 16;
        let height = 4;
        let depth = 1;
        let block_height = 0;
        let block_depth = 0;
        let line_length = 16;
        let line_count = 4;
        let linear: Vec<u8> = (0..64).collect();
        let mut expected =
            vec![0u8; calculate_size(true, 1, width, height, depth, block_height, block_depth)];
        swizzle_subrect(
            &mut expected,
            &linear,
            1,
            width,
            height,
            depth,
            0,
            0,
            line_length,
            line_count,
            block_height,
            block_depth,
            line_length,
        );

        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, src_addr);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, dst_addr);
        eng.write_reg(PITCH_IN, line_length);
        eng.write_reg(LINE_LENGTH, line_length);
        eng.write_reg(LINE_COUNT, line_count);
        write_dma_params(
            &mut eng,
            DST_PARAMS,
            dma::Parameters {
                block_size: dma::BlockSize {
                    raw: block_height << 4 | block_depth << 8,
                },
                width,
                height,
                depth,
                layer: 0,
                origin: dma::Origin { raw: 0 },
            },
        );
        eng.write_reg(LAUNCH_DMA, MULTI_LINE_PITCH_TO_BLOCKLINEAR_LAUNCH);

        let writes = eng.execute_pending(&|addr, buf| {
            if addr == dst_addr as u64 {
                buf.fill(0);
                return;
            }
            let offset = (addr - src_addr as u64) as usize;
            buf.copy_from_slice(&linear[offset..offset + buf.len()]);
        });

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].gpu_va, dst_addr as u64);
        assert_eq!(writes[0].data, expected);
    }

    #[test]
    fn test_multi_line_blocklinear_to_blocklinear_deswizzles_then_reswizzles() {
        let mut eng = new_test_engine();
        let src_addr = 0x1000;
        let dst_addr = 0x8000;
        let width = 16;
        let height = 4;
        let depth = 1;
        let block_height = 0;
        let block_depth = 0;
        let line_length = 16;
        let line_count = 4;
        let linear: Vec<u8> = (0..64).collect();
        let mut tiled =
            vec![0u8; calculate_size(true, 1, width, height, depth, block_height, block_depth)];
        swizzle_subrect(
            &mut tiled,
            &linear,
            1,
            width,
            height,
            depth,
            0,
            0,
            line_length,
            line_count,
            block_height,
            block_depth,
            line_length,
        );

        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, src_addr);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, dst_addr);
        eng.write_reg(LINE_LENGTH, line_length);
        eng.write_reg(LINE_COUNT, line_count);
        let params = dma::Parameters {
            block_size: dma::BlockSize {
                raw: block_height << 4 | block_depth << 8,
            },
            width,
            height,
            depth,
            layer: 0,
            origin: dma::Origin { raw: 0 },
        };
        write_dma_params(&mut eng, SRC_PARAMS, params);
        write_dma_params(&mut eng, DST_PARAMS, params);
        eng.write_reg(LAUNCH_DMA, MULTI_LINE_BLOCKLINEAR_TO_BLOCKLINEAR_LAUNCH);

        let writes = eng.execute_pending(&|addr, buf| {
            if addr == dst_addr as u64 {
                buf.fill(0);
                return;
            }
            let offset = (addr - src_addr as u64) as usize;
            buf.copy_from_slice(&tiled[offset..offset + buf.len()]);
        });

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].gpu_va, dst_addr as u64);
        assert_eq!(writes[0].data, tiled);
    }

    #[test]
    fn test_single_line_pitch_page_kind_copies_line_length_without_line_count() {
        let mut eng = new_test_engine();
        {
            let mut mm = eng.memory_manager.lock();
            mm.map(0x1000, 0x1000, 0x1000, PteKind::PITCH.raw() as u32, false);
            mm.map(0x8000, 0x8000, 0x1000, PteKind::PITCH.raw() as u32, false);
        }

        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x1000);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x8000);
        eng.write_reg(PITCH_IN, 0);
        eng.write_reg(PITCH_OUT, 0);
        eng.write_reg(LINE_LENGTH, 12);
        eng.write_reg(LINE_COUNT, 0);
        eng.write_reg(LAUNCH_DMA, SINGLE_LINE_LAUNCH);

        let src: Vec<u8> = (0x40..0x80).collect();
        let writes = eng.execute_pending(&|addr, buf| {
            let offset = (addr - 0x1000) as usize;
            buf.copy_from_slice(&src[offset..offset + buf.len()]);
        });

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].gpu_va, 0x8000);
        assert_eq!(writes[0].data, src[..12]);
    }

    #[test]
    fn test_single_line_non_pitch_alignment_reports_and_continues_like_upstream() {
        let mut eng = new_test_engine();
        {
            let mut mm = eng.memory_manager.lock();
            mm.map(0x1000, 0x1000, 0x1000, PteKind::Z16.raw() as u32, false);
            mm.map(0x8000, 0x8000, 0x1000, PteKind::PITCH.raw() as u32, false);
        }

        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x1000);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x8000);
        eng.write_reg(LINE_LENGTH, 12);
        eng.write_reg(LINE_COUNT, 0);
        eng.write_reg(LAUNCH_DMA, SINGLE_LINE_LAUNCH);

        let writes = eng.execute_pending(&|_, buf| buf.fill(0xAA));
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].gpu_va, 0x8000);
        assert_eq!(writes[0].data, vec![0xAA; 16]);
    }

    #[test]
    fn test_single_line_blocklinear_to_pitch_uses_upstream_address_conversion() {
        let mut eng = new_test_engine();
        {
            let mut mm = eng.memory_manager.lock();
            mm.map(0x1000, 0x1000, 0x1000, PteKind::Z16.raw() as u32, false);
            mm.map(0x8000, 0x8000, 0x1000, PteKind::PITCH.raw() as u32, false);
        }

        let src_addr = 0x1040;
        let dst_addr = 0x8000;
        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, src_addr);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, dst_addr);
        eng.write_reg(LINE_LENGTH, 32);
        eng.write_reg(LINE_COUNT, 0);
        eng.write_reg(LAUNCH_DMA, SINGLE_LINE_LAUNCH);

        let first = convert_linear_2_blocklinear_addr(src_addr as u64);
        let second = convert_linear_2_blocklinear_addr(src_addr as u64 + 16);
        let writes = eng.execute_pending(&|addr, buf| {
            if addr == first {
                buf.copy_from_slice(&[0x11; 16]);
            } else if addr == second {
                buf.copy_from_slice(&[0x22; 16]);
            } else {
                panic!("unexpected source address 0x{addr:X}");
            }
        });

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].gpu_va, dst_addr as u64);
        assert_eq!(&writes[0].data[..16], &[0x11; 16]);
        assert_eq!(&writes[0].data[16..32], &[0x22; 16]);
    }

    #[test]
    fn test_single_line_pitch_to_blocklinear_emits_converted_writes() {
        let mut eng = new_test_engine();
        {
            let mut mm = eng.memory_manager.lock();
            mm.map(0x1000, 0x1000, 0x1000, PteKind::PITCH.raw() as u32, false);
            mm.map(0x8000, 0x8000, 0x1000, PteKind::Z16.raw() as u32, false);
        }

        let src_addr = 0x1000;
        let dst_addr = 0x8040;
        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, src_addr);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, dst_addr);
        eng.write_reg(LINE_LENGTH, 32);
        eng.write_reg(LINE_COUNT, 0);
        eng.write_reg(LAUNCH_DMA, SINGLE_LINE_LAUNCH);

        let writes = eng.execute_pending(&|addr, buf| {
            if addr == src_addr as u64 {
                buf.copy_from_slice(&[0x33; 16]);
            } else if addr == src_addr as u64 + 16 {
                buf.copy_from_slice(&[0x44; 16]);
            } else {
                panic!("unexpected source address 0x{addr:X}");
            }
        });

        assert_eq!(writes.len(), 2);
        assert_eq!(
            writes[0].gpu_va,
            convert_linear_2_blocklinear_addr(dst_addr as u64)
        );
        assert_eq!(
            writes[1].gpu_va,
            convert_linear_2_blocklinear_addr(dst_addr as u64 + 16)
        );
        assert_eq!(writes[0].data, vec![0x33; 16]);
        assert_eq!(writes[1].data, vec![0x44; 16]);
    }

    #[test]
    fn test_single_line_remap_const_a_clears_u32_words_like_upstream() {
        let mut eng = new_test_engine();

        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x8000);
        eng.write_reg(LINE_LENGTH, 3);
        eng.write_reg(LINE_COUNT, 0);
        eng.write_reg(REMAP_CONSTA_VALUE, 0x1122_3344);
        eng.write_reg(
            REMAP_COMPONENTS,
            REMAP_SWIZZLE_CONST_A | (3 << 16), // dst_x = CONST_A, 4-byte component.
        );
        eng.write_reg(LAUNCH_DMA, SINGLE_LINE_LAUNCH | LAUNCH_REMAP_ENABLE);

        let writes = eng.execute_pending(&|_, _| panic!("CONST_A clear must not read source"));

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].gpu_va, 0x8000);
        assert_eq!(
            writes[0].data,
            vec![0x44, 0x33, 0x22, 0x11, 0x44, 0x33, 0x22, 0x11, 0x44, 0x33, 0x22, 0x11]
        );
    }

    #[test]
    fn test_single_line_remap_const_a_invalid_size_reports_and_continues_like_upstream() {
        let mut eng = new_test_engine();

        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x8000);
        eng.write_reg(LINE_LENGTH, 1);
        eng.write_reg(REMAP_CONSTA_VALUE, 0x1122_3344);
        eng.write_reg(
            REMAP_COMPONENTS,
            REMAP_SWIZZLE_CONST_A | (2 << 16), // Three-byte size triggers upstream ASSERT.
        );
        eng.write_reg(LAUNCH_DMA, SINGLE_LINE_LAUNCH | LAUNCH_REMAP_ENABLE);

        let writes = eng.execute_pending(&|_, _| {});
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].data, vec![0x44, 0x33, 0x22]);
    }

    #[test]
    fn test_dma_fallback_flushes_source_and_invalidates_destination() {
        let mut eng = new_test_engine();
        let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
        let rasterizer = TestRasterizer::new(Arc::clone(&calls));
        bind_memory_rasterizer(
            &mut eng,
            &rasterizer,
            &[(0x1000, 0x2_1000), (0x8000, 0x2_8000)],
        );

        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x1000);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x8000);
        eng.write_reg(PITCH_IN, 16);
        eng.write_reg(PITCH_OUT, 32);
        eng.write_reg(LINE_LENGTH, 8);
        eng.write_reg(LINE_COUNT, 3);
        eng.write_reg(LAUNCH_DMA, MULTI_LINE_PITCH_TO_PITCH_LAUNCH);

        let src = vec![0x55; 0x100];
        let writes = eng.execute_pending(&|addr, buf| {
            if addr == 0x8000 {
                buf.fill(0);
                return;
            }
            let offset = (addr - 0x1000) as usize;
            buf.copy_from_slice(&src[offset..offset + buf.len()]);
        });

        assert_eq!(writes.len(), 1);
        let calls = calls.lock();
        assert_eq!(calls.flushes, vec![(0x2_1000, 40)]);
        assert_eq!(calls.invalidations, vec![(0x2_8000, 72)]);
    }

    #[test]
    fn test_release_one_word_semaphore_queries_payload_fence_after_dma() {
        let mut eng = new_test_engine();
        let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
        let rasterizer = TestRasterizer::new(Arc::clone(&calls));
        eng.bind_rasterizer(&rasterizer);

        eng.write_reg(SEMAPHORE_ADDR_HIGH, 0x123);
        eng.write_reg(SEMAPHORE_ADDR_LOW, 0x4567_8000);
        eng.write_reg(SEMAPHORE_PAYLOAD, 0xCAFE_BABE);
        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x1000);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x8000);
        eng.write_reg(PITCH_IN, 16);
        eng.write_reg(PITCH_OUT, 16);
        eng.write_reg(LINE_LENGTH, 8);
        eng.write_reg(LINE_COUNT, 1);
        eng.write_reg(LAUNCH_DMA, RELEASE_ONE_WORD_SEMAPHORE_LAUNCH);

        let src = vec![0x55; 0x100];
        let writes = eng.execute_pending(&|addr, buf| {
            if addr == 0x8000 {
                buf.fill(0);
                return;
            }
            let offset = (addr - 0x1000) as usize;
            buf.copy_from_slice(&src[offset..offset + buf.len()]);
        });

        assert_eq!(writes.len(), 1);
        let calls = calls.lock();
        assert_eq!(
            calls.queries,
            vec![(
                0x23_4567_8000,
                QueryType::Payload as u32,
                QueryPropertiesFlags::IS_A_FENCE,
                0xCAFE_BABE,
                0,
            )]
        );
    }

    #[test]
    fn test_release_four_word_semaphore_adds_timeout_flag() {
        let mut eng = new_test_engine();
        let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
        let rasterizer = TestRasterizer::new(Arc::clone(&calls));
        eng.bind_rasterizer(&rasterizer);

        eng.write_reg(SEMAPHORE_ADDR_HIGH, 0);
        eng.write_reg(SEMAPHORE_ADDR_LOW, 0x9000);
        eng.write_reg(SEMAPHORE_PAYLOAD, 0x1357_9BDF);
        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x1000);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x8000);
        eng.write_reg(PITCH_IN, 16);
        eng.write_reg(PITCH_OUT, 16);
        eng.write_reg(LINE_LENGTH, 8);
        eng.write_reg(LINE_COUNT, 1);
        eng.write_reg(LAUNCH_DMA, RELEASE_FOUR_WORD_SEMAPHORE_LAUNCH);

        let src = vec![0x66; 0x100];
        let writes = eng.execute_pending(&|addr, buf| {
            if addr == 0x8000 {
                buf.fill(0);
                return;
            }
            let offset = (addr - 0x1000) as usize;
            buf.copy_from_slice(&src[offset..offset + buf.len()]);
        });

        assert_eq!(writes.len(), 1);
        let calls = calls.lock();
        assert_eq!(
            calls.queries,
            vec![(
                0x9000,
                QueryType::Payload as u32,
                QueryPropertiesFlags::IS_A_FENCE | QueryPropertiesFlags::HAS_TIMEOUT,
                0x1357_9BDF,
                0,
            )]
        );
    }

    #[test]
    fn test_zero_length_valid_launch_still_releases_semaphore() {
        let mut eng = new_test_engine();
        let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
        let rasterizer = TestRasterizer::new(Arc::clone(&calls));
        eng.bind_rasterizer(&rasterizer);

        eng.write_reg(SEMAPHORE_ADDR_HIGH, 0);
        eng.write_reg(SEMAPHORE_ADDR_LOW, 0x9000);
        eng.write_reg(SEMAPHORE_PAYLOAD, 0x2468_ACE0);
        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x1000);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x8000);
        eng.write_reg(PITCH_IN, 16);
        eng.write_reg(PITCH_OUT, 16);
        eng.write_reg(LINE_LENGTH, 0);
        eng.write_reg(LINE_COUNT, 1);
        eng.write_reg(LAUNCH_DMA, RELEASE_ONE_WORD_SEMAPHORE_LAUNCH);

        let writes = eng.execute_pending(&|_, _| panic!("zero-length DMA should not read"));

        assert!(writes.is_empty());
        let calls = calls.lock();
        assert_eq!(
            calls.queries,
            vec![(
                0x9000,
                QueryType::Payload as u32,
                QueryPropertiesFlags::IS_A_FENCE,
                0x2468_ACE0,
                0,
            )]
        );
    }

    #[test]
    fn test_accelerated_blocklinear_to_pitch_releases_semaphore_without_pending_write() {
        let mut eng = new_test_engine();
        let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
        let mut rasterizer = TestRasterizer::new(Arc::clone(&calls));
        rasterizer.accelerate_image_to_buffer = true;
        eng.bind_rasterizer(&rasterizer);

        eng.write_reg(SEMAPHORE_ADDR_HIGH, 0);
        eng.write_reg(SEMAPHORE_ADDR_LOW, 0xA000);
        eng.write_reg(SEMAPHORE_PAYLOAD, 0x1020_3040);
        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x1000);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x8000);
        eng.write_reg(PITCH_IN, 16);
        eng.write_reg(PITCH_OUT, 16);
        eng.write_reg(LINE_LENGTH, 16);
        eng.write_reg(LINE_COUNT, 2);
        write_dma_params(
            &mut eng,
            SRC_PARAMS,
            dma::Parameters {
                block_size: dma::BlockSize { raw: 0 },
                width: 16,
                height: 2,
                depth: 1,
                layer: 0,
                origin: dma::Origin { raw: 0 },
            },
        );
        eng.write_reg(
            LAUNCH_DMA,
            MULTI_LINE_BLOCKLINEAR_TO_PITCH_LAUNCH | (LAUNCH_SEMAPHORE_TYPE_RELEASE_ONE_WORD << 3),
        );

        let writes = eng.execute_pending(&|_, _| panic!("accelerated DMA should not read"));

        assert!(writes.is_empty());
        let calls = calls.lock();
        assert_eq!(calls.dma_image_to_buffers.len(), 1);
        assert_eq!(
            calls.queries,
            vec![(
                0xA000,
                QueryType::Payload as u32,
                QueryPropertiesFlags::IS_A_FENCE,
                0x1020_3040,
                0,
            )]
        );
    }

    #[test]
    fn test_call_method_launch_executes_immediately() {
        let mut eng = new_test_engine();
        assert!(!eng.pending_launch);
        eng.call_method(LAUNCH_DMA, MULTI_LINE_PITCH_TO_PITCH_LAUNCH, true);
        assert!(!eng.pending_launch);
    }

    #[test]
    fn test_call_multi_method_launch_executes_immediately() {
        let mut eng = new_test_engine();
        assert!(!eng.pending_launch);
        eng.call_multi_method(LAUNCH_DMA, &[MULTI_LINE_PITCH_TO_PITCH_LAUNCH], 1, 1);
        assert!(!eng.pending_launch);
    }

    #[test]
    fn test_dma_blocklinear_invalid_depth_reports_and_continues_like_upstream() {
        let mut eng = new_test_engine();

        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x1000);
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x2000);
        eng.write_reg(PITCH_IN, 8);
        eng.write_reg(PITCH_OUT, 8);
        eng.write_reg(LINE_LENGTH, 8);
        eng.write_reg(LINE_COUNT, 2);
        eng.write_reg(
            LAUNCH_DMA,
            LAUNCH_DATA_TRANSFER_NON_PIPELINED
                | LAUNCH_DST_MEMORY_LAYOUT_PITCH
                | LAUNCH_MULTI_LINE_ENABLE,
        );

        let writes = eng.execute_pending(&|_, buf| buf.fill(0xAA));
        assert!(!writes.is_empty());
    }

    #[test]
    fn test_constructor_keeps_memory_manager_owner() {
        let memory_manager = Arc::new(Mutex::new(MemoryManager::new(0x44)));
        let eng = MaxwellDMA::new(Arc::clone(&memory_manager));
        assert!(Arc::ptr_eq(&eng.memory_manager, &memory_manager));
    }
}

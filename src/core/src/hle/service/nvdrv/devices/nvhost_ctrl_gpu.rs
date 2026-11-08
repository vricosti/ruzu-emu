// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/nvdrv/devices/nvhost_ctrl_gpu.h
//! Port of zuyu/src/core/hle/service/nvdrv/devices/nvhost_ctrl_gpu.cpp

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::core::SystemRef;
use crate::hle::kernel::k_readable_event::KReadableEvent;
use crate::hle::service::nvdrv::core::container::SessionId;
use crate::hle::service::nvdrv::devices::nvdevice::NvDevice;
use crate::hle::service::nvdrv::devices::nvmap::{read_struct, write_struct};
use crate::hle::service::nvdrv::nvdata::{DeviceFD, Ioctl, NvResult};
use crate::hle::service::nvdrv::nvdrv::EventInterface;

// Eden-NX2 nvhost_ctrl_gpu.cpp anonymous namespace.
const NX2_CHARACTERISTICS_SIZE: u64 = 0xD0;

fn apply_nx2_characteristics(gc: &mut IoctlGpuCharacteristics) {
    gc.arch = 0x170;
    gc.r#impl = 0xE;
    gc.num_tpc_per_gpc = 6;
    gc.sm_arch_sm_version = 0x808;
    gc.sm_arch_spa_version = 0x808;
    gc.threed_class = 0xC797;
    gc.compute_class = 0xC7C0;
    gc.gpfifo_class = 0xC56F;
    gc.dma_copy_class = 0xC7B5;
    gc.chipname = 0x6631306167;
    gc.flags |= 0x100 | (1 << 22) | (1 << 26) | (1 << 34);
    // Nine little-endian s16 interface versions overlay offsets 0x70..=0x80.
    gc.max_fbps_count = 0x00400040;
    gc.fbp_en_mask = 0x00400040;
    gc.max_ltc_per_fbp = 0x00400040;
    gc.max_lts_per_ltc = 0x00400040;
    gc.max_tex_per_tpc = (gc.max_tex_per_tpc & 0xffff0000) | 0x40;
}

fn apply_nx2_characteristics_tail(block: &mut [u8]) {
    const MAX_SUBCONTEXTS_OFFSET: usize = 0xB4;
    const MAX_SUBCONTEXTS: u32 = 64;
    if block.len() >= MAX_SUBCONTEXTS_OFFSET + 4 {
        block[MAX_SUBCONTEXTS_OFFSET..MAX_SUBCONTEXTS_OFFSET + 4]
            .copy_from_slice(&MAX_SUBCONTEXTS.to_le_bytes());
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlGpuCharacteristics {
    pub arch: u32,
    pub r#impl: u32,
    pub rev: u32,
    pub num_gpc: u32,
    pub l2_cache_size: u64,
    pub on_board_video_memory_size: u64,
    pub num_tpc_per_gpc: u32,
    pub bus_type: u32,
    pub big_page_size: u32,
    pub compression_page_size: u32,
    pub pde_coverage_bit_count: u32,
    pub available_big_page_sizes: u32,
    pub gpc_mask: u32,
    pub sm_arch_sm_version: u32,
    pub sm_arch_spa_version: u32,
    pub sm_arch_warp_count: u32,
    pub gpu_va_bit_count: u32,
    pub reserved: u32,
    pub flags: u64,
    pub twod_class: u32,
    pub threed_class: u32,
    pub compute_class: u32,
    pub gpfifo_class: u32,
    pub inline_to_memory_class: u32,
    pub dma_copy_class: u32,
    pub max_fbps_count: u32,
    pub fbp_en_mask: u32,
    pub max_ltc_per_fbp: u32,
    pub max_lts_per_ltc: u32,
    pub max_tex_per_tpc: u32,
    pub max_gpc_count: u32,
    pub rop_l2_en_mask_0: u32,
    pub rop_l2_en_mask_1: u32,
    pub chipname: u64,
    pub gr_compbit_store_base_hw: u64,
}
const _: () = assert!(std::mem::size_of::<IoctlGpuCharacteristics>() == 160);
const _: () = assert!(std::mem::offset_of!(IoctlGpuCharacteristics, twod_class) == 0x58);
const _: () = assert!(std::mem::offset_of!(IoctlGpuCharacteristics, threed_class) == 0x5C);
const _: () = assert!(std::mem::offset_of!(IoctlGpuCharacteristics, compute_class) == 0x60);
const _: () = assert!(std::mem::offset_of!(IoctlGpuCharacteristics, gpfifo_class) == 0x64);
const _: () =
    assert!(std::mem::offset_of!(IoctlGpuCharacteristics, inline_to_memory_class) == 0x68);
const _: () = assert!(std::mem::offset_of!(IoctlGpuCharacteristics, dma_copy_class) == 0x6C);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlCharacteristics {
    pub gpu_characteristics_buf_size: u64,
    pub gpu_characteristics_buf_addr: u64,
    pub gc: IoctlGpuCharacteristics,
}
const _: () = assert!(
    std::mem::size_of::<IoctlCharacteristics>()
        == 16 + std::mem::size_of::<IoctlGpuCharacteristics>()
);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlGpuGetTpcMasksArgs {
    pub mask_buffer_size: u32,
    pub _pad0: u32,
    pub mask_buffer_address: u64,
    pub tcp_mask: u32,
    pub _pad1: u32,
}
const _: () = assert!(std::mem::size_of::<IoctlGpuGetTpcMasksArgs>() == 24);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlActiveSlotMask {
    pub slot: u32,
    pub mask: u32,
}
const _: () = assert!(std::mem::size_of::<IoctlActiveSlotMask>() == 8);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlZcullGetCtxSize {
    pub size: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlNvgpuGpuZcullGetInfoArgs {
    pub width_align_pixels: u32,
    pub height_align_pixels: u32,
    pub pixel_squares_by_aliquots: u32,
    pub aliquot_total: u32,
    pub region_byte_multiplier: u32,
    pub region_header_size: u32,
    pub subregion_header_size: u32,
    pub subregion_width_align_pixels: u32,
    pub subregion_height_align_pixels: u32,
    pub subregion_count: u32,
}
const _: () = assert!(std::mem::size_of::<IoctlNvgpuGpuZcullGetInfoArgs>() == 40);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlZbcSetTable {
    pub color_ds: [u32; 4],
    pub color_l2: [u32; 4],
    pub depth: u32,
    pub format: u32,
    pub r#type: u32,
}
const _: () = assert!(std::mem::size_of::<IoctlZbcSetTable>() == 44);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlZbcQueryTable {
    pub color_ds: [u32; 4],
    pub color_l2: [u32; 4],
    pub depth: u32,
    pub ref_cnt: u32,
    pub format: u32,
    pub r#type: u32,
    pub index_size: u32,
}
const _: () = assert!(std::mem::size_of::<IoctlZbcQueryTable>() == 52);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlFlushL2 {
    pub flush: u32,
    pub reserved: u32,
}
const _: () = assert!(std::mem::size_of::<IoctlFlushL2>() == 8);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlGetGpuTime {
    pub gpu_time: u64,
    pub _pad: [u32; 2],
}
const _: () = assert!(std::mem::size_of::<IoctlGetGpuTime>() == 0x10);

/// nvhost_ctrl_gpu device.
pub struct NvHostCtrlGpu {
    system: SystemRef,
    events_interface: Arc<EventInterface>,
    // NvDevice dispatch uses shared references; upstream's sticky bool is atomic here.
    nx2_caller: AtomicBool,
    error_notifier_event: Option<Arc<Mutex<KReadableEvent>>>,
    unknown_event: Option<Arc<Mutex<KReadableEvent>>>,
}

impl NvHostCtrlGpu {
    pub fn new(system: SystemRef, events_interface: Arc<EventInterface>) -> Self {
        Self {
            system,
            nx2_caller: AtomicBool::new(false),
            error_notifier_event: events_interface.create_event("CtrlGpuErrorNotifier"),
            unknown_event: events_interface.create_event("CtrlGpuUnknownEvent"),
            events_interface,
        }
    }

    fn fill_gpu_characteristics(gc: &mut IoctlGpuCharacteristics) {
        gc.arch = 0x120;
        gc.r#impl = 0xb;
        gc.rev = 0xa1;
        gc.num_gpc = 0x1;
        gc.l2_cache_size = 0x40000;
        gc.on_board_video_memory_size = 0x0;
        gc.num_tpc_per_gpc = 0x2;
        gc.bus_type = 0x20;
        gc.big_page_size = 0x20000;
        gc.compression_page_size = 0x20000;
        gc.pde_coverage_bit_count = 0x1B;
        gc.available_big_page_sizes = 0x30000;
        gc.gpc_mask = 0x1;
        gc.sm_arch_sm_version = 0x503;
        gc.sm_arch_spa_version = 0x503;
        gc.sm_arch_warp_count = 0x80;
        gc.gpu_va_bit_count = 0x28;
        gc.reserved = 0x0;
        gc.flags = 0x55;
        gc.twod_class = 0x902D;
        gc.threed_class = 0xB197;
        gc.compute_class = 0xB1C0;
        gc.gpfifo_class = 0xB06F;
        gc.inline_to_memory_class = 0xA140;
        gc.dma_copy_class = 0xB0B5;
        gc.max_fbps_count = 0x1;
        gc.fbp_en_mask = 0x0;
        gc.max_ltc_per_fbp = 0x2;
        gc.max_lts_per_ltc = 0x1;
        gc.max_tex_per_tpc = 0x0;
        gc.max_gpc_count = 0x1;
        gc.rop_l2_en_mask_0 = 0x21D70;
        gc.rop_l2_en_mask_1 = 0x0;
        gc.chipname = 0x6230326D67;
        gc.gr_compbit_store_base_hw = 0x0;
    }

    pub fn get_characteristics1(&self, params: &mut IoctlCharacteristics) -> NvResult {
        log::debug!("nvhost_ctrl_gpu::GetCharacteristics called");
        Self::fill_gpu_characteristics(&mut params.gc);
        params.gpu_characteristics_buf_size = 0xA0;
        params.gpu_characteristics_buf_addr = 0xdeadbeef;
        if std::env::var_os("RUZU_TRACE_NVHOST_CTRL_GPU_CHAR").is_some() {
            log::info!(
                "[NVHOST_CTRL_GPU_CHAR] size=0x{:X} addr=0x{:X} twod=0x{:04X} threed=0x{:04X} compute=0x{:04X} gpfifo=0x{:04X} inline=0x{:04X} dma=0x{:04X}",
                params.gpu_characteristics_buf_size,
                params.gpu_characteristics_buf_addr,
                params.gc.twod_class,
                params.gc.threed_class,
                params.gc.compute_class,
                params.gc.gpfifo_class,
                params.gc.inline_to_memory_class,
                params.gc.dma_copy_class
            );
        }
        NvResult::Success
    }

    fn get_characteristics3(&self, params: &mut IoctlCharacteristics, inline: &mut [u8]) -> NvResult {
        let nx2 = params.gpu_characteristics_buf_size >= NX2_CHARACTERISTICS_SIZE;
        Self::fill_gpu_characteristics(&mut params.gc);
        params.gpu_characteristics_buf_size = 0xA0;
        params.gpu_characteristics_buf_addr = 0xdeadbeef;
        if nx2 {
            self.nx2_caller.store(true, Ordering::Relaxed);
            super::nx2_stub_device::G_NX2_DRIVER.store(true, Ordering::SeqCst);
            // Production systems own a GPU; isolated device tests can omit it.
            if let Some(gpu) = self.system.get().gpu_core() {
                gpu.set_nx2_ampere(true);
            }
            apply_nx2_characteristics(&mut params.gc);
            params.gpu_characteristics_buf_size = NX2_CHARACTERISTICS_SIZE;
            log::warn!("Switch 2 caller, reporting Ampere chip 0x17E");
        }
        if inline.len() >= std::mem::size_of::<IoctlGpuCharacteristics>() {
            write_struct(inline, &params.gc);
        }
        NvResult::Success
    }

    fn nx2_get_engine_info(&self, input: &[u8], output: &mut [u8], inline: &mut [u8]) -> NvResult {
        // Two source Item records: engine_id, instance, signed runlist_id, reserved.
        const ITEMS: [u32; 8] = [0, 0, 0, 0, 2, 0, 1, 0];
        const NEEDED: u32 = 32;
        let buf_size = input.get(..4).map(|v| u32::from_le_bytes(v.try_into().unwrap())).unwrap_or(0);
        log::warn!("(STUBBED) Switch 2 GetEngineInfo buf_size={buf_size:#x} inline_out={:#x}", inline.len());
        if buf_size >= NEEDED && inline.len() >= NEEDED as usize {
            for (dst, value) in inline[..NEEDED as usize].chunks_exact_mut(4).zip(ITEMS) {
                dst.copy_from_slice(&value.to_le_bytes());
            }
        }
        output.fill(0);
        let copied = input.len().min(output.len());
        output[..copied].copy_from_slice(&input[..copied]);
        if output.len() >= 4 {
            output[..4].copy_from_slice(&NEEDED.to_le_bytes());
        }
        NvResult::Success
    }

    pub fn get_tpc_masks(&self, params: &mut IoctlGpuGetTpcMasksArgs) -> NvResult {
        log::debug!(
            "nvhost_ctrl_gpu::GetTPCMasks called, mask_buffer_size=0x{:X}",
            params.mask_buffer_size
        );
        if params.mask_buffer_size != 0 {
            params.tcp_mask = 3;
        }
        NvResult::Success
    }

    pub fn get_active_slot_mask(&self, params: &mut IoctlActiveSlotMask) -> NvResult {
        log::debug!("nvhost_ctrl_gpu::GetActiveSlotMask called");
        params.slot = 0x07;
        params.mask = 0x01;
        NvResult::Success
    }

    pub fn zcull_get_ctx_size(&self, params: &mut IoctlZcullGetCtxSize) -> NvResult {
        log::debug!("nvhost_ctrl_gpu::ZCullGetCtxSize called");
        params.size = 0x1;
        NvResult::Success
    }

    pub fn zcull_get_info(&self, params: &mut IoctlNvgpuGpuZcullGetInfoArgs) -> NvResult {
        log::debug!("nvhost_ctrl_gpu::ZCullGetInfo called");
        params.width_align_pixels = 0x20;
        params.height_align_pixels = 0x20;
        params.pixel_squares_by_aliquots = 0x400;
        params.aliquot_total = 0x800;
        params.region_byte_multiplier = 0x20;
        params.region_header_size = 0x20;
        params.subregion_header_size = 0xc0;
        params.subregion_width_align_pixels = 0x20;
        params.subregion_height_align_pixels = 0x40;
        params.subregion_count = 0x10;
        NvResult::Success
    }

    pub fn zbc_set_table(&self, _params: &mut IoctlZbcSetTable) -> NvResult {
        log::warn!("nvhost_ctrl_gpu::ZBCSetTable (STUBBED) called");
        NvResult::Success
    }

    pub fn zbc_query_table(&self, _params: &mut IoctlZbcQueryTable) -> NvResult {
        log::warn!("nvhost_ctrl_gpu::ZBCQueryTable (STUBBED) called");
        NvResult::Success
    }

    pub fn flush_l2(&self, _params: &mut IoctlFlushL2) -> NvResult {
        log::warn!("nvhost_ctrl_gpu::FlushL2 (STUBBED) called");
        NvResult::Success
    }

    pub fn get_gpu_time(&self, params: &mut IoctlGetGpuTime) -> NvResult {
        log::debug!("nvhost_ctrl_gpu::GetGpuTime called");
        params.gpu_time = self
            .system
            .get()
            .core_timing()
            .get_global_time_ns()
            .as_nanos() as u64;
        NvResult::Success
    }
}

impl Drop for NvHostCtrlGpu {
    fn drop(&mut self) {
        if let Some(event) = self.error_notifier_event.take() {
            self.events_interface.free_event(event);
        }
        if let Some(event) = self.unknown_event.take() {
            self.events_interface.free_event(event);
        }
    }
}

impl NvDevice for NvHostCtrlGpu {
    fn ioctl1(&self, _fd: DeviceFD, command: Ioctl, input: &[u8], output: &mut [u8]) -> NvResult {
        match command.group() {
            b'G' => match command.cmd() {
                0x1 => {
                    let mut params: IoctlZcullGetCtxSize = read_struct(input);
                    let r = self.zcull_get_ctx_size(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x2 => {
                    let mut params: IoctlNvgpuGpuZcullGetInfoArgs = read_struct(input);
                    let r = self.zcull_get_info(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x3 => {
                    let mut params: IoctlZbcSetTable = read_struct(input);
                    let r = self.zbc_set_table(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x4 => {
                    let mut params: IoctlZbcQueryTable = read_struct(input);
                    let r = self.zbc_query_table(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x5 => {
                    let mut params: IoctlCharacteristics = read_struct(input);
                    let r = self.get_characteristics1(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x6 => {
                    let mut params: IoctlGpuGetTpcMasksArgs = read_struct(input);
                    let r = self.get_tpc_masks(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x7 => {
                    let mut params: IoctlFlushL2 = read_struct(input);
                    let r = self.flush_l2(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x14 => {
                    let mut params: IoctlActiveSlotMask = read_struct(input);
                    let r = self.get_active_slot_mask(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x1c => {
                    let mut params: IoctlGetGpuTime = read_struct(input);
                    let r = self.get_gpu_time(&mut params);
                    write_struct(output, &params);
                    r
                }
                _ => {
                    log::error!("Unimplemented ioctl={:08X}", command.raw);
                    NvResult::NotImplemented
                }
            },
            _ => {
                log::error!("Unimplemented ioctl={:08X}", command.raw);
                NvResult::NotImplemented
            }
        }
    }

    fn ioctl2(
        &self,
        _fd: DeviceFD,
        command: Ioctl,
        _input: &[u8],
        _inline_input: &[u8],
        _output: &mut [u8],
    ) -> NvResult {
        log::error!("Unimplemented ioctl={:08X}", command.raw);
        NvResult::NotImplemented
    }

    fn ioctl3(
        &self,
        _fd: DeviceFD,
        command: Ioctl,
        input: &[u8],
        output: &mut [u8],
        inline_output: &mut [u8],
    ) -> NvResult {
        if command.group() == b'G' {
            match command.cmd() {
                0x5 => {
                    let mut params: IoctlCharacteristics = read_struct(input);
                    let result = self.get_characteristics3(&mut params, inline_output);
                    write_struct(output, &params);
                    if self.nx2_caller.load(Ordering::Relaxed) {
                        apply_nx2_characteristics_tail(inline_output);
                    }
                    return result;
                }
                0x6 => {
                    let mut params: IoctlGpuGetTpcMasksArgs = read_struct(input);
                    let result = self.get_tpc_masks(&mut params);
                    write_struct(output, &params);
                    if inline_output.len() >= 4 {
                        write_struct(inline_output, &params.tcp_mask);
                    }
                    return result;
                }
                0x13 => return NvResult::NotImplemented,
                _ => {}
            }
        }
        if self.nx2_caller.load(Ordering::Relaxed) {
            if command.group() == b'G' && command.cmd() == 0x1F {
                return self.nx2_get_engine_info(input, output, inline_output);
            }
            log::warn!("(STUBBED) Switch 2 ioctl={:08X} group={:X} command={:X}",
                command.raw, command.group(), command.cmd());
            output.fill(0);
            inline_output.fill(0);
            return NvResult::Success;
        }
        log::error!("Unimplemented ioctl={:08X}", command.raw);
        NvResult::NotImplemented
    }

    fn on_open(&self, _session_id: SessionId, _fd: DeviceFD) {}
    fn on_close(&self, _fd: DeviceFD) {}

    fn query_event(&self, event_id: u32) -> Option<Arc<Mutex<KReadableEvent>>> {
        match event_id {
            1 => self.error_notifier_event.clone(),
            2 => self.unknown_event.clone(),
            _ => {
                log::error!("Unknown Ctrl GPU Event {}", event_id);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::System;

    #[test]
    fn nx2_characteristics_layout_and_sticky_detection() {
        let system = System::new_boxed_for_test();
        let events = Arc::new(EventInterface::new(SystemRef::from_ref(&system)));
        let device = NvHostCtrlGpu::new(SystemRef::from_ref(&system), events);
        let mut input = [0u8; 176];
        input[..8].copy_from_slice(&0xD0u64.to_le_bytes());
        let mut output = [0u8; 176];
        let mut inline = [0xAAu8; 0xD0];
        let command = Ioctl { raw: (u32::from(b'G') << 8) | 5 };
        assert_eq!(device.ioctl3(0, command, &input, &mut output, &mut inline), NvResult::Success);
        let params: IoctlCharacteristics = read_struct(&output);
        assert_eq!(params.gpu_characteristics_buf_size, 0xD0);
        assert_eq!(params.gc.arch + params.gc.r#impl, 0x17E);
        assert_eq!(params.gc.sm_arch_sm_version, 0x808);
        assert_eq!(params.gc.flags, 0x55 | 0x100 | (1 << 22) | (1 << 26) | (1 << 34));
        for offset in (0x70..=0x80).step_by(2) {
            assert_eq!(&inline[offset..offset + 2], &0x40u16.to_le_bytes());
        }
        assert_eq!(&inline[0xB4..0xB8], &64u32.to_le_bytes());
        assert!(inline[0xA0..0xB4].iter().all(|&b| b == 0xAA));
        // A later legacy query returns Maxwell, but the caller latch still patches the tail.
        input[..8].copy_from_slice(&0xA0u64.to_le_bytes());
        inline.fill(0xAA);
        device.ioctl3(0, command, &input, &mut output, &mut inline);
        assert_eq!(read_struct::<IoctlCharacteristics>(&output).gc.arch, 0x120);
        assert_eq!(&inline[0xB4..0xB8], &64u32.to_le_bytes());
        assert!(device.nx2_caller.load(Ordering::Relaxed));
    }

    #[test]
    fn nx2_engine_info_sizes_and_ioctl_fallback() {
        let system = System::new_boxed_for_test();
        let events = Arc::new(EventInterface::new(SystemRef::from_ref(&system)));
        let device = NvHostCtrlGpu::new(SystemRef::from_ref(&system), events);
        let command = Ioctl { raw: (u32::from(b'G') << 8) | 0x1F };
        let mut output = [0xAA; 20];
        let mut inline = [0xAA; 36];
        assert_eq!(device.ioctl3(0, command, &[], &mut output, &mut inline), NvResult::NotImplemented);
        device.nx2_caller.store(true, Ordering::Relaxed);
        device.ioctl3(0, command, &[], &mut output, &mut inline);
        assert_eq!(&output[..4], &32u32.to_le_bytes());
        assert_eq!(&output[4..], &[0; 16]);
        assert_eq!(inline, [0xAA; 36]);
        let input = [32, 0, 0, 0, 9, 8, 7, 6];
        device.ioctl3(0, command, &input, &mut output, &mut inline);
        assert_eq!(&output[4..8], &input[4..8]);
        assert_eq!(&inline[..16], &[0; 16]);
        assert_eq!(&inline[16..20], &2u32.to_le_bytes());
        assert_eq!(&inline[24..28], &1u32.to_le_bytes());
        assert_eq!(&inline[32..], &[0xAA; 4]);
        let unknown = Ioctl { raw: 0xFFFF };
        assert_eq!(device.ioctl3(0, unknown, &[], &mut output, &mut inline), NvResult::Success);
        assert_eq!(output, [0; 20]);
        assert_eq!(inline, [0; 36]);
        let unsupported = Ioctl { raw: (u32::from(b'G') << 8) | 0x13 };
        assert_eq!(device.ioctl3(0, unsupported, &[], &mut output, &mut inline), NvResult::NotImplemented);
    }

    #[test]
    fn get_gpu_time_uses_core_timing() {
        let system = System::new_boxed_for_test();
        system.core_timing().add_ticks(1234);

        let events = Arc::new(EventInterface::new(SystemRef::from_ref(&system)));
        let ctrl_gpu = NvHostCtrlGpu::new(SystemRef::from_ref(&system), events);
        let mut params = IoctlGetGpuTime::default();

        let result = ctrl_gpu.get_gpu_time(&mut params);

        assert_eq!(result, NvResult::Success);
        assert_eq!(
            params.gpu_time,
            system.core_timing().get_global_time_ns().as_nanos() as u64
        );
    }
}

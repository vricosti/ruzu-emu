// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/nvdrv/devices/nvdisp_disp0.h
//! Port of zuyu/src/core/hle/service/nvdrv/devices/nvdisp_disp0.cpp

use crate::core::SystemRef;
use crate::gpu_core::{BlendMode as GpuBlendMode, FramebufferConfig as GpuFramebufferConfig};
use crate::hle::service::nvdrv::core::container::SessionId;
use crate::hle::service::nvdrv::core::nvmap::NvMap;
use crate::hle::service::nvdrv::devices::nvdevice::NvDevice;
use crate::hle::service::nvdrv::nvdata::{DeviceFD, Ioctl, NvResult};
use crate::hle::service::nvnflinger::hwc_layer::{HwcLayer, LayerBlending};

/// nvdisp_disp0 device: display compositor.
pub struct NvDispDisp0 {
    system: SystemRef,
    nvmap: *const NvMap,
}

unsafe impl Send for NvDispDisp0 {}
unsafe impl Sync for NvDispDisp0 {}

impl NvDispDisp0 {
    pub fn new(system: SystemRef, nvmap: &NvMap) -> Self {
        Self {
            system,
            nvmap: nvmap as *const _,
        }
    }

    fn nvmap(&self) -> &NvMap {
        unsafe { &*self.nvmap }
    }

    fn convert_blending(blending: LayerBlending) -> GpuBlendMode {
        match blending {
            LayerBlending::None => GpuBlendMode::Opaque,
            LayerBlending::Premultiplied => GpuBlendMode::Premultiplied,
            LayerBlending::Coverage => GpuBlendMode::Coverage,
        }
    }

    /// Performs a screen flip, compositing each buffer.
    pub fn composite(&self, sorted_layers: &[HwcLayer]) {
        let mut output_layers = Vec::with_capacity(sorted_layers.len());
        let mut output_fences = Vec::new();

        for (index, layer) in sorted_layers.iter().enumerate() {
            let address = self.nvmap().get_handle_address(layer.buffer_handle);
            if std::env::var_os("RUZU_TRACE_PRESENT").is_some() {
                log::info!(
                    "[RUZU_COMPOSITE] layer={} handle={} addr=0x{:X} offset=0x{:X} {}x{} stride={} format={} transform=0x{:X} crop=({},{})->({},{}) fences={}",
                    index,
                    layer.buffer_handle,
                    address,
                    layer.offset,
                    layer.width,
                    layer.height,
                    layer.stride,
                    layer.format as u32,
                    layer.transform.bits(),
                    layer.crop_rect.left,
                    layer.crop_rect.top,
                    layer.crop_rect.right,
                    layer.crop_rect.bottom,
                    layer.acquire_fence.num_fences
                );
                for (fence_index, fence) in layer.acquire_fence.fences
                    [..layer.acquire_fence.num_fences as usize]
                    .iter()
                    .enumerate()
                {
                    log::info!(
                        "[RUZU_COMPOSITE_FENCE] layer={} fence={} id={} value={}",
                        index,
                        fence_index,
                        fence.id,
                        fence.value
                    );
                }
            }
            if common::trace::is_enabled(common::trace::cat::HWC) {
                common::trace::emit_raw(
                    common::trace::cat::HWC,
                    &[
                        3,
                        index as u64,
                        layer.buffer_handle as u64,
                        address,
                        layer.offset as u64,
                        layer.width as u64,
                        layer.height as u64,
                        layer.stride as u64,
                        layer.format as u64,
                        layer.transform.bits() as u64,
                        layer.crop_rect.left as u64,
                        layer.crop_rect.top as u64,
                        layer.crop_rect.right as u64,
                        layer.crop_rect.bottom as u64,
                    ],
                );
            }
            output_layers.push(GpuFramebufferConfig {
                address,
                offset: layer.offset,
                width: layer.width,
                height: layer.height,
                stride: layer.stride,
                pixel_format: layer.format,
                transform_flags: layer.transform,
                crop_rect: layer.crop_rect,
                blending: Self::convert_blending(layer.blending),
            });

            output_fences.extend(
                layer.acquire_fence.fences[..layer.acquire_fence.num_fences as usize]
                    .iter()
                    .copied(),
            );
        }

        let system = self.system.get();
        system
            .gpu_core()
            .expect("GPU core must exist before nvdisp composite")
            .request_composite(output_layers, output_fences);
        self.system.do_speed_limiting_now();

        if let Some(stats) = system.get_perf_stats() {
            stats.end_system_frame();
            stats.begin_system_frame();
        }
    }

    /// Wait for registration of the previous composite request.
    pub fn wait_for_composite(&self) {
        self.system
            .get()
            .gpu_core()
            .expect("GPU core must exist before nvdisp composite")
            .wait_for_composite();
    }
}

impl NvDevice for NvDispDisp0 {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn ioctl1(&self, _fd: DeviceFD, command: Ioctl, _input: &[u8], _output: &mut [u8]) -> NvResult {
        log::error!("Unimplemented ioctl={:08X}", command.raw);
        NvResult::NotImplemented
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
        _input: &[u8],
        _output: &mut [u8],
        _inline_output: &mut [u8],
    ) -> NvResult {
        log::error!("Unimplemented ioctl={:08X}", command.raw);
        NvResult::NotImplemented
    }

    fn on_open(&self, _session_id: SessionId, _fd: DeviceFD) {}
    fn on_close(&self, _fd: DeviceFD) {}

    fn query_event(
        &self,
        event_id: u32,
    ) -> Option<
        std::sync::Arc<std::sync::Mutex<crate::hle::kernel::k_readable_event::KReadableEvent>>,
    > {
        log::error!("Unknown DISP Event {}", event_id);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::NvDispDisp0;
    use crate::gpu_core::BlendMode;
    use crate::hle::service::nvnflinger::hwc_layer::LayerBlending;

    #[test]
    fn convert_blending_matches_upstream_values() {
        assert_eq!(
            NvDispDisp0::convert_blending(LayerBlending::None),
            BlendMode::Opaque
        );
        assert_eq!(
            NvDispDisp0::convert_blending(LayerBlending::Premultiplied),
            BlendMode::Premultiplied
        );
        assert_eq!(
            NvDispDisp0::convert_blending(LayerBlending::Coverage),
            BlendMode::Coverage
        );
    }
}

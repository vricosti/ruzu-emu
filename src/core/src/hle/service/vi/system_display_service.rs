// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/vi/system_display_service.cpp/.h

use std::collections::BTreeMap;
use std::sync::Arc;

use common::math_util::Rectangle;

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::nvnflinger::ui::fence::Fence;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

use super::container::Container;
use super::vi_results;
use super::vi_types::*;

pub struct ISystemDisplayService {
    container: Arc<Container>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ISystemDisplayService {
    pub fn new(container: Arc<Container>) -> Self {
        Self {
            container,
            handlers: build_handler_map(&[
                (1200, None, "GetZOrderCountMin"),
                (1202, None, "GetZOrderCountMax"),
                (1203, None, "GetDisplayLogicalResolution"),
                (1204, None, "SetDisplayMagnification"),
                (2201, None, "SetLayerPosition"),
                (2203, None, "SetLayerSize"),
                (2204, Some(Self::get_layer_z), "GetLayerZ"),
                (2205, Some(Self::set_layer_z), "SetLayerZ"),
                (2207, Some(Self::set_layer_visibility), "SetLayerVisibility"),
                (2209, None, "SetLayerAlpha"),
                (2210, None, "SetLayerPositionAndSize"),
                (2312, None, "CreateStrayLayer"),
                (2400, None, "OpenIndirectLayer"),
                (2401, None, "CloseIndirectLayer"),
                (2402, None, "FlipIndirectLayer"),
                (3000, Some(Self::list_display_modes), "ListDisplayModes"),
                (3001, None, "ListDisplayRgbRanges"),
                (3002, None, "ListDisplayContentTypes"),
                (3200, Some(Self::get_display_mode), "GetDisplayMode"),
                (3201, None, "SetDisplayMode"),
                (3202, None, "GetDisplayUnderscan"),
                (3203, None, "SetDisplayUnderscan"),
                (3204, None, "GetDisplayContentType"),
                (3205, None, "SetDisplayContentType"),
                (3206, None, "GetDisplayRgbRange"),
                (3207, None, "SetDisplayRgbRange"),
                (3208, None, "GetDisplayCmuMode"),
                (3209, None, "SetDisplayCmuMode"),
                (3210, None, "GetDisplayContrastRatio"),
                (3211, None, "SetDisplayContrastRatio"),
                (3214, None, "GetDisplayGamma"),
                (3215, None, "SetDisplayGamma"),
                (3216, None, "GetDisplayCmuLuma"),
                (3217, None, "SetDisplayCmuLuma"),
                (3218, None, "SetDisplayCrcMode"),
                (6013, None, "GetLayerPresentationSubmissionTimestamps"),
                (
                    8225,
                    Some(Self::get_shared_buffer_memory_handle_id),
                    "GetSharedBufferMemoryHandleId",
                ),
                (8250, Some(Self::open_shared_layer), "OpenSharedLayer"),
                (8251, None, "CloseSharedLayer"),
                (8252, Some(Self::connect_shared_layer), "ConnectSharedLayer"),
                (8253, None, "DisconnectSharedLayer"),
                (
                    8254,
                    Some(Self::acquire_shared_frame_buffer),
                    "AcquireSharedFrameBuffer",
                ),
                (
                    8255,
                    Some(Self::present_shared_frame_buffer),
                    "PresentSharedFrameBuffer",
                ),
                (
                    8256,
                    Some(Self::get_shared_frame_buffer_acquirable_event),
                    "GetSharedFrameBufferAcquirableEvent",
                ),
                (8257, None, "FillSharedFrameBufferColor"),
                (
                    8258,
                    Some(Self::cancel_shared_frame_buffer),
                    "CancelSharedFrameBuffer",
                ),
                (9000, None, "GetDp2hdmiController"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    fn push_shared_buffer_memory_handle_id_response(
        ctx: &mut HLERequestContext,
        nvmap_handle: i32,
        size: u64,
    ) {
        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(nvmap_handle);
        rb.push_u32(0); // Align upstream's following Out<u64> to eight bytes.
        rb.push_u64(size);
    }

    fn push_acquire_shared_frame_buffer_response(
        ctx: &mut HLERequestContext,
        fence: &Fence,
        slots: [i32; 4],
        target_slot: i64,
    ) {
        let mut rb = ResponseBuilder::new(ctx, 18, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw(fence);
        for slot in slots {
            rb.push_i32(slot);
        }
        rb.push_u32(0); // Align upstream's following Out<s64> to eight bytes.
        rb.push_i64(target_slot);
    }

    fn parse_present_shared_frame_buffer_request(
        ctx: &HLERequestContext,
    ) -> (Fence, Rectangle<i32>, u32, i32, u64, i64) {
        let mut rp = RequestParser::new(ctx);
        let fence = rp.pop_raw();
        let crop_region = rp.pop_raw();
        let window_transform = rp.pop_u32();
        let swap_interval = rp.pop_i32();
        let _padding = rp.pop_u32();
        let layer_id = rp.pop_u64();
        let surface_id = rp.pop_i64();
        (
            fence,
            crop_region,
            window_transform,
            swap_interval,
            layer_id,
            surface_id,
        )
    }

    /// cmd 2205: SetLayerZ
    fn set_layer_z(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let layer_id = rp.pop_u64();
        let z_value = rp.pop_u64();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(
            match svc.container.set_layer_z_index(layer_id, z_value as i32) {
                Ok(()) => RESULT_SUCCESS,
                Err(error) => error,
            },
        );
    }

    /// cmd 2204: GetLayerZ
    fn get_layer_z(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let layer_id = rp.pop_u64();
        match svc.container.get_layer_z_index(layer_id) {
            Ok(z_index) => {
                let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u64(z_index as i64 as u64);
            }
            Err(error) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(error);
            }
        }
    }

    /// cmd 2207: SetLayerVisibility
    fn set_layer_visibility(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let visible = rp.pop_u32() != 0;
        let _padding = rp.pop_u32();
        let layer_id = rp.pop_u64();
        log::debug!(
            "ISystemDisplayService::SetLayerVisibility layer_id={}, visible={}",
            layer_id,
            visible
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(
            match svc.container.set_layer_visibility(layer_id, visible) {
                Ok(()) => RESULT_SUCCESS,
                Err(error) => error,
            },
        );
    }

    /// cmd 3000: ListDisplayModes
    fn list_display_modes(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let display_id = rp.pop_u64();
        log::warn!(
            "ISystemDisplayService::ListDisplayModes (STUBBED) display_id={}",
            display_id
        );

        let mode = DisplayMode {
            width: 1920,
            height: 1080,
            refresh_rate: 60.0,
            unknown: 0,
        };
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &mode as *const DisplayMode as *const u8,
                std::mem::size_of::<DisplayMode>(),
            )
        };
        ctx.write_buffer(bytes, 0);

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(1); // count
    }

    /// cmd 3200: GetDisplayMode
    fn get_display_mode(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let display_id = rp.pop_u64();
        log::warn!(
            "ISystemDisplayService::GetDisplayMode (STUBBED) display_id={}",
            display_id
        );

        let mode = if common::settings::is_docked_mode(&common::settings::values()) {
            DisplayMode {
                width: DisplayResolution::DockedWidth as u32,
                height: DisplayResolution::DockedHeight as u32,
                refresh_rate: 60.0,
                unknown: 0,
            }
        } else {
            DisplayMode {
                width: DisplayResolution::UndockedWidth as u32,
                height: DisplayResolution::UndockedHeight as u32,
                refresh_rate: 60.0,
                unknown: 0,
            }
        };

        Self::push_get_display_mode_response(ctx, &mode);
    }

    // Upstream Out<DisplayMode> is inline CMIF data, unlike ListDisplayModes'
    // OutArray. Serialize the four fields explicitly without a guest buffer.
    fn push_get_display_mode_response(ctx: &mut HLERequestContext, mode: &DisplayMode) {
        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(mode.width);
        rb.push_u32(mode.height);
        rb.push_f32(mode.refresh_rate);
        rb.push_u32(mode.unknown);
    }

    fn get_shared_buffer_memory_handle_id(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let buffer_id = rp.pop_u64();
        let aruid = rp.pop_u64();

        match svc
            .container
            .get_shared_buffer_manager()
            .get_shared_buffer_memory_handle_id(buffer_id, aruid)
        {
            Ok((size, nvmap_handle, pool_layout)) => {
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        &pool_layout as *const _ as *const u8,
                        std::mem::size_of_val(&pool_layout),
                    )
                };
                ctx.write_buffer(bytes, 0);
                Self::push_shared_buffer_memory_handle_id_response(ctx, nvmap_handle, size);
            }
            Err(err) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(err);
            }
        }
    }

    /// cmd 8250: OpenSharedLayer
    fn open_shared_layer(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let layer_id = rp.pop_u64();
        log::info!(
            "ISystemDisplayService::OpenSharedLayer (STUBBED) layer_id={}",
            layer_id
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// cmd 8252: ConnectSharedLayer
    fn connect_shared_layer(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let layer_id = rp.pop_u64();
        log::info!(
            "ISystemDisplayService::ConnectSharedLayer (STUBBED) layer_id={}",
            layer_id
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn acquire_shared_frame_buffer(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let layer_id = rp.pop_u64();

        match svc
            .container
            .get_shared_buffer_manager()
            .acquire_shared_frame_buffer(layer_id)
        {
            Ok((fence, slots, target_slot)) => {
                Self::push_acquire_shared_frame_buffer_response(ctx, &fence, slots, target_slot);
            }
            Err(err) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(err);
            }
        }
    }

    fn present_shared_frame_buffer(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let (fence, crop_region, window_transform, swap_interval, layer_id, surface_id) =
            Self::parse_present_shared_frame_buffer_request(ctx);

        let result = svc
            .container
            .get_shared_buffer_manager()
            .present_shared_frame_buffer(
                fence,
                crop_region,
                window_transform,
                swap_interval,
                layer_id,
                surface_id,
            );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result.err().unwrap_or(RESULT_SUCCESS));
    }

    fn get_shared_frame_buffer_acquirable_event(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let layer_id = rp.pop_u64();

        let event_result = svc
            .container
            .get_shared_buffer_manager()
            .get_shared_frame_buffer_acquirable_event(layer_id);

        let Ok(readable_event) = event_result else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(event_result.err().unwrap());
            return;
        };

        let Some(object_id) = ctx.register_readable_event_object(readable_event) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(vi_results::RESULT_OPERATION_FAILED);
            return;
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    fn cancel_shared_frame_buffer(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let layer_id = rp.pop_u64();
        let slot = rp.pop_i64();

        let result = svc
            .container
            .get_shared_buffer_manager()
            .cancel_shared_frame_buffer(layer_id, slot);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result.err().unwrap_or(RESULT_SUCCESS));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_mode_is_inline_cmif_data_without_an_output_buffer() {
        for (width, height) in [(1280, 720), (1920, 1080)] {
            let mut ctx = HLERequestContext::new();
            let mode = DisplayMode { width, height, refresh_rate: 60.0, unknown: 0 };
            ISystemDisplayService::push_get_display_mode_response(&mut ctx, &mode);
            let start = ctx.data_payload_offset as usize;
            assert_eq!(ctx.write_size - ctx.data_payload_offset, 6);
            assert_eq!(&ctx.cmd_buf[start..start + 6], &[0, 0, width, height, 60.0f32.to_bits(), 0]);
        }
    }

    #[test]
    fn shared_buffer_memory_handle_response_aligns_size_after_nvmap_handle() {
        let mut ctx = HLERequestContext::new();
        ISystemDisplayService::push_shared_buffer_memory_handle_id_response(
            &mut ctx,
            0x1234_5678,
            0x1122_3344_5566_7788,
        );

        let start = ctx.data_payload_offset as usize;
        assert_eq!(
            &ctx.cmd_buf[start..start + 6],
            &[0, 0, 0x1234_5678, 0, 0x5566_7788, 0x1122_3344]
        );
    }

    #[test]
    fn acquire_shared_frame_buffer_response_matches_upstream_cmif_layout() {
        let mut ctx = HLERequestContext::new();
        let fence = Fence::no_fence();
        ISystemDisplayService::push_acquire_shared_frame_buffer_response(
            &mut ctx,
            &fence,
            [0, 1, -1, -1],
            0x1122_3344_5566_7788,
        );

        let start = ctx.data_payload_offset as usize;
        assert_eq!(ctx.write_size - ctx.data_payload_offset, 18);
        assert_eq!(
            ctx.cmd_buf[start + 11..start + 15],
            [0, 1, u32::MAX, u32::MAX]
        );
        assert_eq!(ctx.cmd_buf[start + 15], 0);
        assert_eq!(ctx.cmd_buf[start + 16], 0x5566_7788);
        assert_eq!(ctx.cmd_buf[start + 17], 0x1122_3344);
    }

    #[test]
    fn present_shared_frame_buffer_request_aligns_64_bit_arguments() {
        let mut ctx = HLERequestContext::new();
        let start = ctx.data_payload_offset as usize + 2;
        ctx.cmd_buf[start + 15] = 0xDEAD_BEEF;
        ctx.cmd_buf[start + 16] = 0x5566_7788;
        ctx.cmd_buf[start + 17] = 0x1122_3344;
        ctx.cmd_buf[start + 18] = 0xDDEE_FF00;
        ctx.cmd_buf[start + 19] = 0x99AA_BBCC;

        let (_, _, _, _, layer_id, surface_id) =
            ISystemDisplayService::parse_present_shared_frame_buffer_request(&ctx);
        assert_eq!(layer_id, 0x1122_3344_5566_7788);
        assert_eq!(surface_id as u64, 0x99AA_BBCC_DDEE_FF00);
    }
}

impl SessionRequestHandler for ISystemDisplayService {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
    fn service_name(&self) -> &str {
        ServiceFramework::get_service_name(self)
    }
}

impl ServiceFramework for ISystemDisplayService {
    fn get_service_name(&self) -> &str {
        "vi::ISystemDisplayService"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

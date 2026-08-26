// SPDX-FileCopyrightText: Copyright 2024 ruzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/host1x/ffmpeg/ffmpeg.h` and `ffmpeg.cpp`.
//!
//! Wraps FFmpeg types (AVPacket, AVFrame, AVCodec, AVCodecContext) for video
//! decoding.

use std::collections::VecDeque;
use std::ffi::CStr;
use std::sync::Arc;

use crate::host1x::nvdec_common::VideoCodec;

const AV_NUM_DATA_POINTERS: usize = 8;

mod ffi {
    use libc::{c_char, c_int, c_uchar, c_void, uintptr_t};

    pub type RuzuFfmpegDecoder = c_void;
    pub type RuzuFfmpegHardwareContext = c_void;
    pub type AVFrame = c_void;

    extern "C" {
        pub fn ruzu_ffmpeg_decoder_create(
            codec: u64,
            prefer_mediacodec: c_int,
        ) -> *mut RuzuFfmpegDecoder;
        pub fn ruzu_ffmpeg_decoder_name(decoder: *const RuzuFfmpegDecoder) -> *const c_char;
        #[cfg(target_os = "android")]
        pub fn ruzu_ffmpeg_decoder_set_dimensions(
            decoder: *mut RuzuFfmpegDecoder,
            width: c_int,
            height: c_int,
        );
        pub fn ruzu_ffmpeg_decoder_open(
            decoder: *mut RuzuFfmpegDecoder,
            extradata: *const c_uchar,
            extradata_size: uintptr_t,
        ) -> c_int;
        pub fn ruzu_ffmpeg_decoder_destroy(decoder: *mut RuzuFfmpegDecoder);
        pub fn ruzu_ffmpeg_hardware_context_create() -> *mut RuzuFfmpegHardwareContext;
        pub fn ruzu_ffmpeg_hardware_context_destroy(hardware: *mut RuzuFfmpegHardwareContext);
        pub fn ruzu_ffmpeg_decoder_supports_decoding_on_device(
            codec: u64,
            device_type: c_int,
            out_pix_fmt: *mut c_int,
        ) -> c_int;
        pub fn ruzu_ffmpeg_supported_device_types(
            out: *mut c_int,
            out_capacity: uintptr_t,
        ) -> uintptr_t;
        pub fn ruzu_ffmpeg_preferred_device_types(
            out: *mut c_int,
            out_capacity: uintptr_t,
        ) -> uintptr_t;
        pub fn ruzu_ffmpeg_device_type_name(device_type: c_int) -> *const c_char;
        pub fn ruzu_ffmpeg_decoder_send_packet(
            decoder: *mut RuzuFfmpegDecoder,
            data: *const c_uchar,
            size: uintptr_t,
            pts: i64,
            dts: i64,
        ) -> c_int;
        pub fn ruzu_ffmpeg_decoder_receive_frame_with_hw_transfer(
            decoder: *mut RuzuFfmpegDecoder,
        ) -> *mut AVFrame;
        pub fn ruzu_ffmpeg_hardware_initialize_with_type(
            hardware: *mut RuzuFfmpegHardwareContext,
            device_type: c_int,
        ) -> c_int;
        pub fn ruzu_ffmpeg_hardware_last_error(hardware: *const RuzuFfmpegHardwareContext)
            -> c_int;
        pub fn ruzu_ffmpeg_hardware_vaapi_vendor_name(
            hardware: *const RuzuFfmpegHardwareContext,
            device_type: c_int,
        ) -> *const c_char;
        pub fn ruzu_ffmpeg_decoder_initialize_hardware(
            decoder: *mut RuzuFfmpegDecoder,
            hardware: *const RuzuFfmpegHardwareContext,
            pixel_format: c_int,
        ) -> c_int;
        pub fn ruzu_ffmpeg_decoder_last_error(decoder: *const RuzuFfmpegDecoder) -> c_int;
        pub fn ruzu_ffmpeg_error_is_eof_or_again(error: c_int) -> c_int;
        pub fn ruzu_ffmpeg_error_string(errnum: c_int, out: *mut c_char, out_size: uintptr_t);
        pub fn ruzu_ffmpeg_frame_create() -> *mut AVFrame;
        pub fn ruzu_ffmpeg_frame_destroy(frame: *mut AVFrame);
        pub fn ruzu_ffmpeg_frame_width(frame: *const AVFrame) -> c_int;
        pub fn ruzu_ffmpeg_frame_height(frame: *const AVFrame) -> c_int;
        pub fn ruzu_ffmpeg_frame_format(frame: *const AVFrame) -> c_int;
        pub fn ruzu_ffmpeg_frame_stride(frame: *const AVFrame, plane: c_int) -> c_int;
        pub fn ruzu_ffmpeg_frame_plane(frame: *const AVFrame, plane: c_int) -> *const c_uchar;
        pub fn ruzu_ffmpeg_frame_interlaced(frame: *const AVFrame) -> c_int;
        pub fn ruzu_ffmpeg_frame_is_hardware_decoded(frame: *const AVFrame) -> c_int;
    }
}

fn av_error(ret: i32) -> String {
    let mut buffer = [0 as libc::c_char; 128];
    unsafe {
        ffi::ruzu_ffmpeg_error_string(ret, buffer.as_mut_ptr(), buffer.len());
        CStr::from_ptr(buffer.as_ptr())
            .to_string_lossy()
            .into_owned()
    }
}

fn device_type_name(device_type: u32) -> String {
    unsafe {
        CStr::from_ptr(ffi::ruzu_ffmpeg_device_type_name(device_type as i32))
            .to_string_lossy()
            .into_owned()
    }
}

fn query_device_types(
    query: unsafe extern "C" fn(*mut libc::c_int, libc::uintptr_t) -> libc::uintptr_t,
) -> Vec<u32> {
    let count = unsafe { query(std::ptr::null_mut(), 0) };
    if count == 0 {
        return Vec::new();
    }

    let mut types = vec![0i32; count as usize];
    let written = unsafe { query(types.as_mut_ptr(), types.len()) };
    types.truncate(written.min(types.len()) as usize);
    types.into_iter().map(|value| value as u32).collect()
}

/// Wraps an AVPacket — a container for compressed bitstream data.
///
/// Port of `FFmpeg::Packet`.
pub struct Packet<'a> {
    data: &'a [u8],
    pts: i64,
    dts: i64,
}

impl<'a> Packet<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pts: i64::MIN,
            dts: i64::MIN,
        }
    }

    fn set_timestamps(&mut self, pts: i64, dts: i64) {
        self.pts = pts;
        self.dts = dts;
    }
}

/// Wraps an AVFrame — a container for decoded audio/video data.
///
/// Port of `FFmpeg::Frame`.
pub struct Frame {
    raw: *mut ffi::AVFrame,
}

impl Frame {
    pub fn new() -> Self {
        Self {
            raw: unsafe { ffi::ruzu_ffmpeg_frame_create() },
        }
    }

    fn from_raw(raw: *mut ffi::AVFrame) -> Self {
        Self { raw }
    }

    pub fn get_width(&self) -> i32 {
        unsafe { ffi::ruzu_ffmpeg_frame_width(self.raw.cast_const()) }
    }

    pub fn get_height(&self) -> i32 {
        unsafe { ffi::ruzu_ffmpeg_frame_height(self.raw.cast_const()) }
    }

    pub fn get_pixel_format(&self) -> i32 {
        unsafe { ffi::ruzu_ffmpeg_frame_format(self.raw.cast_const()) }
    }

    pub fn get_stride(&self, plane: usize) -> i32 {
        unsafe { ffi::ruzu_ffmpeg_frame_stride(self.raw.cast_const(), plane as i32) }
    }

    pub fn get_strides(&self) -> [i32; AV_NUM_DATA_POINTERS] {
        let mut strides = [0; AV_NUM_DATA_POINTERS];
        for (plane, stride) in strides.iter_mut().enumerate() {
            *stride = self.get_stride(plane);
        }
        strides
    }

    pub fn get_data(&self, plane: usize) -> *mut u8 {
        self.get_plane_ptr(plane).cast_mut()
    }

    pub fn get_plane(&self, plane: usize) -> *const u8 {
        self.get_plane_ptr(plane)
    }

    pub fn get_plane_ptr(&self, plane: usize) -> *const u8 {
        unsafe { ffi::ruzu_ffmpeg_frame_plane(self.raw.cast_const(), plane as i32) }
    }

    pub fn get_planes(&self) -> [*mut u8; AV_NUM_DATA_POINTERS] {
        let mut planes = [std::ptr::null_mut(); AV_NUM_DATA_POINTERS];
        for (plane, data) in planes.iter_mut().enumerate() {
            *data = self.get_data(plane);
        }
        planes
    }

    pub fn is_interlaced(&self) -> bool {
        unsafe { ffi::ruzu_ffmpeg_frame_interlaced(self.raw.cast_const()) != 0 }
    }

    pub fn is_hardware_decoded(&self) -> bool {
        unsafe { ffi::ruzu_ffmpeg_frame_is_hardware_decoded(self.raw.cast_const()) != 0 }
    }
}

unsafe impl Send for Frame {}
unsafe impl Sync for Frame {}

impl Drop for Frame {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { ffi::ruzu_ffmpeg_frame_destroy(self.raw) };
            self.raw = std::ptr::null_mut();
        }
    }
}

impl Default for Frame {
    fn default() -> Self {
        Self::new()
    }
}

/// Wraps an AVCodec — codec information.
///
/// Port of `FFmpeg::Decoder`.
pub struct Decoder {
    codec: VideoCodec,
    prefer_mediacodec: bool,
}

impl Decoder {
    pub fn new(codec: VideoCodec) -> Self {
        #[cfg(target_os = "android")]
        let prefer_mediacodec = *common::settings::values().nvdec_emulation.get_value()
            == common::settings_enums::NvdecEmulation::Gpu;
        #[cfg(not(target_os = "android"))]
        let prefer_mediacodec = false;
        Self {
            codec,
            prefer_mediacodec,
        }
    }

    pub fn supports_decoding_on_device(&self, device_type: u32) -> Option<i32> {
        let mut pix_fmt = -1;
        let supported = unsafe {
            ffi::ruzu_ffmpeg_decoder_supports_decoding_on_device(
                self.codec as u64,
                device_type as i32,
                &mut pix_fmt,
            )
        };
        (supported != 0).then_some(pix_fmt)
    }
}

/// Wraps AVBufferRef for hardware-accelerated decoding.
///
/// Port of `FFmpeg::HardwareContext`.
pub struct HardwareContext {
    raw: *mut ffi::RuzuFfmpegHardwareContext,
}

impl HardwareContext {
    pub fn new() -> Self {
        Self {
            raw: unsafe { ffi::ruzu_ffmpeg_hardware_context_create() },
        }
    }

    pub fn get_supported_device_types() -> Vec<u32> {
        query_device_types(ffi::ruzu_ffmpeg_supported_device_types)
    }

    fn get_preferred_device_types() -> Vec<u32> {
        query_device_types(ffi::ruzu_ffmpeg_preferred_device_types)
    }

    fn initialize_with_type(&mut self, device_type: u32) -> bool {
        if self.raw.is_null() {
            return false;
        }
        let name = device_type_name(device_type);
        let result =
            unsafe { ffi::ruzu_ffmpeg_hardware_initialize_with_type(self.raw, device_type as i32) };
        if result == 0 {
            let error = unsafe { ffi::ruzu_ffmpeg_hardware_last_error(self.raw.cast_const()) };
            log::debug!("av_hwdevice_ctx_create({name}) failed: {}", av_error(error));
            return false;
        }
        if result < 0 {
            log::debug!("Skipping VDPAU impersonated VAAPI driver");
            return false;
        }
        let vendor_name = unsafe {
            ffi::ruzu_ffmpeg_hardware_vaapi_vendor_name(self.raw.cast_const(), device_type as i32)
        };
        if !vendor_name.is_null() {
            let vendor_name = unsafe { CStr::from_ptr(vendor_name) }.to_string_lossy();
            log::debug!("Using VAAPI driver: {vendor_name}");
        }
        true
    }

    pub fn initialize_for_decoder(
        &mut self,
        decoder_context: &mut DecoderContext,
        decoder: &Decoder,
    ) -> bool {
        let supported_types = Self::get_supported_device_types();
        for device_type in Self::get_preferred_device_types() {
            let name = device_type_name(device_type);
            if !supported_types.contains(&device_type) {
                log::debug!("{name} explicitly unsupported");
                continue;
            }
            if !self.initialize_with_type(device_type) {
                continue;
            }
            if let Some(pixel_format) = decoder.supports_decoding_on_device(device_type) {
                log::info!("Using {name} GPU decoder");
                decoder_context.initialize_hardware_decoder(self, pixel_format);
                return true;
            }
            log::debug!(
                "{} decoder does not support device type {name}",
                decoder_context.decoder_name()
            );
        }
        false
    }
}

impl Default for HardwareContext {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for HardwareContext {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { ffi::ruzu_ffmpeg_hardware_context_destroy(self.raw) };
            self.raw = std::ptr::null_mut();
        }
    }
}

/// Wraps an AVCodecContext.
///
/// Port of `FFmpeg::DecoderContext`.
pub struct DecoderContext {
    raw: *mut ffi::RuzuFfmpegDecoder,
    decode_order: bool,
}

impl DecoderContext {
    pub fn new(decoder: &Decoder) -> Self {
        let raw = unsafe {
            ffi::ruzu_ffmpeg_decoder_create(
                decoder.codec as u64,
                i32::from(decoder.prefer_mediacodec),
            )
        };
        if raw.is_null() {
            log::error!(
                "FFmpeg::DecoderContext::new: failed to allocate codec {:?}",
                decoder.codec
            );
        }
        Self {
            raw,
            decode_order: false,
        }
    }

    pub fn initialize_hardware_decoder(&mut self, context: &HardwareContext, hw_pix_fmt: i32) {
        if self.raw.is_null() || context.raw.is_null() {
            return;
        }
        unsafe {
            ffi::ruzu_ffmpeg_decoder_initialize_hardware(self.raw, context.raw, hw_pix_fmt);
        }
    }

    pub fn open_context(&mut self, _decoder: &Decoder, extradata: &[u8]) -> bool {
        if self.raw.is_null() {
            return false;
        }
        let ret =
            unsafe { ffi::ruzu_ffmpeg_decoder_open(self.raw, extradata.as_ptr(), extradata.len()) };
        if ret < 0 {
            log::error!(
                "FFmpeg::DecoderContext::open_context: avcodec_open2 error: {}",
                av_error(ret)
            );
            return false;
        }
        log::info!("Using decoder {}", self.decoder_name());
        true
    }

    pub fn send_packet(&mut self, packet: &Packet<'_>) -> bool {
        if self.raw.is_null() {
            return false;
        }
        let ret = unsafe {
            ffi::ruzu_ffmpeg_decoder_send_packet(
                self.raw,
                packet.data.as_ptr(),
                packet.data.len(),
                packet.pts,
                packet.dts,
            )
        };
        if ret < 0 && unsafe { ffi::ruzu_ffmpeg_error_is_eof_or_again(ret) } == 0 {
            log::error!(
                "FFmpeg::DecoderContext::send_packet: avcodec_send_packet error: {}",
                av_error(ret)
            );
            return false;
        }
        true
    }

    pub fn receive_frame(&mut self) -> Option<Arc<Frame>> {
        if self.raw.is_null() {
            return None;
        }
        let frame = unsafe { ffi::ruzu_ffmpeg_decoder_receive_frame_with_hw_transfer(self.raw) };
        if frame.is_null() {
            let ret = unsafe { ffi::ruzu_ffmpeg_decoder_last_error(self.raw.cast_const()) };
            if ret < 0 && unsafe { ffi::ruzu_ffmpeg_error_is_eof_or_again(ret) } == 0 {
                log::error!(
                    "FFmpeg::DecoderContext::receive_frame: avcodec_receive_frame error: {}",
                    av_error(ret)
                );
            }
            return None;
        }
        Some(Arc::new(Frame::from_raw(frame)))
    }

    pub fn using_decode_order(&self) -> bool {
        self.decode_order
    }

    fn decoder_name(&self) -> String {
        if self.raw.is_null() {
            return String::new();
        }
        unsafe {
            CStr::from_ptr(ffi::ruzu_ffmpeg_decoder_name(self.raw.cast_const()))
                .to_string_lossy()
                .into_owned()
        }
    }

    #[cfg(target_os = "android")]
    fn set_dimensions(&mut self, dimensions: FrameDimensions) {
        if self.raw.is_null() {
            return;
        }
        unsafe {
            ffi::ruzu_ffmpeg_decoder_set_dimensions(self.raw, dimensions.width, dimensions.height);
        }
    }
}

unsafe impl Send for DecoderContext {}

impl Drop for DecoderContext {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { ffi::ruzu_ffmpeg_decoder_destroy(self.raw) };
            self.raw = std::ptr::null_mut();
        }
    }
}

/// Guest surface offsets associated with one submitted compressed frame.
///
/// Port of `FFmpeg::FrameOffsets`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameOffsets {
    pub interlaced: bool,
    pub hidden: bool,
    pub luma: u64,
    pub luma_bottom: u64,
}

/// Dimensions supplied by NVDEC for decoders that need them before opening.
///
/// Port of `FFmpeg::FrameDimensions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameDimensions {
    pub width: i32,
    pub height: i32,
}

#[cfg(target_os = "android")]
fn find_nal_start_code(data: &[u8], index: usize) -> usize {
    if index + 3 < data.len()
        && data[index] == 0
        && data[index + 1] == 0
        && data[index + 2] == 0
        && data[index + 3] == 1
    {
        return 4;
    }
    if index + 2 < data.len() && data[index] == 0 && data[index + 1] == 0 && data[index + 2] == 1 {
        return 3;
    }
    0
}

#[cfg(target_os = "android")]
fn extract_h264_parameter_set_extradata(packet: &[u8]) -> Vec<u8> {
    let mut extradata = Vec::new();
    let mut index = 0;
    while index < packet.len() {
        let start_code_size = find_nal_start_code(packet, index);
        if start_code_size == 0 {
            index += 1;
            continue;
        }
        let nal_start = index + start_code_size;
        if nal_start >= packet.len() {
            break;
        }
        let nal_type = packet[nal_start] & 0x1f;

        let mut next = nal_start + 1;
        while next < packet.len() && find_nal_start_code(packet, next) == 0 {
            next += 1;
        }

        if nal_type == 7 || nal_type == 8 {
            extradata.extend_from_slice(&[0, 0, 0, 1]);
            extradata.extend_from_slice(&packet[nal_start..next]);
        } else if nal_type == 1 || nal_type == 5 {
            break;
        }
        index = next;
    }
    extradata
}

/// A decoded frame paired with the guest offsets of the packet that produced it.
///
/// Port of `FFmpeg::DecodeApi::DecodedFrame`.
pub struct DecodedFrame {
    pub frame: Arc<Frame>,
    pub offsets: FrameOffsets,
}

/// High-level decode API that manages codec, context, and optional hardware
/// acceleration.
///
/// Port of `FFmpeg::DecodeApi`.
pub struct DecodeApi {
    decoder: Option<Decoder>,
    decoder_context: Option<DecoderContext>,
    hardware_context: Option<HardwareContext>,
    opened: bool,
    defer_android_mediacodec_open: bool,
    needs_h264_extradata: bool,
    next_pts: i64,
    pending_offsets: VecDeque<FrameOffsets>,
}

impl DecodeApi {
    pub fn new() -> Self {
        Self {
            decoder: None,
            decoder_context: None,
            hardware_context: None,
            opened: false,
            defer_android_mediacodec_open: false,
            needs_h264_extradata: false,
            next_pts: 0,
            pending_offsets: VecDeque::new(),
        }
    }

    pub fn initialize(&mut self, codec: VideoCodec) -> bool {
        self.reset();
        self.decoder = Some(Decoder::new(codec));
        self.decoder_context =
            Some(DecoderContext::new(self.decoder.as_ref().expect(
                "decoder was emplaced immediately before its context",
            )));
        if self
            .decoder_context
            .as_ref()
            .is_none_or(|context| context.raw.is_null())
        {
            self.reset();
            return false;
        }

        #[cfg(target_os = "android")]
        let decoder_name = self
            .decoder_context
            .as_ref()
            .expect("decoder context exists")
            .decoder_name();
        #[cfg(target_os = "android")]
        let is_mediacodec = matches!(
            decoder_name.as_str(),
            "h264_mediacodec" | "vp8_mediacodec" | "vp9_mediacodec"
        );
        #[cfg(not(target_os = "android"))]
        let is_mediacodec = false;

        if !is_mediacodec
            && *common::settings::values().nvdec_emulation.get_value()
                == common::settings_enums::NvdecEmulation::Gpu
        {
            let mut hardware_context = HardwareContext::new();
            hardware_context.initialize_for_decoder(
                self.decoder_context
                    .as_mut()
                    .expect("decoder context exists"),
                self.decoder.as_ref().expect("decoder exists"),
            );
            self.hardware_context = Some(hardware_context);
        }

        #[cfg(target_os = "android")]
        {
            self.defer_android_mediacodec_open = is_mediacodec;
            self.needs_h264_extradata = decoder_name == "h264_mediacodec";
            if self.defer_android_mediacodec_open {
                return true;
            }
        }

        let initialized = self
            .decoder_context
            .as_mut()
            .expect("decoder context exists")
            .open_context(self.decoder.as_ref().expect("decoder exists"), &[]);
        let _ = common::trace::emit(
            common::trace::cat::HOST1X_VIDEO,
            &[4, 1, codec as u64, initialized as u64, 0],
        );
        if !initialized {
            self.reset();
            return false;
        }
        self.opened = true;
        true
    }

    pub fn reset(&mut self) {
        self.hardware_context = None;
        self.decoder_context = None;
        self.decoder = None;
        self.opened = false;
        self.defer_android_mediacodec_open = false;
        self.needs_h264_extradata = false;
        self.next_pts = 0;
        self.pending_offsets.clear();
    }

    pub fn using_decode_order(&self) -> bool {
        self.decoder_context
            .as_ref()
            .expect("DecodeApi must be initialized before UsingDecodeOrder")
            .using_decode_order()
    }

    pub fn send_packet(
        &mut self,
        packet_data: &[u8],
        offsets: FrameOffsets,
        dimensions: Option<FrameDimensions>,
    ) -> bool {
        #[cfg(not(target_os = "android"))]
        let _ = dimensions;

        let _ = common::trace::emit(
            common::trace::cat::HOST1X_VIDEO,
            &[4, 2, 0, 0, packet_data.len() as u64],
        );
        if !self.opened {
            let extradata = Vec::new();

            #[cfg(target_os = "android")]
            let extradata = {
                if self.defer_android_mediacodec_open {
                    let Some(dimensions) = dimensions else {
                        return true;
                    };
                    self.decoder_context
                        .as_mut()
                        .expect("deferred decoder context must exist")
                        .set_dimensions(dimensions);
                }

                if self.needs_h264_extradata {
                    let extradata = extract_h264_parameter_set_extradata(packet_data);
                    if extradata.is_empty() {
                        return true;
                    }
                    extradata
                } else {
                    extradata
                }
            };

            let decoder = self
                .decoder
                .as_ref()
                .expect("DecodeApi must be initialized before SendPacket");
            let decoder_context = self
                .decoder_context
                .as_mut()
                .expect("DecodeApi must be initialized before SendPacket");
            if !decoder_context.open_context(decoder, &extradata) {
                self.reset();
                return false;
            }
            self.opened = true;
        }
        let decoder_context = self
            .decoder_context
            .as_mut()
            .expect("DecodeApi must be initialized before SendPacket");
        if !offsets.hidden {
            self.pending_offsets.push_back(offsets);
        }
        let mut packet = Packet::new(packet_data);
        packet.set_timestamps(self.next_pts, self.next_pts);
        self.next_pts += 1;
        decoder_context.send_packet(&packet)
    }

    pub fn receive_frame(&mut self) -> Option<DecodedFrame> {
        let _ = common::trace::emit(common::trace::cat::HOST1X_VIDEO, &[4, 3, 0, 0, 0]);
        let frame = self
            .decoder_context
            .as_mut()
            .expect("DecodeApi must be initialized before ReceiveFrame")
            .receive_frame()?;
        let offsets = self.pending_offsets.pop_front().unwrap_or_default();
        Some(DecodedFrame { frame, offsets })
    }
}

unsafe impl Send for DecodeApi {}

impl Default for DecodeApi {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_frame_exposes_empty_planes_and_strides() {
        let frame = Frame::new();
        assert!(!frame.raw.is_null());
        assert_eq!(frame.get_strides(), [0; AV_NUM_DATA_POINTERS]);
        assert!(frame.get_planes().iter().all(|ptr| ptr.is_null()));
        assert!(frame.get_plane(0).is_null());
        assert!(frame.get_data(0).is_null());
        assert!(!frame.is_hardware_decoded());
    }

    #[test]
    fn decode_api_initializes_h264_software_decoder_in_upstream_presentation_order() {
        let mut api = DecodeApi::new();
        assert!(api.initialize(VideoCodec::H264));
        assert!(!api.using_decode_order());
    }

    #[test]
    fn decoder_context_allocates_native_context_during_construction() {
        let decoder = Decoder::new(VideoCodec::H264);
        let context = DecoderContext::new(&decoder);

        assert!(!context.raw.is_null());
    }

    #[test]
    fn decode_api_reset_clears_packet_correlation_state() {
        let mut api = DecodeApi::new();
        api.next_pts = 17;
        api.pending_offsets.push_back(FrameOffsets {
            luma: 0x1234,
            ..FrameOffsets::default()
        });

        api.reset();

        assert_eq!(api.next_pts, 0);
        assert!(api.pending_offsets.is_empty());
        assert!(!api.opened);
    }

    #[test]
    fn frame_offsets_default_matches_upstream_zero_initialization() {
        assert_eq!(
            FrameOffsets::default(),
            FrameOffsets {
                interlaced: false,
                hidden: false,
                luma: 0,
                luma_bottom: 0,
            }
        );
    }

    #[test]
    fn ffmpeg_hardware_capability_queries_are_wired() {
        let decoder = Decoder::new(VideoCodec::H264);
        for device_type in HardwareContext::get_supported_device_types() {
            let _ = decoder.supports_decoding_on_device(device_type);
        }
    }

    #[test]
    fn preferred_hardware_decoder_order_matches_eden_for_the_target() {
        let names = HardwareContext::get_preferred_device_types()
            .into_iter()
            .map(device_type_name)
            .collect::<Vec<_>>();

        #[cfg(target_os = "windows")]
        let expected = ["cuda", "d3d11va", "dxva2", "d3d12va", "vulkan"];
        #[cfg(target_os = "freebsd")]
        let expected = ["vaapi", "vdpau", "drm", "vulkan"];
        #[cfg(target_vendor = "apple")]
        let expected = ["videotoolbox", "vulkan"];
        #[cfg(target_os = "android")]
        let expected = ["mediacodec", "vulkan"];
        #[cfg(all(
            unix,
            not(target_os = "freebsd"),
            not(target_vendor = "apple"),
            not(target_os = "android")
        ))]
        let expected = ["cuda", "vaapi", "vdpau", "vulkan"];
        #[cfg(not(any(target_os = "windows", unix)))]
        let expected = ["vulkan"];

        assert_eq!(names, expected);
    }

    #[cfg(target_os = "android")]
    #[test]
    fn h264_parameter_set_extradata_matches_eden_annex_b_scan() {
        let packet = [
            0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 3, 4, 0, 0, 1, 0x65, 5,
        ];
        assert_eq!(
            extract_h264_parameter_set_extradata(&packet),
            [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 3, 4]
        );
    }
}

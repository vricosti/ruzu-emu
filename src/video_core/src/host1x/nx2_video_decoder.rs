// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of eden-nx2/video_core/host1x/nx2_video_decoder.{h,cpp}.

use std::collections::BTreeMap;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::Arc;

use super::ffmpeg::ffmpeg::{Decoder, DecoderContext, Frame, HardwareContext, Packet};
use super::nvdec_common::VideoCodec;

// Native FFmpeg constants; the corresponding headers define these exact values.
const AV_NOPTS_VALUE: i64 = i64::MIN;
const AV_INPUT_BUFFER_PADDING_SIZE: usize = 64;
const SWS_BILINEAR: c_int = 2;
const SWS_CS_ITU601: c_int = 5;
const SWS_CS_ITU709: c_int = 1;
const SWS_CS_BT2020: c_int = 9;
const AVCOL_SPC_BT709: c_int = 1;
const AVCOL_SPC_UNSPECIFIED: c_int = 2;
const AVCOL_SPC_BT2020_NCL: c_int = 9;
const AVCOL_SPC_BT2020_CL: c_int = 10;
const AVCOL_RANGE_JPEG: c_int = 2;

extern "C" {
    fn avcodec_send_packet(context: *mut c_void, packet: *const c_void) -> c_int;
    fn avcodec_flush_buffers(context: *mut c_void);
    fn av_get_pix_fmt(name: *const c_char) -> c_int;
    fn av_get_pix_fmt_name(format: c_int) -> *const c_char;
    fn sws_freeContext(context: *mut c_void);
    fn sws_getCachedContext(
        context: *mut c_void,
        src_w: c_int,
        src_h: c_int,
        src_format: c_int,
        dst_w: c_int,
        dst_h: c_int,
        dst_format: c_int,
        flags: c_int,
        src_filter: *mut c_void,
        dst_filter: *mut c_void,
        param: *const f64,
    ) -> *mut c_void;
    fn sws_getCoefficients(colorspace: c_int) -> *const c_int;
    fn sws_setColorspaceDetails(
        context: *mut c_void,
        inv_table: *const c_int,
        src_range: c_int,
        table: *const c_int,
        dst_range: c_int,
        brightness: c_int,
        contrast: c_int,
        saturation: c_int,
    ) -> c_int;
    fn sws_scale(
        context: *mut c_void,
        src: *const *const u8,
        src_stride: *const c_int,
        src_y: c_int,
        src_h: c_int,
        dst: *const *mut u8,
        dst_stride: *const c_int,
    ) -> c_int;
}

pub struct Nx2Picture {
    pub width: u32,
    pub height: u32,
    pub pts: u64,
    pub frame: Arc<Frame>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Nx2PictureFormat {
    Nv12,
    Rgba8,
    Bgra8,
}

fn codec_from_mime(mime: &str) -> Option<VideoCodec> {
    if mime.contains("vp9") {
        Some(VideoCodec::VP9)
    } else if mime.contains("vp8") {
        Some(VideoCodec::VP8)
    } else if mime.contains("av01") || mime.contains("av1") {
        Some(VideoCodec::AV1)
    } else if mime.contains("avc") || mime.contains("264") {
        Some(VideoCodec::H264)
    } else {
        None
    }
}

struct Impl {
    name: String,
    decoder: Option<Decoder>,
    hardware: Option<HardwareContext>,
    context: Option<DecoderContext>,
    scaler: *mut c_void,
    open: bool,
    // Ordered multiset: counts retain duplicate timestamps; size remains O(1).
    pending_pts: BTreeMap<u64, usize>,
    pending_pts_size: usize,
    packet: Vec<u8>,
}

impl Drop for Impl {
    fn drop(&mut self) {
        unsafe { sws_freeContext(self.scaler) };
        self.context.take();
        self.hardware.take();
    }
}

impl Impl {
    // Mechanical equivalent of multiset.erase(iterator), never erase all duplicates.
    fn erase_pts(&mut self, pts: u64) {
        if let Some(count) = self.pending_pts.get_mut(&pts) {
            *count -= 1;
            self.pending_pts_size -= 1;
            if *count == 0 {
                self.pending_pts.remove(&pts);
            }
        }
    }

    fn receive(&mut self, pictures: &mut Vec<Nx2Picture>) {
        while let Some(frame) = self.context.as_mut().unwrap().receive_frame() {
            let best = frame.get_best_effort_timestamp();
            let stamp = if best != AV_NOPTS_VALUE {
                best
            } else {
                frame.get_pts()
            };
            let mut pts = 0;
            if stamp != AV_NOPTS_VALUE {
                pts = stamp as u64;
                self.erase_pts(pts);
            } else if let Some((&first, _)) = self.pending_pts.first_key_value() {
                pts = first;
                self.erase_pts(pts);
            }
            pictures.push(Nx2Picture {
                width: frame.get_width() as u32,
                height: frame.get_height() as u32,
                pts,
                frame,
            });
        }
    }
}

pub struct Nx2VideoDecoder {
    inner: Box<Impl>,
}

impl Nx2VideoDecoder {
    pub fn new(mime: &str) -> Self {
        let mut this = Self {
            inner: Box::new(Impl {
                name: mime.into(),
                decoder: None,
                hardware: None,
                context: None,
                scaler: std::ptr::null_mut(),
                open: false,
                pending_pts: BTreeMap::new(),
                pending_pts_size: 0,
                packet: Vec::new(),
            }),
        };
        let Some(codec) = codec_from_mime(mime) else {
            log::error!("nx2 video: no decoder for codec {:?}", mime);
            return this;
        };
        let state = &mut this.inner;
        state.decoder = Some(Decoder::new(codec));
        let decoder = state.decoder.as_ref().unwrap();
        if decoder.get_codec().is_null() {
            log::error!("nx2 video: FFmpeg has no decoder for {:?}", mime);
            return this;
        }
        state.context = Some(DecoderContext::new(decoder));
        let context = state.context.as_mut().unwrap();
        if *common::settings::values().nvdec_emulation.get_value()
            == common::settings_enums::NvdecEmulation::Gpu
        {
            state.hardware = Some(HardwareContext::new());
            state
                .hardware
                .as_mut()
                .unwrap()
                .initialize_for_decoder(context, decoder);
        }
        state.open = context.open_context(decoder, &[]);
        log::info!(
            "nx2 video: decoder for {:?} {}",
            mime,
            if state.open { "open" } else { "failed to open" }
        );
        this
    }

    pub fn is_open(&self) -> bool {
        self.inner.open
    }
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    pub fn decode(&mut self, data: &[u8], pts: u64, pictures: &mut Vec<Nx2Picture>) {
        let state = &mut self.inner;
        if !state.open || data.is_empty() {
            return;
        }
        state.packet.clear();
        state
            .packet
            .resize(data.len() + AV_INPUT_BUFFER_PADDING_SIZE, 0);
        state.packet[..data.len()].copy_from_slice(data);
        let mut packet = Packet::new(&state.packet[..data.len()]);
        packet.set_pts(pts as i64);
        if state.pending_pts_size >= 64 {
            let first = *state.pending_pts.first_key_value().unwrap().0;
            // Disjoint fields permit retaining packet's borrowed backing through submission.
            let count = state.pending_pts.get_mut(&first).unwrap();
            *count -= 1;
            if *count == 0 {
                state.pending_pts.remove(&first);
            }
            state.pending_pts_size -= 1;
        }
        *state.pending_pts.entry(pts).or_default() += 1;
        state.pending_pts_size += 1;
        state.context.as_mut().unwrap().send_packet(&packet);
        state.receive(pictures);
    }

    pub fn drain(&mut self, pictures: &mut Vec<Nx2Picture>) {
        if !self.inner.open {
            return;
        }
        unsafe {
            avcodec_send_packet(
                self.inner.context.as_mut().unwrap().get_codec_context(),
                std::ptr::null(),
            );
        }
        self.inner.receive(pictures);
        self.reset();
    }

    pub fn reset(&mut self) {
        if self.inner.open {
            unsafe {
                avcodec_flush_buffers(self.inner.context.as_mut().unwrap().get_codec_context());
            }
        }
        self.inner.pending_pts.clear();
        self.inner.pending_pts_size = 0;
    }

    pub fn render(
        &mut self,
        picture: &Nx2Picture,
        format: Nx2PictureFormat,
        width: u32,
        height: u32,
        pitch: u64,
        chroma_offset: u64,
        out: &mut [u8],
    ) {
        let frame = &picture.frame;
        let needed = if format == Nx2PictureFormat::Nv12 {
            chroma_offset.wrapping_add(pitch.wrapping_mul((u64::from(height) + 1) / 2))
        } else {
            pitch.wrapping_mul(u64::from(height))
        };
        if width == 0 || height == 0 || (out.len() as u64) < needed {
            return;
        }
        // Safe slice adaptation: upstream assumes valid strides/planes. Do not let
        // malformed guest geometry make swscale write beyond the Rust allocation.
        let row_bytes = if format == Nx2PictureFormat::Nv12 {
            (u64::from(width) + 1) & !1
        } else {
            u64::from(width) * 4
        };
        if width > i32::MAX as u32
            || height > i32::MAX as u32
            || pitch > i32::MAX as u64
            || pitch < row_bytes
            || pitch
                .checked_mul(u64::from(height))
                .is_none_or(|n| n > out.len() as u64)
            || (format == Nx2PictureFormat::Nv12
                && chroma_offset
                    .checked_add(pitch * ((u64::from(height) + 1) / 2))
                    .is_none_or(|n| n > out.len() as u64))
        {
            return;
        }
        let name: &[u8] = match format {
            Nx2PictureFormat::Nv12 => b"nv12\0",
            Nx2PictureFormat::Rgba8 => b"rgba\0",
            Nx2PictureFormat::Bgra8 => b"bgra\0",
        };
        unsafe {
            let target = av_get_pix_fmt(name.as_ptr().cast());
            // A hardware-decoded picture arrives as NV12 already. Measured with
            // libswscale 7.5 (FFmpeg 6.1): its same-format plane copy returns
            // the full height but leaves the interleaved chroma plane of the
            // destination untouched, so the guest sampled zero chroma and the
            // whole video was green (Mario Kart World course fly-over, VP9
            // 2560x1440). Copy the two planes directly in that case.
            if format == Nx2PictureFormat::Nv12
                && frame.get_pixel_format() == target
                && frame.get_width() == width as c_int
                && frame.get_height() == height as c_int
            {
                let source = frame.get_planes();
                let source_strides = frame.get_strides();
                if source[0].is_null()
                    || source[1].is_null()
                    || source_strides[0] < row_bytes as c_int
                    || source_strides[1] < row_bytes as c_int
                {
                    return;
                }
                copy_nv12_planes(
                    source[0],
                    source_strides[0] as usize,
                    source[1],
                    source_strides[1] as usize,
                    width,
                    height,
                    pitch,
                    chroma_offset,
                    out,
                );
                return;
            }
            if format == Nx2PictureFormat::Nv12
                && frame.get_pixel_format() == av_get_pix_fmt(c"yuv420p".as_ptr())
                && frame.get_color_range() != AVCOL_RANGE_JPEG
                && frame.get_width() == width as c_int
                && frame.get_height() == height as c_int
            {
                let source = frame.get_planes();
                let strides = frame.get_strides();
                let chroma_width = row_bytes as usize / 2;
                if source[..3].iter().any(|p| p.is_null())
                    || strides[0] < width as c_int
                    || strides[1] < chroma_width as c_int
                    || strides[2] < chroma_width as c_int
                {
                    return;
                }
                for y in 0..height as usize {
                    std::ptr::copy_nonoverlapping(
                        source[0].add(y * strides[0] as usize),
                        out.as_mut_ptr().add(y * pitch as usize),
                        width as usize,
                    );
                }
                for y in 0..(height as usize + 1) / 2 {
                    let u = source[1].add(y * strides[1] as usize);
                    let v = source[2].add(y * strides[2] as usize);
                    let dst = &mut out[chroma_offset as usize + y * pitch as usize..];
                    for x in 0..chroma_width {
                        dst[x * 2] = *u.add(x);
                        dst[x * 2 + 1] = *v.add(x);
                    }
                }
                return;
            }
            self.inner.scaler = sws_getCachedContext(
                self.inner.scaler,
                frame.get_width(),
                frame.get_height(),
                frame.get_pixel_format(),
                width as c_int,
                height as c_int,
                target,
                SWS_BILINEAR,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            if self.inner.scaler.is_null() {
                let source_name = av_get_pix_fmt_name(frame.get_pixel_format());
                log::error!(
                    "nx2 video: cannot convert {:?} to {:?}",
                    if source_name.is_null() {
                        None
                    } else {
                        Some(CStr::from_ptr(source_name))
                    },
                    CStr::from_ptr(name.as_ptr().cast())
                );
                return;
            }
            {
                let mut space = SWS_CS_ITU601;
                if frame.get_colorspace() == AVCOL_SPC_BT709
                    || (frame.get_colorspace() == AVCOL_SPC_UNSPECIFIED
                        && frame.get_height() >= 720)
                {
                    space = SWS_CS_ITU709;
                } else if matches!(
                    frame.get_colorspace(),
                    AVCOL_SPC_BT2020_NCL | AVCOL_SPC_BT2020_CL
                ) {
                    space = SWS_CS_BT2020;
                }
                let coefficients = sws_getCoefficients(space);
                let full_range_source = frame.get_color_range() == AVCOL_RANGE_JPEG
                    || [c"yuvj420p", c"yuvj422p", c"yuvj444p", c"yuvj440p", c"yuvj411p"]
                        .iter()
                        .any(|name| frame.get_pixel_format() == av_get_pix_fmt(name.as_ptr()));
                sws_setColorspaceDetails(
                    self.inner.scaler,
                    coefficients,
                    i32::from(full_range_source),
                    coefficients,
                    i32::from(format != Nx2PictureFormat::Nv12),
                    0,
                    1 << 16,
                    1 << 16,
                );
            }
            let mut planes = [
                out.as_mut_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ];
            let mut strides = [pitch as c_int, 0, 0, 0];
            if format == Nx2PictureFormat::Nv12 {
                planes[1] = out.as_mut_ptr().add(chroma_offset as usize);
                strides[1] = pitch as c_int;
            }
            let source = frame.get_planes().map(|p| p.cast_const());
            sws_scale(
                self.inner.scaler,
                source.as_ptr(),
                frame.get_strides().as_ptr(),
                0,
                frame.get_height(),
                planes.as_ptr(),
                strides.as_ptr(),
            );
        }
    }
}

/// Copies an NV12 picture into a destination that uses one pitch for both
/// planes and places the interleaved chroma plane at `chroma_offset`.
///
/// # Safety
/// `luma` must be readable for `height` rows of `luma_stride` bytes and
/// `chroma` for `(height + 1) / 2` rows of `chroma_stride` bytes, each row
/// holding at least `width` rounded up to even bytes. The caller has checked
/// that `out` holds both destination planes.
unsafe fn copy_nv12_planes(
    luma: *const u8,
    luma_stride: usize,
    chroma: *const u8,
    chroma_stride: usize,
    width: u32,
    height: u32,
    pitch: u64,
    chroma_offset: u64,
    out: &mut [u8],
) {
    let pitch = pitch as usize;
    let luma_bytes = width as usize;
    let chroma_bytes = (width as usize + 1) & !1;
    for row in 0..height as usize {
        let source = std::slice::from_raw_parts(luma.add(row * luma_stride), luma_bytes);
        out[row * pitch..][..luma_bytes].copy_from_slice(source);
    }
    let chroma_start = chroma_offset as usize;
    for row in 0..(height as usize + 1) / 2 {
        let source = std::slice::from_raw_parts(chroma.add(row * chroma_stride), chroma_bytes);
        out[chroma_start + row * pitch..][..chroma_bytes].copy_from_slice(source);
    }
}

// Mechanical crate-boundary adapters. Codec state and all operations stay here.
impl From<Nx2Picture> for ruzu_core::host1x_core::Nx2Picture {
    fn from(picture: Nx2Picture) -> Self {
        Self {
            width: picture.width,
            height: picture.height,
            pts: picture.pts,
            frame: picture.frame,
        }
    }
}
impl ruzu_core::host1x_core::Nx2VideoDecoderHandle for Nx2VideoDecoder {
    fn is_open(&self) -> bool {
        Nx2VideoDecoder::is_open(self)
    }
    fn name(&self) -> &str {
        Nx2VideoDecoder::name(self)
    }
    fn decode(
        &mut self,
        data: &[u8],
        pts: u64,
        pictures: &mut Vec<ruzu_core::host1x_core::Nx2Picture>,
    ) {
        let mut decoded = Vec::new();
        Nx2VideoDecoder::decode(self, data, pts, &mut decoded);
        pictures.extend(decoded.into_iter().map(Into::into));
    }
    fn drain(&mut self, pictures: &mut Vec<ruzu_core::host1x_core::Nx2Picture>) {
        let mut decoded = Vec::new();
        Nx2VideoDecoder::drain(self, &mut decoded);
        pictures.extend(decoded.into_iter().map(Into::into));
    }
    fn reset(&mut self) {
        Nx2VideoDecoder::reset(self);
    }
    fn render(
        &mut self,
        picture: &ruzu_core::host1x_core::Nx2Picture,
        format: ruzu_core::host1x_core::Nx2PictureFormat,
        width: u32,
        height: u32,
        pitch: u64,
        chroma_offset: u64,
        out: &mut [u8],
    ) {
        let picture = Nx2Picture {
            width: picture.width,
            height: picture.height,
            pts: picture.pts,
            frame: picture
                .frame
                .clone()
                .downcast::<Frame>()
                .expect("native NX2 frame owner"),
        };
        let format = match format {
            ruzu_core::host1x_core::Nx2PictureFormat::Nv12 => Nx2PictureFormat::Nv12,
            ruzu_core::host1x_core::Nx2PictureFormat::Rgba8 => Nx2PictureFormat::Rgba8,
            ruzu_core::host1x_core::Nx2PictureFormat::Bgra8 => Nx2PictureFormat::Bgra8,
        };
        Nx2VideoDecoder::render(
            self,
            &picture,
            format,
            width,
            height,
            pitch,
            chroma_offset,
            out,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Original generated fixture: solid red 16x16 RGB image encoded by Pillow
    // WebP quality=100, method=6; raw VP8 chunk extracted from RIFF (56 bytes).
    const RED_VP8: &[u8] = &[
        0x50, 0x02, 0x00, 0x9d, 0x01, 0x2a, 0x10, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x25, 0xa0,
        0x02, 0x74, 0xba, 0x01, 0xf8, 0x01, 0xfa, 0x01, 0xfc, 0x00, 0x07, 0x90, 0x00, 0xfe, 0xff,
        0x6f, 0x56, 0xff, 0xfc, 0xb0, 0xfc, 0x7f, 0x52, 0x79, 0xff, 0xcb, 0x10, 0xff, 0xff, 0x13,
        0xd3, 0x9b, 0xc4, 0x54, 0xf2, 0xbf, 0xfe, 0x26, 0x40, 0x00, 0x00,
    ];

    fn software_decoder() -> Nx2VideoDecoder {
        software_decoder_for(VideoCodec::VP8, "video/vp8")
    }

    fn software_decoder_for(codec: VideoCodec, mime: &str) -> Nx2VideoDecoder {
        let decoder = Decoder::new(codec);
        let mut context = DecoderContext::new(&decoder);
        assert!(context.open_context(&decoder, &[]));
        Nx2VideoDecoder {
            inner: Box::new(Impl {
                name: mime.into(),
                decoder: Some(decoder),
                context: Some(context),
                hardware: None,
                scaler: std::ptr::null_mut(),
                open: true,
                pending_pts: BTreeMap::new(),
                pending_pts_size: 0,
                packet: Vec::new(),
            }),
        }
    }

    #[test]
    fn av1_decoder_is_available_and_opens() {
        let decoder = Decoder::new(VideoCodec::AV1);
        assert!(!decoder.get_codec().is_null(), "FFmpeg must provide AV1 decoding");
        let mut context = DecoderContext::new(&decoder);
        assert!(context.open_context(&decoder, &[]));
    }

    #[test]
    fn av1_software_decodes_and_renders_a_frame() {
        // Original fixture: Pillow AVIF quality=100, speed=10, solid red 16x16;
        // elementary AV1 stream extracted from the generated mdat box.
        const RED_AV1: &[u8] = &[
            0x12, 0x00, 0x0a, 0x09, 0x18, 0x0c, 0xff, 0xd8, 0x20, 0x21, 0xa0, 0xd0,
            0x80, 0x32, 0x13, 0x10, 0x00, 0x00, 0x39, 0x50, 0xbb, 0x24, 0x85, 0x8b,
            0x56, 0xea, 0xd5, 0xa8, 0x8a, 0xed, 0x8c, 0x83, 0x54, 0x8e,
        ];
        let mut decoder = software_decoder_for(VideoCodec::AV1, "video/av1");
        let mut pictures = Vec::new();
        decoder.decode(RED_AV1, 123, &mut pictures);
        decoder.drain(&mut pictures);
        assert_eq!(pictures.len(), 1);
        assert_eq!((pictures[0].width, pictures[0].height, pictures[0].pts), (16, 16, 123));
        let mut pixels = [0; 16 * 16 * 4];
        decoder.render(&pictures[0], Nx2PictureFormat::Rgba8, 16, 16, 64, 0, &mut pixels);
        assert!(pixels.chunks_exact(4).all(|p| p[0] >= 245 && p[1] <= 10 && p[2] <= 10 && p[3] == 255));
        // Full-range AV1 must become limited-range NV12, including when scaled.
        for size in [16usize, 8] {
            let chroma = size * size;
            let mut nv12 = vec![0; chroma * 3 / 2];
            decoder.render(&pictures[0], Nx2PictureFormat::Nv12,
                size as u32, size as u32, size as u64, chroma as u64, &mut nv12);
            assert!(nv12[..chroma].iter().all(|&y| (75..=85).contains(&y)), "luma {:?}", &nv12[..size]);
            assert!(nv12[chroma..].chunks_exact(2).all(|uv|
                (85..=100).contains(&uv[0]) && (230..=245).contains(&uv[1])),
                "chroma {:?}", &nv12[chroma..chroma+size]);
        }
    }

    #[test]
    fn planar_yuv420_copy_preserves_samples_and_destination_padding() {
        let mut decoder = software_decoder();
        let mut pictures = Vec::new();
        decoder.decode(RED_VP8, 0, &mut pictures);
        let picture = &pictures[0];
        assert_eq!(picture.frame.get_pixel_format(), unsafe { av_get_pix_fmt(c"yuv420p".as_ptr()) });
        assert_ne!(picture.frame.get_color_range(), AVCOL_RANGE_JPEG);
        let (pitch, chroma_offset) = (32usize, 544usize);
        let mut out = vec![0x5a; chroma_offset + pitch * 8];
        decoder.render(picture, Nx2PictureFormat::Nv12, 16, 16,
            pitch as u64, chroma_offset as u64, &mut out);
        let planes = picture.frame.get_planes();
        let strides = picture.frame.get_strides();
        unsafe {
            for y in 0..16 {
                assert_eq!(&out[y*pitch..y*pitch+16],
                    std::slice::from_raw_parts(planes[0].add(y*strides[0] as usize),16));
                assert!(out[y*pitch+16..(y+1)*pitch].iter().all(|&v| v==0x5a));
            }
            for y in 0..8 {
                for x in 0..8 {
                    assert_eq!(out[chroma_offset+y*pitch+x*2], *planes[1].add(y*strides[1] as usize+x));
                    assert_eq!(out[chroma_offset+y*pitch+x*2+1], *planes[2].add(y*strides[2] as usize+x));
                }
                assert!(out[chroma_offset+y*pitch+16..chroma_offset+(y+1)*pitch].iter().all(|&v| v==0x5a));
            }
        }
        assert!(out[512..chroma_offset].iter().all(|&v| v==0x5a));
    }

    #[test]
    fn nv12_plane_copy_fills_luma_and_interleaved_chroma_at_the_destination_pitch() {
        // 5x3 picture: source strides wider than the row, odd width and height.
        let (width, height) = (5u32, 3u32);
        let (luma_stride, chroma_stride) = (8usize, 10usize);
        let luma: Vec<u8> = (0..luma_stride * 3).map(|i| 0x10 + i as u8).collect();
        let chroma: Vec<u8> = (0..chroma_stride * 2).map(|i| 0x80 + i as u8).collect();
        let (pitch, chroma_offset) = (16u64, 64u64);
        let mut out = vec![0u8; 64 + 32];
        unsafe {
            copy_nv12_planes(
                luma.as_ptr(),
                luma_stride,
                chroma.as_ptr(),
                chroma_stride,
                width,
                height,
                pitch,
                chroma_offset,
                &mut out,
            );
        }
        for row in 0..3 {
            assert_eq!(&out[row * 16..][..5], &luma[row * luma_stride..][..5]);
            assert_eq!(&out[row * 16 + 5..row * 16 + 16], &[0u8; 11]);
        }
        // (height + 1) / 2 chroma rows of width rounded up to even bytes.
        for row in 0..2 {
            assert_eq!(&out[64 + row * 16..][..6], &chroma[row * chroma_stride..][..6]);
            assert_eq!(&out[64 + row * 16 + 6..64 + row * 16 + 16], &[0u8; 10]);
        }
    }

    #[test]
    fn vp8_decodes_timestamps_converts_planes_and_restarts_after_drain() {
        let mut decoder = software_decoder();
        let mut pictures = Vec::new();
        decoder.decode(RED_VP8, u64::MAX, &mut pictures);
        assert_eq!(pictures.len(), 1);
        assert_eq!(
            (pictures[0].width, pictures[0].height, pictures[0].pts),
            (16, 16, u64::MAX)
        );
        assert_eq!(decoder.inner.pending_pts_size, 0);
        assert!(decoder.inner.packet[RED_VP8.len()..]
            .iter()
            .all(|&v| v == 0));
        let mut rgba = [0; 16 * 16 * 4];
        let mut bgra = [0; 16 * 16 * 4];
        decoder.render(
            &pictures[0],
            Nx2PictureFormat::Rgba8,
            16,
            16,
            64,
            0,
            &mut rgba,
        );
        decoder.render(
            &pictures[0],
            Nx2PictureFormat::Bgra8,
            16,
            16,
            64,
            0,
            &mut bgra,
        );
        for (r, b) in rgba.chunks_exact(4).zip(bgra.chunks_exact(4)) {
            assert!(r[0] >= 245 && r[1] <= 10 && r[2] <= 10, "{r:?}");
            assert_eq!(r[3], 255);
            assert_eq!(b, &[r[2], r[1], r[0], 255]);
        }
        let mut nv12 = [0; 384];
        decoder.render(
            &pictures[0],
            Nx2PictureFormat::Nv12,
            16,
            16,
            16,
            256,
            &mut nv12,
        );
        assert!(nv12[..256].iter().all(|&y| (75..=85).contains(&y)));
        for uv in nv12[256..].chunks_exact(2) {
            assert!((85..=95).contains(&uv[0]) && (235..=245).contains(&uv[1]));
        }
        let mut too_short = [0x5a; 12];
        decoder.render(
            &pictures[0],
            Nx2PictureFormat::Rgba8,
            16,
            16,
            64,
            0,
            &mut too_short,
        );
        assert_eq!(too_short, [0x5a; 12]);
        let mut invalid_stride = [0x5a; 1024];
        decoder.render(
            &pictures[0],
            Nx2PictureFormat::Rgba8,
            16,
            16,
            1,
            0,
            &mut invalid_stride,
        );
        assert!(invalid_stride.iter().all(|&v| v == 0x5a));
        let mut scaled = [0; 8 * 8 * 4];
        decoder.render(
            &pictures[0],
            Nx2PictureFormat::Rgba8,
            8,
            8,
            32,
            0,
            &mut scaled,
        );
        assert!(scaled
            .chunks_exact(4)
            .all(|p| p[0] >= 245 && p[1] <= 10 && p[2] <= 10 && p[3] == 255));
        decoder.drain(&mut pictures);
        decoder.decode(RED_VP8, 123, &mut pictures);
        assert_eq!(pictures.len(), 2);
        assert_eq!(pictures[1].pts, 123);
        decoder.reset();
        decoder.decode(RED_VP8, i64::MIN as u64, &mut pictures);
        // AV_NOPTS_VALUE follows the oldest submitted timestamp fallback.
        assert_eq!(pictures[2].pts, i64::MIN as u64);
    }

    #[test]
    fn opaque_decoder_bridge_keeps_frame_owner_pts_and_format() {
        use ruzu_core::host1x_core::{Nx2PictureFormat as Format, Nx2VideoDecoderHandle};
        let mut decoder: Box<dyn Nx2VideoDecoderHandle> = Box::new(software_decoder());
        assert!(decoder.is_open());
        assert_eq!(decoder.name(), "video/vp8");
        let mut pictures = Vec::new();
        decoder.decode(RED_VP8, u64::MAX, &mut pictures);
        assert_eq!(
            (pictures[0].width, pictures[0].height, pictures[0].pts),
            (16, 16, u64::MAX)
        );
        let owner = pictures[0].frame.clone();
        for format in [Format::Rgba8, Format::Bgra8] {
            let mut bytes = [0; 1024];
            decoder.render(&pictures[0], format, 16, 16, 64, 0, &mut bytes);
            let red = if format == Format::Rgba8 { 0 } else { 2 };
            assert!(bytes
                .chunks_exact(4)
                .all(|p| p[red] >= 245 && p[1] <= 10 && p[3] == 255));
            assert!(Arc::ptr_eq(&owner, &pictures[0].frame));
        }
        let mut nv12 = [0; 384];
        decoder.render(&pictures[0], Format::Nv12, 16, 16, 16, 256, &mut nv12);
        assert!(nv12[..256].iter().all(|v| (75..=85).contains(v)));
        decoder.drain(&mut pictures);
        decoder.reset();
        decoder.decode(RED_VP8, 42, &mut pictures);
        assert_eq!(pictures.len(), 2);
        assert_eq!(pictures[1].pts, 42);
    }

    #[test]
    fn mime_order_closed_decoder_and_multiset_duplicate_contracts() {
        assert_eq!(codec_from_mime("vp8-vp9-avc"), Some(VideoCodec::VP9));
        assert_eq!(codec_from_mime("video/vp8"), Some(VideoCodec::VP8));
        assert_eq!(codec_from_mime("video/av1"), Some(VideoCodec::AV1));
        assert_eq!(codec_from_mime("video/av01"), Some(VideoCodec::AV1));
        assert_eq!(codec_from_mime("vp8-av1-avc"), Some(VideoCodec::VP8));
        assert_eq!(codec_from_mime("av01-avc"), Some(VideoCodec::AV1));
        assert_eq!(codec_from_mime("video/avc"), Some(VideoCodec::H264));
        assert_eq!(codec_from_mime("video/h264"), Some(VideoCodec::H264));
        assert_eq!(codec_from_mime("video/VP9"), None);
        let mut decoder = Nx2VideoDecoder::new("unsupported");
        assert!(!decoder.is_open());
        assert_eq!(decoder.name(), "unsupported");
        let mut pictures = Vec::new();
        decoder.decode(RED_VP8, 1, &mut pictures);
        decoder.drain(&mut pictures);
        assert!(pictures.is_empty());
        decoder.inner.pending_pts.insert(7, 2);
        decoder.inner.pending_pts_size = 2;
        decoder.inner.erase_pts(7);
        assert_eq!(decoder.inner.pending_pts[&7], 1);
        decoder.reset();
        assert_eq!(decoder.inner.pending_pts_size, 0);
        assert!(decoder.inner.pending_pts.is_empty());
    }

    #[test]
    fn failed_packets_retain_only_the_last_64_ordered_pending_timestamps() {
        let mut decoder = software_decoder();
        let mut pictures = Vec::new();
        // Invalid VP8 is rejected by FFmpeg but upstream still records submission.
        for pts in 100..166 {
            decoder.decode(&[0], pts, &mut pictures);
        }
        assert!(pictures.is_empty());
        assert_eq!(decoder.inner.pending_pts_size, 64);
        assert_eq!(
            decoder.inner.pending_pts.first_key_value().map(|(&p, _)| p),
            Some(102)
        );
        decoder.reset();
        assert!(decoder.inner.pending_pts.is_empty());
    }
}

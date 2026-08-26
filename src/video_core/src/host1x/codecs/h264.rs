// SPDX-FileCopyrightText: Copyright 2024 ruzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/host1x/codecs/h264.h` and `h264.cpp`.
//!
//! H.264 decoder implementation including the H264BitWriter for composing
//! SPS/PPS headers, and the H264 decoder struct.

use crate::host1x::codecs::decoder::{DecoderImpl, DecoderState};
use crate::host1x::host1x::FrameQueue;
use crate::host1x::nvdec_common::{NvdecRegisters, VideoCodec};
use crate::memory_manager::MemoryManager;
use std::sync::Arc;

// --------------------------------------------------------------------------
// ZigZag LUTs from libavcodec (same as upstream).
// --------------------------------------------------------------------------

const ZIG_ZAG_DIRECT: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

const ZIG_ZAG_SCAN: [u8; 16] = [
    0 + 0 * 4,
    1 + 0 * 4,
    0 + 1 * 4,
    0 + 2 * 4,
    1 + 1 * 4,
    2 + 0 * 4,
    3 + 0 * 4,
    2 + 1 * 4,
    1 + 2 * 4,
    0 + 3 * 4,
    1 + 3 * 4,
    2 + 2 * 4,
    3 + 1 * 4,
    3 + 2 * 4,
    2 + 3 * 4,
    3 + 3 * 4,
];

// --------------------------------------------------------------------------
// H264 Offset (32-bit, shifted by 8)
// --------------------------------------------------------------------------

/// 32-bit offset that stores a shifted address, specific to H264 codec structures.
///
/// Port of `Tegra::Decoders::Offset` in `h264.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Offset {
    offset: u32,
}

impl Offset {
    pub fn address(&self) -> u32 {
        self.offset << 8
    }
}

const _: () = assert!(std::mem::size_of::<Offset>() == 0x4);

// --------------------------------------------------------------------------
// H264ParameterSet — 0x60 bytes
// --------------------------------------------------------------------------

/// Port of `Tegra::Decoders::H264ParameterSet`.
#[repr(C)]
#[derive(Clone, Default)]
pub struct H264ParameterSet {
    pub log2_max_pic_order_cnt_lsb_minus4: i32,      // 0x00
    pub delta_pic_order_always_zero_flag: i32,       // 0x04
    pub frame_mbs_only_flag: i32,                    // 0x08
    pub pic_width_in_mbs: u32,                       // 0x0C
    pub frame_height_in_mbs: u32,                    // 0x10
    pub surface_format: u32, // 0x14 (bitfield: tile_format, gob_height, reserved)
    pub entropy_coding_mode_flag: u32, // 0x18
    pub pic_order_present_flag: i32, // 0x1C
    pub num_refidx_l0_default_active: i32, // 0x20
    pub num_refidx_l1_default_active: i32, // 0x24
    pub deblocking_filter_control_present_flag: i32, // 0x28
    pub redundant_pic_cnt_present_flag: i32, // 0x2C
    pub transform_8x8_mode_flag: u32, // 0x30
    pub pitch_luma: u32,     // 0x34
    pub pitch_chroma: u32,   // 0x38
    pub luma_top_offset: Offset, // 0x3C
    pub luma_bot_offset: Offset, // 0x40
    pub luma_frame_offset: Offset, // 0x44
    pub chroma_top_offset: Offset, // 0x48
    pub chroma_bot_offset: Offset, // 0x4C
    pub chroma_frame_offset: Offset, // 0x50
    pub hist_buffer_size: u32, // 0x54
    /// Bitfield union storage — stored as [u32; 2] to avoid forcing 8-byte
    /// alignment on the containing struct (upstream C++ uses u64 BitField
    /// inside a union, but sizeof(H264DecoderContext) == 0x2FC requires
    /// 4-byte struct alignment).
    pub flags_raw: [u32; 2], // 0x58 (bitfield union, logically u64)
}

const _: () = assert!(std::mem::size_of::<H264ParameterSet>() == 0x60);

impl H264ParameterSet {
    /// Reconstruct the logical u64 from the [u32; 2] storage.
    #[inline]
    fn flags(&self) -> u64 {
        self.flags_raw[0] as u64 | ((self.flags_raw[1] as u64) << 32)
    }

    // Bitfield accessors for flags_raw (at offset 0x58).
    pub fn mbaff_frame(&self) -> u64 {
        self.flags() & 1
    }
    pub fn direct_8x8_inference(&self) -> u64 {
        (self.flags() >> 1) & 1
    }
    pub fn weighted_pred(&self) -> u64 {
        (self.flags() >> 2) & 1
    }
    pub fn constrained_intra_pred(&self) -> u64 {
        (self.flags() >> 3) & 1
    }
    pub fn log2_max_frame_num_minus4(&self) -> u64 {
        (self.flags() >> 8) & 0xF
    }
    pub fn chroma_format_idc(&self) -> u64 {
        (self.flags() >> 12) & 0x3
    }
    pub fn pic_order_cnt_type(&self) -> u64 {
        (self.flags() >> 14) & 0x3
    }
    pub fn pic_init_qp_minus26(&self) -> i64 {
        // 6-bit signed field at bit 16
        let raw = ((self.flags() >> 16) & 0x3F) as i64;
        if raw & 0x20 != 0 {
            raw | !0x3F
        } else {
            raw
        }
    }
    pub fn chroma_qp_index_offset(&self) -> i64 {
        // 5-bit signed field at bit 22
        let raw = ((self.flags() >> 22) & 0x1F) as i64;
        if raw & 0x10 != 0 {
            raw | !0x1F
        } else {
            raw
        }
    }
    pub fn second_chroma_qp_index_offset(&self) -> i64 {
        // 5-bit signed field at bit 27
        let raw = ((self.flags() >> 27) & 0x1F) as i64;
        if raw & 0x10 != 0 {
            raw | !0x1F
        } else {
            raw
        }
    }
    pub fn weighted_bipred_idc(&self) -> u64 {
        (self.flags() >> 32) & 0x3
    }
    pub fn curr_pic_idx(&self) -> u64 {
        (self.flags() >> 34) & 0x7F
    }
    pub fn frame_number(&self) -> u64 {
        (self.flags() >> 46) & 0xFFFF
    }
}

// --------------------------------------------------------------------------
// DpbEntry — 0x10 bytes
// --------------------------------------------------------------------------

/// Port of `Tegra::Decoders::DpbEntry`.
#[repr(C)]
#[derive(Clone, Default)]
pub struct DpbEntry {
    pub flags: u32,
    pub field_order_cnt: [u32; 2],
    pub frame_idx: u32,
}

const _: () = assert!(std::mem::size_of::<DpbEntry>() == 0x10);

// --------------------------------------------------------------------------
// DisplayParam — 0x1C bytes
// --------------------------------------------------------------------------

/// Port of `Tegra::Decoders::DisplayParam`.
#[repr(C)]
#[derive(Clone, Default)]
pub struct DisplayParam {
    pub flags0: u32,
    pub output_top: [i32; 2],
    pub output_bottom: [i32; 2],
    pub histogram_flags1: u32,
    pub histogram_flags2: u32,
}

const _: () = assert!(std::mem::size_of::<DisplayParam>() == 0x1C);

// --------------------------------------------------------------------------
// H264DecoderContext — 0x2FC bytes
// --------------------------------------------------------------------------

/// Port of `Tegra::Decoders::H264DecoderContext`.
#[repr(C)]
#[derive(Clone)]
pub struct H264DecoderContext {
    pub reserved0: [u32; 13],                 // 0x0000
    pub eos: [u8; 16],                        // 0x0034
    pub explicit_eos_present_flag: u8,        // 0x0044
    pub hint_dump_en: u8,                     // 0x0045
    pub _pad0: [u8; 2],                       // 0x0046
    pub stream_len: u32,                      // 0x0048
    pub slice_count: u32,                     // 0x004C
    pub mbhist_buffer_size: u32,              // 0x0050
    pub gptimer_timeout_value: u32,           // 0x0054
    pub h264_parameter_set: H264ParameterSet, // 0x0058
    pub curr_field_order_cnt: [i32; 2],       // 0x00B8
    pub dpb: [DpbEntry; 16],                  // 0x00C0
    pub weight_scale_4x4: [u8; 0x60],         // 0x01C0
    pub weight_scale_8x8: [u8; 0x80],         // 0x0220
    pub num_inter_view_refs_lx: [u8; 2],      // 0x02A0
    pub reserved2: [u8; 14],                  // 0x02A2
    pub inter_view_refidx_lx: [[i8; 16]; 2],  // 0x02B0
    pub lossless_flags: u32,                  // 0x02D0 (bitfield)
    pub display_param: DisplayParam,          // 0x02D4
    pub reserved4: [u32; 3],                  // 0x02F0
}

const _: () = assert!(std::mem::size_of::<H264DecoderContext>() == 0x2FC);

impl Default for H264DecoderContext {
    fn default() -> Self {
        // Safety: zeroed representation is valid for this C-layout struct.
        unsafe { std::mem::zeroed() }
    }
}

impl H264DecoderContext {
    pub fn qpprime_y_zero_transform_bypass_flag(&self) -> u32 {
        (self.lossless_flags >> 1) & 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h264_guest_context_layout_matches_upstream() {
        assert_eq!(std::mem::size_of::<Offset>(), 0x4);
        assert_eq!(std::mem::size_of::<H264ParameterSet>(), 0x60);
        assert_eq!(std::mem::size_of::<DpbEntry>(), 0x10);
        assert_eq!(std::mem::size_of::<DisplayParam>(), 0x1C);
        assert_eq!(std::mem::size_of::<H264DecoderContext>(), 0x2FC);

        assert_eq!(
            std::mem::offset_of!(H264ParameterSet, log2_max_pic_order_cnt_lsb_minus4),
            0x00
        );
        assert_eq!(std::mem::offset_of!(H264ParameterSet, surface_format), 0x14);
        assert_eq!(
            std::mem::offset_of!(H264ParameterSet, luma_top_offset),
            0x3C
        );
        assert_eq!(
            std::mem::offset_of!(H264ParameterSet, chroma_frame_offset),
            0x50
        );
        assert_eq!(std::mem::offset_of!(H264ParameterSet, flags_raw), 0x58);

        assert_eq!(std::mem::offset_of!(H264DecoderContext, stream_len), 0x48);
        assert_eq!(
            std::mem::offset_of!(H264DecoderContext, h264_parameter_set),
            0x58
        );
        assert_eq!(std::mem::offset_of!(H264DecoderContext, dpb), 0xC0);
        assert_eq!(
            std::mem::offset_of!(H264DecoderContext, weight_scale_4x4),
            0x1C0
        );
        assert_eq!(
            std::mem::offset_of!(H264DecoderContext, weight_scale_8x8),
            0x220
        );
        assert_eq!(
            std::mem::offset_of!(H264DecoderContext, display_param),
            0x2D4
        );
    }

    #[test]
    fn h264_parameter_bitfields_match_upstream_positions_and_sign_extension() {
        let mut params = H264ParameterSet::default();
        let flags = 1u64
            | (1 << 1)
            | (1 << 2)
            | (1 << 3)
            | (9 << 8)
            | (3 << 12)
            | (2 << 14)
            | (0b10_0001 << 16)
            | (0b1_0001 << 22)
            | (0b0_1111 << 27)
            | (2 << 32)
            | (0x55 << 34)
            | (0xABCD << 46);
        params.flags_raw = [flags as u32, (flags >> 32) as u32];

        assert_eq!(params.mbaff_frame(), 1);
        assert_eq!(params.direct_8x8_inference(), 1);
        assert_eq!(params.weighted_pred(), 1);
        assert_eq!(params.constrained_intra_pred(), 1);
        assert_eq!(params.log2_max_frame_num_minus4(), 9);
        assert_eq!(params.chroma_format_idc(), 3);
        assert_eq!(params.pic_order_cnt_type(), 2);
        assert_eq!(params.pic_init_qp_minus26(), -31);
        assert_eq!(params.chroma_qp_index_offset(), -15);
        assert_eq!(params.second_chroma_qp_index_offset(), 15);
        assert_eq!(params.weighted_bipred_idc(), 2);
        assert_eq!(params.curr_pic_idx(), 0x55);
        assert_eq!(params.frame_number(), 0xABCD);
    }

    #[test]
    fn h264_exp_golomb_output_matches_upstream_writer() {
        let mut writer = H264BitWriter::new();
        for value in 0..=3 {
            writer.write_ue(value);
        }
        writer.end();

        assert_eq!(writer.get_byte_array(), &[0xA6, 0x48]);
    }

    #[test]
    fn h264_scaling_list_and_rbsp_stop_bit_match_upstream_writer() {
        let mut writer = H264BitWriter::new();
        writer.write_scaling_list(&[8; 16], 0, 16);
        writer.end();

        assert_eq!(writer.get_byte_array(), &[0xFF, 0xFF, 0x80]);
    }
}

// --------------------------------------------------------------------------
// H264BitWriter
// --------------------------------------------------------------------------

/// Bitstream writer for composing H.264 NAL units.
///
/// Port of `Tegra::Decoders::H264BitWriter`.
pub struct H264BitWriter {
    buffer_size: i32,
    buffer: i32,
    buffer_pos: i32,
    byte_array: Vec<u8>,
}

impl H264BitWriter {
    pub fn new() -> Self {
        Self {
            buffer_size: 8,
            buffer: 0,
            buffer_pos: 0,
            byte_array: Vec::new(),
        }
    }

    /// Writes value_sz bits from value into the stream.
    pub fn write_u(&mut self, value: i32, value_sz: i32) {
        self.write_bits(value, value_sz);
    }

    /// Writes a signed Exp-Golomb coded integer.
    pub fn write_se(&mut self, value: i32) {
        self.write_exp_golomb_coded_int(value);
    }

    /// Writes an unsigned Exp-Golomb coded integer.
    pub fn write_ue(&mut self, value: u32) {
        self.write_exp_golomb_coded_uint(value);
    }

    /// Finalize the bitstream.
    pub fn end(&mut self) {
        self.write_bit(true);
        self.flush();
    }

    /// Append a single bit to the stream.
    pub fn write_bit(&mut self, state: bool) {
        self.write_bits(if state { 1 } else { 0 }, 1);
    }

    /// Write scaling list per H.264 spec section 7.3.2.1.1.1.
    pub fn write_scaling_list(&mut self, list: &[u8], start: usize, count: usize) {
        let scan: &[u8] = if count == 16 {
            &ZIG_ZAG_SCAN
        } else {
            &ZIG_ZAG_DIRECT[..count]
        };

        let mut last_scale: u8 = 8;
        for index in 0..count {
            let value = list[start + scan[index] as usize];
            let delta_scale = value as i32 - last_scale as i32;
            self.write_se(delta_scale);
            last_scale = value;
        }
    }

    /// Return the composed byte array.
    pub fn get_byte_array(&self) -> &Vec<u8> {
        &self.byte_array
    }

    // --- Private helpers ---

    fn write_bits(&mut self, value: i32, bit_count: i32) {
        let mut value_pos = 0i32;
        let mut remaining = bit_count;

        while remaining > 0 {
            let free_bits = self.get_free_buffer_bits();
            let copy_size = remaining.min(free_bits);

            let mask = (1 << copy_size) - 1;
            let src_shift = (bit_count - value_pos) - copy_size;
            let dst_shift = (self.buffer_size - self.buffer_pos) - copy_size;

            self.buffer |= ((value >> src_shift) & mask) << dst_shift;

            value_pos += copy_size;
            self.buffer_pos += copy_size;
            remaining -= copy_size;
        }
    }

    fn write_exp_golomb_coded_int(&mut self, mut value: i32) {
        let sign = if value <= 0 { 0 } else { 1 };
        if value < 0 {
            value = -value;
        }
        value = (value << 1) - sign;
        self.write_exp_golomb_coded_uint(value as u32);
    }

    fn write_exp_golomb_coded_uint(&mut self, value: u32) {
        let size = 32 - (value + 1).leading_zeros() as i32;
        self.write_bits(1, size);
        let adjusted = value - ((1u32 << (size - 1)) - 1);
        self.write_bits(adjusted as i32, size - 1);
    }

    fn get_free_buffer_bits(&mut self) -> i32 {
        if self.buffer_pos == self.buffer_size {
            self.flush();
        }
        self.buffer_size - self.buffer_pos
    }

    fn flush(&mut self) {
        if self.buffer_pos == 0 {
            return;
        }
        self.byte_array.push(self.buffer as u8);
        self.buffer = 0;
        self.buffer_pos = 0;
    }
}

impl Default for H264BitWriter {
    fn default() -> Self {
        Self::new()
    }
}

// --------------------------------------------------------------------------
// H264 Decoder
// --------------------------------------------------------------------------

/// H.264 video decoder.
///
/// Port of `Tegra::Decoders::H264`.
pub struct H264 {
    pub state: DecoderState,
    is_first_frame: bool,
    frame_scratch: Vec<u8>,
    current_context: H264DecoderContext,
}

impl H264 {
    pub fn new(
        id: i32,
        memory_manager: Arc<parking_lot::Mutex<MemoryManager>>,
        frame_queue: Arc<FrameQueue>,
    ) -> Self {
        let mut state = DecoderState::new(id, memory_manager, frame_queue);
        state.codec = VideoCodec::H264;
        state.initialized = state.decode_api.initialize(VideoCodec::H264);
        Self {
            state,
            is_first_frame: true,
            frame_scratch: Vec::new(),
            current_context: H264DecoderContext::default(),
        }
    }
}

impl DecoderImpl for H264 {
    fn compose_frame(&mut self, regs: &NvdecRegisters) -> Vec<u8> {
        let memory_manager = Arc::clone(&self.state.memory_manager);
        let mut context_bytes = vec![0u8; std::mem::size_of::<H264DecoderContext>()];
        if !memory_manager
            .lock()
            .read_block(regs.picture_info_offset().address(), &mut context_bytes)
        {
            log::error!(
                "H264::compose_frame: failed to read picture info at 0x{:X}",
                regs.picture_info_offset().address()
            );
            return Vec::new();
        }
        self.current_context = unsafe {
            let mut context = H264DecoderContext::default();
            std::ptr::copy_nonoverlapping(
                context_bytes.as_ptr(),
                (&mut context as *mut H264DecoderContext).cast::<u8>(),
                context_bytes.len(),
            );
            context
        };
        let params = &self.current_context.h264_parameter_set;
        self.state.set_frame_dimensions(
            (params.pic_width_in_mbs as i32) * 16,
            (params.frame_height_in_mbs as i32) * 16,
        );

        let frame_number = self.current_context.h264_parameter_set.frame_number() as i64;
        if !self.is_first_frame && frame_number != 0 {
            self.frame_scratch
                .resize(self.current_context.stream_len as usize, 0);
            if !memory_manager.lock().read_block(
                regs.frame_bitstream_offset().address(),
                &mut self.frame_scratch,
            ) {
                log::error!(
                    "H264::compose_frame: failed to read frame bitstream at 0x{:X}",
                    regs.frame_bitstream_offset().address()
                );
                self.frame_scratch.clear();
            }
            return self.frame_scratch.clone();
        }

        self.is_first_frame = false;

        let params = &self.current_context.h264_parameter_set;
        let mut writer = H264BitWriter::new();

        writer.write_u(1, 24);
        writer.write_u(0, 1);
        writer.write_u(3, 2);
        writer.write_u(7, 5);
        writer.write_u(100, 8);
        writer.write_u(0, 8);
        writer.write_u(31, 8);
        writer.write_ue(0);
        let chroma_format_idc = params.chroma_format_idc() as u32;
        writer.write_ue(chroma_format_idc);
        if chroma_format_idc == 3 {
            writer.write_bit(false);
        }

        writer.write_ue(0);
        writer.write_ue(0);
        writer.write_bit(self.current_context.qpprime_y_zero_transform_bypass_flag() != 0);
        writer.write_bit(false);
        writer.write_ue(params.log2_max_frame_num_minus4() as u32);

        let order_cnt_type = params.pic_order_cnt_type() as u32;
        writer.write_ue(order_cnt_type);
        if order_cnt_type == 0 {
            writer.write_ue(params.log2_max_pic_order_cnt_lsb_minus4 as u32);
        } else if order_cnt_type == 1 {
            writer.write_bit(params.delta_pic_order_always_zero_flag != 0);
            writer.write_se(0);
            writer.write_se(0);
            writer.write_ue(0);
        }

        let pic_height = params.frame_height_in_mbs
            / if params.frame_mbs_only_flag != 0 {
                1
            } else {
                2
            };
        let max_num_ref_frames = (params
            .num_refidx_l0_default_active
            .max(params.num_refidx_l1_default_active)
            + 1)
        .max(4);
        writer.write_ue(max_num_ref_frames as u32);
        writer.write_bit(false);
        writer.write_ue(params.pic_width_in_mbs - 1);
        writer.write_ue(pic_height - 1);
        writer.write_bit(params.frame_mbs_only_flag != 0);
        if params.frame_mbs_only_flag == 0 {
            writer.write_bit(params.mbaff_frame() != 0);
        }
        writer.write_bit(params.direct_8x8_inference() != 0);
        writer.write_bit(false);
        writer.write_bit(false);
        writer.end();

        writer.write_u(1, 24);
        writer.write_u(0, 1);
        writer.write_u(3, 2);
        writer.write_u(8, 5);
        writer.write_ue(0);
        writer.write_ue(0);
        writer.write_bit(params.entropy_coding_mode_flag != 0);
        writer.write_bit(params.pic_order_present_flag != 0);
        writer.write_ue(0);
        writer.write_ue(params.num_refidx_l0_default_active as u32);
        writer.write_ue(params.num_refidx_l1_default_active as u32);
        writer.write_bit(params.weighted_pred() != 0);
        writer.write_u(params.weighted_bipred_idc() as i32, 2);
        writer.write_se(params.pic_init_qp_minus26() as i32);
        writer.write_se(0);
        writer.write_se(params.chroma_qp_index_offset() as i32);
        writer.write_bit(params.deblocking_filter_control_present_flag != 0);
        writer.write_bit(params.constrained_intra_pred() != 0);
        writer.write_bit(params.redundant_pic_cnt_present_flag != 0);
        writer.write_bit(params.transform_8x8_mode_flag != 0);
        writer.write_bit(true);

        for index in 0..6 {
            writer.write_bit(true);
            writer.write_scaling_list(&self.current_context.weight_scale_4x4, index * 16, 16);
        }

        if params.transform_8x8_mode_flag != 0 {
            for index in 0..2 {
                writer.write_bit(true);
                writer.write_scaling_list(&self.current_context.weight_scale_8x8, index * 64, 64);
            }
        }

        writer.write_se(params.second_chroma_qp_index_offset() as i32);
        writer.end();

        let encoded_header = writer.get_byte_array();
        self.frame_scratch.resize(
            encoded_header.len() + self.current_context.stream_len as usize,
            0,
        );
        self.frame_scratch[..encoded_header.len()].copy_from_slice(encoded_header);
        if !memory_manager.lock().read_block(
            regs.frame_bitstream_offset().address(),
            &mut self.frame_scratch[encoded_header.len()..],
        ) {
            log::error!(
                "H264::compose_frame: failed to read frame bitstream at 0x{:X}",
                regs.frame_bitstream_offset().address()
            );
            self.frame_scratch.clear();
        }
        self.frame_scratch.clone()
    }

    fn get_progressive_offsets(&self, regs: &NvdecRegisters) -> (u64, u64) {
        let pic_idx = self.current_context.h264_parameter_set.curr_pic_idx() as usize;
        let luma = regs.surface_luma_offset(pic_idx).address()
            + self
                .current_context
                .h264_parameter_set
                .luma_frame_offset
                .address() as u64;
        let chroma = regs.surface_chroma_offset(pic_idx).address()
            + self
                .current_context
                .h264_parameter_set
                .chroma_frame_offset
                .address() as u64;
        (luma, chroma)
    }

    fn get_interlaced_offsets(&self, regs: &NvdecRegisters) -> (u64, u64, u64, u64) {
        let pic_idx = self.current_context.h264_parameter_set.curr_pic_idx() as usize;
        let luma_base = regs.surface_luma_offset(pic_idx).address();
        let chroma_base = regs.surface_chroma_offset(pic_idx).address();
        let params = &self.current_context.h264_parameter_set;
        (
            luma_base + params.luma_top_offset.address() as u64,
            luma_base + params.luma_bot_offset.address() as u64,
            chroma_base + params.chroma_top_offset.address() as u64,
            chroma_base + params.chroma_bot_offset.address() as u64,
        )
    }

    fn is_interlaced(&self) -> bool {
        self.current_context
            .h264_parameter_set
            .luma_top_offset
            .address()
            != 0
            || self
                .current_context
                .h264_parameter_set
                .luma_bot_offset
                .address()
                != 0
    }

    fn get_current_codec_name(&self) -> &str {
        "H264"
    }

    fn get_current_codec(&self) -> VideoCodec {
        self.state.codec
    }

    fn state(&self) -> &DecoderState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut DecoderState {
        &mut self.state
    }
}

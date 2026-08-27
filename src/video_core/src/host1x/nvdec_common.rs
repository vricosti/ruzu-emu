// SPDX-FileCopyrightText: Copyright 2024 ruzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/host1x/nvdec_common.h`.
//!
//! Common NVDEC types: video codec enum, register offset structures, and the
//! NvdecRegisters union.

/// Video codec identifiers used by the NVDEC hardware.
///
/// Port of `Tegra::Host1x::NvdecCommon::VideoCodec`.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VideoCodec(u64);

#[allow(non_upper_case_globals)]
impl VideoCodec {
    pub const None: Self = Self(0x0);
    pub const H264: Self = Self(0x3);
    pub const VP8: Self = Self(0x5);
    pub const H265: Self = Self(0x7);
    pub const VP9: Self = Self(0x9);

    pub const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl From<u64> for VideoCodec {
    fn from(value: u64) -> Self {
        Self::from_raw(value)
    }
}

impl From<VideoCodec> for u64 {
    fn from(value: VideoCodec) -> Self {
        value.raw()
    }
}

impl std::fmt::Debug for VideoCodec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::None => formatter.write_str("None"),
            Self::H264 => formatter.write_str("H264"),
            Self::VP8 => formatter.write_str("VP8"),
            Self::H265 => formatter.write_str("H265"),
            Self::VP9 => formatter.write_str("VP9"),
            _ => write!(formatter, "Unknown({:#x})", self.raw()),
        }
    }
}

/// 64-bit offset that stores a shifted address.
///
/// Port of `Tegra::Host1x::NvdecCommon::Offset`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Offset {
    offset: u64,
}

impl Offset {
    pub fn address(&self) -> u64 {
        self.offset << 8
    }
}

const _: () = assert!(std::mem::size_of::<Offset>() == 0x8);

#[repr(transparent)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ControlParams(u64);

impl ControlParams {
    pub const fn codec(self) -> VideoCodec {
        VideoCodec::from_raw(self.0 & 0x7)
    }

    pub const fn gp_timer_on(self) -> u64 {
        (self.0 >> 4) & 1
    }

    pub const fn mb_timer_on(self) -> u64 {
        (self.0 >> 13) & 1
    }

    pub const fn intra_frame_pslc(self) -> u64 {
        (self.0 >> 14) & 1
    }

    pub const fn all_intra_frame(self) -> u64 {
        (self.0 >> 17) & 1
    }
}

const _: () = assert!(std::mem::size_of::<ControlParams>() == 0x8);

/// NVDEC register file.
///
/// Port of `Tegra::Host1x::NvdecCommon::NvdecRegisters`.
///
/// NVDEC uses a 32-bit address space mapped to 64-bit, so all register slots
/// are 64-bit wide. The struct is 0xBC0 bytes (NUM_REGS * 8).
///
/// We represent it as a raw array and provide accessor methods matching the
/// upstream named fields at their byte offsets.
pub const NUM_REGS: usize = 0x178;

#[repr(C)]
#[derive(Clone)]
pub struct NvdecRegisters {
    pub reg_array: [u64; NUM_REGS],
}

impl Default for NvdecRegisters {
    fn default() -> Self {
        Self {
            reg_array: [0u64; NUM_REGS],
        }
    }
}

const _: () = assert!(std::mem::size_of::<NvdecRegisters>() == 0xBC0);

/// Macro to compute the register index from a byte offset, matching upstream
/// `NVDEC_REG_INDEX(field_name) = offsetof(...) / sizeof(u64)`.
macro_rules! reg_offset {
    ($byte_offset:expr) => {
        $byte_offset / 8
    };
}

// Register indices matching upstream field offsets.
// set_codec_id is at byte 0x400 => reg index 0x80
// execute is at byte 0x600 => reg index 0xC0
// control_params is at byte 0x800 => reg index 0x100
// picture_info_offset is at byte 0x808 => reg index 0x101
// frame_bitstream_offset is at byte 0x810 => reg index 0x102
// surface_luma_offsets[0] is at byte 0x860 => reg index 0x10C (17 elements)
// surface_chroma_offsets[0] is at byte 0x8E8 => reg index 0x11D (17 elements)
// vp9_prob_tab_buffer_offset is at byte 0xB80 => reg index 0x170

pub const REG_SET_CODEC_ID: usize = reg_offset!(0x400);
pub const REG_EXECUTE: usize = reg_offset!(0x600);
pub const REG_CONTROL_PARAMS: usize = reg_offset!(0x800);
pub const REG_PICTURE_INFO_OFFSET: usize = reg_offset!(0x808);
pub const REG_FRAME_BITSTREAM_OFFSET: usize = reg_offset!(0x810);
pub const REG_FRAME_NUMBER: usize = reg_offset!(0x818);
pub const REG_H264_SLICE_DATA_OFFSETS: usize = reg_offset!(0x820);
pub const REG_H264_MV_DUMP_OFFSET: usize = reg_offset!(0x828);
pub const REG_FRAME_STATS_OFFSET: usize = reg_offset!(0x848);
pub const REG_H264_LAST_SURFACE_LUMA_OFFSET: usize = reg_offset!(0x850);
pub const REG_H264_LAST_SURFACE_CHROMA_OFFSET: usize = reg_offset!(0x858);
pub const REG_SURFACE_LUMA_OFFSETS: usize = reg_offset!(0x860);
pub const REG_SURFACE_CHROMA_OFFSETS: usize = reg_offset!(0x8E8);
pub const REG_PIC_SCRATCH_BUF_OFFSET: usize = reg_offset!(0x970);
pub const REG_EXTERNAL_MVBUFFER_OFFSET: usize = reg_offset!(0x978);
pub const REG_H264_MBHIST_BUFFER_OFFSET: usize = reg_offset!(0xA00);
pub const REG_VP8_PROB_DATA_OFFSET: usize = reg_offset!(0xA80);
pub const REG_VP8_HEADER_PARTITION_BUF_OFFSET: usize = reg_offset!(0xA88);
pub const REG_HVEC_SCALIST_LIST_OFFSET: usize = reg_offset!(0xB00);
pub const REG_HVEC_TILE_SIZES_OFFSET: usize = reg_offset!(0xB08);
pub const REG_HVEC_FILTER_BUFFER_OFFSET: usize = reg_offset!(0xB10);
pub const REG_HVEC_SAO_BUFFER_OFFSET: usize = reg_offset!(0xB18);
pub const REG_HVEC_SLICE_INFO_BUFFER_OFFSET: usize = reg_offset!(0xB20);
pub const REG_HVEC_SLICE_GROUP_INDEX_OFFSET: usize = reg_offset!(0xB28);
pub const REG_VP9_PROB_TAB_BUFFER_OFFSET: usize = reg_offset!(0xB80);
pub const REG_VP9_CTX_COUNTER_BUFFER_OFFSET: usize = reg_offset!(0xB88);
pub const REG_VP9_SEGMENT_READ_BUFFER_OFFSET: usize = reg_offset!(0xB90);
pub const REG_VP9_SEGMENT_WRITE_BUFFER_OFFSET: usize = reg_offset!(0xB98);
pub const REG_VP9_TILE_SIZE_BUFFER_OFFSET: usize = reg_offset!(0xBA0);
pub const REG_VP9_COL_MVWRITE_BUFFER_OFFSET: usize = reg_offset!(0xBA8);
pub const REG_VP9_COL_MVREAD_BUFFER_OFFSET: usize = reg_offset!(0xBB0);
pub const REG_VP9_FILTER_BUFFER_OFFSET: usize = reg_offset!(0xBB8);

impl NvdecRegisters {
    /// Get the set_codec_id register value.
    pub fn set_codec_id(&self) -> VideoCodec {
        VideoCodec::from(self.reg_array[REG_SET_CODEC_ID])
    }

    /// Get the execute register value.
    pub fn execute(&self) -> u64 {
        self.reg_array[REG_EXECUTE]
    }

    pub fn control_params(&self) -> ControlParams {
        ControlParams(self.reg_array[REG_CONTROL_PARAMS])
    }

    fn offset_at(&self, index: usize) -> Offset {
        Offset {
            offset: self.reg_array[index],
        }
    }

    /// Get picture_info_offset as an address.
    pub fn picture_info_offset(&self) -> Offset {
        self.offset_at(REG_PICTURE_INFO_OFFSET)
    }

    /// Get frame_bitstream_offset as an address.
    pub fn frame_bitstream_offset(&self) -> Offset {
        self.offset_at(REG_FRAME_BITSTREAM_OFFSET)
    }

    pub fn frame_number(&self) -> u64 {
        self.reg_array[REG_FRAME_NUMBER]
    }

    pub fn h264_slice_data_offsets(&self) -> Offset {
        self.offset_at(REG_H264_SLICE_DATA_OFFSETS)
    }

    pub fn h264_mv_dump_offset(&self) -> Offset {
        self.offset_at(REG_H264_MV_DUMP_OFFSET)
    }

    pub fn frame_stats_offset(&self) -> Offset {
        self.offset_at(REG_FRAME_STATS_OFFSET)
    }

    pub fn h264_last_surface_luma_offset(&self) -> Offset {
        self.offset_at(REG_H264_LAST_SURFACE_LUMA_OFFSET)
    }

    pub fn h264_last_surface_chroma_offset(&self) -> Offset {
        self.offset_at(REG_H264_LAST_SURFACE_CHROMA_OFFSET)
    }

    /// Access surface_luma_offsets array (17 elements starting at REG_SURFACE_LUMA_OFFSETS).
    pub fn surface_luma_offset(&self, index: usize) -> Offset {
        assert!(index < 17);
        self.offset_at(REG_SURFACE_LUMA_OFFSETS + index)
    }

    /// Access surface_chroma_offsets array (17 elements starting at REG_SURFACE_CHROMA_OFFSETS).
    pub fn surface_chroma_offset(&self, index: usize) -> Offset {
        assert!(index < 17);
        self.offset_at(REG_SURFACE_CHROMA_OFFSETS + index)
    }

    pub fn pic_scratch_buf_offset(&self) -> Offset {
        self.offset_at(REG_PIC_SCRATCH_BUF_OFFSET)
    }

    pub fn external_mvbuffer_offset(&self) -> Offset {
        self.offset_at(REG_EXTERNAL_MVBUFFER_OFFSET)
    }

    pub fn h264_mbhist_buffer_offset(&self) -> Offset {
        self.offset_at(REG_H264_MBHIST_BUFFER_OFFSET)
    }

    pub fn vp8_prob_data_offset(&self) -> Offset {
        self.offset_at(REG_VP8_PROB_DATA_OFFSET)
    }

    pub fn vp8_header_partition_buf_offset(&self) -> Offset {
        self.offset_at(REG_VP8_HEADER_PARTITION_BUF_OFFSET)
    }

    pub fn hvec_scalist_list_offset(&self) -> Offset {
        self.offset_at(REG_HVEC_SCALIST_LIST_OFFSET)
    }

    pub fn hvec_tile_sizes_offset(&self) -> Offset {
        self.offset_at(REG_HVEC_TILE_SIZES_OFFSET)
    }

    pub fn hvec_filter_buffer_offset(&self) -> Offset {
        self.offset_at(REG_HVEC_FILTER_BUFFER_OFFSET)
    }

    pub fn hvec_sao_buffer_offset(&self) -> Offset {
        self.offset_at(REG_HVEC_SAO_BUFFER_OFFSET)
    }

    pub fn hvec_slice_info_buffer_offset(&self) -> Offset {
        self.offset_at(REG_HVEC_SLICE_INFO_BUFFER_OFFSET)
    }

    pub fn hvec_slice_group_index_offset(&self) -> Offset {
        self.offset_at(REG_HVEC_SLICE_GROUP_INDEX_OFFSET)
    }

    /// Get vp9_prob_tab_buffer_offset.
    pub fn vp9_prob_tab_buffer_offset(&self) -> Offset {
        self.offset_at(REG_VP9_PROB_TAB_BUFFER_OFFSET)
    }

    /// Get vp9_ctx_counter_buffer_offset.
    pub fn vp9_ctx_counter_buffer_offset(&self) -> Offset {
        self.offset_at(REG_VP9_CTX_COUNTER_BUFFER_OFFSET)
    }

    pub fn vp9_segment_read_buffer_offset(&self) -> Offset {
        self.offset_at(REG_VP9_SEGMENT_READ_BUFFER_OFFSET)
    }

    pub fn vp9_segment_write_buffer_offset(&self) -> Offset {
        self.offset_at(REG_VP9_SEGMENT_WRITE_BUFFER_OFFSET)
    }

    pub fn vp9_tile_size_buffer_offset(&self) -> Offset {
        self.offset_at(REG_VP9_TILE_SIZE_BUFFER_OFFSET)
    }

    pub fn vp9_col_mvwrite_buffer_offset(&self) -> Offset {
        self.offset_at(REG_VP9_COL_MVWRITE_BUFFER_OFFSET)
    }

    pub fn vp9_col_mvread_buffer_offset(&self) -> Offset {
        self.offset_at(REG_VP9_COL_MVREAD_BUFFER_OFFSET)
    }

    pub fn vp9_filter_buffer_offset(&self) -> Offset {
        self.offset_at(REG_VP9_FILTER_BUFFER_OFFSET)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_preserves_unknown_register_bit_patterns() {
        let unknown = VideoCodec::from(0xfeed_face_cafe_beef);

        assert_eq!(unknown.raw(), 0xfeed_face_cafe_beef);
        assert_ne!(unknown, VideoCodec::None);
        assert_eq!(format!("{unknown:?}"), "Unknown(0xfeedfacecafebeef)");
    }

    #[test]
    fn control_params_extracts_the_upstream_bitfields() {
        let params =
            ControlParams(VideoCodec::VP8.raw() | (1 << 4) | (1 << 13) | (1 << 14) | (1 << 17));

        assert_eq!(params.codec(), VideoCodec::VP8);
        assert_eq!(params.gp_timer_on(), 1);
        assert_eq!(params.mb_timer_on(), 1);
        assert_eq!(params.intra_frame_pslc(), 1);
        assert_eq!(params.all_intra_frame(), 1);
    }

    #[test]
    fn register_indices_match_every_named_upstream_field() {
        let indices = [
            (REG_SET_CODEC_ID, 0x80),
            (REG_EXECUTE, 0xc0),
            (REG_CONTROL_PARAMS, 0x100),
            (REG_PICTURE_INFO_OFFSET, 0x101),
            (REG_FRAME_BITSTREAM_OFFSET, 0x102),
            (REG_FRAME_NUMBER, 0x103),
            (REG_H264_SLICE_DATA_OFFSETS, 0x104),
            (REG_H264_MV_DUMP_OFFSET, 0x105),
            (REG_FRAME_STATS_OFFSET, 0x109),
            (REG_H264_LAST_SURFACE_LUMA_OFFSET, 0x10a),
            (REG_H264_LAST_SURFACE_CHROMA_OFFSET, 0x10b),
            (REG_SURFACE_LUMA_OFFSETS, 0x10c),
            (REG_SURFACE_CHROMA_OFFSETS, 0x11d),
            (REG_PIC_SCRATCH_BUF_OFFSET, 0x12e),
            (REG_EXTERNAL_MVBUFFER_OFFSET, 0x12f),
            (REG_H264_MBHIST_BUFFER_OFFSET, 0x140),
            (REG_VP8_PROB_DATA_OFFSET, 0x150),
            (REG_VP8_HEADER_PARTITION_BUF_OFFSET, 0x151),
            (REG_HVEC_SCALIST_LIST_OFFSET, 0x160),
            (REG_HVEC_TILE_SIZES_OFFSET, 0x161),
            (REG_HVEC_FILTER_BUFFER_OFFSET, 0x162),
            (REG_HVEC_SAO_BUFFER_OFFSET, 0x163),
            (REG_HVEC_SLICE_INFO_BUFFER_OFFSET, 0x164),
            (REG_HVEC_SLICE_GROUP_INDEX_OFFSET, 0x165),
            (REG_VP9_PROB_TAB_BUFFER_OFFSET, 0x170),
            (REG_VP9_CTX_COUNTER_BUFFER_OFFSET, 0x171),
            (REG_VP9_SEGMENT_READ_BUFFER_OFFSET, 0x172),
            (REG_VP9_SEGMENT_WRITE_BUFFER_OFFSET, 0x173),
            (REG_VP9_TILE_SIZE_BUFFER_OFFSET, 0x174),
            (REG_VP9_COL_MVWRITE_BUFFER_OFFSET, 0x175),
            (REG_VP9_COL_MVREAD_BUFFER_OFFSET, 0x176),
            (REG_VP9_FILTER_BUFFER_OFFSET, 0x177),
        ];

        for (actual, expected) in indices {
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn register_file_layout_and_offset_access_match_upstream() {
        assert_eq!(std::mem::size_of::<VideoCodec>(), 8);
        assert_eq!(std::mem::align_of::<VideoCodec>(), 8);
        assert_eq!(std::mem::size_of::<ControlParams>(), 8);
        assert_eq!(std::mem::size_of::<Offset>(), 8);
        assert_eq!(std::mem::size_of::<NvdecRegisters>(), 0xbc0);
        assert_eq!(std::mem::align_of::<NvdecRegisters>(), 8);

        let mut regs = NvdecRegisters::default();
        regs.reg_array[REG_PICTURE_INFO_OFFSET] = 0x1234;
        regs.reg_array[REG_VP9_FILTER_BUFFER_OFFSET] = 0x5678;
        assert_eq!(regs.picture_info_offset().address(), 0x123400);
        assert_eq!(regs.vp9_filter_buffer_offset().address(), 0x567800);
    }
}

// SPDX-FileCopyrightText: Copyright 2024 ruzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of Eden's `video_core/host1x/codec_types.h`.
//!
//! Guest codec payload layouts and the small conversion helpers owned by the
//! upstream header live together here so their binary layout remains directly
//! auditable against the source of truth.

use bitflags::bitflags;

/// 32-bit offset storing an address shifted by eight bits.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Offset {
    offset: u32,
}

impl Offset {
    pub const fn address(&self) -> u32 {
        self.offset << 8
    }
}

const _: () = assert!(std::mem::size_of::<Offset>() == 0x4);

#[repr(C)]
#[derive(Clone, Default)]
pub struct H264ParameterSet {
    pub log2_max_pic_order_cnt_lsb_minus4: i32,
    pub delta_pic_order_always_zero_flag: i32,
    pub frame_mbs_only_flag: i32,
    pub pic_width_in_mbs: u32,
    pub frame_height_in_mbs: u32,
    pub surface_format: u32,
    pub entropy_coding_mode_flag: u32,
    pub pic_order_present_flag: i32,
    pub num_refidx_l0_default_active: i32,
    pub num_refidx_l1_default_active: i32,
    pub deblocking_filter_control_present_flag: i32,
    pub redundant_pic_cnt_present_flag: i32,
    pub transform_8x8_mode_flag: u32,
    pub pitch_luma: u32,
    pub pitch_chroma: u32,
    pub luma_top_offset: Offset,
    pub luma_bot_offset: Offset,
    pub luma_frame_offset: Offset,
    pub chroma_top_offset: Offset,
    pub chroma_bot_offset: Offset,
    pub chroma_frame_offset: Offset,
    pub hist_buffer_size: u32,
    /// The C++ union is logically a `u64`, but the guest ABI gives the
    /// containing structure four-byte alignment. Two words preserve both.
    pub flags_raw: [u32; 2],
}

const _: () = assert!(std::mem::size_of::<H264ParameterSet>() == 0x60);

impl H264ParameterSet {
    #[inline]
    fn flags(&self) -> u64 {
        self.flags_raw[0] as u64 | ((self.flags_raw[1] as u64) << 32)
    }

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
        (self.flags() >> 8) & 0xf
    }

    pub fn chroma_format_idc(&self) -> u64 {
        (self.flags() >> 12) & 0x3
    }

    pub fn pic_order_cnt_type(&self) -> u64 {
        (self.flags() >> 14) & 0x3
    }

    pub fn pic_init_qp_minus26(&self) -> i64 {
        let raw = ((self.flags() >> 16) & 0x3f) as i64;
        if raw & 0x20 != 0 {
            raw | !0x3f
        } else {
            raw
        }
    }

    pub fn chroma_qp_index_offset(&self) -> i64 {
        let raw = ((self.flags() >> 22) & 0x1f) as i64;
        if raw & 0x10 != 0 {
            raw | !0x1f
        } else {
            raw
        }
    }

    pub fn second_chroma_qp_index_offset(&self) -> i64 {
        let raw = ((self.flags() >> 27) & 0x1f) as i64;
        if raw & 0x10 != 0 {
            raw | !0x1f
        } else {
            raw
        }
    }

    pub fn weighted_bipred_idc(&self) -> u64 {
        (self.flags() >> 32) & 0x3
    }

    pub fn curr_pic_idx(&self) -> u64 {
        (self.flags() >> 34) & 0x7f
    }

    pub fn frame_number(&self) -> u64 {
        (self.flags() >> 46) & 0xffff
    }
}

#[repr(C)]
#[derive(Clone, Default)]
pub struct DpbEntry {
    pub flags: u32,
    pub field_order_cnt: [u32; 2],
    pub frame_idx: u32,
}

const _: () = assert!(std::mem::size_of::<DpbEntry>() == 0x10);

#[repr(C)]
#[derive(Clone, Default)]
pub struct DisplayParam {
    pub flags0: u32,
    pub output_top: [i32; 2],
    pub output_bottom: [i32; 2],
    pub histogram_flags1: u32,
    pub histogram_flags2: u32,
}

const _: () = assert!(std::mem::size_of::<DisplayParam>() == 0x1c);

#[repr(C)]
#[derive(Clone)]
pub struct H264DecoderContext {
    pub reserved0: [u32; 13],
    pub eos: [u8; 16],
    pub explicit_eos_present_flag: u8,
    pub hint_dump_en: u8,
    pub _pad0: [u8; 2],
    pub stream_len: u32,
    pub slice_count: u32,
    pub mbhist_buffer_size: u32,
    pub gptimer_timeout_value: u32,
    pub h264_parameter_set: H264ParameterSet,
    pub curr_field_order_cnt: [i32; 2],
    pub dpb: [DpbEntry; 16],
    pub weight_scale_4x4: [u8; 0x60],
    pub weight_scale_8x8: [u8; 0x80],
    pub num_inter_view_refs_lx: [u8; 2],
    pub reserved2: [u8; 14],
    pub inter_view_refidx_lx: [[i8; 16]; 2],
    pub lossless_flags: u32,
    pub display_param: DisplayParam,
    pub reserved4: [u32; 3],
}

const _: () = assert!(std::mem::size_of::<H264DecoderContext>() == 0x2fc);

impl Default for H264DecoderContext {
    fn default() -> Self {
        // All fields are integer storage or integer arrays in the guest ABI.
        unsafe { std::mem::zeroed() }
    }
}

impl H264DecoderContext {
    pub fn qpprime_y_zero_transform_bypass_flag(&self) -> u32 {
        (self.lossless_flags >> 1) & 1
    }
}

/// Surface indices used by the VP9 decoder.
///
/// Port of `Tegra::Decoders::Vp9SurfaceIndex`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vp9SurfaceIndex {
    Last = 0,
    Golden = 1,
    AltRef = 2,
    Current = 3,
}

const _: () = assert!(std::mem::size_of::<Vp9SurfaceIndex>() == 0x4);

/// VP9 frame dimensions.
///
/// Port of `Tegra::Decoders::Vp9FrameDimensions`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Vp9FrameDimensions {
    pub width: i16,
    pub height: i16,
    pub luma_pitch: i16,
    pub chroma_pitch: i16,
}

const _: () = assert!(std::mem::size_of::<Vp9FrameDimensions>() == 0x8);

bitflags! {
    /// Port of `Tegra::Decoders::FrameFlags`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct FrameFlags: u32 {
        const IS_KEY_FRAME = 1 << 0;
        const LAST_FRAME_IS_KEY_FRAME = 1 << 1;
        const FRAME_SIZE_CHANGED = 1 << 2;
        const ERROR_RESILIENT_MODE = 1 << 3;
        const LAST_SHOW_FRAME = 1 << 4;
        const INTRA_ONLY = 1 << 5;
    }
}

const _: () = assert!(std::mem::size_of::<FrameFlags>() == 0x4);

/// Transform sizes.
///
/// Port of `Tegra::Decoders::TxSize`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxSize {
    Tx4x4 = 0,
    Tx8x8 = 1,
    Tx16x16 = 2,
    Tx32x32 = 3,
    TxSizes = 4,
}

const _: () = assert!(std::mem::size_of::<TxSize>() == 0x4);

/// Transform modes.
///
/// Port of `Tegra::Decoders::TxMode`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxMode {
    Only4X4 = 0,
    Allow8X8 = 1,
    Allow16X16 = 2,
    Allow32X32 = 3,
    TxModeSelect = 4,
    TxModes = 5,
}

const _: () = assert!(std::mem::size_of::<TxMode>() == 0x4);

/// VP9 segmentation parameters.
///
/// Port of `Tegra::Decoders::Segmentation`.
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segmentation {
    pub enabled: u8,
    pub update_map: u8,
    pub temporal_update: u8,
    pub abs_delta: u8,
    pub feature_enabled: [[u8; 4]; 8],
    pub feature_data: [[i16; 4]; 8],
}

const _: () = assert!(std::mem::size_of::<Segmentation>() == 0x64);

impl Default for Segmentation {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

/// VP9 loop filter parameters.
///
/// Port of `Tegra::Decoders::LoopFilter`.
#[repr(C)]
#[derive(Debug, Clone, Default)]
pub struct LoopFilter {
    pub mode_ref_delta_enabled: u8,
    pub ref_deltas: [i8; 4],
    pub mode_deltas: [i8; 2],
}

const _: () = assert!(std::mem::size_of::<LoopFilter>() == 0x7);

/// VP9 entropy probabilities.
///
/// Port of `Tegra::Decoders::Vp9EntropyProbs`.
#[repr(C)]
#[derive(Clone)]
pub struct Vp9EntropyProbs {
    pub y_mode_prob: [u8; 36],           // 0x0000
    pub partition_prob: [u8; 64],        // 0x0024
    pub coef_probs: [u8; 1728],          // 0x0064
    pub switchable_interp_prob: [u8; 8], // 0x0724
    pub inter_mode_prob: [u8; 28],       // 0x072C
    pub intra_inter_prob: [u8; 4],       // 0x0748
    pub comp_inter_prob: [u8; 5],        // 0x074C
    pub single_ref_prob: [u8; 10],       // 0x0751
    pub comp_ref_prob: [u8; 5],          // 0x075B
    pub tx_32x32_prob: [u8; 6],          // 0x0760
    pub tx_16x16_prob: [u8; 4],          // 0x0766
    pub tx_8x8_prob: [u8; 2],            // 0x076A
    pub skip_probs: [u8; 3],             // 0x076C
    pub joints: [u8; 3],                 // 0x076F
    pub sign: [u8; 2],                   // 0x0772
    pub classes: [u8; 20],               // 0x0774
    pub class_0: [u8; 2],                // 0x0788
    pub prob_bits: [u8; 20],             // 0x078A
    pub class_0_fr: [u8; 12],            // 0x079E
    pub fr: [u8; 6],                     // 0x07AA
    pub class_0_hp: [u8; 2],             // 0x07B0
    pub high_precision: [u8; 2],         // 0x07B2
}

const _: () = assert!(std::mem::size_of::<Vp9EntropyProbs>() == 0x7B4);
const _: () = assert!(std::mem::offset_of!(Vp9EntropyProbs, partition_prob) == 0x24);
const _: () = assert!(std::mem::offset_of!(Vp9EntropyProbs, switchable_interp_prob) == 0x724);
const _: () = assert!(std::mem::offset_of!(Vp9EntropyProbs, sign) == 0x772);
const _: () = assert!(std::mem::offset_of!(Vp9EntropyProbs, class_0_fr) == 0x79e);
const _: () = assert!(std::mem::offset_of!(Vp9EntropyProbs, high_precision) == 0x7b2);

impl Default for Vp9EntropyProbs {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

/// Decoded VP9 picture information.
///
/// Port of `Tegra::Decoders::Vp9PictureInfo`.
#[derive(Clone, Default)]
pub struct Vp9PictureInfo {
    pub bitstream_size: u32,
    pub frame_offsets: [u64; 4],
    pub ref_frame_sign_bias: [i8; 4],
    pub base_q_index: i32,
    pub y_dc_delta_q: i32,
    pub uv_dc_delta_q: i32,
    pub uv_ac_delta_q: i32,
    pub transform_mode: i32,
    pub interp_filter: i32,
    pub reference_mode: i32,
    pub log2_tile_cols: i32,
    pub log2_tile_rows: i32,
    pub ref_deltas: [i8; 4],
    pub mode_deltas: [i8; 2],
    pub entropy: Vp9EntropyProbs,
    pub frame_size: Vp9FrameDimensions,
    pub first_level: u8,
    pub sharpness_level: u8,
    pub is_key_frame: bool,
    pub intra_only: bool,
    pub last_frame_was_key: bool,
    pub error_resilient_mode: bool,
    pub last_frame_shown: bool,
    pub show_frame: bool,
    pub lossless: bool,
    pub allow_high_precision_mv: bool,
    pub segment_enabled: bool,
    pub mode_ref_delta_enabled: bool,
}

/// Container for a VP9 frame and its bitstream.
///
/// Port of `Tegra::Decoders::Vp9FrameContainer`.
#[derive(Clone, Default)]
pub struct Vp9FrameContainer {
    pub info: Vp9PictureInfo,
    pub bit_stream: Vec<u8>,
}

/// Raw picture info as read from NVDEC memory (0x100 bytes).
///
/// Port of `Tegra::Decoders::PictureInfo`.
#[repr(C)]
#[derive(Clone)]
pub struct PictureInfo {
    pub reserved0: [u32; 12],                   // 0x00
    pub bitstream_size: u32,                    // 0x30
    pub reserved1: [u32; 5],                    // 0x34
    pub last_frame_size: Vp9FrameDimensions,    // 0x48
    pub golden_frame_size: Vp9FrameDimensions,  // 0x50
    pub alt_frame_size: Vp9FrameDimensions,     // 0x58
    pub current_frame_size: Vp9FrameDimensions, // 0x60
    pub vp9_flags: FrameFlags,                  // 0x68
    pub ref_frame_sign_bias: [i8; 4],           // 0x6C
    pub first_level: u8,                        // 0x70
    pub sharpness_level: u8,                    // 0x71
    pub base_q_index: u8,                       // 0x72
    pub y_dc_delta_q: u8,                       // 0x73
    pub uv_ac_delta_q: u8,                      // 0x74
    pub uv_dc_delta_q: u8,                      // 0x75
    pub lossless: u8,                           // 0x76
    pub tx_mode: u8,                            // 0x77
    pub allow_high_precision_mv: u8,            // 0x78
    pub interp_filter: u8,                      // 0x79
    pub reference_mode: u8,                     // 0x7A
    pub _pad0: [u8; 3],                         // 0x7B
    pub log2_tile_cols: u8,                     // 0x7E
    pub log2_tile_rows: u8,                     // 0x7F
    pub segmentation: Segmentation,             // 0x80
    pub loop_filter: LoopFilter,                // 0xE4
    pub _pad1: [u8; 21],                        // 0xEB
}

const _: () = assert!(std::mem::size_of::<PictureInfo>() == 0x100);
const _: () = assert!(std::mem::offset_of!(PictureInfo, bitstream_size) == 0x30);
const _: () = assert!(std::mem::offset_of!(PictureInfo, last_frame_size) == 0x48);
const _: () = assert!(std::mem::offset_of!(PictureInfo, first_level) == 0x70);
const _: () = assert!(std::mem::offset_of!(PictureInfo, segmentation) == 0x80);
const _: () = assert!(std::mem::offset_of!(PictureInfo, loop_filter) == 0xe4);

impl Default for PictureInfo {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

impl PictureInfo {
    /// Convert raw picture info to the decoded Vp9PictureInfo format.
    ///
    /// Port of `PictureInfo::Convert`.
    pub fn convert(&self) -> Vp9PictureInfo {
        Vp9PictureInfo {
            bitstream_size: self.bitstream_size,
            frame_offsets: [0; 4],
            ref_frame_sign_bias: self.ref_frame_sign_bias,
            base_q_index: self.base_q_index as i32,
            y_dc_delta_q: self.y_dc_delta_q as i32,
            uv_dc_delta_q: self.uv_dc_delta_q as i32,
            uv_ac_delta_q: self.uv_ac_delta_q as i32,
            transform_mode: self.tx_mode as i32,
            interp_filter: self.interp_filter as i32,
            reference_mode: self.reference_mode as i32,
            log2_tile_cols: self.log2_tile_cols as i32,
            log2_tile_rows: self.log2_tile_rows as i32,
            ref_deltas: self.loop_filter.ref_deltas,
            mode_deltas: self.loop_filter.mode_deltas,
            entropy: Vp9EntropyProbs::default(),
            frame_size: self.current_frame_size,
            first_level: self.first_level,
            sharpness_level: self.sharpness_level,
            is_key_frame: self.vp9_flags.contains(FrameFlags::IS_KEY_FRAME),
            intra_only: self.vp9_flags.contains(FrameFlags::INTRA_ONLY),
            last_frame_was_key: self.vp9_flags.contains(FrameFlags::LAST_FRAME_IS_KEY_FRAME),
            error_resilient_mode: self.vp9_flags.contains(FrameFlags::ERROR_RESILIENT_MODE),
            last_frame_shown: self.vp9_flags.contains(FrameFlags::LAST_SHOW_FRAME),
            show_frame: true,
            lossless: self.lossless != 0,
            allow_high_precision_mv: self.allow_high_precision_mv != 0,
            segment_enabled: self.segmentation.enabled != 0,
            mode_ref_delta_enabled: self.loop_filter.mode_ref_delta_enabled != 0,
        }
    }
}

/// Raw entropy probabilities as read from NVDEC memory (0xEA0 bytes).
///
/// Port of `Tegra::Decoders::EntropyProbs`.
#[repr(C)]
#[derive(Clone)]
pub struct EntropyProbs {
    pub kf_bmode_prob: [u8; 800],        // 0x0000
    pub kf_bmode_prob_b: [u8; 100],      // 0x0320
    pub ref_pred_probs: [u8; 3],         // 0x0384
    pub mb_segment_tree_probs: [u8; 7],  // 0x0387
    pub segment_pred_probs: [u8; 3],     // 0x038E
    pub ref_scores: [u8; 4],             // 0x0391
    pub prob_comppred: [u8; 2],          // 0x0395
    pub _pad0: [u8; 9],                  // 0x0397
    pub kf_uv_mode_prob: [u8; 80],       // 0x03A0
    pub kf_uv_mode_prob_b: [u8; 10],     // 0x03F0
    pub _pad1: [u8; 6],                  // 0x03FA
    pub inter_mode_prob: [u8; 28],       // 0x0400
    pub intra_inter_prob: [u8; 4],       // 0x041C
    pub _pad2: [u8; 80],                 // 0x0420
    pub tx_8x8_prob: [u8; 2],            // 0x0470
    pub tx_16x16_prob: [u8; 4],          // 0x0472
    pub tx_32x32_prob: [u8; 6],          // 0x0476
    pub y_mode_prob_e8: [u8; 4],         // 0x047C
    pub y_mode_prob_e0e7: [[u8; 8]; 4],  // 0x0480
    pub _pad3: [u8; 64],                 // 0x04A0
    pub partition_prob: [u8; 64],        // 0x04E0
    pub _pad4: [u8; 10],                 // 0x0520
    pub switchable_interp_prob: [u8; 8], // 0x052A
    pub comp_inter_prob: [u8; 5],        // 0x0532
    pub skip_probs: [u8; 3],             // 0x0537
    pub _pad5: [u8; 1],                  // 0x053A
    pub joints: [u8; 3],                 // 0x053B
    pub sign: [u8; 2],                   // 0x053E
    pub class_0: [u8; 2],                // 0x0540
    pub fr: [u8; 6],                     // 0x0542
    pub class_0_hp: [u8; 2],             // 0x0548
    pub high_precision: [u8; 2],         // 0x054A
    pub classes: [u8; 20],               // 0x054C
    pub class_0_fr: [u8; 12],            // 0x0560
    pub pred_bits: [u8; 20],             // 0x056C
    pub single_ref_prob: [u8; 10],       // 0x0580
    pub comp_ref_prob: [u8; 5],          // 0x058A
    pub _pad6: [u8; 17],                 // 0x058F
    pub coef_probs: [u8; 2304],          // 0x05A0
}

const _: () = assert!(std::mem::size_of::<EntropyProbs>() == 0xEA0);
const _: () = assert!(std::mem::offset_of!(EntropyProbs, inter_mode_prob) == 0x400);
const _: () = assert!(std::mem::offset_of!(EntropyProbs, tx_8x8_prob) == 0x470);
const _: () = assert!(std::mem::offset_of!(EntropyProbs, partition_prob) == 0x4e0);
const _: () = assert!(std::mem::offset_of!(EntropyProbs, class_0) == 0x540);
const _: () = assert!(std::mem::offset_of!(EntropyProbs, class_0_fr) == 0x560);
const _: () = assert!(std::mem::offset_of!(EntropyProbs, coef_probs) == 0x5a0);

impl Default for EntropyProbs {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

impl EntropyProbs {
    /// Convert raw entropy probs to the compact Vp9EntropyProbs format.
    ///
    /// Port of `EntropyProbs::Convert`.
    pub fn convert(&self, fc: &mut Vp9EntropyProbs) {
        fc.inter_mode_prob = self.inter_mode_prob;
        fc.intra_inter_prob = self.intra_inter_prob;
        fc.tx_8x8_prob = self.tx_8x8_prob;
        fc.tx_16x16_prob = self.tx_16x16_prob;
        fc.tx_32x32_prob = self.tx_32x32_prob;

        for i in 0..4 {
            for j in 0..9 {
                fc.y_mode_prob[j + 9 * i] = if j < 8 {
                    self.y_mode_prob_e0e7[i][j]
                } else {
                    self.y_mode_prob_e8[i]
                };
            }
        }

        fc.partition_prob = self.partition_prob;
        fc.switchable_interp_prob = self.switchable_interp_prob;
        fc.comp_inter_prob = self.comp_inter_prob;
        fc.skip_probs = self.skip_probs;
        fc.joints = self.joints;
        fc.sign = self.sign;
        fc.class_0 = self.class_0;
        fc.fr = self.fr;
        fc.class_0_hp = self.class_0_hp;
        fc.high_precision = self.high_precision;
        fc.classes = self.classes;
        fc.class_0_fr = self.class_0_fr;
        fc.prob_bits = self.pred_bits;
        fc.single_ref_prob = self.single_ref_prob;
        fc.comp_ref_prob = self.comp_ref_prob;

        // Skip the 4th element as it goes unused.
        let mut j = 0usize;
        let mut i = 0usize;
        while i < self.coef_probs.len() {
            fc.coef_probs[j] = self.coef_probs[i];
            fc.coef_probs[j + 1] = self.coef_probs[i + 1];
            fc.coef_probs[j + 2] = self.coef_probs[i + 2];
            j += 3;
            i += 4;
        }
    }
}

/// Reference frame type for the reference pool.
///
/// Port of `Tegra::Decoders::Ref`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ref {
    Last,
    Golden,
    AltRef,
}

/// Element in the VP9 reference frame pool.
///
/// Port of `Tegra::Decoders::RefPoolElement`.
#[derive(Debug, Clone)]
pub struct RefPoolElement {
    pub frame: i64,
    pub reference: Ref,
    pub refresh: bool,
}

impl Default for RefPoolElement {
    fn default() -> Self {
        Self {
            frame: 0,
            reference: Ref::Last,
            refresh: false,
        }
    }
}

#[repr(C)]
#[derive(Clone)]
pub struct Vp8PictureInfo {
    pub reserved0: [u32; 14],
    pub frame_width: u16,
    pub frame_height: u16,
    pub key_frame: u8,
    pub version: u8,
    pub surface_format: u8,
    pub error_conceal_on: u8,
    pub first_part_size: u32,
    pub hist_buffer_size: u32,
    pub vld_buffer_size: u32,
    pub frame_stride: [u32; 2],
    pub luma_top_offset: u32,
    pub luma_bot_offset: u32,
    pub luma_frame_offset: u32,
    pub chroma_top_offset: u32,
    pub chroma_bot_offset: u32,
    pub chroma_frame_offset: u32,
    pub display_params: [u8; 0x1c],
    pub current_output_memory_layout: i8,
    pub output_memory_layout: [i8; 3],
    pub segmentation_feature_data_update: u8,
    pub _pad: [u8; 3],
    pub result_value: u32,
    pub partition_offset: [u32; 8],
    pub reserved1: [u32; 3],
}

const _: () = assert!(std::mem::size_of::<Vp8PictureInfo>() == 0xc0);
const _: () = assert!(std::mem::offset_of!(Vp8PictureInfo, frame_width) == 0x38);
const _: () = assert!(std::mem::offset_of!(Vp8PictureInfo, first_part_size) == 0x40);
const _: () = assert!(std::mem::offset_of!(Vp8PictureInfo, vld_buffer_size) == 0x48);
const _: () = assert!(std::mem::offset_of!(Vp8PictureInfo, current_output_memory_layout) == 0x88);
const _: () = assert!(std::mem::offset_of!(Vp8PictureInfo, partition_offset) == 0x94);

impl Default for Vp8PictureInfo {
    fn default() -> Self {
        // All fields are integer storage or integer arrays in the guest ABI.
        unsafe { std::mem::zeroed() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_nvdec_layout_matches_codec_types_header() {
        assert_eq!(std::mem::size_of::<Offset>(), 0x4);
        assert_eq!(std::mem::size_of::<H264ParameterSet>(), 0x60);
        assert_eq!(std::mem::size_of::<DpbEntry>(), 0x10);
        assert_eq!(std::mem::size_of::<DisplayParam>(), 0x1c);
        assert_eq!(std::mem::size_of::<H264DecoderContext>(), 0x2fc);
        assert_eq!(std::mem::size_of::<Vp9SurfaceIndex>(), 0x4);
        assert_eq!(std::mem::size_of::<Vp9FrameDimensions>(), 0x8);
        assert_eq!(std::mem::size_of::<FrameFlags>(), 0x4);
        assert_eq!(std::mem::size_of::<TxSize>(), 0x4);
        assert_eq!(std::mem::size_of::<TxMode>(), 0x4);
        assert_eq!(std::mem::size_of::<Segmentation>(), 0x64);
        assert_eq!(std::mem::size_of::<LoopFilter>(), 0x7);
        assert_eq!(std::mem::size_of::<Vp9EntropyProbs>(), 0x7b4);
        assert_eq!(std::mem::size_of::<PictureInfo>(), 0x100);
        assert_eq!(std::mem::size_of::<EntropyProbs>(), 0xea0);
        assert_eq!(std::mem::size_of::<Vp8PictureInfo>(), 0xc0);

        assert_eq!(
            std::mem::offset_of!(H264ParameterSet, log2_max_pic_order_cnt_lsb_minus4),
            0x00
        );
        assert_eq!(std::mem::offset_of!(H264ParameterSet, surface_format), 0x14);
        assert_eq!(
            std::mem::offset_of!(H264ParameterSet, luma_top_offset),
            0x3c
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
        assert_eq!(std::mem::offset_of!(H264DecoderContext, dpb), 0xc0);
        assert_eq!(
            std::mem::offset_of!(H264DecoderContext, weight_scale_4x4),
            0x1c0
        );
        assert_eq!(
            std::mem::offset_of!(H264DecoderContext, weight_scale_8x8),
            0x220
        );
        assert_eq!(
            std::mem::offset_of!(H264DecoderContext, display_param),
            0x2d4
        );

        assert_eq!(std::mem::offset_of!(PictureInfo, bitstream_size), 0x30);
        assert_eq!(std::mem::offset_of!(PictureInfo, last_frame_size), 0x48);
        assert_eq!(std::mem::offset_of!(PictureInfo, first_level), 0x70);
        assert_eq!(std::mem::offset_of!(PictureInfo, segmentation), 0x80);
        assert_eq!(std::mem::offset_of!(PictureInfo, loop_filter), 0xe4);
        assert_eq!(std::mem::offset_of!(EntropyProbs, inter_mode_prob), 0x400);
        assert_eq!(std::mem::offset_of!(EntropyProbs, tx_8x8_prob), 0x470);
        assert_eq!(std::mem::offset_of!(EntropyProbs, partition_prob), 0x4e0);
        assert_eq!(std::mem::offset_of!(EntropyProbs, class_0), 0x540);
        assert_eq!(std::mem::offset_of!(EntropyProbs, class_0_fr), 0x560);
        assert_eq!(std::mem::offset_of!(EntropyProbs, coef_probs), 0x5a0);
        assert_eq!(std::mem::offset_of!(Vp8PictureInfo, frame_width), 0x38);
        assert_eq!(std::mem::offset_of!(Vp8PictureInfo, first_part_size), 0x40);
        assert_eq!(std::mem::offset_of!(Vp8PictureInfo, vld_buffer_size), 0x48);
        assert_eq!(
            std::mem::offset_of!(Vp8PictureInfo, current_output_memory_layout),
            0x88
        );
        assert_eq!(std::mem::offset_of!(Vp8PictureInfo, partition_offset), 0x94);
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
            | (0xabcd << 46);
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
        assert_eq!(params.frame_number(), 0xabcd);
    }

    #[test]
    fn picture_info_conversion_matches_codec_types_header() {
        let mut raw = PictureInfo::default();
        raw.bitstream_size = 0x1234_5678;
        raw.current_frame_size = Vp9FrameDimensions {
            width: 1280,
            height: 720,
            luma_pitch: 1344,
            chroma_pitch: 672,
        };
        raw.vp9_flags = FrameFlags::IS_KEY_FRAME
            | FrameFlags::LAST_FRAME_IS_KEY_FRAME
            | FrameFlags::ERROR_RESILIENT_MODE
            | FrameFlags::LAST_SHOW_FRAME
            | FrameFlags::INTRA_ONLY;
        raw.ref_frame_sign_bias = [-1, 0, 1, 2];
        raw.first_level = 17;
        raw.sharpness_level = 5;
        raw.base_q_index = 0xff;
        raw.y_dc_delta_q = 0x80;
        raw.uv_dc_delta_q = 0x7f;
        raw.uv_ac_delta_q = 0xfe;
        raw.lossless = 1;
        raw.tx_mode = TxMode::Allow32X32 as u8;
        raw.allow_high_precision_mv = 1;
        raw.interp_filter = 3;
        raw.reference_mode = 2;
        raw.log2_tile_cols = 4;
        raw.log2_tile_rows = 1;
        raw.segmentation.enabled = 1;
        raw.loop_filter.mode_ref_delta_enabled = 1;
        raw.loop_filter.ref_deltas = [-1, 2, -3, 4];
        raw.loop_filter.mode_deltas = [-5, 6];

        let converted = raw.convert();
        assert_eq!(converted.bitstream_size, 0x1234_5678);
        assert_eq!(converted.frame_offsets, [0; 4]);
        assert_eq!(converted.frame_size.width, 1280);
        assert_eq!(converted.ref_frame_sign_bias, [-1, 0, 1, 2]);
        assert_eq!(converted.base_q_index, 255);
        assert_eq!(converted.y_dc_delta_q, 128);
        assert_eq!(converted.uv_dc_delta_q, 127);
        assert_eq!(converted.uv_ac_delta_q, 254);
        assert_eq!(converted.transform_mode, TxMode::Allow32X32 as i32);
        assert_eq!(converted.ref_deltas, [-1, 2, -3, 4]);
        assert_eq!(converted.mode_deltas, [-5, 6]);
        assert!(converted.is_key_frame);
        assert!(converted.intra_only);
        assert!(converted.last_frame_was_key);
        assert!(converted.error_resilient_mode);
        assert!(converted.last_frame_shown);
        assert!(converted.show_frame);
        assert!(converted.lossless);
        assert!(converted.allow_high_precision_mv);
        assert!(converted.segment_enabled);
        assert!(converted.mode_ref_delta_enabled);
    }

    #[test]
    fn entropy_conversion_skips_each_unused_fourth_coefficient() {
        let mut raw = EntropyProbs::default();
        for (index, value) in raw.coef_probs.iter_mut().enumerate() {
            *value = (index % 251) as u8;
        }
        for row in 0..4 {
            raw.y_mode_prob_e0e7[row] = std::array::from_fn(|column| (row * 16 + column) as u8);
            raw.y_mode_prob_e8[row] = (row * 16 + 8) as u8;
        }

        let mut converted = Vp9EntropyProbs::default();
        raw.convert(&mut converted);

        let expected_coefficients: Vec<u8> = raw
            .coef_probs
            .chunks_exact(4)
            .flat_map(|chunk| chunk[..3].iter().copied())
            .collect();
        assert_eq!(converted.coef_probs.as_slice(), expected_coefficients);
        for row in 0..4 {
            assert_eq!(
                &converted.y_mode_prob[row * 9..row * 9 + 9],
                &(0..9)
                    .map(|column| (row * 16 + column) as u8)
                    .collect::<Vec<_>>()
            );
        }
    }
}

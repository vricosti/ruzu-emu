// SPDX-FileCopyrightText: Copyright 2024 ruzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/host1x/codecs/vp9.h` and `vp9.cpp`.
//!
//! VP9 video decoder implementation, including the VpxRangeEncoder and
//! VpxBitStreamWriter for composing VP9 compressed/uncompressed headers.

use crate::host1x::codecs::decoder::{DecoderImpl, DecoderState};
use crate::host1x::codecs::vp9_types::{
    EntropyProbs, PictureInfo, Segmentation, Vp9EntropyProbs, Vp9FrameContainer, Vp9PictureInfo,
    Vp9SurfaceIndex,
};
use crate::host1x::host1x::FrameQueue;
use crate::host1x::nvdec_common::{NvdecRegisters, VideoCodec};
use crate::memory_manager::MemoryManager;
use std::sync::Arc;

// --------------------------------------------------------------------------
// Constants from vp9.cpp
// --------------------------------------------------------------------------

/// Probability used for diff updates in the compressed header.
const DIFF_UPDATE_PROBABILITY: u32 = 252;

/// Frame sync code for VP9 uncompressed header.
const FRAME_SYNC_CODE: u32 = 0x498342;

const DEFAULT_PROBS: Vp9EntropyProbs = Vp9EntropyProbs {
    y_mode_prob: [
        65, 32, 18, 144, 162, 194, 41, 51, 98, 132, 68, 18, 165, 217, 196, 45, 40, 78, 173, 80, 19,
        176, 240, 193, 64, 35, 46, 221, 135, 38, 194, 248, 121, 96, 85, 29,
    ],
    partition_prob: [
        199, 122, 141, 0, 147, 63, 159, 0, 148, 133, 118, 0, 121, 104, 114, 0, 174, 73, 87, 0, 92,
        41, 83, 0, 82, 99, 50, 0, 53, 39, 39, 0, 177, 58, 59, 0, 68, 26, 63, 0, 52, 79, 25, 0, 17,
        14, 12, 0, 222, 34, 30, 0, 72, 16, 44, 0, 58, 32, 12, 0, 10, 7, 6, 0,
    ],
    coef_probs: [
        195, 29, 183, 84, 49, 136, 8, 42, 71, 0, 0, 0, 0, 0, 0, 0, 0, 0, 31, 107, 169, 35, 99, 159,
        17, 82, 140, 8, 66, 114, 2, 44, 76, 1, 19, 32, 40, 132, 201, 29, 114, 187, 13, 91, 157, 7,
        75, 127, 3, 58, 95, 1, 28, 47, 69, 142, 221, 42, 122, 201, 15, 91, 159, 6, 67, 121, 1, 42,
        77, 1, 17, 31, 102, 148, 228, 67, 117, 204, 17, 82, 154, 6, 59, 114, 2, 39, 75, 1, 15, 29,
        156, 57, 233, 119, 57, 212, 58, 48, 163, 29, 40, 124, 12, 30, 81, 3, 12, 31, 191, 107, 226,
        124, 117, 204, 25, 99, 155, 0, 0, 0, 0, 0, 0, 0, 0, 0, 29, 148, 210, 37, 126, 194, 8, 93,
        157, 2, 68, 118, 1, 39, 69, 1, 17, 33, 41, 151, 213, 27, 123, 193, 3, 82, 144, 1, 58, 105,
        1, 32, 60, 1, 13, 26, 59, 159, 220, 23, 126, 198, 4, 88, 151, 1, 66, 114, 1, 38, 71, 1, 18,
        34, 114, 136, 232, 51, 114, 207, 11, 83, 155, 3, 56, 105, 1, 33, 65, 1, 17, 34, 149, 65,
        234, 121, 57, 215, 61, 49, 166, 28, 36, 114, 12, 25, 76, 3, 16, 42, 214, 49, 220, 132, 63,
        188, 42, 65, 137, 0, 0, 0, 0, 0, 0, 0, 0, 0, 85, 137, 221, 104, 131, 216, 49, 111, 192, 21,
        87, 155, 2, 49, 87, 1, 16, 28, 89, 163, 230, 90, 137, 220, 29, 100, 183, 10, 70, 135, 2,
        42, 81, 1, 17, 33, 108, 167, 237, 55, 133, 222, 15, 97, 179, 4, 72, 135, 1, 45, 85, 1, 19,
        38, 124, 146, 240, 66, 124, 224, 17, 88, 175, 4, 58, 122, 1, 36, 75, 1, 18, 37, 141, 79,
        241, 126, 70, 227, 66, 58, 182, 30, 44, 136, 12, 34, 96, 2, 20, 47, 229, 99, 249, 143, 111,
        235, 46, 109, 192, 0, 0, 0, 0, 0, 0, 0, 0, 0, 82, 158, 236, 94, 146, 224, 25, 117, 191, 9,
        87, 149, 3, 56, 99, 1, 33, 57, 83, 167, 237, 68, 145, 222, 10, 103, 177, 2, 72, 131, 1, 41,
        79, 1, 20, 39, 99, 167, 239, 47, 141, 224, 10, 104, 178, 2, 73, 133, 1, 44, 85, 1, 22, 47,
        127, 145, 243, 71, 129, 228, 17, 93, 177, 3, 61, 124, 1, 41, 84, 1, 21, 52, 157, 78, 244,
        140, 72, 231, 69, 58, 184, 31, 44, 137, 14, 38, 105, 8, 23, 61, 125, 34, 187, 52, 41, 133,
        6, 31, 56, 0, 0, 0, 0, 0, 0, 0, 0, 0, 37, 109, 153, 51, 102, 147, 23, 87, 128, 8, 67, 101,
        1, 41, 63, 1, 19, 29, 31, 154, 185, 17, 127, 175, 6, 96, 145, 2, 73, 114, 1, 51, 82, 1, 28,
        45, 23, 163, 200, 10, 131, 185, 2, 93, 148, 1, 67, 111, 1, 41, 69, 1, 14, 24, 29, 176, 217,
        12, 145, 201, 3, 101, 156, 1, 69, 111, 1, 39, 63, 1, 14, 23, 57, 192, 233, 25, 154, 215, 6,
        109, 167, 3, 78, 118, 1, 48, 69, 1, 21, 29, 202, 105, 245, 108, 106, 216, 18, 90, 144, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 33, 172, 219, 64, 149, 206, 14, 117, 177, 5, 90, 141, 2, 61, 95, 1,
        37, 57, 33, 179, 220, 11, 140, 198, 1, 89, 148, 1, 60, 104, 1, 33, 57, 1, 12, 21, 30, 181,
        221, 8, 141, 198, 1, 87, 145, 1, 58, 100, 1, 31, 55, 1, 12, 20, 32, 186, 224, 7, 142, 198,
        1, 86, 143, 1, 58, 100, 1, 31, 55, 1, 12, 22, 57, 192, 227, 20, 143, 204, 3, 96, 154, 1,
        68, 112, 1, 42, 69, 1, 19, 32, 212, 35, 215, 113, 47, 169, 29, 48, 105, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 74, 129, 203, 106, 120, 203, 49, 107, 178, 19, 84, 144, 4, 50, 84, 1, 15, 25, 71,
        172, 217, 44, 141, 209, 15, 102, 173, 6, 76, 133, 2, 51, 89, 1, 24, 42, 64, 185, 231, 31,
        148, 216, 8, 103, 175, 3, 74, 131, 1, 46, 81, 1, 18, 30, 65, 196, 235, 25, 157, 221, 5,
        105, 174, 1, 67, 120, 1, 38, 69, 1, 15, 30, 65, 204, 238, 30, 156, 224, 7, 107, 177, 2, 70,
        124, 1, 42, 73, 1, 18, 34, 225, 86, 251, 144, 104, 235, 42, 99, 181, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 85, 175, 239, 112, 165, 229, 29, 136, 200, 12, 103, 162, 6, 77, 123, 2, 53, 84, 75,
        183, 239, 30, 155, 221, 3, 106, 171, 1, 74, 128, 1, 44, 76, 1, 17, 28, 73, 185, 240, 27,
        159, 222, 2, 107, 172, 1, 75, 127, 1, 42, 73, 1, 17, 29, 62, 190, 238, 21, 159, 222, 2,
        107, 172, 1, 72, 122, 1, 40, 71, 1, 18, 32, 61, 199, 240, 27, 161, 226, 4, 113, 180, 1, 76,
        129, 1, 46, 80, 1, 23, 41, 7, 27, 153, 5, 30, 95, 1, 16, 30, 0, 0, 0, 0, 0, 0, 0, 0, 0, 50,
        75, 127, 57, 75, 124, 27, 67, 108, 10, 54, 86, 1, 33, 52, 1, 12, 18, 43, 125, 151, 26, 108,
        148, 7, 83, 122, 2, 59, 89, 1, 38, 60, 1, 17, 27, 23, 144, 163, 13, 112, 154, 2, 75, 117,
        1, 50, 81, 1, 31, 51, 1, 14, 23, 18, 162, 185, 6, 123, 171, 1, 78, 125, 1, 51, 86, 1, 31,
        54, 1, 14, 23, 15, 199, 227, 3, 150, 204, 1, 91, 146, 1, 55, 95, 1, 30, 53, 1, 11, 20, 19,
        55, 240, 19, 59, 196, 3, 52, 105, 0, 0, 0, 0, 0, 0, 0, 0, 0, 41, 166, 207, 104, 153, 199,
        31, 123, 181, 14, 101, 152, 5, 72, 106, 1, 36, 52, 35, 176, 211, 12, 131, 190, 2, 88, 144,
        1, 60, 101, 1, 36, 60, 1, 16, 28, 28, 183, 213, 8, 134, 191, 1, 86, 142, 1, 56, 96, 1, 30,
        53, 1, 12, 20, 20, 190, 215, 4, 135, 192, 1, 84, 139, 1, 53, 91, 1, 28, 49, 1, 11, 20, 13,
        196, 216, 2, 137, 192, 1, 86, 143, 1, 57, 99, 1, 32, 56, 1, 13, 24, 211, 29, 217, 96, 47,
        156, 22, 43, 87, 0, 0, 0, 0, 0, 0, 0, 0, 0, 78, 120, 193, 111, 116, 186, 46, 102, 164, 15,
        80, 128, 2, 49, 76, 1, 18, 28, 71, 161, 203, 42, 132, 192, 10, 98, 150, 3, 69, 109, 1, 44,
        70, 1, 18, 29, 57, 186, 211, 30, 140, 196, 4, 93, 146, 1, 62, 102, 1, 38, 65, 1, 16, 27,
        47, 199, 217, 14, 145, 196, 1, 88, 142, 1, 57, 98, 1, 36, 62, 1, 15, 26, 26, 219, 229, 5,
        155, 207, 1, 94, 151, 1, 60, 104, 1, 36, 62, 1, 16, 28, 233, 29, 248, 146, 47, 220, 43, 52,
        140, 0, 0, 0, 0, 0, 0, 0, 0, 0, 100, 163, 232, 179, 161, 222, 63, 142, 204, 37, 113, 174,
        26, 89, 137, 18, 68, 97, 85, 181, 230, 32, 146, 209, 7, 100, 164, 3, 71, 121, 1, 45, 77, 1,
        18, 30, 65, 187, 230, 20, 148, 207, 2, 97, 159, 1, 68, 116, 1, 40, 70, 1, 14, 29, 40, 194,
        227, 8, 147, 204, 1, 94, 155, 1, 65, 112, 1, 39, 66, 1, 14, 26, 16, 208, 228, 3, 151, 207,
        1, 98, 160, 1, 67, 117, 1, 41, 74, 1, 17, 31, 17, 38, 140, 7, 34, 80, 1, 17, 29, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 37, 75, 128, 41, 76, 128, 26, 66, 116, 12, 52, 94, 2, 32, 55, 1, 10, 16,
        50, 127, 154, 37, 109, 152, 16, 82, 121, 5, 59, 85, 1, 35, 54, 1, 13, 20, 40, 142, 167, 17,
        110, 157, 2, 71, 112, 1, 44, 72, 1, 27, 45, 1, 11, 17, 30, 175, 188, 9, 124, 169, 1, 74,
        116, 1, 48, 78, 1, 30, 49, 1, 11, 18, 10, 222, 223, 2, 150, 194, 1, 83, 128, 1, 48, 79, 1,
        27, 45, 1, 11, 17, 36, 41, 235, 29, 36, 193, 10, 27, 111, 0, 0, 0, 0, 0, 0, 0, 0, 0, 85,
        165, 222, 177, 162, 215, 110, 135, 195, 57, 113, 168, 23, 83, 120, 10, 49, 61, 85, 190,
        223, 36, 139, 200, 5, 90, 146, 1, 60, 103, 1, 38, 65, 1, 18, 30, 72, 202, 223, 23, 141,
        199, 2, 86, 140, 1, 56, 97, 1, 36, 61, 1, 16, 27, 55, 218, 225, 13, 145, 200, 1, 86, 141,
        1, 57, 99, 1, 35, 61, 1, 13, 22, 15, 235, 212, 1, 132, 184, 1, 84, 139, 1, 57, 97, 1, 34,
        56, 1, 14, 23, 181, 21, 201, 61, 37, 123, 10, 38, 71, 0, 0, 0, 0, 0, 0, 0, 0, 0, 47, 106,
        172, 95, 104, 173, 42, 93, 159, 18, 77, 131, 4, 50, 81, 1, 17, 23, 62, 147, 199, 44, 130,
        189, 28, 102, 154, 18, 75, 115, 2, 44, 65, 1, 12, 19, 55, 153, 210, 24, 130, 194, 3, 93,
        146, 1, 61, 97, 1, 31, 50, 1, 10, 16, 49, 186, 223, 17, 148, 204, 1, 96, 142, 1, 53, 83, 1,
        26, 44, 1, 11, 17, 13, 217, 212, 2, 136, 180, 1, 78, 124, 1, 50, 83, 1, 29, 49, 1, 14, 23,
        197, 13, 247, 82, 17, 222, 25, 17, 162, 0, 0, 0, 0, 0, 0, 0, 0, 0, 126, 186, 247, 234, 191,
        243, 176, 177, 234, 104, 158, 220, 66, 128, 186, 55, 90, 137, 111, 197, 242, 46, 158, 219,
        9, 104, 171, 2, 65, 125, 1, 44, 80, 1, 17, 91, 104, 208, 245, 39, 168, 224, 3, 109, 162, 1,
        79, 124, 1, 50, 102, 1, 43, 102, 84, 220, 246, 31, 177, 231, 2, 115, 180, 1, 79, 134, 1,
        55, 77, 1, 60, 79, 43, 243, 240, 8, 180, 217, 1, 115, 166, 1, 84, 121, 1, 51, 67, 1, 16, 6,
    ],
    switchable_interp_prob: [235, 162, 36, 255, 34, 3, 149, 144],
    inter_mode_prob: [
        2, 173, 34, 0, 7, 145, 85, 0, 7, 166, 63, 0, 7, 94, 66, 0, 8, 64, 46, 0, 17, 81, 31, 0, 25,
        29, 30, 0,
    ],
    intra_inter_prob: [9, 102, 187, 225],
    comp_inter_prob: [9, 102, 187, 225, 0],
    single_ref_prob: [33, 16, 77, 74, 142, 142, 172, 170, 238, 247],
    comp_ref_prob: [50, 126, 123, 221, 226],
    tx_32x32_prob: [3, 136, 37, 5, 52, 13],
    tx_16x16_prob: [20, 152, 15, 101],
    tx_8x8_prob: [100, 66],
    skip_probs: [192, 128, 64],
    joints: [32, 64, 96],
    sign: [128, 128],
    classes: [
        224, 144, 192, 168, 192, 176, 192, 198, 198, 245, 216, 128, 176, 160, 176, 176, 192, 198,
        198, 208,
    ],
    class_0: [216, 208],
    prob_bits: [
        136, 140, 148, 160, 176, 192, 224, 234, 234, 240, 136, 140, 148, 160, 176, 192, 224, 234,
        234, 240,
    ],
    class_0_fr: [128, 128, 64, 96, 112, 64, 128, 128, 64, 96, 112, 64],
    fr: [64, 96, 64, 64, 96, 64],
    class_0_hp: [160, 160],
    high_precision: [128, 128],
};
fn calc_min_log2_tile_cols(frame_width: i32) -> i32 {
    let sb64_cols = (frame_width + 63) / 64;
    let mut min_log2 = 0;
    while (64 << min_log2) < sb64_cols {
        min_log2 += 1;
    }
    min_log2
}

fn calc_max_log2_tile_cols(frame_width: i32) -> i32 {
    let sb64_cols = (frame_width + 63) / 64;
    let mut max_log2 = 1;
    while (sb64_cols >> max_log2) >= 4 {
        max_log2 += 1;
    }
    max_log2 - 1
}

fn recenter_non_neg(new_prob: i32, old_prob: i32) -> i32 {
    if new_prob > old_prob * 2 {
        new_prob
    } else if new_prob >= old_prob {
        (new_prob - old_prob) * 2
    } else {
        (old_prob - new_prob) * 2 - 1
    }
}

fn remap_probability(mut new_prob: i32, mut old_prob: i32) -> i32 {
    new_prob -= 1;
    old_prob -= 1;
    let i = if old_prob * 2 <= 0xff {
        (recenter_non_neg(new_prob, old_prob) - 1).max(0)
    } else {
        (recenter_non_neg(0xfe - new_prob, 0xfe - old_prob) - 1).max(0)
    } as u8;
    let i = i as i32;
    if (i + 7) % 13 == 0 {
        (i + 7) / 13 - 1
    } else {
        i + 20 - (i + 7) / 13
    }
}

// --------------------------------------------------------------------------
// VpxRangeEncoder
// --------------------------------------------------------------------------

/// Range encoder for VP9 compressed header bitstreams.
///
/// Port of `Tegra::Decoders::VpxRangeEncoder`.
pub struct VpxRangeEncoder {
    buffer: Vec<u8>,
    low_value: u32,
    range: u32,
    count: i32,
    half_probability: i32,
}

impl VpxRangeEncoder {
    pub fn new() -> Self {
        let mut encoder = Self {
            buffer: Vec::new(),
            low_value: 0,
            range: 0xff,
            count: -24,
            half_probability: 128,
        };
        encoder.write_bit_half(false);
        encoder
    }

    /// Writes the rightmost value_size bits from value into the stream.
    pub fn write(&mut self, value: i32, value_size: i32) {
        for i in (0..value_size).rev() {
            self.write_bit_half((value >> i) & 1 != 0);
        }
    }

    /// Writes a single bit with half probability.
    pub fn write_bit_half(&mut self, bit: bool) {
        self.write_bool(bit, self.half_probability);
    }

    /// Writes a bit encoded with the given probability.
    pub fn write_bool(&mut self, bit: bool, probability: i32) {
        let split =
            1u32.wrapping_add(self.range.wrapping_sub(1).wrapping_mul(probability as u32) >> 8);
        let mut local_range = split;
        if bit {
            self.low_value = self.low_value.wrapping_add(split);
            local_range = self.range.wrapping_sub(split);
        }

        let mut shift = if local_range == 0 {
            0
        } else {
            local_range.leading_zeros() as i32 - 24
        };
        local_range <<= shift;
        self.count += shift;

        if self.count >= 0 {
            let offset = shift - self.count;
            if (self.low_value.wrapping_shl((offset - 1) as u32) >> 31) != 0 {
                let mut pos = self.buffer.len();
                while pos != 0 && self.buffer[pos - 1] == 0xff {
                    self.buffer[pos - 1] = 0;
                    pos -= 1;
                }
                if pos != 0 {
                    self.buffer[pos - 1] = self.buffer[pos - 1].wrapping_add(1);
                }
            }
            self.buffer.push((self.low_value >> (24 - offset)) as u8);
            self.low_value = self.low_value.wrapping_shl(offset as u32);
            shift = self.count;
            self.low_value &= 0x00ff_ffff;
            self.count -= 8;
        }

        self.low_value = self.low_value.wrapping_shl(shift as u32);
        self.range = local_range;
    }

    /// Signal the end of the bitstream.
    pub fn end(&mut self) {
        for _ in 0..32 {
            self.write_bit_half(false);
        }
    }

    pub fn get_buffer(&self) -> &Vec<u8> {
        &self.buffer
    }

    pub fn get_buffer_mut(&mut self) -> &mut Vec<u8> {
        &mut self.buffer
    }
}

impl Default for VpxRangeEncoder {
    fn default() -> Self {
        Self::new()
    }
}

// --------------------------------------------------------------------------
// VpxBitStreamWriter
// --------------------------------------------------------------------------

/// Bitstream writer for VP9 uncompressed headers.
///
/// Port of `Tegra::Decoders::VpxBitStreamWriter`.
pub struct VpxBitStreamWriter {
    buffer_size: i32,
    buffer: i32,
    buffer_pos: i32,
    byte_array: Vec<u8>,
}

impl VpxBitStreamWriter {
    pub fn new() -> Self {
        Self {
            buffer_size: 8,
            buffer: 0,
            buffer_pos: 0,
            byte_array: Vec::new(),
        }
    }

    /// Write an unsigned integer value.
    pub fn write_u(&mut self, value: u32, value_size: u32) {
        self.write_bits(value, value_size);
    }

    /// Write a signed integer value.
    pub fn write_s(&mut self, value: i32, value_size: u32) {
        let sign = value < 0;
        let magnitude = if sign { -value } else { value } as u32;
        self.write_bits((magnitude << 1) | u32::from(sign), value_size + 1);
    }

    /// Write a delta coded value per VP9 spec section 6.2.10.
    pub fn write_delta_q(&mut self, value: u32) {
        if value == 0 {
            self.write_bit(false);
        } else {
            self.write_bit(true);
            self.write_bits(value, 4);
        }
    }

    /// Write a single bit.
    pub fn write_bit(&mut self, state: bool) {
        self.write_bits(if state { 1 } else { 0 }, 1);
    }

    /// Pushes current buffer into byte_array, resets buffer.
    pub fn flush(&mut self) {
        if self.buffer_pos == 0 {
            return;
        }
        self.byte_array.push(self.buffer as u8);
        self.buffer = 0;
        self.buffer_pos = 0;
    }

    /// Returns the composed byte array.
    pub fn get_byte_array(&self) -> &Vec<u8> {
        &self.byte_array
    }

    /// Returns the composed byte array mutably.
    pub fn get_byte_array_mut(&mut self) -> &mut Vec<u8> {
        &mut self.byte_array
    }

    // --- Private ---

    fn write_bits(&mut self, value: u32, bit_count: u32) {
        let mut value_pos = 0u32;
        let mut remaining = bit_count as i32;

        while remaining > 0 {
            let free_bits = self.get_free_buffer_bits();
            let copy_size = remaining.min(free_bits);

            let mask = (1u32 << copy_size) - 1;
            let src_shift = (bit_count as i32 - value_pos as i32) - copy_size;
            let dst_shift = (self.buffer_size - self.buffer_pos) - copy_size;

            self.buffer |= (((value >> src_shift) & mask) << dst_shift) as i32;

            value_pos += copy_size as u32;
            self.buffer_pos += copy_size;
            remaining -= copy_size;
        }
    }

    fn get_free_buffer_bits(&mut self) -> i32 {
        if self.buffer_pos == self.buffer_size {
            self.flush();
        }
        self.buffer_size - self.buffer_pos
    }
}

impl Default for VpxBitStreamWriter {
    fn default() -> Self {
        Self::new()
    }
}

// --------------------------------------------------------------------------
// VP9 Decoder
// --------------------------------------------------------------------------

/// VP9 video decoder.
///
/// Port of `Tegra::Decoders::VP9`.
pub struct Vp9 {
    pub state: DecoderState,
    frame_scratch: Vec<u8>,

    loop_filter_ref_deltas: [i8; 4],
    loop_filter_mode_deltas: [i8; 2],

    next_frame: Vp9FrameContainer,
    frame_ctxs: [Vp9EntropyProbs; 4],
    swap_ref_indices: bool,

    last_segmentation: Segmentation,
    current_picture_info: PictureInfo,
    current_frame_info: Vp9PictureInfo,
    prev_frame_probs: Vp9EntropyProbs,
}

impl Vp9 {
    pub fn new(
        id: i32,
        memory_manager: Arc<parking_lot::Mutex<MemoryManager>>,
        frame_queue: Arc<FrameQueue>,
    ) -> Self {
        let mut state = DecoderState::new(id, memory_manager, frame_queue);
        state.codec = VideoCodec::VP9;
        state.initialized = state.decode_api.initialize(VideoCodec::VP9);
        Self {
            state,
            frame_scratch: Vec::new(),
            loop_filter_ref_deltas: [0; 4],
            loop_filter_mode_deltas: [0; 2],
            next_frame: Vp9FrameContainer::default(),
            frame_ctxs: std::array::from_fn(|_| Vp9EntropyProbs::default()),
            swap_ref_indices: false,
            last_segmentation: Segmentation::default(),
            current_picture_info: PictureInfo::default(),
            current_frame_info: Vp9PictureInfo::default(),
            prev_frame_probs: Vp9EntropyProbs::default(),
        }
    }

    /// Returns true if the most recent frame was hidden.
    fn was_frame_hidden(&self) -> bool {
        !self.current_frame_info.show_frame
    }

    fn write_probability_update_value(writer: &mut VpxRangeEncoder, new_prob: u8, old_prob: u8) {
        let update = new_prob != old_prob;
        writer.write_bool(update, DIFF_UPDATE_PROBABILITY as i32);
        if update {
            Self::write_probability_delta(writer, new_prob, old_prob);
        }
    }

    fn write_probability_update<const N: usize>(
        writer: &mut VpxRangeEncoder,
        new_prob: &[u8; N],
        old_prob: &[u8; N],
    ) {
        for (&new_prob, &old_prob) in new_prob.iter().zip(old_prob) {
            Self::write_probability_update_value(writer, new_prob, old_prob);
        }
    }

    fn write_probability_update_aligned4<const N: usize>(
        writer: &mut VpxRangeEncoder,
        new_prob: &[u8; N],
        old_prob: &[u8; N],
    ) {
        for offset in (0..N).step_by(4) {
            Self::write_probability_update_value(writer, new_prob[offset], old_prob[offset]);
            Self::write_probability_update_value(
                writer,
                new_prob[offset + 1],
                old_prob[offset + 1],
            );
            Self::write_probability_update_value(
                writer,
                new_prob[offset + 2],
                old_prob[offset + 2],
            );
        }
    }

    fn write_probability_delta(writer: &mut VpxRangeEncoder, new_prob: u8, old_prob: u8) {
        Self::encode_term_sub_exp(writer, remap_probability(new_prob as i32, old_prob as i32));
    }

    fn encode_term_sub_exp(writer: &mut VpxRangeEncoder, mut value: i32) {
        if Self::write_less_than(writer, value, 16) {
            writer.write(value, 4);
        } else if Self::write_less_than(writer, value, 32) {
            writer.write(value - 16, 4);
        } else if Self::write_less_than(writer, value, 64) {
            writer.write(value - 32, 5);
        } else {
            value -= 64;
            const SIZE: i32 = 8;
            let mask = (1 << SIZE) - 191;
            let delta = value - mask;
            if delta < 0 {
                writer.write(value, SIZE - 1);
            } else {
                writer.write(delta / 2 + mask, SIZE - 1);
                writer.write(delta & 1, 1);
            }
        }
    }

    fn write_less_than(writer: &mut VpxRangeEncoder, value: i32, test: i32) -> bool {
        let is_lt = value < test;
        writer.write_bit_half(!is_lt);
        is_lt
    }

    fn write_coef_probability_update(
        writer: &mut VpxRangeEncoder,
        tx_mode: i32,
        new_prob: &[u8; 1728],
        old_prob: &[u8; 1728],
    ) {
        const BLOCK_BYTES: usize = 2 * 2 * 6 * 6 * 3;
        for block_index in 0..4 {
            let base_index = block_index * BLOCK_BYTES;
            let update = new_prob[base_index..base_index + BLOCK_BYTES]
                != old_prob[base_index..base_index + BLOCK_BYTES];
            writer.write_bit_half(update);
            if update {
                let mut index = base_index;
                for _ in 0..2 {
                    for _ in 0..2 {
                        for k in 0..6 {
                            for l in 0..6 {
                                if k != 0 || l < 3 {
                                    for offset in 0..3 {
                                        Self::write_probability_update_value(
                                            writer,
                                            new_prob[index + offset],
                                            old_prob[index + offset],
                                        );
                                    }
                                }
                                index += 3;
                            }
                        }
                    }
                }
            }
            if block_index == tx_mode as usize {
                break;
            }
        }
    }

    fn write_mv_probability_update(writer: &mut VpxRangeEncoder, new_prob: u8, old_prob: u8) {
        let update = new_prob != old_prob;
        writer.write_bool(update, DIFF_UPDATE_PROBABILITY as i32);
        if update {
            writer.write((new_prob >> 1) as i32, 7);
        }
    }

    fn write_segmentation(&mut self, writer: &mut VpxBitStreamWriter, regs: &NvdecRegisters) {
        let enabled = self.current_picture_info.segmentation.enabled != 0;
        writer.write_bit(enabled);
        if !enabled {
            return;
        }

        let update_map = self.current_picture_info.segmentation.update_map != 0;
        writer.write_bit(update_map);
        if update_map {
            let mut entropy_probs = EntropyProbs::default();
            let bytes = unsafe {
                std::slice::from_raw_parts_mut(
                    (&mut entropy_probs as *mut EntropyProbs).cast::<u8>(),
                    std::mem::size_of::<EntropyProbs>(),
                )
            };
            self.state
                .memory_manager
                .lock()
                .read_block(regs.vp9_prob_tab_buffer_offset().address(), bytes);
            let write_prob = |writer: &mut VpxBitStreamWriter, probability: u8| {
                let coded = probability != 255;
                writer.write_bit(coded);
                if coded {
                    writer.write_u(probability as u32, 8);
                }
            };
            for &probability in &entropy_probs.mb_segment_tree_probs {
                write_prob(writer, probability);
            }
            let temporal_update = self.current_picture_info.segmentation.temporal_update != 0;
            writer.write_bit(temporal_update);
            if temporal_update {
                for &probability in &entropy_probs.segment_pred_probs {
                    write_prob(writer, probability);
                }
            }
        }

        if self.last_segmentation == self.current_picture_info.segmentation {
            writer.write_bit(false);
            return;
        }
        self.last_segmentation = self.current_picture_info.segmentation.clone();
        writer.write_bit(true);
        writer.write_bit(self.current_picture_info.segmentation.abs_delta != 0);

        const FEATURE_BITS: [u32; 4] = [8, 6, 2, 0];
        for segment in 0..8 {
            for feature in 0..4 {
                let feature_enabled =
                    self.current_picture_info.segmentation.feature_enabled[segment][feature] != 0;
                writer.write_bit(feature_enabled);
                if !feature_enabled || feature == 3 {
                    continue;
                }
                let value = self.current_picture_info.segmentation.feature_data[segment][feature];
                if feature < 2 {
                    writer.write_s(value as i32, FEATURE_BITS[feature]);
                } else {
                    writer.write_u(value as u32, FEATURE_BITS[feature]);
                }
            }
        }
    }

    fn get_vp9_picture_info(&mut self, regs: &NvdecRegisters) -> Vp9PictureInfo {
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(
                (&mut self.current_picture_info as *mut PictureInfo).cast::<u8>(),
                std::mem::size_of::<PictureInfo>(),
            )
        };
        self.state
            .memory_manager
            .lock()
            .read_block(regs.picture_info_offset().address(), bytes);
        let mut info = self.current_picture_info.convert();
        self.insert_entropy(
            regs.vp9_prob_tab_buffer_offset().address(),
            &mut info.entropy,
        );
        for (index, offset) in info.frame_offsets.iter_mut().enumerate() {
            *offset = regs.surface_luma_offset(index).address();
        }
        info
    }

    fn insert_entropy(&self, offset: u64, dst: &mut Vp9EntropyProbs) {
        let mut entropy = EntropyProbs::default();
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(
                (&mut entropy as *mut EntropyProbs).cast::<u8>(),
                std::mem::size_of::<EntropyProbs>(),
            )
        };
        self.state.memory_manager.lock().read_block(offset, bytes);
        entropy.convert(dst);
    }

    fn get_current_frame(&mut self, regs: &NvdecRegisters) -> Vp9FrameContainer {
        let info = self.get_vp9_picture_info(regs);
        let mut current_frame = Vp9FrameContainer {
            bit_stream: vec![0; info.bitstream_size as usize],
            info,
        };
        self.state.memory_manager.lock().read_block(
            regs.frame_bitstream_offset().address(),
            &mut current_frame.bit_stream,
        );

        if !self.next_frame.bit_stream.is_empty() {
            self.next_frame.info.show_frame = current_frame.info.last_frame_shown;
            std::mem::swap(&mut current_frame, &mut self.next_frame);
        } else {
            self.next_frame = current_frame.clone();
        }
        current_frame
    }

    fn compose_compressed_header(&mut self) -> Vec<u8> {
        let mut writer = VpxRangeEncoder::new();
        let update_probs =
            !self.current_frame_info.is_key_frame && self.current_frame_info.show_frame;

        if !self.current_frame_info.lossless {
            if self.current_frame_info.transform_mode >= 3 {
                writer.write(3, 2);
                writer.write_bit_half(self.current_frame_info.transform_mode == 4);
            } else {
                writer.write(self.current_frame_info.transform_mode, 2);
            }
        }

        if self.current_frame_info.transform_mode == 4 {
            Self::write_probability_update(
                &mut writer,
                &self.current_frame_info.entropy.tx_8x8_prob,
                &self.prev_frame_probs.tx_8x8_prob,
            );
            Self::write_probability_update(
                &mut writer,
                &self.current_frame_info.entropy.tx_16x16_prob,
                &self.prev_frame_probs.tx_16x16_prob,
            );
            Self::write_probability_update(
                &mut writer,
                &self.current_frame_info.entropy.tx_32x32_prob,
                &self.prev_frame_probs.tx_32x32_prob,
            );
            if update_probs {
                self.prev_frame_probs.tx_8x8_prob = self.current_frame_info.entropy.tx_8x8_prob;
                self.prev_frame_probs.tx_16x16_prob = self.current_frame_info.entropy.tx_16x16_prob;
                self.prev_frame_probs.tx_32x32_prob = self.current_frame_info.entropy.tx_32x32_prob;
            }
        }

        Self::write_coef_probability_update(
            &mut writer,
            self.current_frame_info.transform_mode,
            &self.current_frame_info.entropy.coef_probs,
            &self.prev_frame_probs.coef_probs,
        );
        Self::write_probability_update(
            &mut writer,
            &self.current_frame_info.entropy.skip_probs,
            &self.prev_frame_probs.skip_probs,
        );
        if update_probs {
            self.prev_frame_probs.coef_probs = self.current_frame_info.entropy.coef_probs;
            self.prev_frame_probs.skip_probs = self.current_frame_info.entropy.skip_probs;
        }

        if !self.current_frame_info.intra_only {
            Self::write_probability_update_aligned4(
                &mut writer,
                &self.current_frame_info.entropy.inter_mode_prob,
                &self.prev_frame_probs.inter_mode_prob,
            );
            if self.current_frame_info.interp_filter == 4 {
                Self::write_probability_update(
                    &mut writer,
                    &self.current_frame_info.entropy.switchable_interp_prob,
                    &self.prev_frame_probs.switchable_interp_prob,
                );
                if update_probs {
                    self.prev_frame_probs.switchable_interp_prob =
                        self.current_frame_info.entropy.switchable_interp_prob;
                }
            }
            Self::write_probability_update(
                &mut writer,
                &self.current_frame_info.entropy.intra_inter_prob,
                &self.prev_frame_probs.intra_inter_prob,
            );

            let sign_bias = &self.current_frame_info.ref_frame_sign_bias;
            if (sign_bias[1] & 1) != (sign_bias[2] & 1) || (sign_bias[1] & 1) != (sign_bias[3] & 1)
            {
                if self.current_frame_info.reference_mode >= 1 {
                    writer.write(1, 1);
                    writer.write_bit_half(self.current_frame_info.reference_mode == 2);
                } else {
                    writer.write(0, 1);
                }
            }

            if self.current_frame_info.reference_mode == 2 {
                Self::write_probability_update(
                    &mut writer,
                    &self.current_frame_info.entropy.comp_inter_prob,
                    &self.prev_frame_probs.comp_inter_prob,
                );
                if update_probs {
                    self.prev_frame_probs.comp_inter_prob =
                        self.current_frame_info.entropy.comp_inter_prob;
                }
            }
            if self.current_frame_info.reference_mode != 1 {
                Self::write_probability_update(
                    &mut writer,
                    &self.current_frame_info.entropy.single_ref_prob,
                    &self.prev_frame_probs.single_ref_prob,
                );
                if update_probs {
                    self.prev_frame_probs.single_ref_prob =
                        self.current_frame_info.entropy.single_ref_prob;
                }
            }
            if self.current_frame_info.reference_mode != 0 {
                Self::write_probability_update(
                    &mut writer,
                    &self.current_frame_info.entropy.comp_ref_prob,
                    &self.prev_frame_probs.comp_ref_prob,
                );
                if update_probs {
                    self.prev_frame_probs.comp_ref_prob =
                        self.current_frame_info.entropy.comp_ref_prob;
                }
            }

            Self::write_probability_update(
                &mut writer,
                &self.current_frame_info.entropy.y_mode_prob,
                &self.prev_frame_probs.y_mode_prob,
            );
            Self::write_probability_update_aligned4(
                &mut writer,
                &self.current_frame_info.entropy.partition_prob,
                &self.prev_frame_probs.partition_prob,
            );
            for index in 0..3 {
                Self::write_mv_probability_update(
                    &mut writer,
                    self.current_frame_info.entropy.joints[index],
                    self.prev_frame_probs.joints[index],
                );
            }
            if update_probs {
                self.prev_frame_probs.inter_mode_prob =
                    self.current_frame_info.entropy.inter_mode_prob;
                self.prev_frame_probs.intra_inter_prob =
                    self.current_frame_info.entropy.intra_inter_prob;
                self.prev_frame_probs.y_mode_prob = self.current_frame_info.entropy.y_mode_prob;
                self.prev_frame_probs.partition_prob =
                    self.current_frame_info.entropy.partition_prob;
                self.prev_frame_probs.joints = self.current_frame_info.entropy.joints;
            }

            for i in 0..2 {
                Self::write_mv_probability_update(
                    &mut writer,
                    self.current_frame_info.entropy.sign[i],
                    self.prev_frame_probs.sign[i],
                );
                for j in 0..10 {
                    let index = i * 10 + j;
                    Self::write_mv_probability_update(
                        &mut writer,
                        self.current_frame_info.entropy.classes[index],
                        self.prev_frame_probs.classes[index],
                    );
                }
                Self::write_mv_probability_update(
                    &mut writer,
                    self.current_frame_info.entropy.class_0[i],
                    self.prev_frame_probs.class_0[i],
                );
                for j in 0..10 {
                    let index = i * 10 + j;
                    Self::write_mv_probability_update(
                        &mut writer,
                        self.current_frame_info.entropy.prob_bits[index],
                        self.prev_frame_probs.prob_bits[index],
                    );
                }
            }

            for i in 0..2 {
                for j in 0..2 {
                    for k in 0..3 {
                        let index = i * 6 + j * 3 + k;
                        Self::write_mv_probability_update(
                            &mut writer,
                            self.current_frame_info.entropy.class_0_fr[index],
                            self.prev_frame_probs.class_0_fr[index],
                        );
                    }
                }
                for j in 0..3 {
                    let index = i * 3 + j;
                    Self::write_mv_probability_update(
                        &mut writer,
                        self.current_frame_info.entropy.fr[index],
                        self.prev_frame_probs.fr[index],
                    );
                }
            }

            if self.current_frame_info.allow_high_precision_mv {
                for index in 0..2 {
                    Self::write_mv_probability_update(
                        &mut writer,
                        self.current_frame_info.entropy.class_0_hp[index],
                        self.prev_frame_probs.class_0_hp[index],
                    );
                    Self::write_mv_probability_update(
                        &mut writer,
                        self.current_frame_info.entropy.high_precision[index],
                        self.prev_frame_probs.high_precision[index],
                    );
                }
            }

            if update_probs {
                self.prev_frame_probs.sign = self.current_frame_info.entropy.sign;
                self.prev_frame_probs.classes = self.current_frame_info.entropy.classes;
                self.prev_frame_probs.class_0 = self.current_frame_info.entropy.class_0;
                self.prev_frame_probs.prob_bits = self.current_frame_info.entropy.prob_bits;
                self.prev_frame_probs.class_0_fr = self.current_frame_info.entropy.class_0_fr;
                self.prev_frame_probs.fr = self.current_frame_info.entropy.fr;
                self.prev_frame_probs.class_0_hp = self.current_frame_info.entropy.class_0_hp;
                self.prev_frame_probs.high_precision =
                    self.current_frame_info.entropy.high_precision;
            }
        }

        writer.end();
        writer.get_buffer().clone()
    }

    fn compose_uncompressed_header(&mut self, regs: &NvdecRegisters) -> VpxBitStreamWriter {
        let mut writer = VpxBitStreamWriter::new();
        writer.write_u(2, 2);
        writer.write_u(0, 2);
        writer.write_bit(false);
        writer.write_bit(!self.current_frame_info.is_key_frame);
        writer.write_bit(self.current_frame_info.show_frame);
        writer.write_bit(self.current_frame_info.error_resilient_mode);

        if self.current_frame_info.is_key_frame {
            writer.write_u(FRAME_SYNC_CODE, 24);
            writer.write_u(0, 3);
            writer.write_u(0, 1);
            writer.write_u(self.current_frame_info.frame_size.width as u32 - 1, 16);
            writer.write_u(self.current_frame_info.frame_size.height as u32 - 1, 16);
            writer.write_bit(false);

            self.prev_frame_probs = DEFAULT_PROBS.clone();
            self.swap_ref_indices = false;
            self.loop_filter_ref_deltas.fill(0);
            self.loop_filter_mode_deltas.fill(0);
            self.frame_ctxs = std::array::from_fn(|_| DEFAULT_PROBS.clone());
            self.current_frame_info.intra_only = true;
        } else {
            if !self.current_frame_info.show_frame {
                writer.write_bit(self.current_frame_info.intra_only);
            } else {
                self.current_frame_info.intra_only = false;
            }
            if !self.current_frame_info.error_resilient_mode {
                writer.write_u(0, 2);
            }

            let current_offsets = self.current_frame_info.frame_offsets;
            let next_offsets = self.next_frame.info.frame_offsets;
            let ref_frames_different = current_offsets[1] != current_offsets[2];
            let next_references_swap =
                next_offsets[1] == current_offsets[2] || next_offsets[2] == current_offsets[1];
            let needs_ref_swap = ref_frames_different && next_references_swap;
            if needs_ref_swap {
                self.swap_ref_indices = !self.swap_ref_indices;
            }

            let mut refresh_frame_flags = 0u32;
            for index in 0..3 {
                if current_offsets[3] == next_offsets[index] {
                    refresh_frame_flags |= 1 << index;
                }
            }
            if self.swap_ref_indices {
                let golden = (refresh_frame_flags >> 1) & 1;
                let alt = (refresh_frame_flags >> 2) & 1;
                refresh_frame_flags &= !0b110;
                refresh_frame_flags |= alt << 1;
                refresh_frame_flags |= golden << 2;
            }

            if self.current_frame_info.intra_only {
                writer.write_u(FRAME_SYNC_CODE, 24);
                writer.write_u(refresh_frame_flags, 8);
                writer.write_u(self.current_frame_info.frame_size.width as u32 - 1, 16);
                writer.write_u(self.current_frame_info.frame_size.height as u32 - 1, 16);
                writer.write_bit(false);
            } else {
                let swap_indices = needs_ref_swap ^ self.swap_ref_indices;
                let ref_frame_index = if swap_indices { [0, 2, 1] } else { [0, 1, 2] };
                writer.write_u(refresh_frame_flags, 8);
                for index in 1..4 {
                    writer.write_u(ref_frame_index[index - 1], 3);
                    writer.write_u(self.current_frame_info.ref_frame_sign_bias[index] as u32, 1);
                }
                writer.write_bit(true);
                writer.write_bit(false);
                writer.write_bit(self.current_frame_info.allow_high_precision_mv);
                writer.write_bit(self.current_frame_info.interp_filter == 4);
                if self.current_frame_info.interp_filter != 4 {
                    writer.write_u(self.current_frame_info.interp_filter as u32, 2);
                }
            }
        }

        if !self.current_frame_info.error_resilient_mode {
            writer.write_bit(true);
            writer.write_bit(true);
        }

        let frame_ctx_idx = usize::from(!self.current_frame_info.show_frame);
        writer.write_u(frame_ctx_idx as u32, 2);
        self.prev_frame_probs = self.frame_ctxs[frame_ctx_idx].clone();
        self.frame_ctxs[frame_ctx_idx] = self.current_frame_info.entropy.clone();

        writer.write_u(self.current_frame_info.first_level as u32, 6);
        writer.write_u(self.current_frame_info.sharpness_level as u32, 3);
        writer.write_bit(self.current_frame_info.mode_ref_delta_enabled);
        if self.current_frame_info.mode_ref_delta_enabled {
            let update_ref = std::array::from_fn::<_, 4, _>(|index| {
                self.loop_filter_ref_deltas[index] != self.current_frame_info.ref_deltas[index]
            });
            let update_mode = std::array::from_fn::<_, 2, _>(|index| {
                self.loop_filter_mode_deltas[index] != self.current_frame_info.mode_deltas[index]
            });
            let delta_update = update_ref.iter().chain(&update_mode).any(|&update| update);
            writer.write_bit(delta_update);
            if delta_update {
                for (index, &update) in update_ref.iter().enumerate() {
                    writer.write_bit(update);
                    if update {
                        writer.write_s(self.current_frame_info.ref_deltas[index] as i32, 6);
                    }
                }
                for (index, &update) in update_mode.iter().enumerate() {
                    writer.write_bit(update);
                    if update {
                        writer.write_s(self.current_frame_info.mode_deltas[index] as i32, 6);
                    }
                }
                self.loop_filter_ref_deltas = self.current_frame_info.ref_deltas;
                self.loop_filter_mode_deltas = self.current_frame_info.mode_deltas;
            }
        }

        writer.write_u(self.current_frame_info.base_q_index as u32, 8);
        writer.write_delta_q(self.current_frame_info.y_dc_delta_q as u32);
        writer.write_delta_q(self.current_frame_info.uv_dc_delta_q as u32);
        writer.write_delta_q(self.current_frame_info.uv_ac_delta_q as u32);
        self.write_segmentation(&mut writer, regs);

        let frame_width = self.current_frame_info.frame_size.width as i32;
        let min_tile_cols_log2 = calc_min_log2_tile_cols(frame_width);
        let max_tile_cols_log2 = calc_max_log2_tile_cols(frame_width);
        let tile_cols_log2_diff = self.current_frame_info.log2_tile_cols - min_tile_cols_log2;
        let tile_cols_log2_inc_mask = (1 << tile_cols_log2_diff) - 1;
        if self.current_frame_info.log2_tile_cols < max_tile_cols_log2 {
            writer.write_u(
                (tile_cols_log2_inc_mask << 1) as u32,
                (tile_cols_log2_diff + 1) as u32,
            );
        } else {
            writer.write_u(tile_cols_log2_inc_mask as u32, tile_cols_log2_diff as u32);
        }

        let tile_rows_nonzero = self.current_frame_info.log2_tile_rows != 0;
        writer.write_bit(tile_rows_nonzero);
        if tile_rows_nonzero {
            writer.write_bit(self.current_frame_info.log2_tile_rows > 1);
        }
        writer
    }
}

impl DecoderImpl for Vp9 {
    fn compose_frame(&mut self, regs: &NvdecRegisters) -> Vec<u8> {
        self.state.vp9_hidden_frame = false;
        let current_frame = self.get_current_frame(regs);
        self.current_frame_info = current_frame.info;
        self.state.set_frame_dimensions(
            self.current_frame_info.frame_size.width as i32,
            self.current_frame_info.frame_size.height as i32,
        );

        let mut uncompressed = self.compose_uncompressed_header(regs);
        let compressed = self.compose_compressed_header();
        uncompressed.write_u(compressed.len() as u32, 16);
        uncompressed.flush();

        self.frame_scratch.clear();
        self.frame_scratch.reserve(
            uncompressed.get_byte_array().len() + compressed.len() + current_frame.bit_stream.len(),
        );
        self.frame_scratch
            .extend_from_slice(uncompressed.get_byte_array());
        self.frame_scratch.extend_from_slice(&compressed);
        self.frame_scratch
            .extend_from_slice(&current_frame.bit_stream);
        self.state.vp9_hidden_frame = self.was_frame_hidden();
        self.frame_scratch.clone()
    }

    fn get_progressive_offsets(&self, regs: &NvdecRegisters) -> (u64, u64) {
        let current = Vp9SurfaceIndex::Current as usize;
        (
            regs.surface_luma_offset(current).address(),
            regs.surface_chroma_offset(current).address(),
        )
    }

    fn get_interlaced_offsets(&self, regs: &NvdecRegisters) -> (u64, u64, u64, u64) {
        let current = Vp9SurfaceIndex::Current as usize;
        let luma = regs.surface_luma_offset(current).address();
        let chroma = regs.surface_chroma_offset(current).address();
        (luma, luma, chroma, chroma)
    }

    fn is_interlaced(&self) -> bool {
        false
    }

    fn get_current_codec_name(&self) -> &str {
        "VP9"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_encoder_matches_upstream_reference_bytes() {
        let mut encoder = VpxRangeEncoder::new();
        encoder.write(3, 2);
        encoder.write_bool(true, 252);
        encoder.write(17, 5);
        encoder.write_bool(false, 128);
        encoder.write(0x55, 7);
        encoder.end();

        assert_eq!(encoder.get_buffer(), &[0x7f, 0xc5, 0x54, 0x00]);
    }

    #[test]
    fn bitstream_writer_matches_upstream_signed_and_delta_encoding() {
        let mut writer = VpxBitStreamWriter::new();
        writer.write_u(2, 2);
        writer.write_s(-3, 6);
        writer.write_delta_q(0);
        writer.write_delta_q(5);
        writer.write_bit(true);
        writer.flush();

        assert_eq!(writer.get_byte_array(), &[0x83, 0xab]);
    }

    #[test]
    fn probability_defaults_and_remapping_match_upstream_boundaries() {
        assert_eq!(DEFAULT_PROBS.y_mode_prob[0], 65);
        assert_eq!(DEFAULT_PROBS.coef_probs[0], 195);
        assert_eq!(DEFAULT_PROBS.coef_probs[1727], 6);
        assert_eq!(DEFAULT_PROBS.high_precision, [128, 128]);
        assert_eq!(remap_probability(1, 1), 20);
        assert_eq!(remap_probability(255, 1), 19);
    }
}

// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/transform_feedback.h and video_core/transform_feedback.cpp
//!
//! Transform feedback state and varying generation.

use crate::engines::maxwell_3d::{StreamOutLayout, NUM_TRANSFORM_FEEDBACK_BUFFERS};
use shader_recompiler::runtime_info::TransformFeedbackVarying;

fn assert_fail_soft(condition: bool, message: impl FnOnce() -> String) {
    if condition {
        return;
    }
    let message = message();
    log::error!("{message}");
    if *common::settings::values().use_debug_asserts.get_value() {
        panic!("{message}");
    }
}

/// Layout for a single transform feedback buffer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct TransformFeedbackLayout {
    pub stream: u32,
    pub varying_count: u32,
    pub stride: u32,
}

/// Complete transform feedback state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct TransformFeedbackState {
    pub layouts: [TransformFeedbackLayout; NUM_TRANSFORM_FEEDBACK_BUFFERS],
    pub varyings: [[StreamOutLayout; 32]; NUM_TRANSFORM_FEEDBACK_BUFFERS],
}

impl Default for TransformFeedbackState {
    fn default() -> Self {
        Self {
            layouts: [TransformFeedbackLayout::default(); NUM_TRANSFORM_FEEDBACK_BUFFERS],
            varyings: [[StreamOutLayout::default(); 32]; NUM_TRANSFORM_FEEDBACK_BUFFERS],
        }
    }
}

/// Vector attribute base offsets used for transform feedback varying mapping.
const VECTORS: [u32; 45] = [
    28,  // gl_Position
    32,  // Generic 0
    36,  // Generic 1
    40,  // Generic 2
    44,  // Generic 3
    48,  // Generic 4
    52,  // Generic 5
    56,  // Generic 6
    60,  // Generic 7
    64,  // Generic 8
    68,  // Generic 9
    72,  // Generic 10
    76,  // Generic 11
    80,  // Generic 12
    84,  // Generic 13
    88,  // Generic 14
    92,  // Generic 15
    96,  // Generic 16
    100, // Generic 17
    104, // Generic 18
    108, // Generic 19
    112, // Generic 20
    116, // Generic 21
    120, // Generic 22
    124, // Generic 23
    128, // Generic 24
    132, // Generic 25
    136, // Generic 26
    140, // Generic 27
    144, // Generic 28
    148, // Generic 29
    152, // Generic 30
    156, // Generic 31
    160, // gl_FrontColor
    164, // gl_FrontSecondaryColor
    160, // gl_BackColor
    164, // gl_BackSecondaryColor
    192, // gl_TexCoord[0]
    196, // gl_TexCoord[1]
    200, // gl_TexCoord[2]
    204, // gl_TexCoord[3]
    208, // gl_TexCoord[4]
    212, // gl_TexCoord[5]
    216, // gl_TexCoord[6]
    220, // gl_TexCoord[7]
];

/// Generate transform feedback varyings from the given state.
///
/// Returns the varying array and the count of used entries.
pub fn make_transform_feedback_varyings(
    state: &TransformFeedbackState,
) -> ([TransformFeedbackVarying; 256], u32) {
    let mut xfb = [TransformFeedbackVarying::default(); 256];
    let mut count = 0u32;

    for buffer in 0..state.layouts.len() {
        let locations = &state.varyings[buffer];
        let layout = &state.layouts[buffer];
        let varying_count = layout.varying_count;
        let mut highest = 0u32;
        let mut offset = 0u32;

        while offset < varying_count {
            let get_attribute = |index: u32| -> u32 {
                let loc = &locations[(index / 4) as usize];
                match index % 4 {
                    0 => loc.attribute0(),
                    1 => loc.attribute1(),
                    2 => loc.attribute2(),
                    3 => loc.attribute3(),
                    _ => unreachable!(),
                }
            };

            let mut varying = TransformFeedbackVarying {
                buffer: buffer as u32,
                stream: layout.stream,
                stride: layout.stride,
                offset: offset.wrapping_mul(4),
                components: 1,
            };

            let base_offset = offset;
            let attribute = get_attribute(offset);

            // Check if this attribute is aligned to a 4-component vector
            let aligned_attr = attribute & !3;
            if VECTORS.contains(&aligned_attr) {
                assert_fail_soft(attribute % 4 == 0, || format!("Unaligned TFB {attribute}"));
                let base_index = attribute / 4;
                while offset.wrapping_add(1) < varying_count
                    && base_index == get_attribute(offset.wrapping_add(1)) / 4
                {
                    offset = offset.wrapping_add(1);
                    varying.components = varying.components.wrapping_add(1);
                }
            }

            if (attribute as usize) < xfb.len() {
                xfb[attribute as usize] = varying;
                count = count.max(attribute);
            }
            highest = highest.max(base_offset.wrapping_add(varying.components).wrapping_mul(4));
            offset = offset.wrapping_add(1);
        }

        assert_fail_soft(highest == layout.stride, || {
            format!(
                "Transform feedback highest {highest} != stride {}",
                layout.stride
            )
        });
    }

    (xfb, count.wrapping_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varying_preserves_layout_stream() {
        let mut state = TransformFeedbackState::default();
        state.layouts[0] = TransformFeedbackLayout {
            stream: 3,
            varying_count: 1,
            stride: 4,
        };
        state.varyings[0][0] = StreamOutLayout::from_raw(32);

        let (varyings, count): ([TransformFeedbackVarying; 256], u32) =
            make_transform_feedback_varyings(&state);

        assert_eq!(count, 33);
        assert_eq!(varyings[32].stream, 3);
    }

    #[test]
    fn last_fixed_function_texture_vector_is_grouped() {
        let mut state = TransformFeedbackState::default();
        state.layouts[0] = TransformFeedbackLayout {
            stream: 0,
            varying_count: 2,
            stride: 8,
        };
        state.varyings[0][0] = StreamOutLayout::from_raw(220 | (221 << 8));

        let (varyings, count) = make_transform_feedback_varyings(&state);

        assert_eq!(count, 221);
        assert_eq!(varyings[220].components, 2);
    }

    #[test]
    fn state_layout_matches_eden_header() {
        assert_eq!(NUM_TRANSFORM_FEEDBACK_BUFFERS, 4);
        assert_eq!(std::mem::size_of::<TransformFeedbackLayout>(), 12);
        assert_eq!(std::mem::align_of::<TransformFeedbackLayout>(), 4);
        assert_eq!(std::mem::offset_of!(TransformFeedbackLayout, stream), 0);
        assert_eq!(
            std::mem::offset_of!(TransformFeedbackLayout, varying_count),
            4
        );
        assert_eq!(std::mem::offset_of!(TransformFeedbackLayout, stride), 8);
        assert_eq!(std::mem::size_of::<TransformFeedbackState>(), 560);
        assert_eq!(std::mem::align_of::<TransformFeedbackState>(), 4);
    }
}

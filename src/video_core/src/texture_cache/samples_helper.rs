// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/texture_cache/samples_helper.h
//!
//! Helpers for converting between MSAA sample counts and log2 representations,
//! and for mapping MsaaMode enumeration values to concrete sample counts.

use crate::textures::texture::MsaaMode;

// ── MsaaMode ───────────────────────────────────────────────────────────

// ── Public helpers ─────────────────────────────────────────────────────

/// Returns (log2_x, log2_y) for a given sample count.
///
/// Port of `SamplesLog2` from samples_helper.h.
pub fn samples_log2(num_samples: i32) -> (i32, i32) {
    match num_samples {
        1 => (0, 0),
        2 => (1, 0),
        4 => (1, 1),
        8 => (2, 1),
        16 => (2, 2),
        _ => {
            debug_assert!(false, "Invalid number of samples={}", num_samples);
            (0, 0)
        }
    }
}

/// Returns the total number of samples for a given MSAA mode.
///
/// Port of `NumSamples` from samples_helper.h.
pub fn num_samples(msaa_mode: MsaaMode) -> i32 {
    match msaa_mode {
        MsaaMode::Msaa1x1 => 1,
        MsaaMode::Msaa2x1 | MsaaMode::Msaa2x1D3d => 2,
        MsaaMode::Msaa2x2 | MsaaMode::Msaa2x2Vc4 | MsaaMode::Msaa2x2Vc12 => 4,
        MsaaMode::Msaa4x2 | MsaaMode::Msaa4x2D3d | MsaaMode::Msaa4x2Vc8 | MsaaMode::Msaa4x2Vc24 => {
            8
        }
        MsaaMode::Msaa4x4 => 16,
    }
}

/// Returns the horizontal sample count for a given MSAA mode.
///
/// Port of `NumSamplesX` from samples_helper.h.
pub fn num_samples_x(msaa_mode: MsaaMode) -> i32 {
    match msaa_mode {
        MsaaMode::Msaa1x1 => 1,
        MsaaMode::Msaa2x1
        | MsaaMode::Msaa2x1D3d
        | MsaaMode::Msaa2x2
        | MsaaMode::Msaa2x2Vc4
        | MsaaMode::Msaa2x2Vc12 => 2,
        MsaaMode::Msaa4x2
        | MsaaMode::Msaa4x2D3d
        | MsaaMode::Msaa4x2Vc8
        | MsaaMode::Msaa4x2Vc24
        | MsaaMode::Msaa4x4 => 4,
    }
}

/// Returns the vertical sample count for a given MSAA mode.
///
/// Port of `NumSamplesY` from samples_helper.h.
pub fn num_samples_y(msaa_mode: MsaaMode) -> i32 {
    match msaa_mode {
        MsaaMode::Msaa1x1 | MsaaMode::Msaa2x1 | MsaaMode::Msaa2x1D3d => 1,
        MsaaMode::Msaa2x2
        | MsaaMode::Msaa2x2Vc4
        | MsaaMode::Msaa2x2Vc12
        | MsaaMode::Msaa4x2
        | MsaaMode::Msaa4x2D3d
        | MsaaMode::Msaa4x2Vc8
        | MsaaMode::Msaa4x2Vc24 => 2,
        MsaaMode::Msaa4x4 => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_samples_log2() {
        assert_eq!(samples_log2(1), (0, 0));
        assert_eq!(samples_log2(2), (1, 0));
        assert_eq!(samples_log2(4), (1, 1));
        assert_eq!(samples_log2(8), (2, 1));
        assert_eq!(samples_log2(16), (2, 2));
    }

    #[test]
    fn every_msaa_mode_uses_the_upstream_sample_dimensions() {
        let cases = [
            (MsaaMode::Msaa1x1, 1, 1, 1),
            (MsaaMode::Msaa2x1, 2, 2, 1),
            (MsaaMode::Msaa2x1D3d, 2, 2, 1),
            (MsaaMode::Msaa2x2, 4, 2, 2),
            (MsaaMode::Msaa2x2Vc4, 4, 2, 2),
            (MsaaMode::Msaa2x2Vc12, 4, 2, 2),
            (MsaaMode::Msaa4x2, 8, 4, 2),
            (MsaaMode::Msaa4x2D3d, 8, 4, 2),
            (MsaaMode::Msaa4x2Vc8, 8, 4, 2),
            (MsaaMode::Msaa4x2Vc24, 8, 4, 2),
            (MsaaMode::Msaa4x4, 16, 4, 4),
        ];
        for (mode, total, x, y) in cases {
            assert_eq!(num_samples(mode), total);
            assert_eq!(num_samples_x(mode), x);
            assert_eq!(num_samples_y(mode), y);
        }
    }
}

// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `frontend/maxwell/decode.h` and `frontend/maxwell/decode.cpp`
//!
//! Maxwell instruction decoder. Maps raw 64-bit instruction words to
//! opcode enum values. The existing `maxwell_opcodes.rs` already contains
//! the decoder logic; this module provides the upstream-matching function
//! signature.

use super::maxwell_opcodes::{decode_opcode, MaxwellOpcode};

/// Decode a Maxwell instruction into its opcode.
///
/// Like Eden's release `Decode`, an unrecognized word continues as NOP.
///
/// Eden's `ASSERT_MSG` is compiled out of release builds. Logging every word
/// here is observably different: a malformed/unbounded shader can contain
/// hundreds of thousands of zero words and spend most of its compilation time
/// formatting the same diagnostic.
pub fn decode(insn: u64) -> MaxwellOpcode {
    decode_opcode(insn).unwrap_or(MaxwellOpcode::NOP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_instruction_uses_upstream_soft_assert_fallback() {
        assert_eq!(decode(0), MaxwellOpcode::NOP);
    }
}

// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of header-only `frontend/ampere/program_override.h`.
use super::lower_to_maxwell::{Instruction, LoweredProgram};

pub fn native_program_key(source: &[Instruction]) -> u64 {
    let mut hash = 14695981039346656037u64;
    for instruction in source {
        for word in instruction.words {
            for byte in 0..4 {
                hash = (hash ^ u64::from((word >> (byte * 8)) & 255)).wrapping_mul(1099511628211);
            }
        }
    }
    hash
}

pub fn encode_program_override(source: &[Instruction], program: &LoweredProgram) -> Vec<u8> {
    let source_bytes = std::mem::size_of_val(source);
    if !program.is_valid()
        || program.source_size as usize != source_bytes
        || program.source_instruction_offsets.len() != source.len()
    {
        return Vec::new();
    }
    let header = [
        0x4e454445,
        0x3230564f,
        source_bytes as u32,
        program.instructions.len() as u32,
        program.required_texture_buffer,
        program.source_instruction_offsets.len() as u32,
    ];
    let mut bytes = Vec::with_capacity(
        24 + source_bytes
            + program.instructions.len() * 8
            + program.source_instruction_offsets.len() * 4,
    );
    for word in header {
        bytes.extend(word.to_le_bytes());
    }
    for instruction in source {
        for word in instruction.words {
            bytes.extend(word.to_le_bytes());
        }
    }
    for word in &program.instructions {
        bytes.extend(word.to_le_bytes());
    }
    for offset in &program.source_instruction_offsets {
        bytes.extend(offset.to_le_bytes());
    }
    bytes
}

pub fn program_override_source_size(bytes: &[u8]) -> u32 {
    if bytes.len() < 24 {
        return 0;
    }
    let header: [u32; 6] =
        std::array::from_fn(|i| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap()));
    let mapped = header[1] == 0x3230564f;
    if header[0] != 0x4e454445
        || (!mapped && header[1] != 0x3130564f)
        || (if mapped {
            header[5] != header[2] / 16
        } else {
            header[5] != 0
        })
        || header[2] == 0
        || header[2] % 16 != 0
        || header[2] > 1048576
        || header[3] == 0
        || header[3] > 1048576
        || (header[4] != u32::MAX && header[4] >= 18)
    {
        return 0;
    }
    if bytes.len() != 24 + header[2] as usize + header[3] as usize * 8 + header[5] as usize * 4 {
        return 0;
    }
    header[2]
}

pub fn decode_program_override(bytes: &[u8], source: &[Instruction]) -> LoweredProgram {
    let mut result = LoweredProgram::default();
    // Corresponds to source's local fail lambda; failed parses discard both
    // output vectors and preserve the default source-size/texture fields.
    let fail = |mut result: LoweredProgram, message| {
        result.instructions.clear();
        result.source_instruction_offsets.clear();
        result.error = message;
        result
    };
    if program_override_source_size(bytes) == 0 {
        return fail(result, "invalid native override header or length");
    }
    let header: [u32; 6] =
        std::array::from_fn(|i| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap()));
    if header[2] as usize != std::mem::size_of_val(source) {
        return fail(result, "native override source length mismatch");
    }
    let mut cursor = 24;
    for instruction in source {
        for word in instruction.words {
            if bytes[cursor..cursor + 4] != word.to_le_bytes() {
                return fail(result, "native override source mismatch");
            }
            cursor += 4;
        }
    }
    for _ in 0..header[3] {
        result.instructions.push(u64::from_le_bytes(
            bytes[cursor..cursor + 8].try_into().unwrap(),
        ));
        cursor += 8;
    }
    if header[5] != 0 {
        for _ in 0..header[5] {
            result.source_instruction_offsets.push(u32::from_le_bytes(
                bytes[cursor..cursor + 4].try_into().unwrap(),
            ));
            cursor += 4;
        }
        let mut previous = 0;
        for &offset in &result.source_instruction_offsets {
            if offset as usize >= result.instructions.len() || offset < previous {
                return fail(result, "native override address map mismatch");
            }
            previous = offset;
        }
    }
    result.source_size = header[2];
    result.required_texture_buffer = header[4];
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Vec<Instruction>, LoweredProgram) {
        (
            vec![
                Instruction {
                    words: [0x03020100, 0x07060504, 0x0b0a0908, 0x0f0e0d0c]
                };
                2
            ],
            LoweredProgram {
                instructions: vec![0x1122334455667788, 0x99aabbccddeeff00],
                source_instruction_offsets: vec![0, 1],
                source_size: 32,
                required_texture_buffer: 17,
                ..Default::default()
            },
        )
    }
    #[test]
    fn override_bytes_and_legacy_version_decode_match_layout() {
        let (source, program) = fixture();
        let bytes = encode_program_override(&source, &program);
        assert_eq!(&bytes[..8], b"EDENOV02");
        assert_eq!(&bytes[24..40], &(0u8..16).collect::<Vec<_>>());
        assert_eq!(
            &bytes[56..64],
            &[0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]
        );
        assert_eq!(bytes.len(), 80);
        let decoded = decode_program_override(&bytes, &source);
        assert!(decoded.is_valid());
        assert_eq!(decoded.instructions, program.instructions);
        assert_eq!(decoded.source_instruction_offsets, [0, 1]);
        assert_eq!(
            (decoded.source_size, decoded.required_texture_buffer),
            (32, 17)
        );
        let mut legacy = bytes[..72].to_vec();
        legacy[4..8].copy_from_slice(&0x3130564fu32.to_le_bytes());
        legacy[20..24].copy_from_slice(&0u32.to_le_bytes());
        let decoded = decode_program_override(&legacy, &source);
        assert!(decoded.is_valid());
        assert!(decoded.source_instruction_offsets.is_empty());
        assert_eq!(decoded.instructions, program.instructions);
    }
    #[test]
    fn overrides_validate_header_source_identity_and_monotonic_offsets() {
        let (source, mut program) = fixture();
        let bytes = encode_program_override(&source, &program);
        for (word, value) in [
            (0, 0),
            (1, 0),
            (2, 0),
            (2, 17),
            (2, 1048592),
            (3, 0),
            (3, 1048577),
            (4, 18),
            (5, 1),
        ] {
            let mut invalid = bytes.clone();
            invalid[word * 4..word * 4 + 4].copy_from_slice(&(value as u32).to_le_bytes());
            assert_eq!(program_override_source_size(&invalid), 0);
        }
        for size in [0, 23, bytes.len() - 1] {
            assert_eq!(program_override_source_size(&bytes[..size]), 0);
        }
        assert_eq!(
            decode_program_override(&bytes, &source[..1]).error,
            "native override source length mismatch"
        );
        let mut other = source.clone();
        other[1].words[3] ^= 1;
        assert_eq!(
            decode_program_override(&bytes, &other).error,
            "native override source mismatch"
        );
        for offsets in [[1, 0], [0, 2]] {
            program.source_instruction_offsets = offsets.to_vec();
            let decoded =
                decode_program_override(&encode_program_override(&source, &program), &source);
            assert_eq!(decoded.error, "native override address map mismatch");
            assert!(
                decoded.instructions.is_empty() && decoded.source_instruction_offsets.is_empty()
            );
            assert_eq!(
                (decoded.source_size, decoded.required_texture_buffer),
                (0, u32::MAX)
            );
        }
        program.source_instruction_offsets = vec![1, 1];
        assert!(
            decode_program_override(&encode_program_override(&source, &program), &source)
                .is_valid()
        );
        program.error = "failed lowering";
        assert!(encode_program_override(&source, &program).is_empty());
        assert_eq!(native_program_key(&[]), 14695981039346656037);
        // FNV-1a reference vector: the byte sequence 00..0f.
        assert_eq!(native_program_key(&source[..1]), 0x7c84dc9477851775);
    }
}

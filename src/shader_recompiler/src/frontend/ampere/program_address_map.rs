// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of header-only `frontend/ampere/program_address_map.h`.

pub const NATIVE_ADDRESS_MAP_MAGIC: u64 = 0x50414d4e4544454e;

pub fn has_native_address_map(code: &[u64]) -> bool {
    code.len() >= 2 && code.last() == Some(&NATIVE_ADDRESS_MAP_MAGIC)
}

pub fn append_native_address_map(code: &mut Vec<u64>, offsets: &[u32]) {
    if offsets.is_empty() {
        return;
    }
    for &index in offsets {
        // Source expression is evaluated as u32 before promotion to u64.
        code.push(u64::from(
            0x50u32
                .wrapping_add(index.wrapping_mul(8))
                .wrapping_add((index.wrapping_add(1) / 3).wrapping_mul(8)),
        ));
    }
    code.push(offsets.len() as u64);
    code.push(NATIVE_ADDRESS_MAP_MAGIC);
}

pub fn native_program_address(code: &[u64], address: u32) -> Option<u32> {
    if !has_native_address_map(code) || address % 16 != 0 {
        return None;
    }
    let count = code[code.len() - 2];
    if count == 0
        || count > 65536
        || count + 2 > code.len() as u64
        || u64::from(address / 16) >= count
    {
        return None;
    }
    let start = code.len() - count as usize - 2;
    let target = code[start + (address / 16) as usize];
    if target < 0x50 || target >= (start * 8) as u64 || target % 8 != 0 || target % 32 == 0 {
        return None;
    }
    Some(target as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn map_skips_control_words_and_preserves_duplicate_instruction_offsets() {
        let mut code = vec![0; 32];
        append_native_address_map(&mut code, &[0, 1, 2, 2, 3, 4, 5]);
        assert_eq!(&code[32..39], &[0x50, 0x58, 0x68, 0x68, 0x70, 0x78, 0x88]);
        for (index, target) in [0x50, 0x58, 0x68, 0x68, 0x70, 0x78, 0x88]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                native_program_address(&code, index as u32 * 16),
                Some(target)
            );
        }
        assert_eq!(native_program_address(&code, 1), None);
        assert_eq!(native_program_address(&code, 7 * 16), None);
        let before = code.clone();
        append_native_address_map(&mut code, &[]);
        assert_eq!(code, before);
        append_native_address_map(&mut code, &[u32::MAX]);
        assert_eq!(code[before.len()], 0x48);
    }
    #[test]
    fn malformed_maps_reject_invalid_counts_targets_and_control_words() {
        assert!(!has_native_address_map(&[NATIVE_ADDRESS_MAP_MAGIC]));
        for count in [0, 65537, u64::MAX, 30] {
            let mut code = vec![0; 20];
            code.extend([count, NATIVE_ADDRESS_MAP_MAGIC]);
            assert!(has_native_address_map(&code));
            assert_eq!(native_program_address(&code, 0), None);
        }
        for target in [0, 0x48, 0x51, 0x60, 0xa0, u64::MAX] {
            let mut code = vec![0; 20];
            code.extend([target, 1, NATIVE_ADDRESS_MAP_MAGIC]);
            assert_eq!(native_program_address(&code, 0), None, "target {target:#x}");
        }
        let mut code = vec![0; 20];
        code.extend([0x98, 1, NATIVE_ADDRESS_MAP_MAGIC]);
        assert_eq!(native_program_address(&code, 0), Some(0x98));
    }
}

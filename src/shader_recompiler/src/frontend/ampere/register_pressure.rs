// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of Fritchii frontend/ampere/register_pressure.h.

use super::instruction::Instruction;

#[derive(Debug)]
pub struct RegisterPressure {
    pub instruction: Instruction,
    pub prefix: Vec<u64>,
    pub suffix: Vec<u64>,
    pub valid: bool,
}

pub fn native_register_copy(destination_bank: u32, destination: u32, source_bank: u32, source: u32) -> u64 {
    0x5c98000000070000 | (10u64 << 44) | u64::from(destination)
        | (u64::from(destination_bank) << 8) | (u64::from(source_bank) << 10)
        | (u64::from(source) << 20)
}

/// Preserve high guest registers in SSA while borrowing aligned low quartets
/// for one native instruction. The expanded instruction owns its temporary bank.
pub fn prepare_register_pressure(input: &Instruction, reserved: u32) -> RegisterPressure {
    let mut result = RegisterPressure {
        instruction: *input, prefix: Vec::new(), suffix: Vec::new(), valid: true,
    };
    if reserved < 4 || reserved > 246 || reserved & 3 != 0 {
        result.valid = false;
        return result;
    }
    let mut operands: Vec<(u32, u32)> = Vec::new();
    let mut operand = |bit, count| operands.push((bit, count));
    let opcode = input.bits(0, 12);
    match opcode {
        0xa02 | 0xc3e | 0x802 | 0xb08 | 0xd08 | 0x908 | 0x919 => operand(16, 1),
        0xa0c | 0x804 | 0x80b | 0x80c => operand(24, 1),
        0x20c | 0x20b => { operand(24, 1); operand(32, 1); }
        0x807 | 0x808 | 0x923 | 0x820 | 0xa20 | 0x421 => {
            operand(16, 1); operand(24, 1);
        }
        0x207 | 0x208 | 0x220 | 0x221 | 0x209 => {
            operand(16, 1); operand(24, 1); operand(32, 1);
        }
        0x805 => operand(16, 2),
        0xc17 | 0x817 => { operand(16, 1); operand(24, 1); }
        0xa24 | 0xa23 | 0x623 | 0x423 => {
            operand(16, 1); operand(24, 1); operand(64, 1);
        }
        0x621 => { operand(16, 1); operand(24, 1); }
        0x321 => { operand(16, input.bits(74, 2) + 1); operand(24, 1); operand(32, 1); }
        0x322 => { operand(24, 1); operand(32, input.bits(74, 2) + 1); }
        0x927 => { operand(24, 1); operand(32, 1); }
        0x224 | 0x223 => { operand(16, 1); operand(24, 1); operand(32, 1); operand(64, 1); }
        0x81a => { operand(16, 1); operand(24, 1); }
        0x308 | 0x202 | 0x243 | 0x245 | 0x305 | 0x306 | 0x307 => { operand(16, 1); operand(32, 1); }
        0xb05 | 0xd05 => operand(16, 1),
        0x59f => { operand(24, 2); operand(32, 1); }
        0x98e => {
            operand(24, if input.bits(72, 1) != 0 { 2 } else { 1 });
            operand(32, if matches!(input.bits(73, 3), 2 | 5) { 2 } else { 1 });
        }
        0x433 | 0x633 => { operand(16, 1); operand(24, 1); }
        0x233 => { operand(16, 1); operand(24, 1); operand(32, 1); }
        0x810 | 0xa10 | 0xc10 | 0x812 | 0x824 | 0x823 | 0x831 | 0x424 | 0x624 => {
            operand(16, 1); operand(24, 1); operand(64, 1);
        }
        0x230 | 0x231 => { operand(16, 1); operand(24, 1); operand(32, 1); operand(64, 1); }
        0x419 | 0x819 => { operand(16, 1); operand(24, 1); operand(64, 1); }
        0x210 | 0x211 | 0x212 => { operand(16, 1); operand(24, 1); operand(32, 1); operand(64, 1); }
        0xa11 => { operand(16, 1); operand(24, 1); operand(64, 1); }
        0x225 | 0x227 | 0x625 | 0x825 => {
            operand(16, if opcode == 0x227 { 1 } else { 2 });
            operand(24, 1);
            if matches!(opcode, 0x225 | 0x227) { operand(32, 1); operand(64, 2); }
            if opcode == 0x625 { operand(64, 1); }
            if opcode == 0x825 { operand(64, 2); }
        }
        0x83b => { operand(16, 1 << input.bits(72, 2)); operand(24, 1); }
        0x23c => {
            operand(16, 2);
            operand(24, if input.bits(75, 1) != 0 { 4 } else { 2 });
            operand(32, if input.bits(75, 1) != 0 { 2 } else { 1 });
            operand(64, 2);
        }
        0x388 | 0x984 | 0x988 => {
            let count = match input.bits(73, 3) { 6 => 4, 5 => 2, _ => 1 };
            operand(if opcode == 0x984 { 16 } else { 32 }, count);
            operand(24, 1);
        }
        0x387 | 0x983 | 0x987 => {
            let count = match input.bits(73, 3) { 6 => 4, 5 => 2, _ => 1 };
            operand(if opcode == 0x983 { 16 } else { 32 }, count);
            operand(24, 1);
        }
        0x381 | 0x386 | 0x981 | 0x986 => {
            let count = match input.bits(73, 3) { 6 => 4, 5 => 2, _ => 1 };
            operand(if matches!(opcode, 0x386 | 0x986) { 32 } else { 16 }, count);
            operand(24, if matches!(opcode, 0x381 | 0x386) || input.bits(90, 1) != 0 { 2 } else { 1 });
        }
        _ => { result.valid = false; return result; }
    }
    let mut used = [false; 256];
    let mut groups = [false; 64];
    for &(bit, count) in &operands {
        let reg = input.bits(bit, 8);
        if reg == 255 { continue; }
        if reg + count > 255 || (reg < reserved && reg + count > reserved)
            || (reg >= reserved && reg / 4 != (reg + count - 1) / 4)
        {
            result.valid = false;
            return result;
        }
        for component in 0..count {
            used[(reg + component) as usize] = true;
            if reg + component >= reserved { groups[((reg + component) / 4) as usize] = true; }
        }
    }
    let mut mapped = [0u32; 64];
    for group in 0..groups.len() {
        if !groups[group] { continue; }
        let mut borrow = reserved as usize;
        while borrow >= 4 {
            borrow -= 4;
            if !used[borrow..borrow + 4].iter().any(|&value| value) { break; }
        }
        if used[borrow..borrow + 4].iter().any(|&value| value) {
            result.valid = false;
            return result;
        }
        mapped[group] = borrow as u32;
        for component in 0..4 {
            let low = (borrow + component) as u32;
            let high = (group * 4 + component) as u32;
            used[low as usize] = true;
            result.prefix.push(native_register_copy(2, low, 0, low));
            result.prefix.push(native_register_copy(0, low, 1, high));
            result.suffix.push(native_register_copy(1, high, 0, low));
            result.suffix.push(native_register_copy(0, low, 2, low));
        }
    }
    for &(bit, _) in &operands {
        let reg = input.bits(bit, 8);
        if reg == 255 || !groups[(reg / 4) as usize] { continue; }
        let replacement = mapped[(reg / 4) as usize] + reg % 4;
        let word = (bit / 32) as usize;
        let shift = bit % 32;
        result.instruction.words[word] = (result.instruction.words[word] & !(255 << shift)) | (replacement << shift);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_pressure_matches_verbatim_upstream_cpp() {
        let mut count = 0;
        for (line_number, line) in include_str!("register_pressure.fixture").lines().enumerate() {
            if line.starts_with('#') || line.is_empty() { continue; }
            let fields: Vec<_> = line.split('|').collect();
            let input: Vec<u32> = fields[0].split_whitespace().map(|x| x.parse().unwrap()).collect();
            let expected: Vec<u32> = fields[1].split_whitespace().map(|x| x.parse().unwrap()).collect();
            let instruction = Instruction { words: input[1..].try_into().unwrap() };
            let actual = prepare_register_pressure(&instruction, input[0]);
            assert_eq!(actual.valid, expected[0] != 0, "valid at line {line_number}");
            assert_eq!(actual.instruction.words, expected[1..], "instruction at line {line_number}");
            for (kind, words, text) in [("prefix", actual.prefix, fields[2]), ("suffix", actual.suffix, fields[3])] {
                let expected: Vec<u64> = text.split_whitespace().map(|x| u64::from_str_radix(x, 16).unwrap()).collect();
                assert_eq!(words, expected, "{kind} at line {line_number}");
            }
            count += 1;
        }
        assert_eq!(count, 6266);
    }
}

// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Lichii shader_recompiler/frontend/ampere/trap_guards.h (b3f983a6b8, 4c6ef2b810).

use std::fmt;

use super::instruction::Instruction;

/// Port of `Shader::Ampere::TrapGuardError` (a `Shader::RuntimeError` upstream). It is raised
/// per dispatch and, unlike other shader errors, never recorded as a rejected program: the
/// constants can make the guard provable on a later dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrapGuardError;

impl fmt::Display for TrapGuardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Native compute trap is reachable or its uniform guard is unknown")
    }
}

impl std::error::Error for TrapGuardError {}

/// Port of `Shader::Ampere::VerifyTrapGuards`: evaluate only a bounded, uniform prologue.
/// Unknown inputs or reachable traps reject the dispatch. `read` is upstream's
/// `TrapGuardReader` (bank, byte offset) -> constant; `None` is upstream's empty reader.
pub fn verify_trap_guards(
    prefix: &[Instruction],
    read: Option<&dyn Fn(u32, u32) -> Option<u32>>,
) -> bool {
    if prefix.is_empty() {
        return true;
    }
    let Some(read) = read else {
        return false;
    };
    if prefix.len() > 256 {
        return false;
    }
    let mut regs: [Option<u32>; 256] = [None; 256];
    let mut predicates: [Option<bool>; 8] = [None; 8];
    regs[255] = Some(0);
    predicates[7] = Some(true);
    let pred = |predicates: &[Option<bool>; 8], code: u32| -> Option<bool> {
        predicates[(code & 7) as usize].map(|value| value != ((code & 8) != 0))
    };
    let mut pc = 0usize;
    let mut steps = 0u32;
    while pc < prefix.len() {
        if steps >= 256 {
            return false;
        }
        steps += 1;
        let insn = &prefix[pc];
        let op = insn.bits(0, 12);
        let dest = insn.bits(16, 8);
        let Some(execute) = pred(&predicates, insn.bits(12, 4)) else {
            return false;
        };
        if !execute {
            pc += 1;
            continue;
        }
        // Bit 63 negates a 0x624 constant buffer addend; other forms keep rejecting it.
        let negated_cbuf = op == 0x624 && insn.bits(63, 1) != 0;
        let cbuf = || -> Option<u32> {
            if insn.bits(59, 4) != 0
                || (insn.bits(63, 1) != 0 && !negated_cbuf)
                || insn.bits(38, 16) % 4 != 0
            {
                return None;
            }
            read(insn.bits(54, 5), insn.bits(38, 16))
        };
        let operand = |regs: &[Option<u32>; 256], which: u32| -> Option<u32> {
            if which == 0 {
                return regs[insn.bits(24, 8) as usize];
            }
            match insn.bits(9, 3) {
                1 => regs[insn.bits(32, 8) as usize],
                4 => Some(insn.words[1]),
                5 => cbuf(),
                _ => None,
            }
        };
        let value: Option<u32>;
        if op == 0x95c {
            return false;
        }
        if op == 0x947 {
            if (insn.words[0] & 0xffff_0000) != 0 || insn.words[2] != 0x0380_0000 || insn.words[1] % 16 != 0 {
                return false;
            }
            let target = pc as i64 + 1 + i64::from(insn.words[1] as i32 / 16);
            if target <= pc as i64 {
                return false;
            }
            if target >= prefix.len() as i64 {
                return true;
            }
            pc = target as usize;
            continue;
        }
        if op == 0x388 {
            // STS does not write registers used by the uniform guard.
            if insn.words[2] != 0 {
                return false;
            }
            pc += 1;
            continue;
        }
        if op == 0x624 || op == 0x424 || op == 0x224 {
            if insn.bits(24, 8) != 255 || insn.bits(64, 8) != 255 || insn.words[2] != 0x078e_00ff {
                return false;
            }
            value = if op == 0x624 {
                let constant = cbuf();
                if negated_cbuf { constant.map(|c| 0u32.wrapping_sub(c)) } else { constant }
            } else if op == 0x424 {
                Some(insn.words[1])
            } else {
                regs[insn.bits(32, 8) as usize]
            };
        } else if op == 0x207 || op == 0x807 {
            if insn.words[2] & !0x0780_0000 != 0 {
                return false;
            }
            let Some(select) = pred(&predicates, insn.bits(87, 4)) else {
                return false;
            };
            value = if select { operand(&regs, 0) } else { operand(&regs, 1) };
        } else if op == 0x212 || op == 0x812 || op == 0xa12 {
            if (insn.words[2] & !0xffff) != 0x078e_0000
                || (op != 0x812 && insn.bits(62, 2) != 0)
                || (op == 0x212 && (insn.words[1] & !0xff) != 0)
            {
                return false;
            }
            let (Some(a), Some(b), Some(c)) =
                (operand(&regs, 0), operand(&regs, 1), regs[insn.bits(64, 8) as usize])
            else {
                return false;
            };
            let lut = insn.bits(72, 8);
            let mut result = 0u32;
            for bit in 0..32 {
                let index = (((a >> bit) & 1) << 2) | (((b >> bit) & 1) << 1) | ((c >> bit) & 1);
                result |= ((lut >> index) & 1) << bit;
            }
            value = Some(result);
        } else if op == 0x210 || op == 0x810 || op == 0xa10 {
            if insn.words[2] != 0x07ff_e0ff || (op != 0x810 && insn.bits(62, 2) != 0) {
                return false;
            }
            let (Some(a), Some(b)) = (operand(&regs, 0), operand(&regs, 1)) else {
                return false;
            };
            value = Some(a.wrapping_add(b));
        } else if op == 0x20c || op == 0x80c || op == 0xa0c {
            if (insn.words[2] & !0x078e_7e00) != 0x0070_0070
                || insn.bits(74, 2) > 2
                || (op != 0x80c && insn.bits(62, 2) != 0)
                || (op == 0x20c && (insn.words[1] & !0xff) != 0)
            {
                return false;
            }
            let (Some(a), Some(b), Some(condition)) =
                (operand(&regs, 0), operand(&regs, 1), pred(&predicates, insn.bits(87, 4)))
            else {
                return false;
            };
            let signed_compare = insn.bits(73, 1) != 0;
            let less = if signed_compare { (a as i32) < (b as i32) } else { a < b };
            let equal = a == b;
            let comparisons =
                [false, less, equal, less || equal, !less && !equal, !equal, !less, true];
            let comparison = comparisons[insn.bits(76, 3) as usize];
            let combine = insn.bits(74, 2);
            let target = insn.bits(81, 3);
            if target != 7 {
                predicates[target as usize] = Some(match combine {
                    0 => comparison && condition,
                    1 => comparison || condition,
                    _ => comparison != condition,
                });
            }
            pc += 1;
            continue;
        } else {
            return false;
        }
        let Some(value) = value else {
            return false;
        };
        if dest != 255 {
            regs[dest as usize] = Some(value);
        }
        pc += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    // Port of Lichii src/tests/video_core/ampere_trap_guards.cpp.
    use super::*;
    use crate::frontend::ampere::lower_to_maxwell::{lower_to_maxwell_with_options, LoweringOptions};
    use std::collections::HashMap;

    const fn native(w: [u32; 4]) -> Instruction {
        Instruction { words: w }
    }
    const EXIT: Instruction = native([0x794d, 0, 0x0380_0000, 0]);

    fn lower(code: &[Instruction], traps: &[u32]) -> bool {
        lower_to_maxwell_with_options(code, LoweringOptions { tensor: false, verified_traps: traps })
            .is_valid()
    }

    #[test]
    fn uniform_native_trap_guards_reject_changed_constants_and_unknown_inputs() {
        let input = [
            native([0xff03_7624, 0, 0x078e_00ff, 0]),
            native([0x0300_720c, 255, 0x03f0_6270, 0]),
            native([0x0947, 16, 0x0380_0000, 0]),
            native([0x795c, 4, 0x0030_0000, 0]),
            EXIT,
        ];
        let constant = std::cell::Cell::new(0u32);
        let read = |bank: u32, offset: u32| -> Option<u32> {
            assert_eq!((bank, offset), (0, 0));
            Some(constant.get())
        };
        let prefix = &input[..4];
        for value in [0u32, 1, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff] {
            constant.set(value);
            assert_eq!(verify_trap_guards(prefix, Some(&read)), value < 0x8000_0000);
        }
        assert!(!verify_trap_guards(prefix, None));
        assert!(!verify_trap_guards(prefix, Some(&|_, _| None)));
        constant.set(0);
        let mut unknown = input;
        unknown[0] = native([0xff03_7224, 255, 0x078e_0004, 0]);
        assert!(!verify_trap_guards(&unknown[..4], Some(&read)));
        unknown = input;
        unknown[0] = native([0x0003_7919, 0, 0x2100, 0]);
        assert!(!verify_trap_guards(&unknown[..4], Some(&read)));
        unknown = input;
        unknown[2].words[1] = 0xffff_fff0;
        assert!(!verify_trap_guards(&unknown[..4], Some(&read)));
        assert!(!lower(&input, &[]));
        assert!(lower(&input, &[48]));
        let overwritten = [input[0], native([0x0003_7984, 0, 0, 0]), input[1], input[2], input[3]];
        assert!(!verify_trap_guards(&overwritten, Some(&read)));
    }

    #[test]
    fn byte_fill_validates_both_rectangle_bounds_on_every_dispatch() {
        let code = [
            native([0xff01_7624, 0x0a00, 0x078e_00ff, 0]),
            native([0xff00_7a0c, 0x5f00, 0x03f0_5270, 0]),
            native([0xff03_7624, 0x5b00, 0x078e_00ff, 0]),
            native([0xff00_7807, 1, 0x0400_0000, 0]),
            native([0xff00_7a0c, 0x5f00, 0x03f0_5270, 0]),
            native([0x0302_7a12, 0x5a00, 0x078e_fcff, 0]),
            native([0xff00_7388, 0, 0, 0]),
            native([0x0200_780c, 0xffff_ffff, 0x0470_4670, 0]),
            native([0x0947, 0x10, 0x0380_0000, 0]),
            native([0x795c, 4, 0x0030_0000, 0]),
            native([0x0300_7a10, 0x5d00, 0x07ff_e0ff, 0]),
            native([0xff02_7624, 0x5c00, 0x078e_00ff, 0]),
            native([0x7a0c, 0x5900, 0x03f0_4270, 0]),
            native([0x0200_7a10, 0x5a00, 0x07ff_e0ff, 0]),
            native([0x7a0c, 0x5800, 0x0470_3270, 0]),
            native([0xff00_7a0c, 0x5f00, 0x0070_2670, 0]),
            native([0x0947, 0x10, 0x0380_0000, 0]),
            native([0x795c, 4, 0x0030_0000, 0]),
            EXIT,
        ];
        let constants = std::cell::RefCell::new(HashMap::from([
            (0x28u32, 0u32), (0x160, 1280), (0x164, 720), (0x168, 0), (0x16c, 0),
            (0x170, 1280), (0x174, 720), (0x17c, 1),
        ]));
        let read = |bank: u32, offset: u32| -> Option<u32> {
            if bank != 0 {
                return None;
            }
            constants.borrow().get(&offset).copied()
        };
        let prefix = &code[..18];
        assert!(verify_trap_guards(prefix, Some(&read)));
        for offset in [0x168u32, 0x16c] {
            constants.borrow_mut().insert(offset, 1); // Rectangle exceeds its buffer by one pixel.
            assert!(!verify_trap_guards(prefix, Some(&read)));
            constants.borrow_mut().insert(offset, 0xffff_ffff); // Negative origin reaches the first trap.
            assert!(!verify_trap_guards(prefix, Some(&read)));
            constants.borrow_mut().insert(offset, 0);
            assert!(verify_trap_guards(prefix, Some(&read)));
        }
        constants.borrow_mut().remove(&0x174);
        assert!(!verify_trap_guards(prefix, Some(&read)));
        constants.borrow_mut().insert(0x174, 720);
        assert!(verify_trap_guards(prefix, Some(&read)));
        assert!(!lower(&code, &[]));
        let lowered = lower_to_maxwell_with_options(&code, LoweringOptions { tensor: false, verified_traps: &[0x90, 0x110] });
        assert!(lowered.is_valid(), "{}", lowered.error);
        assert!(!lower(&code, &[0x90]));
        let mut invalid = code;
        invalid[17].words[1] = 8;
        assert!(!lower(&invalid, &[0x90, 0x110]));
    }
}

// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of header-only `frontend/ampere/program_capture.h`.
use super::lower_to_maxwell::Instruction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Result {
    Continue,
    Complete,
    InvalidTarget,
}

#[derive(Default)]
pub struct ProgramCaptureState {
    last_call_target: u32,
    last_forward_target: u32,
    main_exited: bool,
    computed_branch: bool,
}

impl ProgramCaptureState {
    pub const MAXIMUM_INSTRUCTIONS: u32 = 65536;

    pub fn observe(&mut self, instruction: &Instruction, index: u32) -> Result {
        let opcode = instruction.bits(0, 12);
        if opcode == 0x944 || opcode == 0x947 || opcode == 0x945 {
            let raw =
                u64::from(instruction.words[1]) | (u64::from(instruction.words[2] & 0x3ffff) << 32);
            let relative = if opcode == 0x945 {
                i64::from(instruction.words[1] as i32)
            } else {
                ((raw << 14) as i64) >> 14
            };
            let target = i64::from(index) * 16 + 16 + relative;
            if target < 0
                || target % 16 != 0
                || target >= i64::from(Self::MAXIMUM_INSTRUCTIONS) * 16
            {
                return Result::InvalidTarget;
            }
            let noinc_call = opcode == 0x944 && instruction.words[2] & 0x400000 != 0;
            if opcode == 0x944 && !noinc_call {
                self.last_call_target = self.last_call_target.max((target / 16) as u32);
            } else {
                self.last_forward_target = self.last_forward_target.max((target / 16) as u32);
            }
            if opcode == 0x947
                && self.computed_branch
                && instruction.bits(12, 4) == 7
                && target == i64::from(index) * 16
            {
                return Result::Complete;
            }
        }
        if opcode == 0x949 || opcode == 0x958 {
            self.computed_branch = true;
        }
        if self.computed_branch {
            return Result::Continue;
        }
        if opcode == 0x94d && instruction.bits(12, 4) == 7 && index >= self.last_forward_target {
            if self.last_call_target == 0 {
                return Result::Complete;
            }
            self.main_exited = true;
        }
        if opcode == 0x950
            && instruction.bits(12, 4) == 7
            && self.main_exited
            && index >= self.last_call_target
            && index >= self.last_forward_target
        {
            return Result::Complete;
        }
        Result::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn op(opcode: u32, predicate: u32, relative: i64) -> Instruction {
        let bits = relative as u64 & ((1u64 << 50) - 1);
        Instruction {
            words: [
                opcode | (predicate << 12),
                bits as u32,
                (bits >> 32) as u32,
                0,
            ],
        }
    }
    #[test]
    fn calls_require_main_exit_and_last_return_but_noinc_is_a_jump() {
        let mut capture = ProgramCaptureState::default();
        assert_eq!(capture.observe(&op(0x944, 7, 48), 0), Result::Continue); // target4
        assert_eq!(capture.observe(&op(0x950, 7, 0), 1), Result::Continue);
        assert_eq!(capture.observe(&op(0x94d, 7, 0), 2), Result::Continue);
        assert_eq!(capture.observe(&op(0x950, 7, 0), 3), Result::Continue);
        assert_eq!(capture.observe(&op(0x950, 0, 0), 4), Result::Continue);
        assert_eq!(capture.observe(&op(0x950, 7, 0), 5), Result::Complete);
        let mut noinc = op(0x944, 7, 48);
        noinc.words[2] |= 0x400000;
        let mut capture = ProgramCaptureState::default();
        assert_eq!(capture.observe(&noinc, 0), Result::Continue);
        assert_eq!(capture.observe(&op(0x94d, 7, 0), 2), Result::Continue);
        assert_eq!(capture.observe(&op(0x94d, 7, 0), 4), Result::Complete);
    }
    #[test]
    fn computed_branches_require_unconditional_self_branch() {
        for opcode in [0x949, 0x958] {
            let mut capture = ProgramCaptureState::default();
            assert_eq!(capture.observe(&op(opcode, 7, 0), 0), Result::Continue);
            assert_eq!(capture.observe(&op(0x94d, 7, 0), 1), Result::Continue);
            assert_eq!(capture.observe(&op(0x950, 7, 0), 2), Result::Continue);
            assert_eq!(capture.observe(&op(0x947, 0, -16), 3), Result::Continue);
            assert_eq!(capture.observe(&op(0x947, 7, -16), 4), Result::Complete);
        }
    }
    #[test]
    fn relative_targets_validate_signed_width_alignment_and_capture_limit() {
        for opcode in [0x944, 0x947, 0x945] {
            for relative in [-32, 1, 65536 * 16 - 16] {
                assert_eq!(
                    ProgramCaptureState::default().observe(&op(opcode, 7, relative), 0),
                    Result::InvalidTarget
                );
            }
            assert_eq!(
                ProgramCaptureState::default().observe(&op(opcode, 7, -16), 0),
                Result::Continue
            );
            assert_eq!(
                ProgramCaptureState::default().observe(&op(opcode, 7, 65536 * 16 - 32), 0),
                Result::Continue
            );
        }
        let mut short = op(0x945, 7, -16);
        short.words[2] = 0;
        assert_eq!(
            ProgramCaptureState::default().observe(&short, 0),
            Result::Continue
        );
        short.words[0] = 0x947 | (7 << 12);
        assert_eq!(
            ProgramCaptureState::default().observe(&short, 0),
            Result::InvalidTarget
        );
    }
}

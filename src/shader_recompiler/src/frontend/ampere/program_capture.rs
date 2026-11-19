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

pub struct ProgramCaptureState {
    exit_setup: [Instruction; 5],
    last_call_target: u32,
    last_forward_target: u32,
    main_exited: bool,
    computed_branch: bool,
}

impl Default for ProgramCaptureState {
    fn default() -> Self {
        Self {
            exit_setup: [Instruction { words: [0; 4] }; 5],
            last_call_target: 0,
            last_forward_target: 0,
            main_exited: false,
            computed_branch: false,
        }
    }
}

impl ProgramCaptureState {
    pub const MAXIMUM_INSTRUCTIONS: u32 = 65536;

    pub fn observe(&mut self, instruction: &Instruction, index: u32) -> Result {
        let opcode = instruction.bits(0, 12);
        if (index as usize) < self.exit_setup.len() {
            self.exit_setup[index as usize] = *instruction;
            let setup = &self.exit_setup;
            if index as usize + 1 == setup.len()
                && setup.iter().all(|insn| insn.words[3] & 0xc00001ff == 0)
                && setup[0].words[0] == 0x0000734e
                && setup[0].words[1] == 0 && setup[0].words[2] == 0
                && setup[1].words[0] == 0x00037802 && setup[1].words[2] == 0xf00
                && setup[2].words[0] == 0x00007210 && setup[2].words[1] == 3
                && setup[2].words[2] == 0x07f1e0ff
                && setup[3].words[1] == 0xff
                && ((setup[3].words[0] == 0x01017210 && setup[3].words[2] == 0x007fe4ff)
                    || (setup[3].words[0] == 0xff017224 && setup[3].words[2] == 0x000e0401))
                && instruction.words[0] == 0x00007357
                && instruction.words[1] == 0 && instruction.words[2] == 0
            {
                let target = setup[1].words[1];
                if target < setup.len() as u32 * 16 || target % 16 != 0
                    || target >= Self::MAXIMUM_INSTRUCTIONS * 16 {
                    return Result::InvalidTarget;
                }
                self.last_forward_target = self.last_forward_target.max(target / 16);
            }
        }
        if opcode == 0x944 || opcode == 0x947 || opcode == 0x945 {
            // BRA's low two offset bits encode .DIV/.CONV, not displacement.
            let low = instruction.words[1] & if opcode == 0x947 { !3 } else { u32::MAX };
            let raw =
                u64::from(low) | (u64::from(instruction.words[2] & 0x3ffff) << 32);
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
            // NOINC does not push a return address, but RET.NODEC still returns
            // through the link register prepared before the call.
            if opcode == 0x944 {
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
            if index >= self.last_call_target {
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
    fn calls_require_main_exit_and_last_return_including_noinc() {
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
        let mut capture = ProgramCaptureState::default();
        assert_eq!(capture.observe(&noinc, 0), Result::Continue);
        assert_eq!(capture.observe(&op(0x94d, 7, 0), 2), Result::Continue);
        assert_eq!(capture.observe(&op(0x950, 7, 0), 4), Result::Complete);
    }

    #[test]
    fn branch_modes_do_not_extend_or_misalign_capture_targets() {
        for mode in 0..4 {
            let mut branch = op(0x947, 7, 16);
            branch.words[1] |= mode;
            let mut capture = ProgramCaptureState::default();
            assert_eq!(capture.observe(&branch, 0), Result::Continue);
            assert_eq!(capture.observe(&op(0x94d, 7, 0), 1), Result::Continue);
            assert_eq!(capture.observe(&op(0x94d, 7, 0), 2), Result::Complete);
        }
    }

    #[test]
    fn exit_setup_keeps_forward_exit_and_validates_target() {
        for third in [[0x01017210, 0xff, 0x007fe4ff, 0], [0xff017224, 0xff, 0x000e0401, 0]] {
            for target in [0, 64, 81, 96, ProgramCaptureState::MAXIMUM_INSTRUCTIONS * 16] {
                let words = [
                    [0x734e, 0, 0, 0], [0x00037802, target, 0xf00, 0],
                    [0x7210, 3, 0x07f1e0ff, 0], third, [0x7357, 0, 0, 0],
                ];
                let mut capture = ProgramCaptureState::default();
                for (index, words) in words.into_iter().enumerate() {
                    let result = capture.observe(&Instruction { words }, index as u32);
                    assert_eq!(result, if index == 4 && target != 96 { Result::InvalidTarget } else { Result::Continue });
                }
                if target == 96 {
                    assert_eq!(capture.observe(&op(0x94d, 7, 0), 5), Result::Continue);
                    assert_eq!(capture.observe(&op(0x94d, 7, 0), 6), Result::Complete);
                }
            }
        }
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
            for relative in [-32, 4, 65536 * 16 - 16] {
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

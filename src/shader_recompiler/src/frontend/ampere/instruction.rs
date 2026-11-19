// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of Fritchii frontend/ampere/instruction.h.

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Instruction {
    pub words: [u32; 4],
}

impl Instruction {
    pub fn bits(&self, offset: u32, count: u32) -> u32 {
        let index = (offset / 32) as usize;
        let mut bits = u64::from(self.words[index]);
        if index + 1 < self.words.len() {
            bits |= u64::from(self.words[index + 1]) << 32;
        }
        ((bits >> (offset % 32)) & ((1u64 << count) - 1)) as u32
    }
}

const _: () = assert!(std::mem::size_of::<Instruction>() == 16);

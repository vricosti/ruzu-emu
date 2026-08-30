// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `frontend/maxwell/location.h`
//!
//! Instruction location (program counter) with Maxwell alignment handling.
//! Maxwell instructions are 8 bytes each with control codes every 4th slot
//! (every 32 bytes the first 8 bytes are a control code).

use std::fmt;

/// Instruction location in Maxwell shader binary.
///
/// Handles the Maxwell alignment scheme where every 32-byte group
/// starts with an 8-byte control code, so instructions are at offsets
/// 8, 16, 24 within each group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Location {
    offset: u32,
}

const VIRTUAL_BIAS: u32 = 4;

impl Location {
    /// Create a new location from a byte offset.
    ///
    /// The offset must be a multiple of 8.
    pub fn new(initial_offset: u32) -> Self {
        assert!(
            initial_offset % 8 == 0,
            "initial_offset={} is not a multiple of 8",
            initial_offset
        );
        let mut loc = Self {
            offset: initial_offset,
        };
        loc.align();
        loc
    }

    /// Create a location for an absolute branch target.
    pub fn with_offset(&self, offset: u32) -> Self {
        Self::new(offset)
    }

    /// Create a virtual location (offset by VIRTUAL_BIAS).
    pub fn virtual_loc(&self) -> Self {
        Self {
            offset: self.offset - VIRTUAL_BIAS,
        }
    }

    /// Get the byte offset.
    pub fn offset(&self) -> u32 {
        self.offset
    }

    /// Check if this is a virtual location.
    pub fn is_virtual(&self) -> bool {
        self.offset % 8 == VIRTUAL_BIAS
    }

    /// Advance to the next instruction.
    pub fn step(&mut self) {
        self.offset += 8 + if self.offset % 32 == 24 { 8 } else { 0 };
    }

    /// Return the next instruction location.
    pub fn next(mut self) -> Self {
        self.step();
        self
    }

    /// Go back to the previous instruction.
    pub fn back(&mut self) {
        self.offset -= 8 + if self.offset % 32 == 8 { 8 } else { 0 };
    }

    /// Return the previous instruction location.
    pub fn prev(mut self) -> Self {
        self.back();
        self
    }

    /// Port of upstream `Location::operator+(int)`.
    pub fn add_instructions(mut self, mut count: i32) -> Self {
        while count > 0 {
            count -= 1;
            self.step();
        }
        while count < 0 {
            count += 1;
            self.back();
        }
        self
    }

    fn align(&mut self) {
        self.offset += if self.offset % 32 == 0 { 8 } else { 0 };
    }
}

impl Default for Location {
    fn default() -> Self {
        Self { offset: 0xcccccccc }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04x}", self.offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduling_grid_is_anchored_at_absolute_offset_zero() {
        let mut location = Location::new(0x850);
        assert_eq!(location.offset(), 0x850);
        location.step();
        assert_eq!(location.offset(), 0x858);
        location.step();
        assert_eq!(location.offset(), 0x868);
    }

    #[test]
    fn constructor_skips_an_absolute_scheduling_word() {
        assert_eq!(Location::new(0x860).offset(), 0x868);
    }
}

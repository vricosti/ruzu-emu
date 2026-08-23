use crate::ir::cond::Cond;

/// If-Then execution state for Thumb mode.
///
/// The IT state is an 8-bit value:
///   Bits [7:5] = base condition code (3 bits of cond)
///   Bit  [4]   = condition inversion for first instruction
///   Bits [3:0] = mask (number of remaining instructions)
///
/// When mask is 0, we're not in an IT block.
/// The mask shifts left after each instruction, ending the block when bit 4 becomes 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ITState(pub u8);

impl ITState {
    pub fn new(value: u8) -> Self {
        Self(value)
    }

    pub fn value(self) -> u8 {
        self.0
    }

    /// Are we currently inside an IT block?
    pub fn is_in_it_block(self) -> bool {
        (self.0 & 0xF) != 0
    }

    /// Is this the last instruction in the current IT block?
    pub fn is_last_in_it_block(self) -> bool {
        (self.0 & 0xF) == 0x8
    }

    /// Get the condition code for the current instruction in the IT block.
    pub fn cond(self) -> Cond {
        if self.0 == 0 {
            return Cond::AL;
        }
        // Upper 4 bits encode the condition
        let c = (self.0 >> 4) & 0xF;
        Cond::from_u8(c)
    }

    /// Advance to the next instruction in the IT block.
    /// Shifts the mask left by 1, clearing the IT state when done.
    pub fn advance(&mut self) {
        self.0 = (self.0 & 0xE0) | ((self.0 << 1) & 0x1F);
        if (self.0 & 0xF) == 0 {
            self.0 = 0; // Exited IT block
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_it_state_empty() {
        let it = ITState::new(0);
        assert!(!it.is_in_it_block());
        assert_eq!(it.cond(), Cond::AL);
    }

    #[test]
    fn test_it_state_single_instruction() {
        // IT EQ: cond=0000 (EQ), mask=1000
        let it = ITState::new(0x08);
        assert!(it.is_in_it_block());
        assert!(it.is_last_in_it_block());
        assert_eq!(it.cond(), Cond::EQ);
    }

    #[test]
    fn test_it_state_advance() {
        // ITTE EQ: cond=0000, mask=1010 → 3 instructions
        let mut it = ITState::new(0x0A); // 0000_1010
        assert!(it.is_in_it_block());
        assert!(!it.is_last_in_it_block());

        it.advance(); // consumed 1st, mask → 0100
        assert!(it.is_in_it_block());
        assert!(!it.is_last_in_it_block());

        it.advance(); // consumed 2nd, mask → 1000 (last)
        assert!(it.is_in_it_block());
        assert!(it.is_last_in_it_block());

        it.advance(); // consumed 3rd, mask → 0000 (exit)
        assert!(!it.is_in_it_block());
    }

    #[test]
    fn test_it_state_four_instructions() {
        // ITTT AL: cond=1110 (AL), mask=1111
        let mut it = ITState::new(0xEF);
        assert!(it.is_in_it_block());
        assert_eq!(it.cond(), Cond::AL);

        it.advance();
        assert!(it.is_in_it_block());
        it.advance();
        assert!(it.is_in_it_block());
        it.advance();
        assert!(it.is_in_it_block());
        assert!(it.is_last_in_it_block());
        it.advance();
        assert!(!it.is_in_it_block());
    }
}

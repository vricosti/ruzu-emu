/// A32 coprocessor register.
///
/// Upstream owner: `interface/A32/coprocessor_util.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CoprocReg {
    C0 = 0,
    C1,
    C2,
    C3,
    C4,
    C5,
    C6,
    C7,
    C8,
    C9,
    C10,
    C11,
    C12,
    C13,
    C14,
    C15,
}

impl CoprocReg {
    pub fn from_u8(value: u8) -> Self {
        assert!(value < 16, "Invalid coprocessor register: {}", value);
        // SAFETY: CoprocReg has one contiguous repr(u8) variant for every
        // value accepted by the assertion above.
        unsafe { std::mem::transmute(value) }
    }

    pub fn number(self) -> usize {
        self as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coprocessor_register_layout_matches_upstream() {
        for value in 0..16 {
            let register = CoprocReg::from_u8(value);
            assert_eq!(register as u8, value);
            assert_eq!(register.number(), value as usize);
        }
        assert_eq!(std::mem::size_of::<CoprocReg>(), 1);
        assert_eq!(std::mem::align_of::<CoprocReg>(), 1);
    }
}

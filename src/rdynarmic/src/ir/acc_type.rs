use std::fmt;

/// Memory access type for load/store operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AccType {
    Normal = 0,
    Vec = 1,
    Stream = 2,
    VecStream = 3,
    Atomic = 4,
    Ordered = 5,
    OrderedRw = 6,
    LimitedOrdered = 7,
    Unpriv = 8,
    Ifetch = 9,
    Ptw = 10,
    Dc = 11,
    Ic = 12,
    Dczva = 13,
    At = 14,
    Swap = 15,
}

impl fmt::Display for AccType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[cfg(test)]
mod tests {
    use super::AccType;

    #[test]
    fn inventory_and_discriminants_match_upstream() {
        let access_types = [
            AccType::Normal,
            AccType::Vec,
            AccType::Stream,
            AccType::VecStream,
            AccType::Atomic,
            AccType::Ordered,
            AccType::OrderedRw,
            AccType::LimitedOrdered,
            AccType::Unpriv,
            AccType::Ifetch,
            AccType::Ptw,
            AccType::Dc,
            AccType::Ic,
            AccType::Dczva,
            AccType::At,
            AccType::Swap,
        ];

        assert_eq!(std::mem::size_of::<AccType>(), 1);
        assert_eq!(std::mem::align_of::<AccType>(), 1);
        for (expected, access_type) in access_types.into_iter().enumerate() {
            assert_eq!(access_type as usize, expected);
        }
    }
}

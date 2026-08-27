//! Port of `common/div_ceil.h`.
//!
//! Ceiled integer division utilities.

/// Ceiled integer division: returns ceil(number / divisor).
/// Both number and divisor should be unsigned.
#[inline]
pub const fn div_ceil(number: u64, divisor: u64) -> u64 {
    number.wrapping_add(divisor).wrapping_sub(1) / divisor
}

/// Ceiled division for u32.
#[inline]
pub const fn div_ceil_u32(number: u32, divisor: u32) -> u32 {
    number.wrapping_add(divisor).wrapping_sub(1) / divisor
}

/// Ceiled division for usize.
#[inline]
pub const fn div_ceil_usize(number: usize, divisor: usize) -> usize {
    number.wrapping_add(divisor).wrapping_sub(1) / divisor
}

/// Ceiled integer division with a log2 divisor.
/// Equivalent to `ceil(value / 2^alignment_log2)`.
#[inline]
pub const fn div_ceil_log2(value: u64, alignment_log2: u32) -> u64 {
    value.wrapping_add(1u64 << alignment_log2).wrapping_sub(1) >> alignment_log2
}

/// Ceiled integer division with a log2 divisor for u32.
#[inline]
pub const fn div_ceil_log2_u32(value: u32, alignment_log2: u32) -> u32 {
    value.wrapping_add(1u32 << alignment_log2).wrapping_sub(1) >> alignment_log2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_div_ceil() {
        assert_eq!(div_ceil(0, 4), 0);
        assert_eq!(div_ceil(1, 4), 1);
        assert_eq!(div_ceil(4, 4), 1);
        assert_eq!(div_ceil(5, 4), 2);
        assert_eq!(div_ceil(8, 4), 2);
        assert_eq!(div_ceil(9, 4), 3);
    }

    #[test]
    fn test_div_ceil_log2() {
        // divisor = 2^2 = 4
        assert_eq!(div_ceil_log2(0, 2), 0);
        assert_eq!(div_ceil_log2(1, 2), 1);
        assert_eq!(div_ceil_log2(4, 2), 1);
        assert_eq!(div_ceil_log2(5, 2), 2);

        // divisor = 2^12 = 4096
        assert_eq!(div_ceil_log2(0, 12), 0);
        assert_eq!(div_ceil_log2(1, 12), 1);
        assert_eq!(div_ceil_log2(4096, 12), 1);
        assert_eq!(div_ceil_log2(4097, 12), 2);
    }

    #[test]
    fn unsigned_ceil_arithmetic_wraps_like_cpp() {
        assert_eq!(div_ceil(u64::MAX, 2), 0);
        assert_eq!(div_ceil_u32(u32::MAX, 2), 0);
        assert_eq!(div_ceil_usize(usize::MAX, 2), 0);
        assert_eq!(div_ceil_log2(u64::MAX, 1), 0);
        assert_eq!(div_ceil_log2_u32(u32::MAX, 1), 0);
    }
}

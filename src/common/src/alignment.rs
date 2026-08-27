//! Port of `common/alignment.h`.

/// Align a value up to the given alignment (works for both signed and unsigned).
/// The C++ version casts to unsigned internally; here we provide separate fns.
#[inline]
pub const fn align_up(value: u64, size: u64) -> u64 {
    let remainder = value % size;
    if remainder == 0 {
        value
    } else {
        value.wrapping_sub(remainder).wrapping_add(size)
    }
}

/// Align up using signed arithmetic, matching the C++ template for signed types.
#[inline]
pub const fn align_up_signed(value: i64, size: u64) -> i64 {
    let u = value as u64;
    align_up(u, size) as i64
}

/// Align up using a log2 alignment value.
#[inline]
pub const fn align_up_log2(value: u64, align_log2: u32) -> u64 {
    value.wrapping_add((1u64 << align_log2) - 1) >> align_log2 << align_log2
}

/// Align a value down to the given alignment.
#[inline]
pub const fn align_down(value: u64, size: u64) -> u64 {
    value - value % size
}

/// Align down using signed arithmetic.
#[inline]
pub const fn align_down_signed(value: i64, size: u64) -> i64 {
    let u = value as u64;
    align_down(u, size) as i64
}

/// Check if a value is word-aligned (4-byte aligned).
#[inline]
pub const fn is_word_aligned(value: u64) -> bool {
    (value & 0b11) == 0
}

/// Check if a value is aligned to the given alignment.
/// Alignment must be a power of two.
#[inline]
pub const fn is_aligned(value: u64, alignment: u64) -> bool {
    let mask = alignment.wrapping_sub(1);
    (value & mask) == 0
}

/// Integer division rounding up. Equivalent to `DivideUp` in C++.
#[inline]
pub const fn divide_up(x: u64, y: u64) -> u64 {
    x.wrapping_add(y.wrapping_sub(1)) / y
}

/// Return the least significant set bit of x.
#[inline]
pub const fn least_significant_one_bit(x: u64) -> u64 {
    x & x.wrapping_neg()
}

/// Reset (clear) the least significant set bit of x.
#[inline]
pub const fn reset_least_significant_one_bit(x: u64) -> u64 {
    x & (x.wrapping_sub(1))
}

/// Check if x is a power of two.
#[inline]
pub const fn is_power_of_two(x: u64) -> bool {
    x > 0 && reset_least_significant_one_bit(x) == 0
}

/// Return the largest power of two that is <= x.
/// x must be > 0.
#[inline]
pub const fn floor_power_of_two(x: u64) -> u64 {
    1u64 << (63 - x.leading_zeros() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_align_up() {
        assert_eq!(align_up(0, 4096), 0);
        assert_eq!(align_up(1, 4096), 4096);
        assert_eq!(align_up(4096, 4096), 4096);
        assert_eq!(align_up(4097, 4096), 8192);
    }

    #[test]
    fn test_align_down() {
        assert_eq!(align_down(0, 4096), 0);
        assert_eq!(align_down(1, 4096), 0);
        assert_eq!(align_down(4096, 4096), 4096);
        assert_eq!(align_down(4097, 4096), 4096);
    }

    #[test]
    fn test_is_word_aligned() {
        assert!(is_word_aligned(0));
        assert!(is_word_aligned(4));
        assert!(!is_word_aligned(1));
        assert!(!is_word_aligned(3));
    }

    #[test]
    fn test_is_aligned() {
        assert!(is_aligned(0, 16));
        assert!(is_aligned(16, 16));
        assert!(!is_aligned(1, 16));
    }

    #[test]
    fn test_is_power_of_two() {
        assert!(is_power_of_two(1));
        assert!(is_power_of_two(2));
        assert!(is_power_of_two(4));
        assert!(!is_power_of_two(0));
        assert!(!is_power_of_two(3));
    }

    #[test]
    fn test_floor_power_of_two() {
        assert_eq!(floor_power_of_two(1), 1);
        assert_eq!(floor_power_of_two(3), 2);
        assert_eq!(floor_power_of_two(4), 4);
        assert_eq!(floor_power_of_two(5), 4);
    }

    #[test]
    fn test_align_up_log2() {
        assert_eq!(align_up_log2(0, 12), 0);
        assert_eq!(align_up_log2(1, 12), 4096);
        assert_eq!(align_up_log2(4096, 12), 4096);
    }

    #[test]
    fn unsigned_alignment_arithmetic_wraps_like_cpp() {
        assert_eq!(align_up(u64::MAX, 2), 0);
        assert_eq!(align_up_log2(u64::MAX, 1), 0);
        assert_eq!(divide_up(u64::MAX, 2), 0);
        assert!(is_aligned(0, 0));
    }

    #[test]
    fn test_least_significant_one_bit() {
        assert_eq!(least_significant_one_bit(0b1100), 0b0100);
        assert_eq!(least_significant_one_bit(0b1010), 0b0010);
    }

    #[test]
    fn test_reset_least_significant_one_bit() {
        assert_eq!(reset_least_significant_one_bit(0b1100), 0b1000);
        assert_eq!(reset_least_significant_one_bit(0b1000), 0);
    }
}

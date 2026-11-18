//! Port of Dynarmic `common/fp/util.h`.

use super::fpcr::Fpcr;
use super::info::FloatFormat;

/// Is floating point value a zero?
pub fn is_zero<F: FloatFormat>(value: F, fpcr: Fpcr) -> bool {
    if fpcr.fz() {
        return (value.to_bits() & F::EXPONENT_MASK) == 0;
    }
    (value.to_bits() & !F::SIGN_MASK) == 0
}

/// Is floating point value an infinity?
pub fn is_inf<F: FloatFormat>(value: F) -> bool {
    (value.to_bits() & !F::SIGN_MASK) == F::infinity(false).to_bits()
}

/// Is floating point value a QNaN?
pub fn is_qnan<F: FloatFormat>(value: F) -> bool {
    let qnan_bits = F::EXPONENT_MASK | F::MANTISSA_MSB;
    (value.to_bits() & qnan_bits) == qnan_bits
}

/// Is floating point value a SNaN?
pub fn is_snan<F: FloatFormat>(value: F) -> bool {
    let qnan_bits = F::EXPONENT_MASK | F::MANTISSA_MSB;
    let snan_bits = F::EXPONENT_MASK;
    (value.to_bits() & qnan_bits) == snan_bits && (value.to_bits() & F::MANTISSA_MASK) != 0
}

/// Is floating point value a NaN?
pub fn is_nan<F: FloatFormat>(value: F) -> bool {
    is_qnan(value) || is_snan(value)
}

/// Given a single argument, return the NaN value which would be returned by an ARM processor.
/// If the argument isn't a NaN, returns `None`.
pub fn process_nans1<F: FloatFormat>(a: F) -> Option<F> {
    if is_snan(a) {
        Some(F::from_bits(a.to_bits() | F::MANTISSA_MSB))
    } else if is_qnan(a) {
        Some(a)
    } else {
        None
    }
}

/// Given a pair of arguments, return the NaN value which would be returned by an ARM processor.
/// If neither argument is a NaN, returns `None`.
pub fn process_nans2<F: FloatFormat>(a: F, b: F) -> Option<F> {
    if is_snan(a) {
        Some(F::from_bits(a.to_bits() | F::MANTISSA_MSB))
    } else if is_snan(b) {
        Some(F::from_bits(b.to_bits() | F::MANTISSA_MSB))
    } else if is_qnan(a) {
        Some(a)
    } else if is_qnan(b) {
        Some(b)
    } else {
        None
    }
}

/// Given three arguments, return the NaN value which would be returned by an ARM processor.
/// If none of the arguments is a NaN, returns `None`.
pub fn process_nans3<F: FloatFormat>(a: F, b: F, c: F) -> Option<F> {
    if is_snan(a) {
        Some(F::from_bits(a.to_bits() | F::MANTISSA_MSB))
    } else if is_snan(b) {
        Some(F::from_bits(b.to_bits() | F::MANTISSA_MSB))
    } else if is_snan(c) {
        Some(F::from_bits(c.to_bits() | F::MANTISSA_MSB))
    } else if is_qnan(a) {
        Some(a)
    } else if is_qnan(b) {
        Some(b)
    } else if is_qnan(c) {
        Some(c)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_like_upstream() {
        assert!(is_qnan(0x7fc0_0000u32));
        assert!(is_snan(0x7f80_0001u32));
        assert!(!is_snan(0x7f80_0000u32));
        assert!(is_inf(0xff80_0000u32));
        assert!(is_nan(0xffff_ffffu32));
        assert!(is_zero(0x0000_0001u32, Fpcr::new(1 << 24)));
        assert!(!is_zero(0x0000_0001u32, Fpcr::new(0)));
    }

    #[test]
    fn process_nans_prioritises_signalling_then_first_operand() {
        assert_eq!(
            process_nans2(0x7fc1_0000u32, 0x7f80_0001),
            Some(0x7fc0_0001)
        );
        assert_eq!(
            process_nans2(0x7fc1_0000u32, 0x7fc2_0000),
            Some(0x7fc1_0000)
        );
        assert_eq!(
            process_nans2(0x3f80_0000u32, 0x7fc2_0000),
            Some(0x7fc2_0000)
        );
        assert_eq!(process_nans2(0x3f80_0000u32, 0x4000_0000), None);
        assert_eq!(
            process_nans3(0x7fc1_0000u32, 0x0, 0xff80_0001),
            Some(0xffc0_0001)
        );
    }
}

//! Port of zuyu/src/common/uuid.h and zuyu/src/common/uuid.cpp
//! Status: COMPLET
//! Derniere synchro: 2026-03-05

use crate::tiny_mt::TinyMT;
use std::fmt;
use std::hash::{Hash, Hasher};

/// A 128-bit UUID matching the Switch/zuyu UUID type.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct UUID {
    pub uuid: [u8; 16],
}

const RAW_STRING_SIZE: usize = 32; // sizeof(UUID) * 2
const FORMATTED_STRING_SIZE: usize = RAW_STRING_SIZE + 4; // 36 chars: 8-4-4-4-12

impl UUID {
    /// Creates a new UUID with all bytes set to zero (invalid).
    pub const fn new() -> Self {
        Self { uuid: [0u8; 16] }
    }

    /// Constructs a UUID from a 16-byte array.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self { uuid: bytes }
    }

    /// Constructs a UUID from a string.
    ///
    /// Accepts either:
    /// 1. A 32 hexadecimal character string representing the bytes
    /// 2. A RFC 4122 formatted UUID string (xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx)
    pub fn from_string(uuid_string: &str) -> Self {
        Self {
            uuid: construct_uuid(uuid_string),
        }
    }

    /// Returns whether the stored UUID is valid (non-zero).
    pub const fn is_valid(&self) -> bool {
        let mut i = 0;
        while i < 16 {
            if self.uuid[i] != 0 {
                return true;
            }
            i += 1;
        }
        false
    }

    /// Returns whether the stored UUID is invalid (all zeros).
    pub const fn is_invalid(&self) -> bool {
        !self.is_valid()
    }

    /// Returns a 32 hexadecimal character string representing the bytes of the UUID.
    pub fn raw_string(&self) -> String {
        let mut s = String::with_capacity(32);
        for &b in &self.uuid {
            s.push_str(&format!("{:02x}", b));
        }
        s
    }

    /// Returns a RFC 4122 formatted UUID string.
    pub fn formatted_string(&self) -> String {
        format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            self.uuid[0], self.uuid[1], self.uuid[2], self.uuid[3],
            self.uuid[4], self.uuid[5],
            self.uuid[6], self.uuid[7],
            self.uuid[8], self.uuid[9],
            self.uuid[10], self.uuid[11], self.uuid[12], self.uuid[13], self.uuid[14], self.uuid[15],
        )
    }

    /// Returns a 64-bit hash of the UUID.
    pub fn hash_value(&self) -> u64 {
        let upper = u64::from_le_bytes(self.uuid[0..8].try_into().unwrap());
        let lower = u64::from_le_bytes(self.uuid[8..16].try_into().unwrap());
        upper ^ lower.rotate_left(1)
    }

    /// Copies the contents of the UUID into a u128.
    pub fn as_u128(&self) -> u128 {
        u128::from_le_bytes(self.uuid)
    }

    /// Creates the default UUID "Eden Default UID".
    pub const fn make_default() -> Self {
        Self {
            uuid: [
                b'E', b'd', b'e', b'n', b' ', b'D', b'e', b'f', b'a', b'u', b'l', b't', b' ', b'U',
                b'I', b'D',
            ],
        }
    }

    /// Creates a random UUID.
    pub fn make_random() -> Self {
        let seed = rand_seed();
        Self::make_random_with_seed(seed)
    }

    /// Creates a random UUID with a given seed.
    pub fn make_random_with_seed(seed: u32) -> Self {
        let mut rng = TinyMT::new();
        rng.initialize(seed);

        let mut uuid = Self::new();
        rng.generate_random_bytes(&mut uuid.uuid);
        uuid
    }

    /// Creates a random UUID that is RFC 4122 Version 4 compliant.
    pub fn make_random_rfc4122_v4() -> Self {
        let mut uuid = Self::make_random();

        // Set the two most significant bits of clock_seq_hi_and_reserved
        uuid.uuid[8] = 0x80 | (uuid.uuid[8] & 0x3F);

        // Set the four most significant bits of time_hi_and_version to 0100 (version 4)
        uuid.uuid[6] = 0x40 | (uuid.uuid[6] & 0x0F);

        uuid
    }

    /// Port of UUID::MakeRFC4122V5: consume the first 16 SHA-1 digest bytes.
    pub fn make_rfc4122_v5(sha1: [u8; 16]) -> Self {
        let mut uuid = Self::from_bytes(sha1);
        uuid.uuid[8] = 0x80 | (uuid.uuid[8] & 0x3f);
        uuid.uuid[6] = 0x50 | (uuid.uuid[6] & 0x0f);
        uuid
    }
}

impl Default for UUID {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for UUID {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "UUID({})", self.formatted_string())
    }
}

impl fmt::Display for UUID {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.formatted_string())
    }
}

impl Hash for UUID {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash_value());
    }
}

/// An invalid UUID with all bytes set to 0.
pub const INVALID_UUID: UUID = UUID::new();

// Helper functions

fn hex_char_to_byte(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => {
            debug_assert!(false, "{} is not a hexadecimal digit!", c as char);
            None
        }
    }
}

fn construct_from_raw_string(raw: &[u8]) -> [u8; 16] {
    let mut uuid = [0u8; 16];
    for i in (0..RAW_STRING_SIZE).step_by(2) {
        let upper = match hex_char_to_byte(raw[i]) {
            Some(v) => v,
            None => return [0u8; 16],
        };
        let lower = match hex_char_to_byte(raw[i + 1]) {
            Some(v) => v,
            None => return [0u8; 16],
        };
        uuid[i / 2] = (upper << 4) | lower;
    }
    uuid
}

fn construct_from_formatted_string(formatted: &[u8]) -> [u8; 16] {
    let mut uuid = [0u8; 16];
    // Positions of hex chars in formatted string (skip dashes at positions 8, 13, 18, 23)
    let mut byte_idx = 0;
    let mut str_idx = 0;

    while byte_idx < 16 && str_idx < formatted.len() {
        if formatted[str_idx] == b'-' {
            str_idx += 1;
            continue;
        }
        if str_idx + 1 >= formatted.len() {
            return [0u8; 16];
        }
        let upper = match hex_char_to_byte(formatted[str_idx]) {
            Some(v) => v,
            None => return [0u8; 16],
        };
        let lower = match hex_char_to_byte(formatted[str_idx + 1]) {
            Some(v) => v,
            None => return [0u8; 16],
        };
        uuid[byte_idx] = (upper << 4) | lower;
        byte_idx += 1;
        str_idx += 2;
    }

    uuid
}

fn construct_uuid(uuid_string: &str) -> [u8; 16] {
    let length = uuid_string.len();

    if length == 0 {
        return [0u8; 16];
    }

    let bytes = uuid_string.as_bytes();

    if length == RAW_STRING_SIZE {
        return construct_from_raw_string(bytes);
    }

    if length == FORMATTED_STRING_SIZE {
        return construct_from_formatted_string(bytes);
    }

    debug_assert!(
        false,
        "UUID string has an invalid length of {} characters!",
        length
    );

    [0u8; 16]
}

fn rand_seed() -> u32 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u32)
        .unwrap_or(42)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_v5_preserves_digest_except_version_and_variant_bits() {
        let input = [0xff; 16];
        let uuid = UUID::make_rfc4122_v5(input);
        let mut expected = input;
        expected[6] = 0x5f;
        expected[8] = 0xbf;
        assert_eq!(uuid.uuid, expected);
        assert_eq!(std::mem::size_of::<UUID>(), 16);
        let zero = UUID::make_rfc4122_v5([0; 16]);
        assert_eq!(zero.uuid[6], 0x50);
        assert_eq!(zero.uuid[8], 0x80);
    }

    #[test]
    fn test_uuid_default() {
        let uuid = UUID::new();
        assert!(uuid.is_invalid());
    }

    #[test]
    fn test_uuid_make_default() {
        let uuid = UUID::make_default();
        assert!(uuid.is_valid());
        assert_eq!(&uuid.uuid, b"Eden Default UID");
    }

    #[test]
    fn test_uuid_from_raw_string() {
        let uuid = UUID::from_string("00112233445566778899aabbccddeeff");
        assert_eq!(
            uuid.uuid,
            [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff
            ]
        );
    }

    #[test]
    fn test_uuid_from_formatted_string() {
        let uuid = UUID::from_string("00112233-4455-6677-8899-aabbccddeeff");
        assert_eq!(
            uuid.uuid,
            [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff
            ]
        );
    }

    #[test]
    fn test_uuid_raw_string_roundtrip() {
        let original = "00112233445566778899aabbccddeeff";
        let uuid = UUID::from_string(original);
        assert_eq!(uuid.raw_string(), original);
    }

    #[test]
    fn test_uuid_rfc4122_v4() {
        let uuid = UUID::make_random_rfc4122_v4();
        // Check version bits (byte 6, upper nibble should be 4)
        assert_eq!(uuid.uuid[6] >> 4, 4);
        // Check variant bits (byte 8, upper 2 bits should be 10)
        assert_eq!(uuid.uuid[8] >> 6, 2);
    }

    #[test]
    fn test_uuid_hash_value_matches_upstream_algorithm() {
        let uuid = UUID::from_bytes([
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ]);
        let upper = u64::from_le_bytes(uuid.uuid[0..8].try_into().unwrap());
        let lower = u64::from_le_bytes(uuid.uuid[8..16].try_into().unwrap());
        assert_eq!(uuid.hash_value(), upper ^ lower.rotate_left(1));
    }
}

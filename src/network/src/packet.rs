// SPDX-FileCopyrightText: Copyright 2017 Citra Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/network/packet.h and packet.cpp
//!
//! A class that serializes data for network transfer. It also handles
//! endianness (network byte order).

/// A packet for network serialization / deserialization.
/// Maps to C++ `Network::Packet`.
pub struct Packet {
    /// Data stored in the packet.
    data: Vec<u8>,
    /// Current reading position in the packet.
    read_pos: usize,
    /// Reading state of the packet.
    is_valid: bool,
}

impl Default for Packet {
    fn default() -> Self {
        Self::new()
    }
}

impl Packet {
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            read_pos: 0,
            is_valid: true,
        }
    }

    /// Append raw data to the end of the packet.
    pub fn append(&mut self, bytes: &[u8]) {
        self.data.extend_from_slice(bytes);
    }

    /// Read raw bytes from the current read position.
    pub fn read_raw(&mut self, size: usize) -> Option<Vec<u8>> {
        if !self.check_size(size) {
            return None;
        }
        let result = self.data[self.read_pos..self.read_pos + size].to_vec();
        self.read_pos += size;
        Some(result)
    }

    /// Clear the packet. After calling Clear, the packet is empty.
    pub fn clear(&mut self) {
        self.data.clear();
        self.read_pos = 0;
        self.is_valid = true;
    }

    /// Ignores bytes while reading.
    pub fn ignore_bytes(&mut self, length: u32) {
        self.read_pos += length as usize;
    }

    /// Get the data contained in the packet.
    pub fn get_data(&self) -> &[u8] {
        &self.data
    }

    /// Returns the number of bytes in the packet.
    pub fn get_data_size(&self) -> usize {
        self.data.len()
    }

    /// Returns true if all data was read.
    pub fn end_of_packet(&self) -> bool {
        self.read_pos >= self.data.len()
    }

    /// Returns true if the packet is in a valid state.
    pub fn is_valid(&self) -> bool {
        self.is_valid
    }

    // -----------------------------------------------------------------------
    // Read overloads
    // -----------------------------------------------------------------------

    pub fn read_bool(&mut self) -> Option<bool> {
        self.read_u8().map(|v| v != 0)
    }

    pub fn read_i8(&mut self) -> Option<i8> {
        self.read_raw(1).map(|b| b[0] as i8)
    }

    pub fn read_u8(&mut self) -> Option<u8> {
        self.read_raw(1).map(|b| b[0])
    }

    pub fn read_i16(&mut self) -> Option<i16> {
        self.read_raw(2).map(|b| i16::from_be_bytes([b[0], b[1]]))
    }

    pub fn read_u16(&mut self) -> Option<u16> {
        self.read_raw(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn read_i32(&mut self) -> Option<i32> {
        self.read_raw(4)
            .map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn read_u32(&mut self) -> Option<u32> {
        self.read_raw(4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn read_i64(&mut self) -> Option<i64> {
        self.read_raw(8)
            .map(|b| i64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    pub fn read_u64(&mut self) -> Option<u64> {
        self.read_raw(8)
            .map(|b| u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    pub fn read_f32(&mut self) -> Option<f32> {
        self.read_raw(4)
            .map(|b| f32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn read_f64(&mut self) -> Option<f64> {
        self.read_raw(8)
            .map(|b| f64::from_ne_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    pub fn read_string(&mut self) -> Option<String> {
        self.read_string_bytes()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Packet::Read(std::string&): protocol strings are bytes, not necessarily
    /// UTF-8 (notably after the server's byte-count chat truncation). Text-only
    /// consumers use read_string; relays must preserve the original bytes.
    pub fn read_string_bytes(&mut self) -> Option<Vec<u8>> {
        let length = self.read_u32()? as usize;
        self.read_raw(length)
    }

    pub fn read_vec_u8(&mut self) -> Option<Vec<u8>> {
        let size = self.read_u32()? as usize;
        let mut out = Vec::with_capacity(size);
        for _ in 0..size {
            out.push(self.read_u8()?);
        }
        Some(out)
    }

    pub fn read_array<const S: usize>(&mut self) -> Option<[u8; S]> {
        let mut out = [0u8; S];
        for item in out.iter_mut() {
            *item = self.read_u8()?;
        }
        Some(out)
    }

    pub fn read_vec_string(&mut self) -> Option<Vec<String>> {
        let size = self.read_u32()? as usize;
        let mut out = Vec::with_capacity(size);
        for _ in 0..size {
            out.push(self.read_string()?);
        }
        Some(out)
    }

    // -----------------------------------------------------------------------
    // Write overloads
    // -----------------------------------------------------------------------

    pub fn write_bool(&mut self, value: bool) {
        self.write_u8(value as u8);
    }

    pub fn write_i8(&mut self, value: i8) {
        self.data.push(value as u8);
    }

    pub fn write_u8(&mut self, value: u8) {
        self.data.push(value);
    }

    pub fn write_i16(&mut self, value: i16) {
        self.data.extend_from_slice(&value.to_be_bytes());
    }

    pub fn write_u16(&mut self, value: u16) {
        self.data.extend_from_slice(&value.to_be_bytes());
    }

    pub fn write_i32(&mut self, value: i32) {
        self.data.extend_from_slice(&value.to_be_bytes());
    }

    pub fn write_u32(&mut self, value: u32) {
        self.data.extend_from_slice(&value.to_be_bytes());
    }

    pub fn write_i64(&mut self, value: i64) {
        self.data.extend_from_slice(&value.to_be_bytes());
    }

    pub fn write_u64(&mut self, value: u64) {
        self.data.extend_from_slice(&value.to_be_bytes());
    }

    pub fn write_f32(&mut self, value: f32) {
        self.data.extend_from_slice(&value.to_ne_bytes());
    }

    pub fn write_f64(&mut self, value: f64) {
        self.data.extend_from_slice(&value.to_ne_bytes());
    }

    pub fn write_string(&mut self, value: &str) {
        self.write_string_bytes(value.as_bytes());
    }

    /// Packet::Write(const std::string&), including embedded NUL/invalid UTF-8.
    pub fn write_string_bytes(&mut self, value: &[u8]) {
        let length = value.len() as u32;
        self.write_u32(length);
        if length > 0 {
            self.data.extend_from_slice(&value[..length as usize]);
        }
    }

    pub fn write_vec_u8(&mut self, value: &[u8]) {
        self.write_u32(value.len() as u32);
        for &b in value {
            self.write_u8(b);
        }
    }

    pub fn write_array<const S: usize>(&mut self, value: &[u8; S]) {
        for &b in value {
            self.write_u8(b);
        }
    }

    pub fn write_vec_string(&mut self, value: &[String]) {
        self.write_u32(value.len() as u32);
        for s in value {
            self.write_string(s);
        }
    }

    // -----------------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------------

    /// Check if the packet can extract a given number of bytes.
    fn check_size(&mut self, size: usize) -> bool {
        self.is_valid = self.is_valid && (self.read_pos + size <= self.data.len());
        self.is_valid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_strings_preserve_arbitrary_bytes_and_byte_truncation() {
        let bytes = [b'a', 0, 0xff, 0xc3];
        let mut packet = Packet::new();
        packet.write_string_bytes(&bytes);
        assert_eq!(packet.get_data(), &[0, 0, 0, 4, b'a', 0, 0xff, 0xc3]);
        assert_eq!(packet.read_string_bytes().unwrap(), bytes);
        assert!(packet.end_of_packet());
        assert!(packet.is_valid());

        // A byte limit may cut a multibyte character. Forward those bytes just
        // as std::string::resize does, without inserting replacement characters.
        let mut message = vec![b'a'; 499];
        message.extend_from_slice("é".as_bytes());
        message.truncate(500);
        packet.clear();
        packet.write_string_bytes(&message);
        assert_eq!(packet.read_string_bytes().unwrap(), message);
    }

    #[test]
    fn protocol_string_lengths_preserve_packet_failure_state() {
        let mut packet = Packet::new();
        packet.write_string_bytes(&[]);
        packet.write_u32(3);
        packet.append(&[1, 2]);
        assert_eq!(packet.read_string_bytes(), Some(Vec::new()));
        assert_eq!(packet.read_string_bytes(), None);
        assert!(!packet.is_valid());
        packet.append(&[3]);
        assert_eq!(packet.read_string_bytes(), None);
        packet.clear();
        packet.append(&[0, 0]);
        assert_eq!(packet.read_string_bytes(), None);
        assert!(!packet.is_valid());
    }

    #[test]
    fn test_write_read_u8() {
        let mut pkt = Packet::new();
        pkt.write_u8(0xAB);
        assert_eq!(pkt.get_data_size(), 1);

        let mut reader = Packet::new();
        reader.append(pkt.get_data());
        assert_eq!(reader.read_u8(), Some(0xAB));
        assert!(reader.end_of_packet());
    }

    #[test]
    fn test_write_read_u32_network_order() {
        let mut pkt = Packet::new();
        pkt.write_u32(0x12345678);

        let mut reader = Packet::new();
        reader.append(pkt.get_data());
        assert_eq!(reader.read_u32(), Some(0x12345678));
    }

    #[test]
    fn test_write_read_string() {
        let mut pkt = Packet::new();
        pkt.write_string("hello");

        let mut reader = Packet::new();
        reader.append(pkt.get_data());
        assert_eq!(reader.read_string(), Some("hello".to_string()));
    }

    #[test]
    fn test_write_read_bool() {
        let mut pkt = Packet::new();
        pkt.write_bool(true);
        pkt.write_bool(false);

        let mut reader = Packet::new();
        reader.append(pkt.get_data());
        assert_eq!(reader.read_bool(), Some(true));
        assert_eq!(reader.read_bool(), Some(false));
    }

    #[test]
    fn test_ignore_bytes() {
        let mut pkt = Packet::new();
        pkt.write_u8(1);
        pkt.write_u8(2);
        pkt.write_u8(3);

        let mut reader = Packet::new();
        reader.append(pkt.get_data());
        reader.ignore_bytes(2);
        assert_eq!(reader.read_u8(), Some(3));
    }

    #[test]
    fn test_read_past_end_returns_none() {
        let mut pkt = Packet::new();
        pkt.write_u8(1);

        let mut reader = Packet::new();
        reader.append(pkt.get_data());
        let _ = reader.read_u8();
        assert_eq!(reader.read_u8(), None);
        assert!(!reader.is_valid());
    }

    #[test]
    fn test_clear() {
        let mut pkt = Packet::new();
        pkt.write_u32(42);
        pkt.clear();
        assert_eq!(pkt.get_data_size(), 0);
        assert!(pkt.end_of_packet());
        assert!(pkt.is_valid());
    }

    #[test]
    fn test_write_read_array() {
        let mut pkt = Packet::new();
        pkt.write_array(&[10u8, 20, 30, 40]);

        let mut reader = Packet::new();
        reader.append(pkt.get_data());
        assert_eq!(reader.read_array::<4>(), Some([10, 20, 30, 40]));
    }
}

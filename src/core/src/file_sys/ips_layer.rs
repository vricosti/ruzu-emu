// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/file_sys/ips_layer.h and ips_layer.cpp
//! Status: COMPLET
//! Derniere synchro: 2026-03-12
//!
//! IPS/IPS32 patching and IPSwitch text patch compilation.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::vfs::vfs_types::VirtualFile;
use super::vfs::vfs_vector::VectorVfsFile;

// =============================================================================
// IPS/IPS32 binary patch format
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IpsFileType {
    Ips,
    Ips32,
    Error,
}

/// Identify the IPS file type from its 5-byte magic.
fn identify_magic(magic: &[u8]) -> IpsFileType {
    if magic.starts_with(b"PATCH") {
        IpsFileType::Ips
    } else if magic.starts_with(b"IPS32") {
        IpsFileType::Ips32
    } else {
        IpsFileType::Error
    }
}

/// Check if the data represents the EOF marker for the given IPS type.
fn is_eof(file_type: IpsFileType, data: &[u8]) -> bool {
    match file_type {
        IpsFileType::Ips => data.starts_with(b"EOF"),
        IpsFileType::Ips32 => data.starts_with(b"EEOF"),
        IpsFileType::Error => false,
    }
}

/// Applies an IPS/IPS32 binary patch to a source file.
/// Returns the patched file, or None if the patch is invalid.
///
/// Corresponds to upstream `PatchIPS`.
pub fn patch_ips(source: &VirtualFile, ips: &VirtualFile) -> Option<VirtualFile> {
    let file_type = identify_magic(&ips.read_bytes(5, 0));
    if file_type == IpsFileType::Error {
        return None;
    }

    let mut in_data = source.read_all_bytes();
    if in_data.is_empty() {
        return None;
    }

    let record_size = if file_type == IpsFileType::Ips { 3 } else { 4 };
    let mut temp = vec![0u8; record_size];
    let mut offset: usize = 5; // After header

    loop {
        let len = temp.len();
        let read = ips.read(&mut temp, len, offset);
        if read != len {
            break;
        }
        offset += temp.len();

        if is_eof(file_type, &temp) {
            break;
        }

        let real_offset: u32 = if file_type == IpsFileType::Ips32 {
            (temp[0] as u32) << 24
                | (temp[1] as u32) << 16
                | (temp[2] as u32) << 8
                | (temp[3] as u32)
        } else {
            (temp[0] as u32) << 16 | (temp[1] as u32) << 8 | (temp[2] as u32)
        };

        if real_offset as usize > in_data.len() {
            return None;
        }

        let mut size_buf = [0u8; 2];
        if ips.read(&mut size_buf, 2, offset) != 2 {
            return None;
        }
        let data_size = u16::from_be_bytes(size_buf);
        offset += 2;

        if data_size == 0 {
            // RLE record
            let mut rle_size_buf = [0u8; 2];
            if ips.read(&mut rle_size_buf, 2, offset) != 2 {
                return None;
            }
            let mut rle_size = u16::from_be_bytes(rle_size_buf) as usize;
            offset += 2;

            let data = ips.read_byte(offset)?;
            offset += 1;

            if real_offset as usize + rle_size > in_data.len() {
                rle_size = in_data.len() - real_offset as usize;
            }
            for i in 0..rle_size {
                in_data[real_offset as usize + i] = data;
            }
        } else {
            // Standard patch record
            let mut read_size = data_size as usize;
            if real_offset as usize + read_size > in_data.len() {
                read_size = in_data.len() - real_offset as usize;
            }
            let actual = ips.read(&mut in_data[real_offset as usize..], read_size, offset);
            if actual != data_size as usize {
                return None;
            }
            offset += data_size as usize;
        }
    }

    if !is_eof(file_type, &temp) {
        return None;
    }

    let name = source.get_name();
    let parent = source.get_containing_directory();
    Some(Arc::new(VectorVfsFile::new(in_data, name, parent)))
}

// =============================================================================
// IPSwitch text patch compiler
// =============================================================================

/// A single IPSwitch patch (a set of offset-to-bytes records).
struct IpSwitchPatch {
    enabled: bool,
    records: BTreeMap<u32, Vec<u8>>,
}

/// IPSwitch text patch compiler.
///
/// Corresponds to upstream `IPSwitchCompiler`.
pub struct IpSwitchCompiler {
    valid: bool,
    patch_text: VirtualFile,
    patches: Vec<IpSwitchPatch>,
    nso_build_id: [u8; 0x20],
    is_little_endian: bool,
    offset_shift: i64,
    print_values: bool,
    last_comment: String,
}

fn starts_with(base: &str, check: &str) -> bool {
    base.starts_with(check)
}

fn escape_string_sequences(input: String) -> String {
    // Upstream EscapeStringSequences consumes each escape once, without
    // interpreting the resulting text again (in particular an escaped slash).
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        output.push(if ch == '\\' {
            match chars.next() {
                Some('a') => '\x07',
                Some('b') => '\x08',
                Some('e') => '\x1b',
                Some('f') => '\x0c',
                Some('n') => '\n',
                Some('r') => '\r',
                Some('t') => '\t',
                Some('v') => '\x0b',
                Some(other) => other,
                None => '\\',
            }
        } else {
            ch
        });
    }
    output
}

/// Parse an integer using the base-prefix rules of C's `strtoll(..., 0)`.
fn parse_integer_auto(input: &str) -> i64 {
    let input = input.trim();
    let (negative, digits) = input
        .strip_prefix('-')
        .map_or((false, input), |digits| (true, digits));
    let digits = digits.strip_prefix('+').unwrap_or(digits);
    let (radix, digits) = if let Some(digits) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        (16, digits)
    } else if digits.len() > 1 && digits.starts_with('0') {
        (8, &digits[1..])
    } else {
        (10, digits)
    };

    i64::from_str_radix(digits, radix)
        .ok()
        .and_then(|value| {
            if negative {
                value.checked_neg()
            } else {
                Some(value)
            }
        })
        .unwrap_or(0)
}

/// Parse a hex string into bytes. If `little_endian` is true, reverse the
/// byte order of each pair.
fn hex_string_to_vec(hex: &str, little_endian: bool) -> Vec<u8> {
    let hex = hex.trim();
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let mut chars = hex.chars();
    while let (Some(a), Some(b)) = (chars.next(), chars.next()) {
        if let Ok(byte) = u8::from_str_radix(&format!("{}{}", a, b), 16) {
            bytes.push(byte);
        }
    }
    if little_endian {
        bytes.reverse();
    }
    bytes
}

/// Parse a hex string of length 64 (padded) into a [u8; 0x20] array.
fn hex_string_to_array_20(hex: &str) -> [u8; 0x20] {
    let padded = format!("{:0<64}", hex);
    let mut result = [0u8; 0x20];
    for i in 0..0x20 {
        result[i] = u8::from_str_radix(&padded[i * 2..i * 2 + 2], 16).unwrap_or(0);
    }
    result
}

impl IpSwitchCompiler {
    /// Create a new IPSwitch compiler from a patch text file.
    /// Immediately parses the file.
    pub fn new(patch_text: VirtualFile) -> Self {
        let mut compiler = Self {
            valid: false,
            patch_text,
            patches: Vec::new(),
            nso_build_id: [0u8; 0x20],
            is_little_endian: false,
            offset_shift: 0,
            print_values: false,
            last_comment: String::new(),
        };
        compiler.parse();
        compiler
    }

    pub fn get_build_id(&self) -> [u8; 0x20] {
        self.nso_build_id
    }

    pub fn is_valid(&self) -> bool {
        self.valid
    }

    fn parse_flag(&mut self, line: &str) {
        if let Some(rest) = line.strip_prefix("@flag offset_shift ") {
            self.offset_shift = parse_integer_auto(rest);
        } else if line.starts_with("@little-endian") {
            self.is_little_endian = true;
        } else if line.starts_with("@big-endian") {
            self.is_little_endian = false;
        } else if line.starts_with("@flag print_values") {
            self.print_values = true;
        }
    }

    fn parse(&mut self) {
        let bytes = self.patch_text.read_all_bytes();
        let text = String::from_utf8_lossy(&bytes);

        let lines: Vec<String> = text
            .lines()
            .map(|l| {
                let mut s = l.to_string();
                if s.ends_with('\r') {
                    s.pop();
                }
                s
            })
            .collect();

        let mut i = 0;
        while i < lines.len() {
            let mut line = lines[i].clone();

            // Remove midline comments (\\)
            let mut comment_index: Option<usize> = None;
            let mut within_string = false;
            let line_bytes = line.as_bytes();
            let mut k = 0;
            while k < line_bytes.len() {
                if line_bytes[k] == b'"' && k > 0 && line_bytes[k - 1] != b'\\' {
                    within_string = !within_string;
                } else if line_bytes[k] == b'\\'
                    && k + 1 < line_bytes.len()
                    && line_bytes[k + 1] == b'\\'
                {
                    let _ = within_string;
                    comment_index = Some(k);
                    break;
                }
                k += 1;
            }

            if !starts_with(&line, "//") {
                if let Some(ci) = comment_index {
                    self.last_comment = line[ci + 2..].to_string();
                    line = line[..ci].to_string();
                }
            }

            if starts_with(&line, "@stop") {
                break;
            } else if starts_with(&line, "@nsobid-") {
                let raw_build_id = &line[8..];
                self.nso_build_id = hex_string_to_array_20(raw_build_id);
            } else if starts_with(&line, "#") {
                log::info!(
                    "[IPSwitchCompiler ('{}')] Forced output comment: {}",
                    self.patch_text.get_name(),
                    &line[1..]
                );
            } else if starts_with(&line, "//") {
                let comment = &line[2..];
                self.last_comment = comment.trim_start().to_string();
            } else if starts_with(&line, "@enabled") || starts_with(&line, "@disabled") {
                let enabled = starts_with(&line, "@enabled");
                log::info!(
                    "[IPSwitchCompiler ('{}')] Parsing patch '{}' ({})",
                    self.patch_text.get_name(),
                    self.last_comment,
                    &line[1..]
                );

                let mut patch = IpSwitchPatch {
                    enabled,
                    records: BTreeMap::new(),
                };

                // Read rest of patch
                loop {
                    if i + 1 >= lines.len() {
                        break;
                    }
                    i += 1;
                    let patch_line = &lines[i];

                    if starts_with(patch_line, "//") || starts_with(patch_line, "#") {
                        continue;
                    }

                    if starts_with(patch_line, "@enabled") || starts_with(patch_line, "@disabled") {
                        i -= 1;
                        break;
                    }

                    if starts_with(patch_line, "@") {
                        self.parse_flag(patch_line);
                        continue;
                    }

                    // Minimum length: 8 hex + space + 2 hex = 11
                    if patch_line.len() < 11 {
                        break;
                    }

                    let offset_str = &patch_line[..8];
                    let offset = u64::from_str_radix(offset_str, 16)
                        .unwrap_or(0)
                        .wrapping_add(self.offset_shift as u64);

                    let replace: Vec<u8>;
                    if patch_line.len() > 9 && patch_line.as_bytes()[9] == b'"' {
                        // String replacement
                        let rest = &patch_line[10..];
                        let mut search_from = 0;
                        let end = loop {
                            if let Some(pos) = rest[search_from..].find('"') {
                                let actual = search_from + pos;
                                if actual > 0 && rest.as_bytes()[actual - 1] == b'\\' {
                                    search_from = actual + 1;
                                    continue;
                                }
                                break actual;
                            } else {
                                return;
                            }
                        };
                        let value = rest[..end].to_string();
                        let value = escape_string_sequences(value);
                        replace = value.into_bytes();
                    } else {
                        // Hex replacement
                        let rest = &patch_line[9..];
                        let end = rest
                            .find(|c: char| c == ' ' || c == '/' || c == '\r' || c == '\n')
                            .unwrap_or(rest.len());
                        let value = &rest[..end];
                        replace = hex_string_to_vec(value, self.is_little_endian);
                    }

                    if self.print_values {
                        let hex: String = replace.iter().map(|b| format!("{:02X}", b)).collect();
                        log::info!(
                            "[IPSwitchCompiler ('{}')] - Patching at 0x{:08X} with '{}'",
                            self.patch_text.get_name(),
                            offset,
                            hex
                        );
                    }

                    patch.records.insert(offset as u32, replace);
                }

                self.patches.push(patch);
            } else if starts_with(&line, "@") {
                self.parse_flag(&line);
            }

            i += 1;
        }

        self.valid = true;
    }

    /// Apply all enabled patches to the input file.
    ///
    /// Corresponds to upstream `IPSwitchCompiler::Apply`.
    pub fn apply(&self, source: &VirtualFile) -> Option<VirtualFile> {
        if !self.valid {
            return None;
        }

        let mut in_data = source.read_all_bytes();

        for patch in &self.patches {
            if !patch.enabled {
                continue;
            }

            for (&offset, replace) in &patch.records {
                let offset = offset as usize;
                if offset >= in_data.len() {
                    continue;
                }
                let mut replace_size = replace.len();
                if offset + replace_size > in_data.len() {
                    replace_size = in_data.len() - offset;
                }
                in_data[offset..offset + replace_size].copy_from_slice(&replace[..replace_size]);
            }
        }

        let name = source.get_name();
        let parent = source.get_containing_directory();
        Some(Arc::new(VectorVfsFile::new(in_data, name, parent)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identify_magic() {
        assert_eq!(identify_magic(b"PATCHrecord"), IpsFileType::Ips);
        assert_eq!(identify_magic(b"PATCH"), IpsFileType::Ips);
        assert_eq!(identify_magic(b"IPS32"), IpsFileType::Ips32);
        assert_eq!(identify_magic(b"OTHER"), IpsFileType::Error);
        assert_eq!(identify_magic(b"PAT"), IpsFileType::Error);
    }

    #[test]
    fn test_is_eof() {
        assert!(is_eof(IpsFileType::Ips, b"EOFextra"));
        assert!(is_eof(IpsFileType::Ips32, b"EEOFextra"));
        assert!(is_eof(IpsFileType::Ips, b"EOF"));
        assert!(!is_eof(IpsFileType::Ips, b"EEOF"));
        assert!(is_eof(IpsFileType::Ips32, b"EEOF"));
        assert!(!is_eof(IpsFileType::Ips32, b"EOF"));
    }

    #[test]
    fn test_hex_string_to_vec() {
        assert_eq!(
            hex_string_to_vec("DEADBEEF", false),
            vec![0xDE, 0xAD, 0xBE, 0xEF]
        );
        assert_eq!(
            hex_string_to_vec("DEADBEEF", true),
            vec![0xEF, 0xBE, 0xAD, 0xDE]
        );
    }

    #[test]
    fn test_hex_string_to_array_20() {
        let result = hex_string_to_array_20("01020304");
        assert_eq!(result[0], 0x01);
        assert_eq!(result[1], 0x02);
        assert_eq!(result[2], 0x03);
        assert_eq!(result[3], 0x04);
        // Rest should be zero (padded)
        assert_eq!(result[4], 0x00);
    }

    #[test]
    fn test_escape_string_sequences() {
        assert_eq!(escape_string_sequences(r"\\n".into()), r"\n");
        assert_eq!(escape_string_sequences("\\e\\z\\".into()), "\x1bz\\");
        assert_eq!(escape_string_sequences("x".repeat(1024)), "x".repeat(1024));
        assert_eq!(
            escape_string_sequences("hello\\nworld".to_string()),
            "hello\nworld"
        );
        assert_eq!(
            escape_string_sequences("tab\\there".to_string()),
            "tab\there"
        );
    }

    #[test]
    fn ipswitch_applies_records_larger_than_256_bytes() {
        for record in ["AB".repeat(1024), format!("\"{}\"", "x".repeat(1024))] {
            let expected = if record.starts_with('"') { b'x' } else { 0xab };
            let patch_text: VirtualFile = Arc::new(VectorVfsFile::new(
                format!("@enabled\n00000000 {record}\n@stop\n").into_bytes(),
                "long.pchtxt".into(), None,
            ));
            let source: VirtualFile = Arc::new(VectorVfsFile::new(
                vec![0; 1025], "input".into(), None,
            ));
            let result = IpSwitchCompiler::new(patch_text).apply(&source).unwrap().read_all_bytes();
            assert_eq!(&result[..1024], vec![expected; 1024]);
            assert_eq!(result[1024], 0);
        }
    }

    #[test]
    fn ipswitch_accepts_prefixed_offset_shift_and_comments_inside_patch() {
        let patch_text: VirtualFile = Arc::new(VectorVfsFile::new(
            b"// Free homebrew patch\n@enabled\n@flag offset_shift 0x2\n// keep parsing\n# keep parsing too\n00000000 AA\n@stop\n".to_vec(),
            "synthetic.pchtxt".to_string(),
            None,
        ));
        let source: VirtualFile = Arc::new(VectorVfsFile::new(
            vec![0; 4],
            "homebrew.bin".to_string(),
            None,
        ));

        let compiler = IpSwitchCompiler::new(patch_text);
        let patched = compiler.apply(&source).unwrap();

        assert!(compiler.is_valid());
        assert_eq!(patched.read_all_bytes(), vec![0, 0, 0xAA, 0]);
    }
}

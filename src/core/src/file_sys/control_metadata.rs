// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Ported from: core/file_sys/control_metadata.h and control_metadata.cpp
// NACP control metadata parsing.

use super::vfs::vfs_types::VirtualFile;

// ============================================================================
// Constants
// ============================================================================

/// Language name strings, indexed by Language enum.
/// Corresponds to upstream `LANGUAGE_NAMES`.
pub const LANGUAGE_NAMES: [&str; Language::Count as usize] = [
    "AmericanEnglish",
    "BritishEnglish",
    "Japanese",
    "French",
    "German",
    "LatinAmericanSpanish",
    "Spanish",
    "Italian",
    "Dutch",
    "CanadianFrench",
    "Portuguese",
    "Russian",
    "Korean",
    "TraditionalChinese",
    "SimplifiedChinese",
    "BrazilianPortuguese",
    "Polish",
    "Thai",
];

/// Mapping from system language index to NACP language code.
/// Mechanical table form of upstream GetLanguageEntry's language switch.
const LANGUAGE_TO_CODES: [Language; 20] = [
    Language::Japanese,
    Language::AmericanEnglish,
    Language::French,
    Language::German,
    Language::Italian,
    Language::Spanish,
    Language::SimplifiedChinese,
    Language::Korean,
    Language::Dutch,
    Language::Portuguese,
    Language::Russian,
    Language::TraditionalChinese,
    Language::BritishEnglish,
    Language::CanadianFrench,
    Language::LatinAmericanSpanish,
    Language::SimplifiedChinese,
    Language::TraditionalChinese,
    Language::BrazilianPortuguese,
    Language::Polish,
    Language::Thai,
];

const MAX_EXPANDED_LANG_SIZE: usize = std::mem::size_of::<LanguageEntry>() * 32;

// ============================================================================
// Enums
// ============================================================================

/// A language on the NX.
/// Corresponds to upstream `Language`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Language {
    AmericanEnglish = 0,
    BritishEnglish = 1,
    Japanese = 2,
    French = 3,
    German = 4,
    LatinAmericanSpanish = 5,
    Spanish = 6,
    Italian = 7,
    Dutch = 8,
    CanadianFrench = 9,
    Portuguese = 10,
    Russian = 11,
    Korean = 12,
    TraditionalChinese = 13,
    SimplifiedChinese = 14,
    BrazilianPortuguese = 15,
    Polish = 16,
    Thai = 17,
    Count = 18,
    Default = 255,
}

// ============================================================================
// Binary structures
// ============================================================================

/// A localized entry containing strings within the NACP.
/// Corresponds to upstream `LanguageEntry` — 0x300 bytes.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct LanguageEntry {
    pub application_name: [u8; 0x200],
    pub developer_name: [u8; 0x100],
}

const _: () = assert!(std::mem::size_of::<LanguageEntry>() == 0x300);

impl Default for LanguageEntry {
    fn default() -> Self {
        Self {
            application_name: [0u8; 0x200],
            developer_name: [0u8; 0x100],
        }
    }
}

impl LanguageEntry {
    pub fn get_application_name(&self) -> String {
        string_from_fixed_buffer(&self.application_name)
    }

    pub fn get_developer_name(&self) -> String {
        string_from_fixed_buffer(&self.developer_name)
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct CompressedLanguageEntryData {
    pub buffer_size: u16,
    pub buffer: [u8; 0x2FFE],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub union LanguageEntryData {
    pub language_entries: [LanguageEntry; 16],
    pub compressed_data: CompressedLanguageEntryData,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TitleDataFormat {
    Uncompressed = 0,
    Compressed = 1,
}

/// The raw NACP file format — 0x4000 bytes.
/// Corresponds to upstream `RawNACP`.
#[derive(Clone)]
#[repr(C)]
pub struct RawNACP {
    pub language_entries: LanguageEntryData,
    pub isbn: [u8; 0x25],
    pub startup_user_account: u8,
    pub user_account_switch_lock: u8,
    pub addon_content_registration_type: u8,
    pub application_attribute: u32,
    pub supported_languages: u32,
    pub parental_control: u32,
    pub screenshot_enabled: u8,
    pub video_capture_mode: u8,
    pub data_loss_confirmation: u8,
    pub _pad0: u8,
    pub presence_group_id: u64,
    pub rating_age: [u8; 0x20],
    pub version_string: [u8; 0x10],
    pub dlc_base_title_id: u64,
    pub save_data_owner_id: u64,
    pub user_account_save_data_size: u64,
    pub user_account_save_data_journal_size: u64,
    pub device_save_data_size: u64,
    pub device_save_data_journal_size: u64,
    pub bcat_delivery_cache_storage_size: u64,
    pub application_error_code_category: [u8; 8],
    pub local_communication: [u64; 0x8],
    pub logo_type: u8,
    pub logo_handling: u8,
    pub runtime_add_on_content_install: u8,
    pub _pad1: [u8; 5],
    pub seed_for_pseudo_device_id: u64,
    pub bcat_passphrase: [u8; 0x41],
    pub _pad2: [u8; 7],
    pub user_account_save_data_max_size: u64,
    pub user_account_save_data_max_journal_size: u64,
    pub device_save_data_max_size: u64,
    pub device_save_data_max_journal_size: u64,
    pub temporary_storage_size: u64,
    pub cache_storage_size: u64,
    pub cache_storage_journal_size: u64,
    pub cache_storage_data_and_journal_max_size: u64,
    pub cache_storage_max_index: u16,
    pub _pad3: [u8; 0x8B],
    // Raw byte, rather than enum, so unknown on-disk discriminants remain valid
    // Rust data. Upstream treats anything except Compressed as uncompressed.
    pub titles_data_format: u8,
    pub _pad4: [u8; 0xDEA],
}

const _: () = assert!(std::mem::size_of::<RawNACP>() == 0x4000);
const _: () = assert!(std::mem::size_of::<LanguageEntryData>() == 0x3000);
const _: () = assert!(std::mem::offset_of!(RawNACP, titles_data_format) == 0x3215);

// ============================================================================
// NACP
// ============================================================================

/// A class representing the NACP control metadata format.
/// Corresponds to upstream `NACP`.
#[derive(Clone)]
pub struct NACP {
    raw: Box<RawNACP>,
    language_entries: Vec<LanguageEntry>,
}

impl NACP {
    /// Create an empty NACP.
    pub fn new() -> Self {
        Self {
            raw: unsafe { Box::new(std::mem::zeroed()) },
            language_entries: Vec::new(),
        }
    }

    /// Parse NACP from a VFS file.
    pub fn from_file(file: &VirtualFile) -> Self {
        let mut nacp = Self::new();
        let size = std::mem::size_of::<RawNACP>();
        let raw_ptr = nacp.raw.as_mut() as *mut RawNACP as *mut u8;
        let mut buf = vec![0u8; size];
        let read = file.read(&mut buf, size, 0);
        if read > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(buf.as_ptr(), raw_ptr, read.min(size));
            }
        }
        if nacp.raw.titles_data_format == TitleDataFormat::Compressed as u8 {
            let compressed = unsafe { &nacp.raw.language_entries.compressed_data };
            let size = u16::from_le(compressed.buffer_size) as usize;
            if let Some(payload) = compressed.buffer.get(..size) {
                if let Some(decoded) = inflate_raw_deflate(payload) {
                    // Upstream memcpy assumes whole entries. Reject malformed
                    // trailing bytes instead of overflowing a floor-sized vector.
                    if decoded.len() % std::mem::size_of::<LanguageEntry>() == 0 {
                        nacp.language_entries = decoded
                            .chunks_exact(0x300)
                            .map(|bytes| unsafe {
                                std::ptr::read_unaligned(bytes.as_ptr().cast::<LanguageEntry>())
                            })
                            .collect();
                    }
                }
            }
        } else {
            nacp.language_entries = unsafe { nacp.raw.language_entries.language_entries }.to_vec();
        }
        nacp
    }

    /// Get the language entry for the current language setting.
    /// Falls back to the first non-empty entry, then AmericanEnglish.
    ///
    /// Upstream selects the entry using `Settings::values.language_index`; a
    /// hardcoded index 0 means Japanese (the first Switch language code), so
    /// some title would come back as its Japanese
    /// (katakana) name. `settings::values().language_index` is a `Language`
    /// enum whose ordinal matches the Switch language-code index used by
    /// `LANGUAGE_TO_CODES`.
    pub fn get_language_entry(&self) -> &LanguageEntry {
        let index = (*common::settings::values().language_index.get_value()) as usize;
        self.get_language_entry_with_index(index)
    }

    /// Get the language entry for a specific language index (system setting).
    pub fn get_language_entry_with_index(&self, language_index: usize) -> &LanguageEntry {
        let language = LANGUAGE_TO_CODES
            .get(language_index)
            .copied()
            .unwrap_or(Language::AmericanEnglish);

        let lang_idx = language as usize;
        if let Some(entry) = self.language_entries.get(lang_idx) {
            if !entry.get_application_name().is_empty() {
                return entry;
            }
        }

        // Fallback: find first non-empty
        for entry in &self.language_entries {
            if !entry.get_application_name().is_empty() {
                return entry;
            }
        }

        // Final fallback: first decoded entry, or an empty entry if none exist.
        static EMPTY_ENTRY: LanguageEntry = LanguageEntry {
            application_name: [0; 0x200],
            developer_name: [0; 0x100],
        };
        self.language_entries.first().unwrap_or(&EMPTY_ENTRY)
    }

    pub fn get_application_names(&self) -> Vec<String> {
        self.language_entries
            .iter()
            .map(LanguageEntry::get_application_name)
            .collect()
    }

    pub fn get_application_name(&self) -> String {
        self.get_language_entry().get_application_name()
    }

    pub fn get_developer_name(&self) -> String {
        self.get_language_entry().get_developer_name()
    }

    pub fn get_title_id(&self) -> u64 {
        self.raw.save_data_owner_id
    }

    pub fn get_dlc_base_title_id(&self) -> u64 {
        self.raw.dlc_base_title_id
    }

    pub fn get_version_string(&self) -> String {
        string_from_fixed_buffer(&self.raw.version_string)
    }

    pub fn get_default_normal_save_size(&self) -> u64 {
        self.raw.user_account_save_data_size
    }

    pub fn get_default_journal_save_size(&self) -> u64 {
        self.raw.user_account_save_data_journal_size
    }

    pub fn get_user_account_switch_lock(&self) -> bool {
        self.raw.user_account_switch_lock != 0
    }

    pub fn get_supported_languages(&self) -> u32 {
        self.raw.supported_languages
    }

    pub fn get_device_save_data_size(&self) -> u64 {
        self.raw.device_save_data_size
    }

    pub fn get_parental_control_flag(&self) -> u32 {
        self.raw.parental_control
    }

    pub fn get_rating_age(&self) -> &[u8; 0x20] {
        &self.raw.rating_age
    }

    pub fn get_raw_bytes(&self) -> Vec<u8> {
        let size = std::mem::size_of::<RawNACP>();
        let mut out = vec![0u8; size];
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.raw.as_ref() as *const RawNACP as *const u8,
                out.as_mut_ptr(),
                size,
            );
        }
        out
    }
}

impl Default for NACP {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn string_from_fixed_buffer(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

fn inflate_raw_deflate(compressed: &[u8]) -> Option<Vec<u8>> {
    if compressed.is_empty() {
        return None;
    }
    let mut decoder = flate2::Decompress::new(false);
    let mut decoded = vec![0; MAX_EXPANDED_LANG_SIZE];
    let status = decoder
        .decompress(compressed, &mut decoded, flate2::FlushDecompress::Finish)
        .ok()?;
    // Require a complete stream within the same fixed output bound as Eden.
    if status != flate2::Status::StreamEnd {
        return None;
    }
    decoded.truncate(decoder.total_out() as usize);
    Some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_bytes(bytes: Vec<u8>) -> NACP {
        let file: VirtualFile = std::sync::Arc::new(
            super::super::vfs::vfs_vector::VectorVfsFile::new(bytes, "control.nacp".into(), None),
        );
        NACP::from_file(&file)
    }

    fn compressed_nacp(entries: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(entries).unwrap();
        let payload = encoder.finish().unwrap();
        assert!(payload.len() <= 0x2FFE);
        let mut raw = vec![0; 0x4000];
        raw[..2].copy_from_slice(&(payload.len() as u16).to_le_bytes());
        raw[2..2 + payload.len()].copy_from_slice(&payload);
        raw[0x3215] = TitleDataFormat::Compressed as u8;
        raw
    }

    #[test]
    fn compressed_titles_select_extended_languages_and_preserve_raw_bytes() {
        let mut entries = vec![0; 18 * 0x300];
        for (index, name) in [(0, "Homebrew"), (16, "Polish title"), (17, "Thai title")] {
            entries[index * 0x300..index * 0x300 + name.len()].copy_from_slice(name.as_bytes());
        }
        let raw = compressed_nacp(&entries);
        let nacp = parse_bytes(raw.clone());
        assert_eq!(nacp.get_raw_bytes(), raw);
        assert_eq!(nacp.get_application_names().len(), 18);
        assert_eq!(
            nacp.get_language_entry_with_index(18)
                .get_application_name(),
            "Polish title"
        );
        assert_eq!(
            nacp.get_language_entry_with_index(19)
                .get_application_name(),
            "Thai title"
        );
        assert_eq!(
            nacp.get_language_entry_with_index(999)
                .get_application_name(),
            "Homebrew"
        );
        assert_eq!(
            nacp.get_language_entry_with_index(0).get_application_name(),
            "Homebrew"
        );
    }

    #[test]
    fn uncompressed_titles_and_empty_metadata_keep_upstream_fallbacks() {
        let mut raw = vec![0; 0x4000];
        raw[3 * 0x300..3 * 0x300 + 8].copy_from_slice(b"Homebrew");
        let nacp = parse_bytes(raw.clone());
        assert_eq!(nacp.get_raw_bytes(), raw);
        assert_eq!(nacp.get_application_names().len(), 16);
        assert_eq!(
            nacp.get_language_entry_with_index(19)
                .get_application_name(),
            "Homebrew"
        );
        assert!(NACP::new().get_application_names().is_empty());
        assert_eq!(NACP::new().get_application_name(), "");
    }

    #[test]
    fn malformed_compressed_titles_are_bounded_and_do_not_become_raw_titles() {
        assert_eq!(
            parse_bytes(compressed_nacp(&vec![0; MAX_EXPANDED_LANG_SIZE]))
                .get_application_names()
                .len(),
            32
        );
        for entries in [vec![0; 0x301], vec![0; 33 * 0x300]] {
            let nacp = parse_bytes(compressed_nacp(&entries));
            assert!(nacp.get_application_names().is_empty());
            assert_eq!(nacp.get_application_name(), "");
        }
        let mut raw = vec![0; 0x4000];
        raw[0x3215] = 1;
        for size in [0u16, 1, u16::MAX] {
            raw[..2].copy_from_slice(&size.to_le_bytes());
            assert!(parse_bytes(raw.clone()).get_application_names().is_empty());
        }
        let mut truncated = compressed_nacp(&vec![0; 18 * 0x300]);
        let size = u16::from_le_bytes([truncated[0], truncated[1]]);
        truncated[..2].copy_from_slice(&(size - 1).to_le_bytes());
        assert!(parse_bytes(truncated).get_application_names().is_empty());
        assert_eq!(
            std::mem::offset_of!(RawNACP, cache_storage_max_index),
            0x3188
        );
        assert_eq!(std::mem::offset_of!(RawNACP, titles_data_format), 0x3215);
    }

    #[test]
    fn language_names_match_upstream_language_order() {
        assert_eq!(LANGUAGE_NAMES.len(), Language::Count as usize);
        assert_eq!(
            LANGUAGE_NAMES[Language::AmericanEnglish as usize],
            "AmericanEnglish"
        );
        assert_eq!(
            LANGUAGE_NAMES[Language::BrazilianPortuguese as usize],
            "BrazilianPortuguese"
        );
        assert_eq!(LANGUAGE_NAMES[Language::Polish as usize], "Polish");
        assert_eq!(LANGUAGE_NAMES[Language::Thai as usize], "Thai");
    }
}

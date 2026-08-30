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
/// Corresponds to upstream `language_to_codes`.
const LANGUAGE_TO_CODES: [Language; 18] = [
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
];

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
#[derive(Clone)]
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

/// The raw NACP file format — 0x4000 bytes.
/// Corresponds to upstream `RawNACP`.
#[derive(Clone)]
#[repr(C)]
pub struct RawNACP {
    pub language_entries: [LanguageEntry; 16],
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
    pub _pad3: [u8; 0xE76],
}

const _: () = assert!(std::mem::size_of::<RawNACP>() == 0x4000);

// ============================================================================
// NACP
// ============================================================================

/// A class representing the NACP control metadata format.
/// Corresponds to upstream `NACP`.
#[derive(Clone)]
pub struct NACP {
    raw: Box<RawNACP>,
}

impl NACP {
    /// Create an empty NACP.
    pub fn new() -> Self {
        Self {
            raw: unsafe { Box::new(std::mem::zeroed()) },
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
        let index = language_index.min(LANGUAGE_TO_CODES.len() - 1);
        let language = LANGUAGE_TO_CODES[index];

        let lang_idx = language as usize;
        if lang_idx < 16 {
            let entry = &self.raw.language_entries[lang_idx];
            if !entry.get_application_name().is_empty() {
                return entry;
            }
        }

        // Fallback: find first non-empty
        for entry in &self.raw.language_entries {
            if !entry.get_application_name().is_empty() {
                return entry;
            }
        }

        // Final fallback: AmericanEnglish
        &self.raw.language_entries[Language::AmericanEnglish as usize]
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

#[cfg(test)]
mod tests {
    use super::*;

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

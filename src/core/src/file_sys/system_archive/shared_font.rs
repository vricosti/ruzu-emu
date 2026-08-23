// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Ported from: core/file_sys/system_archive/shared_font.h / .cpp
// Status: COMPLETE
//
// Synthesizes shared font system archives. Each font is packed into BFTTF
// format by XOR-encrypting with a known key, matching the upstream
// PackBFTTF function.

use std::sync::Arc;

use crate::file_sys::system_archive::data::{
    font_chinese_simplified, font_chinese_traditional, font_extended_chinese_simplified,
    font_korean, font_nintendo_extended, font_standard,
};
use crate::file_sys::vfs::vfs_types::{VirtualDir, VirtualFile};
use crate::file_sys::vfs::vfs_vector::{VectorVfsDirectory, VectorVfsFile};

/// Expected magic value for BFTTF encryption.
/// Matches upstream `EXPECTED_MAGIC`.
const EXPECTED_MAGIC: u32 = 0x36F81A1E;

/// Expected result value for BFTTF encryption.
/// Matches upstream `EXPECTED_RESULT`.
const EXPECTED_RESULT: u32 = 0x7F9A0218;

/// Pack font data into BFTTF format matching upstream `PackBFTTF`.
///
/// The process:
/// 1. Interpret the raw font bytes as u32 words
/// 2. Prepend a 2-word header: [EXPECTED_MAGIC, encrypted_size]
/// 3. XOR each data word with the derived key
/// 4. Write everything as big-endian u32s into the output buffer
fn pack_bfttf(data: &[u8], name: &str) -> VirtualFile {
    // Interpret the input as u32 words (little-endian from the byte array).
    let num_words = data.len() / 4;
    let mut input_words = vec![0u32; num_words];
    for i in 0..num_words {
        input_words[i] = u32::from_le_bytes([
            data[i * 4],
            data[i * 4 + 1],
            data[i * 4 + 2],
            data[i * 4 + 3],
        ]);
    }

    // Derive key: swap32(EXPECTED_RESULT ^ EXPECTED_MAGIC)
    let key = (EXPECTED_RESULT ^ EXPECTED_MAGIC).swap_bytes();

    // Build the transformed font with 2-word header.
    let transformed_len = input_words.len() + 2;
    let mut transformed = vec![0u32; transformed_len];
    transformed[0] = EXPECTED_MAGIC.swap_bytes();
    transformed[1] = ((input_words.len() as u32) * 4).swap_bytes() ^ key;
    for (i, &word) in input_words.iter().enumerate() {
        transformed[i + 2] = word ^ key;
    }

    // Serialize to bytes.
    let mut output = vec![0u8; transformed_len * 4];
    for (i, &word) in transformed.iter().enumerate() {
        let bytes = word.to_ne_bytes();
        output[i * 4..i * 4 + 4].copy_from_slice(&bytes);
    }

    Arc::new(VectorVfsFile::new(output, name.to_string(), None))
}

/// Synthesize the Nintendo Extended font archive.
///
/// Corresponds to upstream `FontNintendoExtension()`.
pub fn font_nintendo_extension() -> Option<VirtualDir> {
    Some(Arc::new(VectorVfsDirectory::new(
        vec![
            pack_bfttf(
                &font_nintendo_extended::FONT_NINTENDO_EXTENDED,
                "nintendo_ext_003.bfttf",
            ),
            pack_bfttf(
                &font_nintendo_extended::FONT_NINTENDO_EXTENDED,
                "nintendo_ext2_003.bfttf",
            ),
        ],
        vec![],
        String::new(),
        None,
    )))
}

/// Synthesize the Standard font archive.
///
/// Corresponds to upstream `FontStandard()`.
pub fn font_standard() -> Option<VirtualDir> {
    Some(Arc::new(VectorVfsDirectory::new(
        vec![pack_bfttf(
            &font_standard::FONT_STANDARD,
            "nintendo_udsg-r_std_003.bfttf",
        )],
        vec![],
        String::new(),
        None,
    )))
}

/// Synthesize the Korean font archive.
///
/// Corresponds to upstream `FontKorean()`.
pub fn font_korean() -> Option<VirtualDir> {
    Some(Arc::new(VectorVfsDirectory::new(
        vec![pack_bfttf(
            &font_korean::FONT_KOREAN,
            "nintendo_udsg-r_ko_003.bfttf",
        )],
        vec![],
        String::new(),
        None,
    )))
}

/// Synthesize the Chinese Traditional font archive.
///
/// Corresponds to upstream `FontChineseTraditional()`.
pub fn font_chinese_traditional() -> Option<VirtualDir> {
    Some(Arc::new(VectorVfsDirectory::new(
        vec![pack_bfttf(
            &font_chinese_traditional::FONT_CHINESE_TRADITIONAL,
            "nintendo_udjxh-db_zh-tw_003.bfttf",
        )],
        vec![],
        String::new(),
        None,
    )))
}

/// Synthesize the Chinese Simple font archive.
///
/// Corresponds to upstream `FontChineseSimple()`.
pub fn font_chinese_simple() -> Option<VirtualDir> {
    Some(Arc::new(VectorVfsDirectory::new(
        vec![
            pack_bfttf(
                &font_chinese_simplified::FONT_CHINESE_SIMPLIFIED,
                "nintendo_udsg-r_org_zh-cn_003.bfttf",
            ),
            pack_bfttf(
                &font_extended_chinese_simplified::FONT_EXTENDED_CHINESE_SIMPLIFIED,
                "nintendo_udsg-r_ext_zh-cn_003.bfttf",
            ),
        ],
        vec![],
        String::new(),
        None,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_font_nintendo_extension_has_two_files() {
        let dir = font_nintendo_extension().expect("should return Some");
        assert_eq!(dir.get_files().len(), 2);
    }

    #[test]
    fn test_font_standard_has_one_file() {
        let dir = font_standard().expect("should return Some");
        assert_eq!(dir.get_files().len(), 1);
        assert_eq!(
            dir.get_files()[0].get_name(),
            "nintendo_udsg-r_std_003.bfttf"
        );
    }

    #[test]
    fn test_font_chinese_simple_has_two_files() {
        let dir = font_chinese_simple().expect("should return Some");
        assert_eq!(dir.get_files().len(), 2);
    }

    #[test]
    fn test_pack_bfttf_starts_with_magic() {
        let file = pack_bfttf(&[0u8; 16], "test.bfttf");
        let data = file.read_all_bytes();
        // First 4 bytes should be swap32(EXPECTED_MAGIC)
        let first_word = u32::from_ne_bytes([data[0], data[1], data[2], data[3]]);
        assert_eq!(first_word, EXPECTED_MAGIC.swap_bytes());
    }

    #[test]
    fn test_pack_bfttf_size() {
        // 16 bytes input = 4 words. Output = (4 + 2) * 4 = 24 bytes.
        let file = pack_bfttf(&[0u8; 16], "test.bfttf");
        assert_eq!(file.get_size(), 24);
    }

    #[test]
    fn test_pack_bfttf_round_trips_through_platform_decrypt() {
        let input = &font_standard::FONT_STANDARD[..0x40];
        let file = pack_bfttf(input, "test.bfttf");
        let data = file.read_all_bytes();
        let words: Vec<u32> = data
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]).swap_bytes())
            .collect();
        let mut output = vec![0u8; input.len()];

        assert!(
            crate::hle::service::ns::platform_service_manager::decrypt_shared_font_to_ttf(
                &words,
                &mut output
            )
        );
        assert_eq!(output, input);
    }
}

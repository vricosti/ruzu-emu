// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Ported from: core/file_sys/system_archive/ng_word.h / .cpp
// Status: COMPLETE
//
// Synthesizes the NgWord1 and NgWord2 system archives with placeholder
// filter data matching upstream behavior.

use std::sync::Arc;

use crate::file_sys::vfs::vfs_types::{VirtualDir, VirtualFile};
use crate::file_sys::vfs::vfs_vector::{make_array_file, VectorVfsDirectory};

// ============================================================================
// NgWord1 data
// ============================================================================

mod ng_word_1_data {
    /// Number of numbered word txt files (0x00 .. 0x0F).
    pub const NUMBER_WORD_TXT_FILES: usize = 0x10;

    /// version.dat — corresponds to 11.0.1 System Version.
    pub const VERSION_DAT: [u8; 4] = [0x00, 0x00, 0x00, 0x20];

    /// Placeholder word list in UTF-16BE: "^verybadword$\n"
    pub const WORD_TXT: [u8; 30] = [
        0xFE, 0xFF, 0x00, 0x5E, 0x00, 0x76, 0x00, 0x65, 0x00, 0x72, 0x00, 0x79, 0x00, 0x62, 0x00,
        0x61, 0x00, 0x64, 0x00, 0x77, 0x00, 0x6F, 0x00, 0x72, 0x00, 0x64, 0x00, 0x24, 0x00, 0x0A,
    ];
}

/// Synthesize the NgWord1 archive.
///
/// Creates a directory "data" containing 0x10 numbered .txt files,
/// a common.txt, and version.dat — matching upstream exactly.
pub fn ng_word_1() -> Option<VirtualDir> {
    let mut files: Vec<VirtualFile> = Vec::with_capacity(ng_word_1_data::NUMBER_WORD_TXT_FILES + 2);

    for i in 0..ng_word_1_data::NUMBER_WORD_TXT_FILES {
        files.push(make_array_file(
            ng_word_1_data::WORD_TXT.to_vec(),
            format!("{}.txt", i),
            None,
        ));
    }

    files.push(make_array_file(
        ng_word_1_data::WORD_TXT.to_vec(),
        "common.txt".to_string(),
        None,
    ));
    files.push(make_array_file(
        ng_word_1_data::VERSION_DAT.to_vec(),
        "version.dat".to_string(),
        None,
    ));

    Some(Arc::new(VectorVfsDirectory::new(
        files,
        vec![],
        "data".to_string(),
        None,
    )))
}

// ============================================================================
// NgWord2 data
// ============================================================================

mod ng_word_2_data {
    /// Number of AC NX file triplets.
    pub const NUMBER_AC_NX_FILES: usize = 0x10;

    /// version.dat — corresponds to 11.0.1 System Version.
    pub const VERSION_DAT: [u8; 4] = [0x00, 0x00, 0x00, 0x1A];

    /// Compressed AC NX data — deserializes to no bad words.
    pub const AC_NX_DATA: [u8; 0x2C] = [
        0x1F, 0x8B, 0x08, 0x08, 0xD5, 0x2C, 0x09, 0x5C, 0x04, 0x00, 0x61, 0x63, 0x72, 0x61, 0x77,
        0x00, 0xED, 0xC1, 0x01, 0x0D, 0x00, 0x00, 0x00, 0xC2, 0x20, 0xFB, 0xA7, 0xB6, 0xC7, 0x07,
        0x0C, 0x00, 0x00, 0x00, 0xC8, 0x3B, 0x11, 0x00, 0x1C, 0xC7, 0x00, 0x10, 0x00, 0x00,
    ];
}

/// Synthesize the NgWord2 archive.
///
/// Creates a directory "data" containing triplets of ac_N_b1_nx, ac_N_b2_nx,
/// ac_N_not_b_nx files for N in 0..0x10, plus common variants and version.dat.
pub fn ng_word_2() -> Option<VirtualDir> {
    let mut files: Vec<VirtualFile> =
        Vec::with_capacity(ng_word_2_data::NUMBER_AC_NX_FILES * 3 + 4);

    for i in 0..ng_word_2_data::NUMBER_AC_NX_FILES {
        files.push(make_array_file(
            ng_word_2_data::AC_NX_DATA.to_vec(),
            format!("ac_{}_b1_nx", i),
            None,
        ));
        files.push(make_array_file(
            ng_word_2_data::AC_NX_DATA.to_vec(),
            format!("ac_{}_b2_nx", i),
            None,
        ));
        files.push(make_array_file(
            ng_word_2_data::AC_NX_DATA.to_vec(),
            format!("ac_{}_not_b_nx", i),
            None,
        ));
    }

    files.push(make_array_file(
        ng_word_2_data::AC_NX_DATA.to_vec(),
        "ac_common_b1_nx".to_string(),
        None,
    ));
    files.push(make_array_file(
        ng_word_2_data::AC_NX_DATA.to_vec(),
        "ac_common_b2_nx".to_string(),
        None,
    ));
    files.push(make_array_file(
        ng_word_2_data::AC_NX_DATA.to_vec(),
        "ac_common_not_b_nx".to_string(),
        None,
    ));
    files.push(make_array_file(
        ng_word_2_data::VERSION_DAT.to_vec(),
        "version.dat".to_string(),
        None,
    ));

    Some(Arc::new(VectorVfsDirectory::new(
        files,
        vec![],
        "data".to_string(),
        None,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ng_word_1_file_count() {
        let dir = ng_word_1().expect("ng_word_1 should return Some");
        assert_eq!(dir.get_name(), "data");
        // 0x10 numbered files + common.txt + version.dat = 18
        assert_eq!(dir.get_files().len(), 18);
    }

    #[test]
    fn test_ng_word_1_has_version_dat() {
        let dir = ng_word_1().unwrap();
        assert!(dir.get_file("version.dat").is_some());
    }

    #[test]
    fn test_ng_word_1_has_common_txt() {
        let dir = ng_word_1().unwrap();
        assert!(dir.get_file("common.txt").is_some());
    }

    #[test]
    fn test_ng_word_2_file_count() {
        let dir = ng_word_2().expect("ng_word_2 should return Some");
        assert_eq!(dir.get_name(), "data");
        // 0x10 * 3 + 3 common + 1 version.dat = 52
        assert_eq!(dir.get_files().len(), 52);
    }

    #[test]
    fn test_ng_word_2_has_common_files() {
        let dir = ng_word_2().unwrap();
        assert!(dir.get_file("ac_common_b1_nx").is_some());
        assert!(dir.get_file("ac_common_b2_nx").is_some());
        assert!(dir.get_file("ac_common_not_b_nx").is_some());
    }
}

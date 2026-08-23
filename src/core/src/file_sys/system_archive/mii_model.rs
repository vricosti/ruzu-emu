// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Ported from: core/file_sys/system_archive/mii_model.h / .cpp
// Status: COMPLETE
//
// Synthesizes the MiiModel system archive with placeholder NFTR/NFSR data.

use std::sync::Arc;

use crate::file_sys::vfs::vfs_types::VirtualDir;
use crate::file_sys::vfs::vfs_vector::{make_array_file, VectorVfsDirectory};

/// NFTR standard header — 'N','F','T','R' + version 1 + padding.
const NFTR_STANDARD: [u8; 0x10] = [
    b'N', b'F', b'T', b'R', 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// NFSR standard header — 'N','F','S','R' + version 1 + padding.
const NFSR_STANDARD: [u8; 0x10] = [
    b'N', b'F', b'S', b'R', 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Synthesize the Mii model archive.
///
/// Creates a VirtualDir "data" containing placeholder texture and shape files:
/// - NXTextureLowLinear.dat (NFTR)
/// - NXTextureLowSRGB.dat (NFTR)
/// - NXTextureMidLinear.dat (NFTR)
/// - NXTextureMidSRGB.dat (NFTR)
/// - ShapeHigh.dat (NFSR)
/// - ShapeMid.dat (NFSR)
pub fn mii_model() -> Option<VirtualDir> {
    let dir = Arc::new(VectorVfsDirectory::new(
        vec![],
        vec![],
        "data".to_string(),
        None,
    ));

    dir.add_file(make_array_file(
        NFTR_STANDARD.to_vec(),
        "NXTextureLowLinear.dat".to_string(),
        None,
    ));
    dir.add_file(make_array_file(
        NFTR_STANDARD.to_vec(),
        "NXTextureLowSRGB.dat".to_string(),
        None,
    ));
    dir.add_file(make_array_file(
        NFTR_STANDARD.to_vec(),
        "NXTextureMidLinear.dat".to_string(),
        None,
    ));
    dir.add_file(make_array_file(
        NFTR_STANDARD.to_vec(),
        "NXTextureMidSRGB.dat".to_string(),
        None,
    ));
    dir.add_file(make_array_file(
        NFSR_STANDARD.to_vec(),
        "ShapeHigh.dat".to_string(),
        None,
    ));
    dir.add_file(make_array_file(
        NFSR_STANDARD.to_vec(),
        "ShapeMid.dat".to_string(),
        None,
    ));

    Some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mii_model_has_six_files() {
        let dir = mii_model().expect("mii_model should return Some");
        assert_eq!(dir.get_name(), "data");
        assert_eq!(dir.get_files().len(), 6);
    }

    #[test]
    fn test_mii_model_file_names() {
        let dir = mii_model().unwrap();
        let files = dir.get_files();
        let names: Vec<String> = files.iter().map(|f| f.get_name()).collect();
        assert!(names.contains(&"NXTextureLowLinear.dat".to_string()));
        assert!(names.contains(&"ShapeHigh.dat".to_string()));
        assert!(names.contains(&"ShapeMid.dat".to_string()));
    }
}

// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Ported from: core/file_sys/system_archive/time_zone_binary.h / .cpp
// Status: COMPLETE

use std::sync::Arc;

use super::data::time_zone_binary as nx_tzdb;
use crate::file_sys::vfs::vfs_types::{VirtualDir, VirtualFile};
use crate::file_sys::vfs::vfs_vector::{make_array_file, VectorVfsDirectory};

use nx_tzdb::EmbeddedFile;

/// Corresponds to upstream `tzdb_zoneinfo_dirs`.
const TZDB_ZONEINFO_DIRS: &[(&str, &[EmbeddedFile])] = &[
    ("Africa", nx_tzdb::AFRICA),
    ("America", nx_tzdb::AMERICA),
    ("Antarctica", nx_tzdb::ANTARCTICA),
    ("Arctic", nx_tzdb::ARCTIC),
    ("Asia", nx_tzdb::ASIA),
    ("Atlantic", nx_tzdb::ATLANTIC),
    ("Australia", nx_tzdb::AUSTRALIA),
    ("Brazil", nx_tzdb::BRAZIL),
    ("Canada", nx_tzdb::CANADA),
    ("Chile", nx_tzdb::CHILE),
    ("Etc", nx_tzdb::ETC),
    ("Europe", nx_tzdb::EUROPE),
    ("Indian", nx_tzdb::INDIAN),
    ("Mexico", nx_tzdb::MEXICO),
    ("Pacific", nx_tzdb::PACIFIC),
    ("US", nx_tzdb::US),
];

/// Corresponds to upstream `tzdb_america_dirs`.
const TZDB_AMERICA_DIRS: &[(&str, &[EmbeddedFile])] = &[
    ("Argentina", nx_tzdb::AMERICA_ARGENTINA),
    ("Indiana", nx_tzdb::AMERICA_INDIANA),
    ("Kentucky", nx_tzdb::AMERICA_KENTUCKY),
    ("North_Dakota", nx_tzdb::AMERICA_NORTH_DAKOTA),
];

/// Corresponds to upstream `GenerateFiles`.
fn generate_files(files: &[EmbeddedFile]) -> Vec<VirtualFile> {
    files
        .iter()
        .map(|file| make_array_file(file.data.to_vec(), file.name.to_string(), None))
        .collect()
}

/// Corresponds to upstream `GenerateZoneinfoFiles`.
fn generate_zoneinfo_files() -> Vec<VirtualFile> {
    generate_files(nx_tzdb::ZONEINFO)
}

/// Synthesize the `TimeZoneBinary` system archive.
///
/// Corresponds to upstream `FileSys::SystemArchive::TimeZoneBinary`.
pub fn time_zone_binary() -> Option<VirtualDir> {
    let america_sub_dirs = TZDB_AMERICA_DIRS
        .iter()
        .map(|(dir_name, files)| {
            Arc::new(VectorVfsDirectory::new(
                generate_files(files),
                Vec::new(),
                (*dir_name).to_string(),
                None,
            )) as VirtualDir
        })
        .collect::<Vec<_>>();

    let mut america_sub_dirs = Some(america_sub_dirs);
    let zoneinfo_sub_dirs = TZDB_ZONEINFO_DIRS
        .iter()
        .map(|(dir_name, files)| {
            let sub_dirs = if *dir_name == "America" {
                america_sub_dirs.take().unwrap_or_default()
            } else {
                Vec::new()
            };
            Arc::new(VectorVfsDirectory::new(
                generate_files(files),
                sub_dirs,
                (*dir_name).to_string(),
                None,
            )) as VirtualDir
        })
        .collect::<Vec<_>>();

    let zoneinfo_dir: VirtualDir = Arc::new(VectorVfsDirectory::new(
        generate_zoneinfo_files(),
        zoneinfo_sub_dirs,
        "zoneinfo".to_string(),
        None,
    ));

    Some(Arc::new(VectorVfsDirectory::new(
        generate_files(nx_tzdb::BASE),
        vec![zoneinfo_dir],
        "data".to_string(),
        None,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_EMBEDDED_GROUPS: &[&[EmbeddedFile]] = &[
        nx_tzdb::BASE,
        nx_tzdb::ZONEINFO,
        nx_tzdb::AFRICA,
        nx_tzdb::AMERICA,
        nx_tzdb::AMERICA_ARGENTINA,
        nx_tzdb::AMERICA_INDIANA,
        nx_tzdb::AMERICA_KENTUCKY,
        nx_tzdb::AMERICA_NORTH_DAKOTA,
        nx_tzdb::ANTARCTICA,
        nx_tzdb::ARCTIC,
        nx_tzdb::ASIA,
        nx_tzdb::ATLANTIC,
        nx_tzdb::AUSTRALIA,
        nx_tzdb::BRAZIL,
        nx_tzdb::CANADA,
        nx_tzdb::CHILE,
        nx_tzdb::ETC,
        nx_tzdb::EUROPE,
        nx_tzdb::INDIAN,
        nx_tzdb::MEXICO,
        nx_tzdb::PACIFIC,
        nx_tzdb::US,
    ];

    #[test]
    fn embedded_data_matches_upstream_nx_tzdb_221202_archive() {
        assert_eq!(
            ALL_EMBEDDED_GROUPS
                .iter()
                .map(|group| group.len())
                .sum::<usize>(),
            599
        );
        assert_eq!(
            ALL_EMBEDDED_GROUPS
                .iter()
                .flat_map(|group| group.iter())
                .map(|file| file.data.len())
                .sum::<usize>(),
            314_337
        );
    }

    #[test]
    fn time_zone_binary_matches_upstream_archive_shape() {
        let root = time_zone_binary().expect("embedded archive is always available");
        assert_eq!(root.get_name(), "data");

        let root_files = root.get_files();
        assert_eq!(
            root_files
                .iter()
                .map(|file| file.get_name())
                .collect::<Vec<_>>(),
            vec!["binaryList.txt", "version.txt"]
        );

        let zoneinfo = root
            .get_subdirectory("zoneinfo")
            .expect("zoneinfo directory");
        assert_eq!(zoneinfo.get_subdirectories().len(), 16);

        let america = zoneinfo
            .get_subdirectory("America")
            .expect("America directory");
        assert_eq!(
            america
                .get_subdirectories()
                .iter()
                .map(|directory| directory.get_name())
                .collect::<Vec<_>>(),
            vec!["Argentina", "Indiana", "Kentucky", "North_Dakota"]
        );
    }

    #[test]
    fn time_zone_binary_contains_switch_format_paris_rule() {
        let root = time_zone_binary().expect("embedded archive is always available");
        let paris = root
            .get_file_relative("/zoneinfo/Europe/Paris")
            .expect("Europe/Paris rule");
        let bytes = paris.read_all_bytes();

        assert_eq!(&bytes[..4], b"TZif");
        assert!(root
            .get_file_relative("/binaryList.txt")
            .expect("binary list")
            .read_all_bytes()
            .windows(b"Europe/Paris".len())
            .any(|window| window == b"Europe/Paris"));
        assert_eq!(
            root.get_file_relative("/version.txt")
                .expect("version")
                .read_all_bytes(),
            b"221202\n"
        );
    }
}

// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Ported from: Fritchii (Eden) core/loader/nup.h and nup.cpp — loader for the
// NUP (NX Package) container format.
//
// Loads a NUP container: loose ExeFS/RomFS trees for a base program plus its
// updates in one file. The newest update package is layered over the base;
// add-on content packages are recognized but not yet mounted.

use crate::file_sys::control_metadata::NACP as FileSysNACP;
use crate::file_sys::nup_container::{NupContainer, PackageInfo, PackageKind};
use crate::file_sys::romfs::create_romfs;
use crate::file_sys::vfs::vfs_types::VirtualFile;

use super::deconstructed_rom_directory::AppLoaderDeconstructedRomDirectory;
use super::loader::{
    AppLoader, FileType, FileTypeIdentifier, KProcess, LoadResult, Modules, ResultStatus, System,
    NACP,
};

// ============================================================================
// NACP building helpers
// ============================================================================

/// NACP stores 16 uncompressed language entries; map BCP-47 tags onto them.
///
/// Maps to upstream `LanguageSlot` in `nup.cpp`.
struct LanguageSlot {
    tag: &'static str,
    index: usize,
}

/// Maps to upstream `LanguageSlots`.
const LANGUAGE_SLOTS: [LanguageSlot; 18] = [
    LanguageSlot {
        tag: "en-us",
        index: 0,
    },
    LanguageSlot {
        tag: "en-gb",
        index: 1,
    },
    LanguageSlot {
        tag: "ja",
        index: 2,
    },
    LanguageSlot {
        tag: "fr",
        index: 3,
    },
    LanguageSlot {
        tag: "de",
        index: 4,
    },
    LanguageSlot {
        tag: "es-419",
        index: 5,
    },
    LanguageSlot {
        tag: "es",
        index: 6,
    },
    LanguageSlot {
        tag: "it",
        index: 7,
    },
    LanguageSlot {
        tag: "nl",
        index: 8,
    },
    LanguageSlot {
        tag: "fr-ca",
        index: 9,
    },
    LanguageSlot {
        tag: "pt",
        index: 10,
    },
    LanguageSlot {
        tag: "ru",
        index: 11,
    },
    LanguageSlot {
        tag: "ko",
        index: 12,
    },
    LanguageSlot {
        tag: "zh-hant",
        index: 13,
    },
    LanguageSlot {
        tag: "zh-hans",
        index: 14,
    },
    LanguageSlot {
        tag: "pt-br",
        index: 15,
    },
    LanguageSlot {
        tag: "pl",
        index: 16,
    },
    LanguageSlot {
        tag: "th",
        index: 17,
    },
];

/// Copies a NUL-terminated name into the fixed application-name buffer.
///
/// Maps to upstream `CopyNameToEntry`.
fn copy_name_to_entry(nacp: &mut FileSysNACP, index: usize, name: &str) {
    nacp.set_language_entry_name(index, name);
}

/// Copies a NUL-terminated developer string into the fixed buffer.
///
/// Maps to upstream `CopyDeveloperToEntry`.
fn copy_developer_to_entry(nacp: &mut FileSysNACP, index: usize, developer: &str) {
    nacp.set_language_entry_developer(index, developer);
}

/// Builds a NACP from the NUP package meta TLVs and the display version.
///
/// Maps to upstream `BuildNacp` in `nup.cpp`.
///
/// Rust adaptation: upstream writes `nacp->raw.…` directly; the setters on
/// `control_metadata::NACP` are the equivalent owned API (see DIFF.md).
fn build_nacp(base: &PackageInfo, version: &str) -> Option<NACP> {
    let effective_title = if !base.title_name.is_empty() {
        base.title_name.clone()
    } else {
        base.source_dump.clone()
    };
    if effective_title.is_empty()
        && base.localized_titles.is_empty()
        && base.publisher.is_empty()
        && version.is_empty()
    {
        return None;
    }
    let mut nacp = FileSysNACP::new();
    nacp.set_supported_languages(0x3FFFF);

    for (lang, name) in &base.localized_titles {
        for slot in &LANGUAGE_SLOTS {
            // The uncompressed RawNACP only stores 16 entries; Polish/Thai
            // exist only in the compressed format, so they fall back to the
            // default.
            if slot.index < 16 && slot.tag == lang.as_str() {
                copy_name_to_entry(&mut nacp, slot.index, name);
            }
        }
    }
    if !effective_title.is_empty() {
        copy_name_to_entry(&mut nacp, 0, &effective_title);
    }
    if !base.publisher.is_empty() {
        for index in 0..16 {
            copy_developer_to_entry(&mut nacp, index, &base.publisher);
        }
    }
    if !version.is_empty() {
        nacp.set_version_string(version);
    }
    nacp.set_save_data_owner_id(base.title_id);

    nacp.sync_language_entries_from_raw(16);
    Some(nacp)
}

/// Formats a packed u32 version (mmmmmmmm.vvvvpppp is upstream's packed
/// `mmcvpp` layout) as "major.minor.patch".
///
/// Maps to upstream `DecodeVersion` in `nup.cpp`.
fn decode_version(version: u32) -> String {
    if version == 0 {
        return String::new();
    }
    format!(
        "{}.{}.{}",
        version >> 16,
        (version >> 8) & 0xFF,
        version & 0xFF
    )
}

// ============================================================================
// AppLoaderNup
// ============================================================================

/// Loads a NUP (NX Package) container.
///
/// Maps to upstream `Loader::AppLoader_NUP`.
pub struct AppLoaderNup {
    file: VirtualFile,
    is_loaded: bool,
    nup: NupContainer,
    has_program: bool,
    secondary_loader: Option<Box<dyn AppLoader>>,
    nacp_file: Option<FileSysNACP>,
    icon_file: Option<VirtualFile>,
    base_title_id: u64,
    display_title: String,
    display_version: String,
}

impl FileTypeIdentifier for AppLoaderNup {
    /// Identifies whether or not the given file is a NUP container.
    ///
    /// Maps to upstream `AppLoader_NUP::IdentifyType`.
    fn identify_type(nup_file: &VirtualFile) -> FileType {
        if NupContainer::is_nup_file(nup_file) {
            FileType::NUP
        } else {
            FileType::Error
        }
    }
}

impl AppLoaderNup {
    /// Create a new NUP loader.
    ///
    /// Maps to upstream `AppLoader_NUP::AppLoader_NUP`.
    pub fn new(file: VirtualFile) -> Self {
        let nup = NupContainer::new(Some(file.clone()));
        let mut this = Self {
            file,
            is_loaded: false,
            has_program: false,
            nup,
            secondary_loader: None,
            nacp_file: None,
            icon_file: None,
            base_title_id: 0,
            display_title: String::new(),
            display_version: String::new(),
        };
        if this.nup.get_status() != ResultStatus::Success {
            return this;
        }

        let base_index = this.nup.find_base_package();
        let update_index = this.nup.find_best_update_package();

        // The program comes from the base package; a standalone update
        // container (SPEC §4.1.2) carries a complete patched tree and boots on
        // its own.
        let base_with_exefs = base_index.filter(|base| this.nup.get_exefs_dir(*base).is_some());
        let update_with_exefs =
            update_index.filter(|update| this.nup.get_exefs_dir(*update).is_some());
        let program_index: Option<usize>;
        let exefs_index: usize;
        if let Some(base) = base_with_exefs {
            program_index = Some(base);
            exefs_index = update_with_exefs.unwrap_or(base);
        } else if let Some(update) = update_with_exefs {
            log::info!("NUP container is a standalone update; booting the update program directly");
            program_index = Some(update);
            exefs_index = update;
        } else {
            log::error!("NUP container has no package with a usable ExeFS");
            return this;
        }
        let program_index = program_index.expect("program_index set with exefs_index");

        let program = this.nup.get_packages()[program_index].clone();
        this.base_title_id = program.title_id;
        this.display_title = program.title_name.clone();
        this.has_program = true;

        let Some(exefs) = this.nup.get_exefs_dir(exefs_index) else {
            log::error!("NUP package has no usable ExeFS");
            return this;
        };
        if exefs.get_file("main").is_none() || exefs.get_file("main.npdm").is_none() {
            log::error!("NUP package has no usable ExeFS");
            return this;
        }
        this.secondary_loader = Some(Box::new(
            AppLoaderDeconstructedRomDirectory::new_from_directory(exefs, true, false),
        ));

        let version = if let Some(update) = update_index {
            let update = &this.nup.get_packages()[update];
            if update.display_version.is_empty() {
                decode_version(update.version)
            } else {
                update.display_version.clone()
            }
        } else {
            if program.display_version.is_empty() {
                decode_version(program.version)
            } else {
                program.display_version.clone()
            }
        };
        this.display_version = version.clone();
        this.nacp_file = build_nacp(&program, &version);
        if !program.icon.is_empty() {
            this.icon_file = Some(std::sync::Arc::new(
                crate::file_sys::vfs::vfs_vector::VectorVfsFile::new(
                    program.icon.clone(),
                    "icon.dat".to_owned(),
                    None,
                ),
            ));
        } else {
            for (lang, icon) in &program.localized_icons {
                if lang == "en-us" {
                    this.icon_file = Some(std::sync::Arc::new(
                        crate::file_sys::vfs::vfs_vector::VectorVfsFile::new(
                            icon.clone(),
                            format!("icon_{lang}.dat"),
                            None,
                        ),
                    ));
                }
            }
            if this.icon_file.is_none() {
                if let Some((lang, icon)) = program.localized_icons.first() {
                    this.icon_file = Some(std::sync::Arc::new(
                        crate::file_sys::vfs::vfs_vector::VectorVfsFile::new(
                            icon.clone(),
                            format!("icon_{lang}.dat"),
                            None,
                        ),
                    ));
                }
            }
        }

        let addon_count = this
            .nup
            .get_packages()
            .iter()
            .filter(|package| package.kind == PackageKind::Addon)
            .count();
        if addon_count > 0 {
            log::warn!(
                "NUP container has {addon_count} add-on content package(s); AoC mounting is not supported yet"
            );
        }
        this
    }
}

impl AppLoader for AppLoaderNup {
    fn get_file_type(&self) -> FileType {
        FileType::NUP
    }

    /// Maps to upstream `AppLoader_NUP::Load`. Process/filesystem registration
    /// is performed by `System::load` in the Rust port (see nsp.rs).
    fn load(&mut self, process: &mut KProcess, system: &mut System) -> LoadResult {
        if self.is_loaded {
            return (ResultStatus::ErrorAlreadyLoaded, None);
        }
        if self.nup.get_status() != ResultStatus::Success {
            return (self.nup.get_status(), None);
        }
        if !self.has_program {
            return (ResultStatus::ErrorNUPNoBasePackage, None);
        }
        let Some(secondary) = self.secondary_loader.as_mut() else {
            return (ResultStatus::ErrorNUPNoBasePackage, None);
        };

        let result = secondary.load(process, system);
        if result.0 == ResultStatus::Success {
            self.is_loaded = true;
        }
        result
    }

    /// Maps to upstream `AppLoader_NUP::VerifyIntegrity`.
    fn verify_integrity(&self, progress_callback: &dyn Fn(usize, usize) -> bool) -> ResultStatus {
        if self.nup.get_status() != ResultStatus::Success {
            return self.nup.get_status();
        }
        self.nup.verify_integrity(Some(progress_callback))
    }

    /// Maps to upstream `AppLoader_NUP::ReadRomFS`.
    fn read_rom_fs(&self, out_file: &mut Option<VirtualFile>) -> ResultStatus {
        let base_romfs = self
            .nup
            .find_base_package()
            .and_then(|index| self.nup.get_romfs_dir(index));
        let update_romfs = self
            .nup
            .find_best_update_package()
            .and_then(|index| self.nup.get_romfs_dir(index));
        // A standalone update carries the complete merged tree; a base+update
        // pair overlays the update onto the base (SPEC §4.1.4).
        let merged = NupContainer::merge_shadow_dirs(base_romfs.as_ref(), update_romfs.as_ref());
        if merged.is_none() {
            return ResultStatus::ErrorNoRomFS;
        }
        match create_romfs(merged, None) {
            Some(romfs) => {
                *out_file = Some(romfs);
                ResultStatus::Success
            }
            None => ResultStatus::ErrorNoRomFS,
        }
    }

    /// The update is already merged into the exposed RomFS.
    /// Maps to upstream `AppLoader_NUP::IsRomFSUpdatable`.
    fn is_rom_fs_updatable(&self) -> bool {
        false
    }

    /// Maps to upstream `AppLoader_NUP::ReadProgramId`.
    fn read_program_id(&self, out_program_id: &mut u64) -> ResultStatus {
        if !self.has_program {
            return ResultStatus::ErrorNotInitialized;
        }
        *out_program_id = self.base_title_id;
        ResultStatus::Success
    }

    /// Maps to upstream `AppLoader_NUP::ReadProgramIds`.
    fn read_program_ids(&self, out_program_ids: &mut Vec<u64>) -> ResultStatus {
        if !self.has_program {
            return ResultStatus::ErrorNotInitialized;
        }
        out_program_ids.push(self.base_title_id);
        ResultStatus::Success
    }

    /// Maps to upstream `AppLoader_NUP::ReadIcon`.
    fn read_icon(&self, buffer: &mut Vec<u8>) -> ResultStatus {
        match &self.icon_file {
            Some(icon) => {
                *buffer = icon.read_all_bytes();
                ResultStatus::Success
            }
            None => ResultStatus::ErrorNoIcon,
        }
    }

    /// Maps to upstream `AppLoader_NUP::ReadTitle`.
    fn read_title(&self, title: &mut String) -> ResultStatus {
        if !self.display_title.is_empty() {
            *title = self.display_title.clone();
            return ResultStatus::Success;
        }
        match &self.nacp_file {
            Some(nacp) => {
                *title = nacp.get_application_name();
                if title.is_empty() {
                    ResultStatus::ErrorNoControl
                } else {
                    ResultStatus::Success
                }
            }
            None => ResultStatus::ErrorNoControl,
        }
    }

    /// Maps to upstream `AppLoader_NUP::ReadControlData`.
    fn read_control_data(&self, control: &mut NACP) -> ResultStatus {
        match &self.nacp_file {
            Some(nacp) => {
                *control = nacp.clone();
                ResultStatus::Success
            }
            None => ResultStatus::ErrorNoControl,
        }
    }

    /// Session-profile extension: the NPDM of the effective (update-patched)
    /// ExeFS selects the hardware profile, like the DeconstructedRomDirectory
    /// path.
    fn read_hardware_profile(
        &mut self,
        system: &mut System,
    ) -> Result<crate::hardware_properties::HardwareProfile, ResultStatus> {
        if self.nup.get_status() != ResultStatus::Success {
            return Err(self.nup.get_status());
        }
        if !self.has_program {
            return Err(ResultStatus::ErrorNUPNoBasePackage);
        }
        self.secondary_loader
            .as_mut()
            .ok_or(ResultStatus::ErrorNUPNoBasePackage)?
            .read_hardware_profile(system)
    }

    /// Maps to upstream `AppLoader_NUP::ReadNSOModules`.
    fn read_nso_modules(&self, modules: &mut Modules) -> ResultStatus {
        match &self.secondary_loader {
            Some(loader) => loader.read_nso_modules(modules),
            None => ResultStatus::ErrorNotInitialized,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::file_sys::vfs::vfs_vector::VectorVfsFile;

    #[test]
    fn load_tracks_success_and_allows_retry_after_failure() {
        struct Child(bool);
        impl AppLoader for Child {
            fn get_file_type(&self) -> FileType {
                FileType::DeconstructedRomDirectory
            }
            fn load(&mut self, _: &mut KProcess, _: &mut System) -> LoadResult {
                let success = self.0;
                self.0 = true;
                (
                    if success {
                        ResultStatus::Success
                    } else {
                        ResultStatus::ErrorNotInitialized
                    },
                    None,
                )
            }
        }
        let mut loader = AppLoaderNup::new(container(PackageKind::Base, 0));
        loader.secondary_loader = Some(Box::new(Child(false)));
        let mut process = KProcess::new();
        let mut system = System::new(None, None);
        assert_eq!(
            loader.load(&mut process, &mut system).0,
            ResultStatus::ErrorNotInitialized
        );
        assert!(!loader.is_loaded);
        assert_eq!(
            loader.load(&mut process, &mut system).0,
            ResultStatus::Success
        );
        assert!(loader.is_loaded);
        assert_eq!(
            loader.load(&mut process, &mut system).0,
            ResultStatus::ErrorAlreadyLoaded
        );
    }

    /// Builds a minimal NUP container identical to the one in
    /// `nup_container::tests` (one base package with ExeFS+RomFS and meta
    /// TLVs), then a variant whose only package is a standalone update.
    fn build_container(kind: PackageKind, flags: u8) -> Vec<u8> {
        const MAIN_NPDM: &[u8] = b"npdm-bytes";
        const MAIN_NSO: &[u8] = b"nso-bytes";
        const DATA_BIN: &[u8] = b"romfs-data";
        const TITLE_ID: u64 = 0x0100_4860_24DE_C000;
        const VERSION: u32 = 0x0002_0000; // 2.0.0

        let mut pool: Vec<u8> = Vec::new();
        let mut push_name = |name: &str| -> (u32, u32) {
            let offset = pool.len() as u32;
            pool.extend_from_slice(name.as_bytes());
            (offset, name.len() as u32)
        };
        let (root_off, root_len) = push_name("root");
        let (exefs_off, exefs_len) = push_name("exefs");
        let (romfs_off, romfs_len) = push_name("romfs");
        let (main_off, main_len) = push_name("main");
        let (npdm_off, npdm_len) = push_name("main.npdm");
        let (data_off, data_len) = push_name("data.bin");

        let needed: u64 = 0x30 + 0x40 + 3 * 0x18 + 3 * 0x50;
        let pool_offset = needed + 5 * 4;

        let mut toc: Vec<u8> = Vec::new();
        toc.extend_from_slice(&0x01434F54u32.to_le_bytes());
        toc.extend_from_slice(&1u32.to_le_bytes());
        toc.extend_from_slice(&3u64.to_le_bytes());
        toc.extend_from_slice(&3u64.to_le_bytes());
        toc.extend_from_slice(&pool_offset.to_le_bytes());
        toc.extend_from_slice(&(pool.len() as u64).to_le_bytes());
        toc.extend_from_slice(&0u64.to_le_bytes());

        // Package record with the requested kind/flags.
        toc.extend_from_slice(&TITLE_ID.to_le_bytes());
        toc.extend_from_slice(&0u32.to_le_bytes()); // root_dir
        toc.extend_from_slice(&1u32.to_le_bytes()); // exefs_dir
        toc.extend_from_slice(&2u32.to_le_bytes()); // romfs_dir
        toc.extend_from_slice(&VERSION.to_le_bytes());
        toc.push(kind as u8);
        toc.push(flags);
        toc.extend_from_slice(&[0; 6]);
        toc.extend_from_slice(&0u64.to_le_bytes()); // meta_offset (no meta)
        toc.extend_from_slice(&0u32.to_le_bytes());
        toc.extend_from_slice(&[0; 20]);

        let dir_record =
            |name_offset: u32, name_length: u32, first_child: u32, child_count: u32| {
                let mut record = Vec::new();
                record.extend_from_slice(&name_offset.to_le_bytes());
                record.extend_from_slice(&name_length.to_le_bytes());
                record.extend_from_slice(&first_child.to_le_bytes());
                record.extend_from_slice(&child_count.to_le_bytes());
                record.extend_from_slice(&0u64.to_le_bytes());
                record
            };
        toc.extend_from_slice(&dir_record(root_off, root_len, 0, 2));
        toc.extend_from_slice(&dir_record(exefs_off, exefs_len, 2, 2));
        toc.extend_from_slice(&dir_record(romfs_off, romfs_len, 4, 1));

        let file_record = |name_offset: u32,
                           name_length: u32,
                           size: u64,
                           extent_offset: u64,
                           extent_size: u64,
                           codec: u8,
                           sha: &[u8; 0x20]| {
            let mut record = Vec::new();
            record.extend_from_slice(&name_offset.to_le_bytes());
            record.extend_from_slice(&name_length.to_le_bytes());
            record.extend_from_slice(&size.to_le_bytes());
            record.extend_from_slice(&extent_offset.to_le_bytes());
            record.extend_from_slice(&extent_size.to_le_bytes());
            record.extend_from_slice(sha);
            record.push(codec);
            record.push(0);
            record.extend_from_slice(&[0; 14]);
            record
        };

        let blob_offset = ((0x80 + needed as usize + 20 + pool.len()) + 0xF) & !0xF;
        // Every member extent is 16-byte aligned, like the real writer.
        let main_extent = blob_offset as u64;
        let npdm_extent = (((main_extent as usize) + MAIN_NSO.len()) + 0xF) & !0xF;
        let data_extent = ((npdm_extent + MAIN_NPDM.len()) + 0xF) & !0xF;
        let npdm_extent = npdm_extent as u64;
        let data_extent = data_extent as u64;
        toc.extend_from_slice(&file_record(
            main_off,
            main_len,
            MAIN_NSO.len() as u64,
            main_extent,
            MAIN_NSO.len() as u64,
            0,
            &sha256_bytes(MAIN_NSO),
        ));
        toc.extend_from_slice(&file_record(
            npdm_off,
            npdm_len,
            MAIN_NPDM.len() as u64,
            npdm_extent,
            MAIN_NPDM.len() as u64,
            0,
            &sha256_bytes(MAIN_NPDM),
        ));
        toc.extend_from_slice(&file_record(
            data_off,
            data_len,
            DATA_BIN.len() as u64,
            data_extent,
            DATA_BIN.len() as u64,
            0,
            &sha256_bytes(DATA_BIN),
        ));
        for child in [1u32, 2u32, 3u32, 4u32, 5u32] {
            toc.extend_from_slice(&child.to_le_bytes());
        }
        toc.extend_from_slice(&pool);
        // Pad the TOC so the assembled file really places the blob at
        // blob_offset.
        while toc.len() < blob_offset - 0x80 {
            toc.push(0);
        }

        let mut blob: Vec<u8> = Vec::new();
        blob.extend_from_slice(MAIN_NSO);
        while blob.len() < npdm_extent as usize - blob_offset {
            blob.push(0);
        }
        blob.extend_from_slice(MAIN_NPDM);
        while blob.len() < data_extent as usize - blob_offset {
            blob.push(0);
        }
        blob.extend_from_slice(DATA_BIN);

        let toc_offset = 0x80u64;
        let toc_size = blob_offset as u64 - toc_offset;
        let file_size = blob_offset as u64 + blob.len() as u64;

        let mut superblock: Vec<u8> = Vec::new();
        superblock.extend_from_slice(&[b'N', b'U', b'P', 0x01]);
        superblock.extend_from_slice(&0x80u16.to_le_bytes());
        superblock.extend_from_slice(&1u16.to_le_bytes());
        superblock.extend_from_slice(&toc_offset.to_le_bytes());
        superblock.extend_from_slice(&toc_size.to_le_bytes());
        superblock.extend_from_slice(&sha256_bytes(&toc));
        superblock.extend_from_slice(&(blob_offset as u64).to_le_bytes());
        superblock.extend_from_slice(&file_size.to_le_bytes());
        superblock.extend_from_slice(&1u32.to_le_bytes());
        superblock.extend_from_slice(&0u32.to_le_bytes());
        superblock.extend_from_slice(&[0; 0x10]);
        superblock.extend_from_slice(&[0; 0x20]);
        assert_eq!(superblock.len(), 0x80);

        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&superblock);
        bytes.extend_from_slice(&toc);
        bytes.extend_from_slice(&blob);
        assert_eq!(bytes.len() as u64, file_size);
        bytes
    }

    /// Local SHA-256 helper for test fixtures.
    pub(super) fn sha256_bytes(data: &[u8]) -> [u8; 0x20] {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(data);
        hasher.finalize().into()
    }

    fn container(kind: PackageKind, flags: u8) -> VirtualFile {
        Arc::new(VectorVfsFile::new(
            build_container(kind, flags),
            "test.nup".to_owned(),
            None,
        ))
    }

    #[test]
    fn test_decode_version_matches_upstream_formatting() {
        assert_eq!(decode_version(0), "");
        assert_eq!(decode_version(0x0001_0000), "1.0.0");
        assert_eq!(decode_version(0x0002_0103), "2.1.3");
    }

    #[test]
    fn test_nup_loader_selects_the_base_package_and_exposes_metadata() {
        let loader = AppLoaderNup::new(container(PackageKind::Base, 0));
        assert_eq!(loader.nup.get_status(), ResultStatus::Success);
        assert!(loader.has_program);
        assert_eq!(loader.base_title_id, 0x0100_4860_24DE_C000);
        assert_eq!(loader.get_file_type(), FileType::NUP);

        let mut program_id = 0u64;
        assert_eq!(
            loader.read_program_id(&mut program_id),
            ResultStatus::Success
        );
        assert_eq!(program_id, 0x0100_4860_24DE_C000);
        let mut program_ids = Vec::new();
        assert_eq!(
            loader.read_program_ids(&mut program_ids),
            ResultStatus::Success
        );
        assert_eq!(program_ids, vec![0x0100_4860_24DE_C000]);

        // Version 2.0.0 with no meta TLVs decodes from the packed version.
        assert_eq!(loader.display_version, "2.0.0");
        assert_eq!(
            loader.read_title(&mut String::new()),
            ResultStatus::ErrorNoControl
        );

        let mut icon = Vec::new();
        assert_eq!(loader.read_icon(&mut icon), ResultStatus::ErrorNoIcon);
    }

    #[test]
    fn test_standalone_update_container_boots_the_update() {
        let loader = AppLoaderNup::new(container(
            PackageKind::Update,
            // PACKAGE_FLAG_STANDALONE_UPDATE in nup_container.rs.
            0x01,
        ));
        assert!(loader.has_program);
        assert!(loader.secondary_loader.is_some());
    }

    #[test]
    fn test_container_without_a_base_package_is_rejected() {
        // An Addon-only container has no bootable program.
        let loader = AppLoaderNup::new(container(PackageKind::Addon, 0));
        assert!(!loader.has_program);
        assert!(loader.secondary_loader.is_none());
        let mut out = None;
        assert_eq!(loader.read_rom_fs(&mut out), ResultStatus::ErrorNoRomFS);
    }
}

// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Ported from: core/file_sys/content_archive.h and content_archive.cpp
// NCA (Nintendo Content Archive) parsing.

use std::sync::{Arc, Mutex};

use super::fssystem::compression_configuration::get_nca_compression_configuration;
use super::fssystem::crypto_configuration::get_crypto_configuration;
use super::fssystem::nca_file_system_driver::NcaFileSystemDriver;
use super::fssystem::nca_header::{NcaFsEncryptionType, NcaFsType};
use super::fssystem::nca_reader::{NcaFsHeaderReader, NcaReader};
use super::partition_filesystem::{PartitionFilesystem, ResultStatus};
use super::vfs::vfs::VfsDirectory;
use super::vfs::vfs_types::{VirtualDir, VirtualFile};
use crate::crypto::key_manager::{KeyManager, S128KeyType};

// ============================================================================
// Types and enums
// ============================================================================

/// Describes the type of content within an NCA archive.
/// Corresponds to upstream `NCAContentType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum NCAContentType {
    /// Executable-related data
    Program = 0,
    /// Metadata
    Meta = 1,
    /// Access control data
    Control = 2,
    /// Information related to the game manual
    Manual = 3,
    /// System data
    Data = 4,
    /// Data that can be accessed by applications
    PublicData = 5,
}

/// Rights ID — 16 bytes.
pub type RightsId = [u8; 0x10];

/// Compute the master key ID from key generation.
/// Corresponds to upstream anonymous `MasterKeyIdForKeyGeneration`.
fn master_key_id_for_key_generation(key_generation: u8) -> u8 {
    key_generation.max(1) - 1
}

/// Check whether a VFS directory is an ExeFS partition.
/// Corresponds to upstream `IsDirectoryExeFS`.
pub fn is_directory_exefs(pfs: &VirtualDir) -> bool {
    pfs.get_file("main").is_some() && pfs.get_file("main.npdm").is_some()
}

/// Check whether a VFS directory is a Logo partition.
/// Corresponds to upstream `IsDirectoryLogoPartition`.
pub fn is_directory_logo_partition(pfs: &VirtualDir) -> bool {
    pfs.get_file("NintendoLogo.png").is_some() && pfs.get_file("StartupMovie.gif").is_some()
}

// ============================================================================
// NCA
// ============================================================================

/// An implementation of VfsDirectory that represents a Nintendo Content Archive (NCA) container.
/// After construction, use get_status() to determine if the file is valid and ready to be used.
///
/// Corresponds to upstream `NCA`.
pub struct NCA {
    dirs: Vec<VirtualDir>,
    files: Vec<VirtualFile>,

    romfs: Option<VirtualFile>,
    exefs: Option<VirtualDir>,
    logo: Option<VirtualDir>,
    file: VirtualFile,

    status: ResultStatus,

    is_update: bool,

    reader: Option<Arc<NcaReader>>,
    keys: Arc<Mutex<KeyManager>>,
}

impl NCA {
    /// Construct an NCA from a VFS file.
    ///
    /// Corresponds to upstream `NCA::NCA`.
    pub fn new(file: VirtualFile, base_nca: Option<&NCA>) -> Self {
        Self::new_with_options(file, base_nca, false)
    }

    /// Construct an NCA, optionally allowing an update NCA without a base.
    ///
    /// Corresponds to upstream `NCA::NCA(file, base_nca, allow_missing_base)`.
    pub fn new_with_options(
        file: VirtualFile,
        base_nca: Option<&NCA>,
        allow_missing_base: bool,
    ) -> Self {
        let file_name = file.get_name();
        let keys = KeyManager::instance();
        let mut nca = Self {
            dirs: Vec::new(),
            files: Vec::new(),
            romfs: None,
            exefs: None,
            logo: None,
            file: file.clone(),
            status: ResultStatus::Success,
            is_update: false,
            reader: None,
            keys,
        };

        // Create and initialize the NCA reader.
        let mut reader = NcaReader::new();
        let crypto_cfg = get_crypto_configuration();
        let compression_cfg = get_nca_compression_configuration();

        if let Err(_rc) = reader.initialize(file, &crypto_cfg, &compression_cfg) {
            nca.status = ResultStatus::ErrorBadNCAHeader;
            return nca;
        }

        // Lock the key manager once for all key lookups.
        let keys = nca.keys.lock().unwrap();

        // Ensure we have the proper key area keys to continue.
        let master_key_id = master_key_id_for_key_generation(reader.get_key_generation());
        if !keys.has_key_128(
            S128KeyType::KeyArea,
            master_key_id as u64,
            reader.get_key_index() as u64,
        ) {
            nca.status = ResultStatus::ErrorMissingKeyAreaKey;
            drop(keys);
            nca.reader = Some(Arc::new(reader));
            return nca;
        }

        // Handle rights ID (external titlekey decryption).
        // This must be done before wrapping reader in Arc so we can call
        // set_external_decryption_key on the mutable reader.
        let mut rights_id = [0u8; 0x10];
        rights_id.copy_from_slice(reader.get_rights_id());
        if rights_id != [0u8; 0x10] {
            // External decryption key required; provide it here.
            let rights_id_u128_lo = u64::from_le_bytes(rights_id[..8].try_into().unwrap());
            let rights_id_u128_hi = u64::from_le_bytes(rights_id[8..16].try_into().unwrap());

            let titlekey =
                keys.get_key_128(S128KeyType::Titlekey, rights_id_u128_hi, rights_id_u128_lo);
            if titlekey == [0u8; 16] {
                nca.status = ResultStatus::ErrorMissingTitlekey;
                drop(keys);
                nca.reader = Some(Arc::new(reader));
                return nca;
            }

            if !keys.has_key_128(S128KeyType::Titlekek, master_key_id as u64, 0) {
                nca.status = ResultStatus::ErrorMissingTitlekek;
                drop(keys);
                nca.reader = Some(Arc::new(reader));
                return nca;
            }

            let titlekek = keys.get_key_128(S128KeyType::Titlekek, master_key_id as u64, 0);

            // Decrypt the titlekey using AES-ECB with the titlekek.
            use crate::crypto::aes_util::{AesCipher, Mode, Op};
            let mut cipher = AesCipher::new_128(titlekek, Mode::ECB);
            let mut decrypted_titlekey = [0u8; 16];
            cipher.transcode(&titlekey, &mut decrypted_titlekey, Op::Decrypt);

            // Set the external decryption key on the reader.
            // Corresponds to upstream reader->SetExternalDecryptionKey.
            reader.set_external_decryption_key(&decrypted_titlekey);
        }

        drop(keys);

        let reader = Arc::new(reader);

        // Iterate filesystem sections.
        let fs_count = reader.get_fs_count();
        log::debug!(
            "NCA '{}': fs_count={}, content_type={}",
            file_name,
            fs_count,
            reader.get_content_type()
        );
        let fs = if let Some(base) = base_nca {
            if let Some(ref base_reader) = base.reader {
                NcaFileSystemDriver::with_original(base_reader.clone(), reader.clone())
            } else {
                NcaFileSystemDriver::new(reader.clone())
            }
        } else {
            NcaFileSystemDriver::new(reader.clone())
        };

        for i in 0..fs_count {
            let mut header_reader = NcaFsHeaderReader::new();
            let storage = match fs.open_storage(&mut header_reader, i) {
                Ok(s) => {
                    log::debug!(
                        "NCA '{}': section {} opened OK, fs_type={}, size={}",
                        file_name,
                        i,
                        header_reader.get_fs_type(),
                        s.get_size()
                    );
                    s
                }
                Err(_rc) => {
                    log::error!(
                        "NCA '{}': File reader errored out during read of section {} (rc={:?})",
                        file_name,
                        i,
                        _rc
                    );
                    nca.status = ResultStatus::ErrorBadNCAHeader;
                    nca.reader = Some(reader);
                    return nca;
                }
            };

            // Check filesystem type.
            if header_reader.get_fs_type() == NcaFsType::RomFs as u8 {
                nca.files.push(storage.clone());
                nca.romfs = Some(storage.clone());
            }

            if header_reader.get_fs_type() == NcaFsType::PartitionFs as u8 {
                let pfs = PartitionFilesystem::new(storage.clone());
                log::debug!(
                    "NCA '{}': PFS from section {} status={:?}, files={}",
                    file_name,
                    i,
                    pfs.get_status(),
                    pfs.get_files().len()
                );
                if pfs.get_status() == ResultStatus::Success {
                    let npfs: VirtualDir = Arc::new(pfs);
                    nca.dirs.push(npfs.clone());
                    if is_directory_exefs(&npfs) {
                        nca.exefs = Some(npfs);
                    } else if is_directory_logo_partition(&npfs) {
                        nca.logo = Some(npfs);
                    } else {
                        continue;
                    }
                }
            }

            if header_reader.get_encryption_type() == NcaFsEncryptionType::AesCtrEx as u8 {
                nca.is_update = true;
            }
        }

        if nca.is_update && base_nca.is_none() && !allow_missing_base {
            nca.status = ResultStatus::ErrorMissingBKTRBaseRomFS;
        } else {
            nca.status = ResultStatus::Success;
        }

        nca.reader = Some(reader);
        nca
    }

    pub fn get_status(&self) -> ResultStatus {
        self.status
    }

    pub fn get_type(&self) -> NCAContentType {
        if let Some(ref reader) = self.reader {
            // Safety: content_type values 0-5 map to NCAContentType variants.
            let ct = reader.get_content_type();
            match ct {
                0 => NCAContentType::Program,
                1 => NCAContentType::Meta,
                2 => NCAContentType::Control,
                3 => NCAContentType::Manual,
                4 => NCAContentType::Data,
                5 => NCAContentType::PublicData,
                _ => NCAContentType::Program,
            }
        } else {
            NCAContentType::Program
        }
    }

    pub fn get_title_id(&self) -> u64 {
        let program_id = self
            .reader
            .as_ref()
            .map(|r| r.get_program_id())
            .unwrap_or(0);
        if self.is_update {
            program_id | 0x800
        } else {
            program_id
        }
    }

    pub fn get_rights_id(&self) -> RightsId {
        if let Some(ref reader) = self.reader {
            *reader.get_rights_id()
        } else {
            [0u8; 0x10]
        }
    }

    pub fn get_sdk_version(&self) -> u32 {
        self.reader
            .as_ref()
            .map(|r| r.get_sdk_addon_version())
            .unwrap_or(0)
    }

    pub fn get_key_generation(&self) -> u8 {
        self.reader
            .as_ref()
            .map(|r| r.get_key_generation())
            .unwrap_or(0)
    }

    pub fn is_update(&self) -> bool {
        self.is_update
    }

    pub fn get_romfs(&self) -> Option<VirtualFile> {
        self.romfs.clone()
    }

    pub fn get_exefs(&self) -> Option<VirtualDir> {
        self.exefs.clone()
    }

    pub fn get_base_file(&self) -> VirtualFile {
        self.file.clone()
    }

    pub fn get_logo_partition(&self) -> Option<VirtualDir> {
        self.logo.clone()
    }

    pub fn get_name(&self) -> String {
        self.file.get_name()
    }
}

impl VfsDirectory for NCA {
    fn get_files(&self) -> Vec<VirtualFile> {
        if self.status != ResultStatus::Success {
            return vec![];
        }
        self.files.clone()
    }

    fn get_subdirectories(&self) -> Vec<VirtualDir> {
        if self.status != ResultStatus::Success {
            return vec![];
        }
        self.dirs.clone()
    }

    fn get_name(&self) -> String {
        self.file.get_name()
    }

    fn get_parent_directory(&self) -> Option<VirtualDir> {
        self.file.get_containing_directory()
    }

    fn is_writable(&self) -> bool {
        false
    }

    fn is_readable(&self) -> bool {
        true
    }

    fn create_subdirectory(&self, _name: &str) -> Option<VirtualDir> {
        None
    }

    fn create_file(&self, _name: &str) -> Option<VirtualFile> {
        None
    }

    fn delete_subdirectory(&self, _name: &str) -> bool {
        false
    }

    fn delete_file(&self, _name: &str) -> bool {
        false
    }

    fn rename(&self, _name: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_sys::fssystem::nca_header::NcaHeader;
    use crate::file_sys::vfs::vfs_vector::VectorVfsFile;

    #[test]
    fn missing_key_area_key_matches_upstream_status() {
        let mut header: NcaHeader = unsafe { std::mem::zeroed() };
        header.magic = NcaHeader::MAGIC3;
        header.sdk_addon_version = 0x000B_0000;
        header.key_generation = u8::MAX;
        header.key_generation_2 = u8::MAX;
        header.content_size = NcaHeader::SIZE as u64;

        let header_bytes = unsafe {
            std::slice::from_raw_parts(&header as *const NcaHeader as *const u8, NcaHeader::SIZE)
        }
        .to_vec();
        let file: VirtualFile = Arc::new(VectorVfsFile::new(
            header_bytes,
            "synthetic.nca".to_string(),
            None,
        ));

        let master_key_id = master_key_id_for_key_generation(u8::MAX);
        assert!(!KeyManager::instance().lock().unwrap().has_key_128(
            S128KeyType::KeyArea,
            master_key_id as u64,
            0,
        ));

        let nca = NCA::new(file, None);
        assert_eq!(nca.get_status(), ResultStatus::ErrorMissingKeyAreaKey);
    }

    #[test]
    fn empty_file_is_a_bad_header_not_a_null_file() {
        let file: VirtualFile = Arc::new(VectorVfsFile::new(
            Vec::new(),
            "empty.nca".to_string(),
            None,
        ));

        let nca = NCA::new(file, None);
        assert_eq!(nca.get_status(), ResultStatus::ErrorBadNCAHeader);
    }
}

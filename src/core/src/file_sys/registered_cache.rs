// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Ported from: core/file_sys/registered_cache.h / .cpp

use std::collections::BTreeMap;
use std::sync::Arc;

use super::card_image::XCI;
use super::common_funcs::AOC_TITLE_ID_MASK;
use super::content_archive::{NCAContentType, NCA};
use super::control_metadata::NACP;
use super::nca_metadata::{
    CNMTHeader, ContentRecord, ContentRecordType, OptionalHeader, TitleType, CNMT,
    EMPTY_META_CONTENT_RECORD,
};
use super::partition_filesystem::ResultStatus;
use super::romfs::extract_romfs;
use super::submission_package::NSP;
use super::vfs::vfs::{VfsDirectory, VfsFile};
use super::vfs::vfs_concat::ConcatenatedVfsFile;
use super::vfs::vfs_types::{VirtualDir, VirtualFile};

// ============================================================================
// Private helpers (correspond to anonymous helpers in registered_cache.cpp)
// ============================================================================

/// The size of blocks to use when VFS raw copying into NAND.
/// Corresponds to upstream `VFS_RC_LARGE_COPY_BLOCK`.
const VFS_RC_LARGE_COPY_BLOCK: usize = 0x400000;

/// Check if a directory name follows the two-digit dir format: 000000XX.
/// Corresponds to upstream `FollowsTwoDigitDirFormat`.
fn follows_two_digit_dir_format(name: &str) -> bool {
    name.len() == 8
        && name.starts_with("000000")
        && name[6..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Check if a filename follows the NCA ID format: XXXXXXXX...32hex.nca or .cnmt.nca.
/// Corresponds to upstream `FollowsNcaIdFormat`.
fn follows_nca_id_format(name: &str) -> bool {
    let lower = name.to_lowercase();
    if lower.len() == 36 && lower.ends_with(".nca") {
        lower[..32].chars().all(|c| c.is_ascii_hexdigit())
    } else if lower.len() == 41 && lower.ends_with(".cnmt.nca") {
        lower[..32].chars().all(|c| c.is_ascii_hexdigit())
    } else {
        false
    }
}

/// Convert a hex string to a 16-byte array (NcaId).
/// Parses the first 32 hex chars of the input.
fn hex_string_to_nca_id(hex: &str) -> NcaId {
    let mut id = [0u8; 0x10];
    let hex_bytes = hex.as_bytes();
    for i in 0..0x10 {
        let hi = hex_char_to_nibble(hex_bytes.get(i * 2).copied().unwrap_or(b'0'));
        let lo = hex_char_to_nibble(hex_bytes.get(i * 2 + 1).copied().unwrap_or(b'0'));
        id[i] = (hi << 4) | lo;
    }
    id
}

fn hex_char_to_nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => 0,
    }
}

/// Convert a NcaId to a hex string.
fn nca_id_to_hex(id: &NcaId, uppercase: bool) -> String {
    if uppercase {
        id.iter().map(|b| format!("{:02X}", b)).collect()
    } else {
        id.iter().map(|b| format!("{:02x}", b)).collect()
    }
}

/// Get the relative path from an NCA ID for storage in the registered cache.
/// Corresponds to upstream `GetRelativePathFromNcaID`.
fn get_relative_path_from_nca_id(
    nca_id: &NcaId,
    second_hex_upper: bool,
    within_two_digit: bool,
    cnmt_suffix: bool,
) -> String {
    let hex_str = nca_id_to_hex(nca_id, second_hex_upper);
    if !within_two_digit {
        if cnmt_suffix {
            format!("{}.cnmt.nca", hex_str)
        } else {
            format!("/{}.nca", hex_str)
        }
    } else {
        // Hash the NCA ID with SHA-256 to get the two-digit dir component
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(nca_id);
        let hash = hasher.finalize();

        if cnmt_suffix {
            format!("/000000{:02X}/{}.cnmt.nca", hash[0], hex_str)
        } else {
            format!("/000000{:02X}/{}.nca", hash[0], hex_str)
        }
    }
}

/// Title type names for CNMT naming.
/// Corresponds to upstream `TITLE_TYPE_NAMES` in registered_cache.cpp.
const TITLE_TYPE_NAMES: &[&str] = &[
    "SystemProgram",
    "SystemData",
    "SystemUpdate",
    "BootImagePackage",
    "BootImagePackageSafe",
    "Application",
    "Patch",
    "AddOnContent",
    "", // DeltaTitle
];

/// Get the CNMT filename for a given title type and title ID.
/// Corresponds to upstream `GetCNMTName`.
fn get_cnmt_name(title_type: TitleType, title_id: u64) -> String {
    let mut index = title_type as usize;
    // If the index is after the jump in TitleType, subtract it out.
    if index >= TitleType::Application as usize {
        index -= TitleType::Application as usize - TitleType::FirmwarePackageB as usize;
    }
    let type_name = TITLE_TYPE_NAMES.get(index).copied().unwrap_or("");
    format!("{}_{:016x}.cnmt", type_name, title_id)
}

/// NCA ID — first 16 bytes of SHA-256 over the entire file.
pub type NcaId = [u8; 0x10];

/// Rust counterpart of upstream `ContentProviderParsingFunction`.
type ContentProviderParsingFunction =
    Box<dyn Fn(&VirtualFile, &NcaId) -> Option<VirtualFile> + Send + Sync>;

/// Rust counterpart of upstream `VfsCopyFunction`.
pub type VfsCopyFunction<'a> = dyn Fn(&dyn VfsFile, &dyn VfsFile, usize) -> bool + Send + Sync + 'a;

/// Result of an install operation.
/// Corresponds to upstream `InstallResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallResult {
    Success,
    OverwriteExisting,
    ErrorAlreadyExists,
    ErrorCopyFailed,
    ErrorMetaFailed,
    ErrorBaseInstall,
}

/// An entry in a content provider.
/// Corresponds to upstream `ContentProviderEntry`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContentProviderEntry {
    pub title_id: u64,
    pub record_type: ContentRecordType,
}

impl ContentProviderEntry {
    pub fn debug_info(&self) -> String {
        format!("GitID={:016X}, Type={:?}", self.title_id, self.record_type)
    }
}

/// Get the update title ID for a base title.
/// Corresponds to upstream `GetUpdateTitleID`.
pub fn get_update_title_id(base_title_id: u64) -> u64 {
    base_title_id | 0x800
}

/// Convert NCA content type to content record type.
/// Corresponds to upstream `GetCRTypeFromNCAType`.
pub fn get_cr_type_from_nca_type(nca_type: u8) -> ContentRecordType {
    use super::content_archive::NCAContentType;
    match nca_type {
        x if x == NCAContentType::Program as u8 => ContentRecordType::Program,
        x if x == NCAContentType::Meta as u8 => ContentRecordType::Meta,
        x if x == NCAContentType::Control as u8 => ContentRecordType::Control,
        x if x == NCAContentType::Data as u8 => ContentRecordType::Data,
        x if x == NCAContentType::PublicData as u8 => ContentRecordType::Data,
        x if x == NCAContentType::Manual as u8 => ContentRecordType::HtmlDocument,
        _ => {
            log::error!("Invalid NCAContentType={:02X}", nca_type);
            ContentRecordType::Meta
        }
    }
}

/// Get the base title ID from a title ID.
/// Corresponds to upstream `GetBaseTitleID`.
pub fn get_base_title_id(title_id: u64) -> u64 {
    super::common_funcs::get_base_title_id(title_id)
}

// ============================================================================
// ContentProvider trait
// ============================================================================

/// Content provider interface.
/// Corresponds to upstream `ContentProvider`.
pub trait ContentProvider: Send + Sync {
    fn refresh(&mut self);
    fn has_entry(&self, title_id: u64, record_type: ContentRecordType) -> bool;
    fn get_entry_version(&self, title_id: u64) -> Option<u32>;
    fn get_entry_unparsed(
        &self,
        title_id: u64,
        record_type: ContentRecordType,
    ) -> Option<VirtualFile>;
    fn get_entry_raw(&self, title_id: u64, record_type: ContentRecordType) -> Option<VirtualFile>;
    fn list_entries_filter(
        &self,
        title_type: Option<TitleType>,
        record_type: Option<ContentRecordType>,
        title_id: Option<u64>,
    ) -> Vec<ContentProviderEntry>;

    fn supports_origin_tracking(&self) -> bool {
        false
    }

    fn list_update_versions(&self, _title_id: u64) -> Vec<ExternalUpdateEntry> {
        Vec::new()
    }

    fn get_entry_for_version(
        &self,
        _title_id: u64,
        _content_type: ContentRecordType,
        _version: u32,
    ) -> Option<VirtualFile> {
        None
    }

    fn list_entries_filter_origin(
        &self,
        _title_type: Option<TitleType>,
        _record_type: Option<ContentRecordType>,
        _title_id: Option<u64>,
    ) -> Vec<(ContentProviderUnionSlot, ContentProviderEntry)> {
        Vec::new()
    }

    fn get_entry_version_from_slot(
        &self,
        _slot: ContentProviderUnionSlot,
        _title_id: u64,
    ) -> Option<u32> {
        None
    }

    fn get_entry_raw_from_slot(
        &self,
        _slot: ContentProviderUnionSlot,
        _title_id: u64,
        _record_type: ContentRecordType,
    ) -> Option<VirtualFile> {
        None
    }

    /// Construct an NCA from the raw entry file.
    /// Corresponds to upstream `ContentProvider::GetEntry`.
    fn get_entry(
        &self,
        title_id: u64,
        record_type: ContentRecordType,
    ) -> Option<super::content_archive::NCA> {
        let raw = self.get_entry_raw(title_id, record_type)?;
        Some(super::content_archive::NCA::new(raw, None))
    }

    fn has_entry_by_provider_entry(&self, entry: ContentProviderEntry) -> bool {
        self.has_entry(entry.title_id, entry.record_type)
    }

    fn list_entries(&self) -> Vec<ContentProviderEntry> {
        self.list_entries_filter(None, None, None)
    }
}

// ============================================================================
// PlaceholderCache
// ============================================================================

/// Placeholder cache for install operations.
/// Corresponds to upstream `PlaceholderCache`.
pub struct PlaceholderCache {
    dir: VirtualDir,
}

impl PlaceholderCache {
    pub fn new(dir: VirtualDir) -> Self {
        Self { dir }
    }

    /// Create a placeholder file with the given NCA ID and size.
    /// Corresponds to upstream `PlaceholderCache::Create`.
    pub fn create(&self, id: &NcaId, size: u64) -> bool {
        let path = get_relative_path_from_nca_id(id, false, true, false);
        if self.dir.get_file_relative(&path).is_some() {
            return false;
        }

        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(id);
        let hash = hasher.finalize();
        let dirname = format!("000000{:02X}", hash[0]);

        let dir2 =
            match super::vfs::vfs::get_or_create_directory_relative(self.dir.as_ref(), &dirname) {
                Some(d) => d,
                None => return false,
            };

        let filename = format!("{}.nca", nca_id_to_hex(id, false));
        let file = match dir2.create_file(&filename) {
            Some(f) => f,
            None => return false,
        };

        file.resize(size as usize)
    }

    /// Delete a placeholder file with the given NCA ID.
    /// Corresponds to upstream `PlaceholderCache::Delete`.
    pub fn delete_placeholder(&self, id: &NcaId) -> bool {
        let path = get_relative_path_from_nca_id(id, false, true, false);
        if self.dir.get_file_relative(&path).is_none() {
            return false;
        }

        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(id);
        let hash = hasher.finalize();
        let dirname = format!("000000{:02X}", hash[0]);

        let dir2 =
            match super::vfs::vfs::get_or_create_directory_relative(self.dir.as_ref(), &dirname) {
                Some(d) => d,
                None => return false,
            };

        dir2.delete_file(&format!("{}.nca", nca_id_to_hex(id, false)))
    }

    /// Check if a placeholder file exists for the given NCA ID.
    /// Corresponds to upstream `PlaceholderCache::Exists`.
    pub fn exists(&self, id: &NcaId) -> bool {
        let path = get_relative_path_from_nca_id(id, false, true, false);
        self.dir.get_file_relative(&path).is_some()
    }

    /// Write data to a placeholder file at the given offset.
    /// Corresponds to upstream `PlaceholderCache::Write`.
    pub fn write(&self, id: &NcaId, offset: u64, data: &[u8]) -> bool {
        let path = get_relative_path_from_nca_id(id, false, true, false);
        let file = match self.dir.get_file_relative(&path) {
            Some(f) => f,
            None => return false,
        };
        file.write(data, data.len(), offset as usize) == data.len()
    }

    /// Install a completed placeholder into a registered content cache.
    /// Corresponds to upstream `PlaceholderCache::Register`.
    pub fn register(
        &self,
        cache: &mut RegisteredCache,
        placeholder: &NcaId,
        install: &NcaId,
    ) -> bool {
        let path = get_relative_path_from_nca_id(placeholder, false, true, false);
        let Some(file) = self.dir.get_file_relative(&path) else {
            return false;
        };

        let nca = NCA::new(file, None);
        if cache.raw_install_nca(&nca, &super::vfs::vfs::vfs_raw_copy, false, Some(*install))
            != InstallResult::Success
        {
            return false;
        }

        self.delete_placeholder(placeholder)
    }

    /// Return the non-zero rights ID of a valid placeholder NCA.
    /// Corresponds to upstream `PlaceholderCache::GetRightsID`.
    pub fn get_rights_id(&self, id: &NcaId) -> Option<[u8; 0x10]> {
        let path = get_relative_path_from_nca_id(id, false, true, false);
        let file = self.dir.get_file_relative(&path)?;
        let nca = NCA::new(file, None);
        if nca.get_status() != ResultStatus::Success
            && nca.get_status() != ResultStatus::ErrorMissingBKTRBaseRomFS
        {
            return None;
        }

        let rights_id = nca.get_rights_id();
        (rights_id != [0; 0x10]).then_some(rights_id)
    }

    /// Get the size of a placeholder file.
    /// Corresponds to upstream `PlaceholderCache::Size`.
    pub fn size(&self, id: &NcaId) -> u64 {
        let path = get_relative_path_from_nca_id(id, false, true, false);
        match self.dir.get_file_relative(&path) {
            Some(f) => f.get_size() as u64,
            None => 0,
        }
    }

    /// Set the size of a placeholder file.
    /// Corresponds to upstream `PlaceholderCache::SetSize`.
    pub fn set_size(&self, id: &NcaId, new_size: u64) -> bool {
        let path = get_relative_path_from_nca_id(id, false, true, false);
        match self.dir.get_file_relative(&path) {
            Some(f) => f.resize(new_size as usize),
            None => false,
        }
    }

    /// Clean all placeholder files.
    /// Corresponds to upstream `PlaceholderCache::CleanAll`.
    pub fn clean_all(&self) -> bool {
        match self.dir.get_parent_directory() {
            Some(parent) => parent.clean_subdirectory_recursive(&self.dir.get_name()),
            None => false,
        }
    }

    /// List all NCA IDs in the placeholder cache.
    /// Corresponds to upstream `PlaceholderCache::List`.
    pub fn list(&self) -> Vec<NcaId> {
        let mut out = Vec::new();
        for sdir in self.dir.get_subdirectories() {
            for file in sdir.get_files() {
                let name = file.get_name();
                if name.len() == 36 && name.ends_with(".nca") {
                    let hex_part = &name[..32];
                    if hex_part.chars().all(|c| c.is_ascii_hexdigit()) {
                        out.push(hex_string_to_nca_id(hex_part));
                    }
                }
            }
        }
        out
    }

    /// Generate a random NCA ID.
    /// Corresponds to upstream `PlaceholderCache::Generate`.
    pub fn generate() -> NcaId {
        use std::time::SystemTime;
        // Simple pseudo-random generation using system time.
        // Not cryptographically secure, but matches upstream intent
        // (which uses std::random_device).
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let mut id = [0u8; 0x10];
        let bytes = now.to_le_bytes();
        id[..8].copy_from_slice(&bytes[..8]);
        // Mix in another value for the second half
        let v2 = now.wrapping_mul(6364136223846793005).wrapping_add(1);
        let bytes2 = v2.to_le_bytes();
        id[8..].copy_from_slice(&bytes2[..8]);
        id
    }
}

// ============================================================================
// RegisteredCache
// ============================================================================

/// Registered cache — catalogues NCAs in the registered directory structure.
///
/// Nintendo's registered format follows this structure:
/// ```text
/// Root
///   | 000000XX  <- XX is derived from SHA-256 of the NcaID
///       | <hash>.nca <- hash is the NcaID (first half of SHA256 over entire file)
///         | 00
///         | 01    <- Actual content split along 4GB boundaries (optional)
/// ```
///
/// Corresponds to upstream `RegisteredCache`.
pub struct RegisteredCache {
    dir: VirtualDir,
    /// Conversion from stored bytes to the raw NCA view. SDMC uses NAX decryption.
    parser: ContentProviderParsingFunction,
    /// maps tid -> NcaID of meta
    meta_id: BTreeMap<u64, NcaId>,
    /// maps tid -> CNMT parsed from meta NCAs
    meta: BTreeMap<u64, CNMT>,
    /// maps tid -> CNMT from yuzu_meta directory
    yuzu_meta: BTreeMap<u64, CNMT>,
}

impl RegisteredCache {
    pub fn new(dir: VirtualDir) -> Self {
        Self::new_with_parser(dir, |file, _id| Some(Arc::clone(file)))
    }

    /// Construct with the same per-storage parsing callback as upstream.
    pub fn new_with_parser<F>(dir: VirtualDir, parser: F) -> Self
    where
        F: Fn(&VirtualFile, &NcaId) -> Option<VirtualFile> + Send + Sync + 'static,
    {
        let mut cache = Self {
            dir,
            parser: Box::new(parser),
            meta_id: BTreeMap::new(),
            meta: BTreeMap::new(),
            yuzu_meta: BTreeMap::new(),
        };
        cache.refresh();
        cache
    }

    /// Accumulate all NCA file IDs from the directory structure.
    /// Corresponds to upstream `RegisteredCache::AccumulateFiles`.
    fn accumulate_files(&self) -> Vec<NcaId> {
        let mut ids = Vec::new();
        for d2_dir in self.dir.get_subdirectories() {
            let name = d2_dir.get_name();
            if follows_nca_id_format(&name) {
                ids.push(hex_string_to_nca_id(&name[..32]));
                continue;
            }

            if !follows_two_digit_dir_format(&name) {
                continue;
            }

            for nca_dir in d2_dir.get_subdirectories() {
                let nca_name = nca_dir.get_name();
                if follows_nca_id_format(&nca_name) {
                    ids.push(hex_string_to_nca_id(&nca_name[..32]));
                }
            }

            for nca_file in d2_dir.get_files() {
                let nca_name = nca_file.get_name();
                if follows_nca_id_format(&nca_name) {
                    ids.push(hex_string_to_nca_id(&nca_name[..32]));
                }
            }
        }

        for d2_file in self.dir.get_files() {
            let name = d2_file.get_name();
            if follows_nca_id_format(&name) {
                ids.push(hex_string_to_nca_id(&name[..32]));
            }
        }

        ids
    }

    /// Process NCA files: find Meta-type NCAs and parse their CNMTs.
    /// Corresponds to upstream `RegisteredCache::ProcessFiles` (registered_cache.cpp).
    ///
    /// Upstream opens each NCA, checks it's Meta type, extracts section 0's
    /// RomFS, finds .cnmt files, and parses them into CNMT records.
    /// NCA::get_romfs() / NCA section access requires the crypto pipeline
    /// (KeyManager for title key decryption) to extract section contents.
    fn process_files(&mut self, ids: &[NcaId]) {
        use super::vfs::vfs::VfsDirectory;

        for id in ids {
            let file = match self.get_file_at_id(id) {
                Some(f) => f,
                None => continue,
            };

            let Some(parsed_file) = (self.parser)(&file, id) else {
                continue;
            };
            let nca = NCA::new(parsed_file, None);
            if nca.get_status() != super::partition_filesystem::ResultStatus::Success
                || nca.get_type() != NCAContentType::Meta
            {
                continue;
            }

            // Upstream: nca->GetSubdirectories()[0] returns section 0 as a VfsDir.
            // NCA implements VfsDirectory; GetSubdirectories returns its sections.
            let subdirs = nca.get_subdirectories();
            if subdirs.is_empty() {
                continue;
            }
            let section0 = &subdirs[0];

            for section0_file in section0.get_files() {
                let name = section0_file.get_name();
                if !name.ends_with(".cnmt") {
                    continue;
                }

                let cnmt = CNMT::from_file(&section0_file);
                let title_id = cnmt.get_title_id();
                self.meta.insert(title_id, cnmt);
                self.meta_id.insert(title_id, *id);
                break;
            }
        }
    }

    /// Open a file or directory-concatenated file at a given path.
    /// Corresponds to upstream `RegisteredCache::OpenFileOrDirectoryConcat`.
    fn open_file_or_directory_concat(&self, path: &str) -> Option<VirtualFile> {
        // Try as a file first
        if let Some(file) = self.dir.get_file_relative(path) {
            return Some(file);
        }

        // Try as a directory (split NCA)
        let nca_dir = self.dir.get_directory_relative(path)?;
        let files = nca_dir.get_files();

        if files.len() == 1 && files[0].get_name() == "00" {
            return Some(files[0].clone());
        }

        // Concatenate split files
        let mut concat = Vec::new();
        for i in 0..0x100usize {
            let upper_name = format!("{:02X}", i);
            let lower_name = format!("{:02x}", i);
            if let Some(next) = nca_dir.get_file(&upper_name) {
                concat.push(next);
            } else if let Some(next) = nca_dir.get_file(&lower_name) {
                concat.push(next);
            } else {
                break;
            }
        }

        if concat.is_empty() {
            return None;
        }

        let name = concat[0].get_name();
        ConcatenatedVfsFile::make_concatenated_file(name, concat)
    }

    /// Get the file for a given NCA ID, trying all storage modes.
    /// Corresponds to upstream `RegisteredCache::GetFileAtID`.
    fn get_file_at_id(&self, id: &NcaId) -> Option<VirtualFile> {
        // Try all five relevant modes of file storage:
        // (bit 2 = uppercase/lower, bit 1 = within a two-digit dir, bit 0 = .cnmt suffix)
        for i in 0u8..8 {
            if (i % 2) == 1 && i != 7 {
                continue;
            }
            let path = get_relative_path_from_nca_id(
                id,
                (i & 0b100) == 0,
                (i & 0b010) == 0,
                (i & 0b001) == 0b001,
            );
            if let Some(file) = self.open_file_or_directory_concat(&path) {
                return Some(file);
            }
        }
        None
    }

    /// Look up an NCA ID from the metadata maps.
    /// Corresponds to upstream `RegisteredCache::GetNcaIDFromMetadata`.
    fn get_nca_id_from_metadata(
        &self,
        title_id: u64,
        record_type: ContentRecordType,
    ) -> Option<NcaId> {
        if record_type == ContentRecordType::Meta {
            if let Some(id) = self.meta_id.get(&title_id) {
                return Some(*id);
            }
        }

        // Check yuzu_meta first, then regular meta
        if let Some(id) = check_map_for_content_record(&self.yuzu_meta, title_id, record_type) {
            return Some(id);
        }
        check_map_for_content_record(&self.meta, title_id, record_type)
    }

    /// Apply `proc` and `filter` to the same metadata records, in the same
    /// order, as upstream `RegisteredCache::IterateAllMetadata`.
    fn iterate_all_metadata<T, P, F>(&self, out: &mut Vec<T>, proc: P, filter: F)
    where
        P: Fn(&CNMT, &ContentRecord) -> T,
        F: Fn(&CNMT, &ContentRecord) -> bool,
    {
        for cnmt in self.meta.values() {
            if filter(cnmt, &EMPTY_META_CONTENT_RECORD) {
                out.push(proc(cnmt, &EMPTY_META_CONTENT_RECORD));
            }
            for record in cnmt.get_content_records() {
                if self.get_file_at_id(&record.nca_id).is_some() && filter(cnmt, record) {
                    out.push(proc(cnmt, record));
                }
            }
        }
        for cnmt in self.yuzu_meta.values() {
            for record in cnmt.get_content_records() {
                if self.get_file_at_id(&record.nca_id).is_some() && filter(cnmt, record) {
                    out.push(proc(cnmt, record));
                }
            }
        }
    }

    /// Accumulate CNMTs from the yuzu_meta directory.
    /// Corresponds to upstream `RegisteredCache::AccumulateYuzuMeta`.
    fn accumulate_yuzu_meta(&mut self) {
        let meta_dir = match self.dir.get_subdirectory("yuzu_meta") {
            Some(d) => d,
            None => return,
        };

        for file in meta_dir.get_files() {
            if file.get_extension() != "cnmt" {
                continue;
            }
            let cnmt = CNMT::from_file(&file);
            self.yuzu_meta.insert(cnmt.get_title_id(), cnmt);
        }
    }

    /// Remove an existing entry by title ID.
    /// Corresponds to upstream `RegisteredCache::RemoveExistingEntry`.
    pub fn remove_existing_entry(&self, title_id: u64) -> bool {
        let mut removed_data = false;

        let delete_nca = |id: &NcaId| -> bool {
            let path = get_relative_path_from_nca_id(id, false, true, false);
            if self.dir.get_file_relative(&path).is_some() {
                return self.dir.delete_file(&path);
            }
            if self.dir.get_directory_relative(&path).is_some() {
                return self.dir.delete_subdirectory_recursive(&path);
            }
            false
        };

        // If entry exists, remove all associated NCAs
        if self.has_entry(title_id, ContentRecordType::Meta) {
            log::info!(
                "Previously installed entry for title_id={:016X} detected! Attempting to remove...",
                title_id
            );

            let meta_old_id = self
                .get_nca_id_from_metadata(title_id, ContentRecordType::Meta)
                .unwrap_or([0u8; 0x10]);
            let program_id = self
                .get_nca_id_from_metadata(title_id, ContentRecordType::Program)
                .unwrap_or([0u8; 0x10]);
            let data_id = self
                .get_nca_id_from_metadata(title_id, ContentRecordType::Data)
                .unwrap_or([0u8; 0x10]);
            let control_id = self
                .get_nca_id_from_metadata(title_id, ContentRecordType::Control)
                .unwrap_or([0u8; 0x10]);
            let html_id = self
                .get_nca_id_from_metadata(title_id, ContentRecordType::HtmlDocument)
                .unwrap_or([0u8; 0x10]);
            let legal_id = self
                .get_nca_id_from_metadata(title_id, ContentRecordType::LegalInformation)
                .unwrap_or([0u8; 0x10]);

            removed_data |= delete_nca(&meta_old_id)
                || delete_nca(&program_id)
                || delete_nca(&data_id)
                || delete_nca(&control_id)
                || delete_nca(&html_id)
                || delete_nca(&legal_id);
        }

        // Remove yuzu_meta entries for patch programs
        for i in 0u8..0x10 {
            if let Some(meta_dir) = self.dir.create_subdirectory("yuzu_meta") {
                let filename = get_cnmt_name(TitleType::Update, title_id + i as u64);
                if meta_dir.get_file(&filename).is_some() {
                    removed_data |= meta_dir.delete_file(&filename);
                }
            }
        }

        removed_data
    }

    /// Install all content from an XCI's secure partition.
    /// Corresponds to upstream `RegisteredCache::InstallEntry(const XCI&)`.
    pub fn install_entry_xci(
        &mut self,
        xci: &XCI,
        overwrite_if_exists: bool,
        copy: &VfsCopyFunction<'_>,
    ) -> InstallResult {
        let Some(nsp) = xci.get_secure_partition_nsp() else {
            return InstallResult::ErrorMetaFailed;
        };
        self.install_entry_nsp(&nsp, overwrite_if_exists, copy)
    }

    /// Install all content described by the NSP's metadata NCA.
    /// Corresponds to upstream `RegisteredCache::InstallEntry(const NSP&)`.
    pub fn install_entry_nsp(
        &mut self,
        nsp: &NSP,
        overwrite_if_exists: bool,
        copy: &VfsCopyFunction<'_>,
    ) -> InstallResult {
        let ncas = nsp.get_ncas_collapsed();
        let Some(meta_nca) = ncas
            .iter()
            .find(|nca| nca.get_type() == NCAContentType::Meta)
        else {
            log::error!(
                "The file you are attempting to install does not have a metadata NCA and is therefore malformed. Check your encryption keys."
            );
            return InstallResult::ErrorMetaFailed;
        };

        let meta_name = meta_nca.get_name();
        let meta_id = hex_string_to_nca_id(meta_name.get(..32).unwrap_or(""));
        let subdirectories = meta_nca.get_subdirectories();
        if subdirectories.is_empty() {
            log::error!(
                "The file you are attempting to install does not contain a section0 within the metadata NCA and is therefore malformed. Verify that the file is valid."
            );
            return InstallResult::ErrorMetaFailed;
        }
        let cnmt_files = subdirectories[0].get_files();
        if cnmt_files.is_empty() {
            log::error!(
                "The file you are attempting to install does not contain a CNMT within the metadata NCA and is therefore malformed. Verify that the file is valid."
            );
            return InstallResult::ErrorMetaFailed;
        }
        let cnmt = CNMT::from_file(&cnmt_files[0]);
        let title_id = cnmt.get_title_id();
        if title_id == get_base_title_id(title_id) && cnmt.get_title_version() == 0 {
            return InstallResult::ErrorBaseInstall;
        }

        let replaced_existing = self.remove_existing_entry(title_id);
        let meta_result = self.raw_install_nca(meta_nca, copy, overwrite_if_exists, Some(meta_id));
        if meta_result != InstallResult::Success {
            return meta_result;
        }

        for record in cnmt.get_content_records() {
            if record.record_type == ContentRecordType::DeltaFragment {
                continue;
            }
            let filename = format!("{}.nca", nca_id_to_hex(&record.nca_id, false));
            let Some(file) = nsp.get_file(&filename) else {
                return InstallResult::ErrorCopyFailed;
            };
            let nca = NCA::new(file, None);
            let result = if nca.get_status() == ResultStatus::ErrorMissingBKTRBaseRomFS
                && nca.get_title_id() != title_id
            {
                self.install_entry_nca_with_record(
                    &nca,
                    cnmt.get_header(),
                    record,
                    overwrite_if_exists,
                    copy,
                )
            } else {
                self.raw_install_nca(&nca, copy, overwrite_if_exists, Some(record.nca_id))
            };
            if result != InstallResult::Success {
                return result;
            }
        }

        self.refresh();
        if replaced_existing {
            InstallResult::OverwriteExisting
        } else {
            InstallResult::Success
        }
    }

    /// Install a standalone NCA and synthesize its yuzu_meta CNMT.
    /// Corresponds to upstream `RegisteredCache::InstallEntry(const NCA&, TitleType)`.
    pub fn install_entry_nca(
        &mut self,
        nca: &NCA,
        title_type: TitleType,
        overwrite_if_exists: bool,
        copy: &VfsCopyFunction<'_>,
    ) -> InstallResult {
        let mut header = CNMTHeader::default();
        header.title_id = nca.get_title_id();
        header.title_type = title_type as u8;
        header.table_offset = 0x10;
        header.number_content_entries = 1;

        use sha2::{Digest, Sha256};
        let mut record = ContentRecord::default();
        record.record_type = get_cr_type_from_nca_type(nca.get_type() as u8);
        let data = nca.get_base_file().read_bytes(0x10_0000, 0);
        record.hash.copy_from_slice(&Sha256::digest(&data));
        record.nca_id.copy_from_slice(&record.hash[..0x10]);
        let cnmt = CNMT::from_parts(header, OptionalHeader::default(), vec![record], Vec::new());
        if !self.raw_install_yuzu_meta(&cnmt) {
            return InstallResult::ErrorMetaFailed;
        }
        self.raw_install_nca(nca, copy, overwrite_if_exists, Some(record.nca_id))
    }

    /// Install a multiprogram patch NCA using the base CNMT record.
    /// Corresponds to upstream `RegisteredCache::InstallEntry(const NCA&, const CNMTHeader&, ...)`.
    pub fn install_entry_nca_with_record(
        &mut self,
        nca: &NCA,
        base_header: &CNMTHeader,
        base_record: &ContentRecord,
        overwrite_if_exists: bool,
        copy: &VfsCopyFunction<'_>,
    ) -> InstallResult {
        let mut header = CNMTHeader::default();
        header.title_id = nca.get_title_id();
        header.title_version = base_header.title_version;
        header.title_type = base_header.title_type;
        header.table_offset = 0x10;
        header.number_content_entries = 1;
        let cnmt = CNMT::from_parts(
            header,
            OptionalHeader::default(),
            vec![*base_record],
            Vec::new(),
        );
        if !self.raw_install_yuzu_meta(&cnmt) {
            return InstallResult::ErrorMetaFailed;
        }
        self.raw_install_nca(nca, copy, overwrite_if_exists, Some(base_record.nca_id))
    }

    /// Delete an NCA by its content ID.
    /// Corresponds to upstream `RegisteredCache::Delete`.
    pub fn delete(&self, id: &NcaId) -> bool {
        let path = get_relative_path_from_nca_id(id, false, true, false);
        if let Some(file) = self.dir.get_file_relative(&path) {
            return file
                .get_containing_directory()
                .is_some_and(|parent| parent.delete_file(&file.get_name()));
        }
        if let Some(directory) = self.dir.get_directory_relative(&path) {
            return directory
                .get_parent_directory()
                .is_some_and(|parent| parent.delete_subdirectory_recursive(&directory.get_name()));
        }
        true
    }

    /// Raw-copy an NCA into the registered layout.
    /// Corresponds to upstream `RegisteredCache::RawInstallNCA`.
    fn raw_install_nca(
        &mut self,
        nca: &NCA,
        copy: &VfsCopyFunction<'_>,
        overwrite_if_exists: bool,
        override_id: Option<NcaId>,
    ) -> InstallResult {
        let input = nca.get_base_file();
        let id = if let Some(id) = override_id {
            id
        } else {
            use sha2::{Digest, Sha256};
            let data = input.read_bytes(0x10_0000, 0);
            let hash = Sha256::digest(&data);
            let mut id = [0u8; 0x10];
            id.copy_from_slice(&hash[..0x10]);
            id
        };

        let path = get_relative_path_from_nca_id(&id, false, true, false);
        let existing = self.get_file_at_id(&id);
        if existing.is_some() && !overwrite_if_exists {
            log::warn!("Attempting to overwrite existing NCA. Skipping...");
            return InstallResult::ErrorAlreadyExists;
        }

        if let Some(existing) = existing {
            log::warn!("Overwriting existing NCA...");
            if let Some(containing_directory) = existing.get_containing_directory() {
                // Upstream deliberately ignores the deletion result and lets the
                // subsequent CreateFileRelative/copy decide whether installation fails.
                containing_directory.delete_file(&existing.get_name());
            }
        }

        let Some(output) = self.dir.create_file_relative(&path) else {
            return InstallResult::ErrorCopyFailed;
        };
        if copy(input.as_ref(), output.as_ref(), VFS_RC_LARGE_COPY_BLOCK) {
            InstallResult::Success
        } else {
            InstallResult::ErrorCopyFailed
        }
    }

    /// Install a raw CNMT into the yuzu_meta directory.
    /// Corresponds to upstream `RegisteredCache::RawInstallYuzuMeta`.
    pub fn raw_install_yuzu_meta(&mut self, cnmt: &CNMT) -> bool {
        let meta_dir = match self.dir.create_subdirectory("yuzu_meta") {
            Some(d) => d,
            None => return false,
        };
        let filename = get_cnmt_name(cnmt.get_type(), cnmt.get_title_id());

        if meta_dir.get_file(&filename).is_none() {
            let out = match meta_dir.create_file(&filename) {
                Some(f) => f,
                None => return false,
            };
            let buffer = cnmt.serialize();
            out.resize(buffer.len());
            out.write_bytes(&buffer, 0);
        } else {
            let out = match meta_dir.get_file(&filename) {
                Some(f) => f,
                None => return false,
            };
            let mut old_cnmt = CNMT::from_file(&out);
            if old_cnmt.union_records(cnmt) {
                out.resize(0);
                let buffer = old_cnmt.serialize();
                out.resize(buffer.len());
                out.write_bytes(&buffer, 0);
            }
        }

        self.refresh();
        self.yuzu_meta
            .values()
            .any(|c| c.get_type() == cnmt.get_type() && c.get_title_id() == cnmt.get_title_id())
    }
}

/// Check a map of CNMTs for a content record matching the given title ID and type.
///
/// Port of upstream `CheckMapForContentRecord`: indexed programs store
/// `id_offset` on the content record of the base title.
fn check_map_for_content_record(
    map: &BTreeMap<u64, CNMT>,
    title_id: u64,
    record_type: ContentRecordType,
) -> Option<NcaId> {
    let mut cnmt = map.get(&title_id);
    let mut id_offset = 0u8;

    if cnmt.is_none() {
        let program_index = title_id & AOC_TITLE_ID_MASK;
        if program_index == 0 {
            return None;
        }
        cnmt = map.get(&(title_id & !AOC_TITLE_ID_MASK));
        if cnmt.is_none() {
            return None;
        }
        id_offset = program_index as u8;
    }

    let cnmt = cnmt?;
    if let Some(record) = cnmt
        .get_content_records()
        .iter()
        .find(|rec| rec.record_type == record_type && rec.id_offset == id_offset)
    {
        return Some(record.nca_id);
    }

    if id_offset != 0 {
        return None;
    }

    cnmt.get_content_records()
        .iter()
        .find(|rec| rec.record_type == record_type)
        .map(|rec| rec.nca_id)
}

impl ContentProvider for RegisteredCache {
    /// Scan the directory structure and build the metadata index.
    /// Corresponds to upstream `RegisteredCache::Refresh`.
    fn refresh(&mut self) {
        self.meta_id.clear();
        self.meta.clear();
        self.yuzu_meta.clear();

        let ids = self.accumulate_files();
        self.process_files(&ids);
        self.accumulate_yuzu_meta();
    }

    fn has_entry(&self, title_id: u64, record_type: ContentRecordType) -> bool {
        self.get_entry_raw(title_id, record_type).is_some()
    }

    fn get_entry_version(&self, title_id: u64) -> Option<u32> {
        if let Some(cnmt) = self.meta.get(&title_id) {
            return Some(cnmt.get_title_version());
        }
        if let Some(cnmt) = self.yuzu_meta.get(&title_id) {
            return Some(cnmt.get_title_version());
        }
        None
    }

    fn get_entry_unparsed(
        &self,
        title_id: u64,
        record_type: ContentRecordType,
    ) -> Option<VirtualFile> {
        let id = self.get_nca_id_from_metadata(title_id, record_type)?;
        self.get_file_at_id(&id)
    }

    fn get_entry_raw(&self, title_id: u64, record_type: ContentRecordType) -> Option<VirtualFile> {
        let id = self.get_nca_id_from_metadata(title_id, record_type)?;
        let file = self.get_file_at_id(&id)?;
        (self.parser)(&file, &id)
    }

    fn list_entries_filter(
        &self,
        title_type: Option<TitleType>,
        record_type: Option<ContentRecordType>,
        title_id: Option<u64>,
    ) -> Vec<ContentProviderEntry> {
        let mut out = Vec::new();
        self.iterate_all_metadata(
            &mut out,
            |cnmt, record| ContentProviderEntry {
                title_id: cnmt.get_title_id(),
                record_type: record.record_type,
            },
            |cnmt, record| {
                title_type.is_none_or(|expected| expected == cnmt.get_type())
                    && record_type.is_none_or(|expected| expected == record.record_type)
                    && title_id.is_none_or(|expected| expected == cnmt.get_title_id())
            },
        );
        out
    }
}

// ============================================================================
// ContentProviderUnionSlot
// ============================================================================

/// Slot identifier for ContentProviderUnion.
/// Corresponds to upstream `ContentProviderUnionSlot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContentProviderUnionSlot {
    SysNAND,
    UserNAND,
    SDMC,
    External,
    FrontendManual,
}

// ============================================================================
// ContentProviderUnion
// ============================================================================

/// Combines multiple ContentProviders into one interface.
/// Corresponds to upstream `ContentProviderUnion`.
pub struct ContentProviderUnion {
    providers: BTreeMap<ContentProviderUnionSlot, *mut dyn ContentProvider>,
}

// Safety: the pointers are managed by the caller's lifetime.
unsafe impl Send for ContentProviderUnion {}
unsafe impl Sync for ContentProviderUnion {}

impl ContentProviderUnion {
    pub fn new() -> Self {
        Self {
            providers: BTreeMap::new(),
        }
    }

    /// # Safety
    /// The caller must ensure the provider outlives this ContentProviderUnion.
    pub unsafe fn set_slot(
        &mut self,
        slot: ContentProviderUnionSlot,
        provider: *mut dyn ContentProvider,
    ) {
        self.providers.insert(slot, provider);
    }

    pub fn clear_slot(&mut self, slot: ContentProviderUnionSlot) {
        self.providers.remove(&slot);
    }

    /// Upstream `ContentProviderUnion::SetExternalProvider`.
    ///
    /// # Safety
    /// The provider must outlive this union.
    pub unsafe fn set_external_provider(&mut self, provider: *mut dyn ContentProvider) {
        self.set_slot(ContentProviderUnionSlot::External, provider);
    }

    #[cfg(test)]
    pub fn has_slot(&self, slot: ContentProviderUnionSlot) -> bool {
        self.providers.contains_key(&slot)
    }
}

impl ContentProvider for ContentProviderUnion {
    fn refresh(&mut self) {
        for &ptr in self.providers.values() {
            unsafe { (*ptr).refresh() };
        }
    }

    fn has_entry(&self, title_id: u64, record_type: ContentRecordType) -> bool {
        self.providers
            .values()
            .any(|&ptr| unsafe { (*ptr).has_entry(title_id, record_type) })
    }

    fn get_entry_version(&self, title_id: u64) -> Option<u32> {
        for &ptr in self.providers.values() {
            if let Some(v) = unsafe { (*ptr).get_entry_version(title_id) } {
                return Some(v);
            }
        }
        None
    }

    fn get_entry_unparsed(
        &self,
        title_id: u64,
        record_type: ContentRecordType,
    ) -> Option<VirtualFile> {
        for &ptr in self.providers.values() {
            if let Some(f) = unsafe { (*ptr).get_entry_unparsed(title_id, record_type) } {
                return Some(f);
            }
        }
        None
    }

    fn get_entry_raw(&self, title_id: u64, record_type: ContentRecordType) -> Option<VirtualFile> {
        for &ptr in self.providers.values() {
            if let Some(f) = unsafe { (*ptr).get_entry_raw(title_id, record_type) } {
                return Some(f);
            }
        }
        None
    }

    fn list_entries_filter(
        &self,
        title_type: Option<TitleType>,
        record_type: Option<ContentRecordType>,
        title_id: Option<u64>,
    ) -> Vec<ContentProviderEntry> {
        let mut result = Vec::new();
        for &ptr in self.providers.values() {
            result.extend(unsafe { (*ptr).list_entries_filter(title_type, record_type, title_id) });
        }
        result.sort();
        result.dedup();
        result
    }

    fn supports_origin_tracking(&self) -> bool {
        true
    }

    fn list_update_versions(&self, title_id: u64) -> Vec<ExternalUpdateEntry> {
        let mut result = Vec::new();
        for &provider in self.providers.values() {
            result.extend(unsafe { (*provider).list_update_versions(title_id) });
        }
        result.sort_by(|left, right| right.version.cmp(&left.version));
        result.dedup_by_key(|entry| entry.version);
        result
    }

    fn get_entry_for_version(
        &self,
        title_id: u64,
        content_type: ContentRecordType,
        version: u32,
    ) -> Option<VirtualFile> {
        for &provider in self.providers.values() {
            if let Some(file) =
                unsafe { (*provider).get_entry_for_version(title_id, content_type, version) }
            {
                return Some(file);
            }
        }
        None
    }

    fn list_entries_filter_origin(
        &self,
        title_type: Option<TitleType>,
        record_type: Option<ContentRecordType>,
        title_id: Option<u64>,
    ) -> Vec<(ContentProviderUnionSlot, ContentProviderEntry)> {
        ContentProviderUnion::list_entries_filter_origin(
            self,
            None,
            title_type,
            record_type,
            title_id,
        )
    }

    fn get_entry_version_from_slot(
        &self,
        slot: ContentProviderUnionSlot,
        title_id: u64,
    ) -> Option<u32> {
        self.providers
            .get(&slot)
            .and_then(|provider| unsafe { (**provider).get_entry_version(title_id) })
    }

    fn get_entry_raw_from_slot(
        &self,
        slot: ContentProviderUnionSlot,
        title_id: u64,
        record_type: ContentRecordType,
    ) -> Option<VirtualFile> {
        self.providers
            .get(&slot)
            .and_then(|provider| unsafe { (**provider).get_entry_raw(title_id, record_type) })
    }
}

impl ContentProviderUnion {
    /// List entries with their origin slot.
    /// Corresponds to upstream `ContentProviderUnion::ListEntriesFilterOrigin`.
    pub fn list_entries_filter_origin(
        &self,
        origin: Option<ContentProviderUnionSlot>,
        title_type: Option<TitleType>,
        record_type: Option<ContentRecordType>,
        title_id: Option<u64>,
    ) -> Vec<(ContentProviderUnionSlot, ContentProviderEntry)> {
        let mut result = Vec::new();
        for (&slot, &ptr) in &self.providers {
            if let Some(o) = origin {
                if o != slot {
                    continue;
                }
            }
            let vec = unsafe { (*ptr).list_entries_filter(title_type, record_type, title_id) };
            for entry in vec {
                result.push((slot, entry));
            }
        }
        result.sort();
        result.dedup();
        result
    }

    /// Find which slot contains a given entry.
    /// Corresponds to upstream `ContentProviderUnion::GetSlotForEntry`.
    pub fn get_slot_for_entry(
        &self,
        title_id: u64,
        record_type: ContentRecordType,
    ) -> Option<ContentProviderUnionSlot> {
        for (&slot, &ptr) in &self.providers {
            if unsafe { (*ptr).has_entry(title_id, record_type) } {
                return Some(slot);
            }
        }
        None
    }
}

impl Default for ContentProviderUnion {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// ManualContentProvider
// ============================================================================

const CONTENT_RECORD_TYPE_COUNT: usize = ContentRecordType::DeltaFragment as usize + 1;

/// One externally discovered update version and its content files.
/// Corresponds to upstream `ExternalUpdateEntry`.
#[derive(Clone)]
pub struct ExternalUpdateEntry {
    pub title_id: u64,
    pub version: u32,
    pub version_string: String,
    pub files: [Option<VirtualFile>; CONTENT_RECORD_TYPE_COUNT],
}

fn title_type_from_raw(value: u8) -> Option<TitleType> {
    Some(match value {
        0x01 => TitleType::SystemProgram,
        0x02 => TitleType::SystemDataArchive,
        0x03 => TitleType::SystemUpdate,
        0x04 => TitleType::FirmwarePackageA,
        0x05 => TitleType::FirmwarePackageB,
        0x80 => TitleType::Application,
        0x81 => TitleType::Update,
        0x82 => TitleType::AOC,
        0x83 => TitleType::DeltaTitle,
        _ => return None,
    })
}

fn content_record_type_from_raw(value: u8) -> Option<ContentRecordType> {
    Some(match value {
        0 => ContentRecordType::Meta,
        1 => ContentRecordType::Program,
        2 => ContentRecordType::Data,
        3 => ContentRecordType::Control,
        4 => ContentRecordType::HtmlDocument,
        5 => ContentRecordType::LegalInformation,
        6 => ContentRecordType::DeltaFragment,
        _ => return None,
    })
}

/// Eden `OpenContainerAsNsp`, including the direct-parser fallback.
fn open_container_as_nsp(file: VirtualFile) -> Option<std::sync::Arc<NSP>> {
    let nsp = std::sync::Arc::new(NSP::new(file.clone(), 0, 0));
    if nsp.get_status() == ResultStatus::Success {
        return Some(nsp);
    }

    let xci = XCI::new(file, 0, 0);
    if xci.get_status() == ResultStatus::Success {
        return xci.get_secure_partition_nsp();
    }
    None
}

fn container_versions(nsp: &NSP) -> (BTreeMap<u64, u32>, BTreeMap<u64, String>) {
    let mut versions = BTreeMap::new();
    let mut version_strings = BTreeMap::new();

    for (&title_id, nca_map) in nsp.get_ncas() {
        for (&(title_type_raw, content_type_raw), nca) in nca_map {
            let Some(title_type) = title_type_from_raw(title_type_raw) else {
                continue;
            };
            let Some(content_type) = content_record_type_from_raw(content_type_raw) else {
                continue;
            };

            if content_type == ContentRecordType::Meta {
                if let Some(cnmt_file) = nca.get_subdirectories().first().and_then(|directory| {
                    directory
                        .get_files()
                        .into_iter()
                        .find(|file| file.get_extension() == "cnmt")
                }) {
                    let cnmt = CNMT::from_file(&cnmt_file);
                    versions.insert(cnmt.get_title_id(), cnmt.get_title_version());
                }
            }

            if title_type == TitleType::Update && content_type == ContentRecordType::Control {
                let version = nca
                    .get_romfs()
                    .and_then(|romfs| extract_romfs(Some(romfs)))
                    .and_then(|directory| {
                        directory
                            .get_file("control.nacp")
                            .or_else(|| directory.get_file("Control.nacp"))
                    })
                    .map(|file| NACP::from_file(&file).get_version_string())
                    .unwrap_or_default();
                if !version.is_empty() {
                    version_strings.insert(title_id, version);
                }
            }
        }
    }

    (versions, version_strings)
}

/// Manually registered content provider.
/// Corresponds to upstream `ManualContentProvider`.
pub struct ManualContentProvider {
    entries: BTreeMap<(TitleType, ContentRecordType, u64), VirtualFile>,
    multi_version_entries: Vec<ExternalUpdateEntry>,
}

impl ManualContentProvider {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            multi_version_entries: Vec::new(),
        }
    }

    pub fn add_entry(
        &mut self,
        title_type: TitleType,
        content_type: ContentRecordType,
        title_id: u64,
        file: VirtualFile,
    ) {
        self.entries
            .insert((title_type, content_type, title_id), file);
    }

    pub fn add_entry_with_version(
        &mut self,
        title_type: TitleType,
        content_type: ContentRecordType,
        title_id: u64,
        version: u32,
        version_string: &str,
        file: VirtualFile,
    ) {
        if title_type != TitleType::Update {
            self.add_entry(title_type, content_type, title_id, file);
            return;
        }

        if let Some(entry) = self
            .multi_version_entries
            .iter_mut()
            .find(|entry| entry.title_id == title_id && entry.version == version)
        {
            entry.files[content_type as usize] = Some(file.clone());
            if !version_string.is_empty() {
                entry.version_string = version_string.to_owned();
            }
        } else {
            let mut files = std::array::from_fn(|_| None);
            files[content_type as usize] = Some(file.clone());
            self.multi_version_entries.push(ExternalUpdateEntry {
                title_id,
                version,
                version_string: version_string.to_owned(),
                files,
            });
        }

        let key = (title_type, content_type, title_id);
        if self.entries.contains_key(&key)
            && self
                .multi_version_entries
                .iter()
                .any(|entry| entry.title_id == title_id && entry.version > version)
        {
            return;
        }
        self.entries.insert(key, file);
    }

    /// Add every content entry found in an NSP/XCI container.
    /// Corresponds to upstream `ManualContentProvider::AddEntriesFromContainer`.
    pub fn add_entries_from_container(
        &mut self,
        file: VirtualFile,
        only_content: bool,
        base_program_id: Option<u64>,
    ) -> bool {
        let Some(nsp) = open_container_as_nsp(file) else {
            return false;
        };
        if nsp.get_ncas().is_empty() {
            return false;
        }

        let (versions, version_strings) = container_versions(&nsp);
        let mut added_entries = false;
        for (&title_id, nca_map) in nsp.get_ncas() {
            if base_program_id.is_some_and(|base| get_base_title_id(title_id) != base) {
                continue;
            }
            for (&(title_type_raw, content_type_raw), nca) in nca_map {
                let Some(title_type) = title_type_from_raw(title_type_raw) else {
                    continue;
                };
                let Some(content_type) = content_record_type_from_raw(content_type_raw) else {
                    continue;
                };
                if only_content && title_type != TitleType::Update && title_type != TitleType::AOC {
                    continue;
                }

                let entry_file = nca.get_base_file();
                if title_type == TitleType::Update {
                    self.add_entry_with_version(
                        title_type,
                        content_type,
                        title_id,
                        versions.get(&title_id).copied().unwrap_or(0),
                        version_strings
                            .get(&title_id)
                            .map(String::as_str)
                            .unwrap_or_default(),
                        entry_file,
                    );
                } else {
                    self.add_entry(title_type, content_type, title_id, entry_file);
                }
                added_entries = true;
            }
        }
        added_entries
    }

    pub fn clear_all_entries(&mut self) {
        self.entries.clear();
        self.multi_version_entries.clear();
    }

    pub fn list_update_versions(&self, title_id: u64) -> Vec<ExternalUpdateEntry> {
        let mut versions: Vec<_> = self
            .multi_version_entries
            .iter()
            .filter(|entry| entry.title_id == title_id)
            .cloned()
            .collect();
        versions.sort_by(|left, right| right.version.cmp(&left.version));
        versions
    }

    pub fn get_entry_for_version(
        &self,
        title_id: u64,
        content_type: ContentRecordType,
        version: u32,
    ) -> Option<VirtualFile> {
        self.multi_version_entries
            .iter()
            .find(|entry| entry.title_id == title_id && entry.version == version)
            .and_then(|entry| entry.files[content_type as usize].clone())
    }
}

impl ContentProvider for ManualContentProvider {
    fn refresh(&mut self) {
        // No-op for manual provider.
    }

    fn has_entry(&self, title_id: u64, record_type: ContentRecordType) -> bool {
        self.entries
            .keys()
            .any(|&(_, rt, tid)| rt == record_type && tid == title_id)
    }

    fn get_entry_version(&self, _title_id: u64) -> Option<u32> {
        None
    }

    fn list_update_versions(&self, title_id: u64) -> Vec<ExternalUpdateEntry> {
        ManualContentProvider::list_update_versions(self, title_id)
    }

    fn get_entry_for_version(
        &self,
        title_id: u64,
        content_type: ContentRecordType,
        version: u32,
    ) -> Option<VirtualFile> {
        ManualContentProvider::get_entry_for_version(self, title_id, content_type, version)
    }

    fn get_entry_unparsed(
        &self,
        title_id: u64,
        record_type: ContentRecordType,
    ) -> Option<VirtualFile> {
        self.entries
            .iter()
            .find(|(&(_, rt, tid), _)| rt == record_type && tid == title_id)
            .map(|(_, f)| f.clone())
    }

    fn get_entry_raw(&self, title_id: u64, record_type: ContentRecordType) -> Option<VirtualFile> {
        self.get_entry_unparsed(title_id, record_type)
    }

    fn list_entries_filter(
        &self,
        title_type: Option<TitleType>,
        record_type: Option<ContentRecordType>,
        title_id: Option<u64>,
    ) -> Vec<ContentProviderEntry> {
        self.entries
            .keys()
            .filter(|&&(tt, rt, tid)| {
                title_type.map_or(true, |t| t == tt)
                    && record_type.map_or(true, |r| r == rt)
                    && title_id.map_or(true, |id| id == tid)
            })
            .map(|&(_, rt, tid)| ContentProviderEntry {
                title_id: tid,
                record_type: rt,
            })
            .collect()
    }
}

impl Default for ManualContentProvider {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// ExternalContentProvider
// ============================================================================

/// Eden `ExternalContentProvider`: recursively scans configured game
/// directories and exposes update/DLC content, including multiple update
/// versions.
pub struct ExternalContentProvider {
    load_dirs: Vec<VirtualDir>,
    entries: ManualContentProvider,
    versions: BTreeMap<u64, u32>,
}

impl ExternalContentProvider {
    pub fn new(load_dirs: Vec<VirtualDir>) -> Self {
        let mut provider = Self {
            load_dirs,
            entries: ManualContentProvider::new(),
            versions: BTreeMap::new(),
        };
        provider.refresh();
        provider
    }

    pub fn add_directory(&mut self, directory: VirtualDir) {
        self.scan_directory(&directory);
        self.load_dirs.push(directory);
    }

    pub fn clear_directories(&mut self) {
        self.load_dirs.clear();
        self.entries.clear_all_entries();
        self.versions.clear();
    }

    fn scan_directory(&mut self, directory: &VirtualDir) {
        for file in directory.get_files() {
            match file.get_extension().to_ascii_lowercase().as_str() {
                "nsp" | "xci" => self.process_container(file),
                _ => {}
            }
        }
        for subdirectory in directory.get_subdirectories() {
            self.scan_directory(&subdirectory);
        }
    }

    fn process_container(&mut self, file: VirtualFile) {
        let Some(nsp) = open_container_as_nsp(file.clone()) else {
            return;
        };
        let (versions, _) = container_versions(&nsp);
        self.versions.extend(versions);
        self.entries.add_entries_from_container(file, true, None);
    }

    pub fn list_update_versions(&self, title_id: u64) -> Vec<ExternalUpdateEntry> {
        self.entries.list_update_versions(title_id)
    }

    pub fn get_entry_for_version(
        &self,
        title_id: u64,
        content_type: ContentRecordType,
        version: u32,
    ) -> Option<VirtualFile> {
        self.entries
            .get_entry_for_version(title_id, content_type, version)
    }
}

impl ContentProvider for ExternalContentProvider {
    fn refresh(&mut self) {
        self.entries.clear_all_entries();
        self.versions.clear();
        for directory in self.load_dirs.clone() {
            self.scan_directory(&directory);
        }
    }

    fn has_entry(&self, title_id: u64, record_type: ContentRecordType) -> bool {
        self.entries.has_entry(title_id, record_type)
    }

    fn get_entry_version(&self, title_id: u64) -> Option<u32> {
        self.versions.get(&title_id).copied()
    }

    fn get_entry_unparsed(
        &self,
        title_id: u64,
        record_type: ContentRecordType,
    ) -> Option<VirtualFile> {
        self.entries.get_entry_unparsed(title_id, record_type)
    }

    fn get_entry_raw(&self, title_id: u64, record_type: ContentRecordType) -> Option<VirtualFile> {
        self.entries.get_entry_raw(title_id, record_type)
    }

    fn list_entries_filter(
        &self,
        title_type: Option<TitleType>,
        record_type: Option<ContentRecordType>,
        title_id: Option<u64>,
    ) -> Vec<ContentProviderEntry> {
        self.entries
            .list_entries_filter(title_type, record_type, title_id)
    }

    fn list_update_versions(&self, title_id: u64) -> Vec<ExternalUpdateEntry> {
        ExternalContentProvider::list_update_versions(self, title_id)
    }

    fn get_entry_for_version(
        &self,
        title_id: u64,
        content_type: ContentRecordType,
        version: u32,
    ) -> Option<VirtualFile> {
        ExternalContentProvider::get_entry_for_version(self, title_id, content_type, version)
    }
}

impl Default for ExternalContentProvider {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

#[cfg(test)]
mod registered_cache_install_tests {
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::file_sys::fs_filesystem::OpenMode;
    use crate::file_sys::vfs::vfs_real::RealVfsFilesystem;
    use crate::file_sys::vfs::vfs_vector::VectorVfsFile;

    #[test]
    fn register_moves_placeholder_to_registered_layout_and_delete_removes_it() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("ruzu-registered-cache-{unique}"));
        let placeholder_path = base.join("placehld");
        let registered_path = base.join("registered");
        fs::create_dir_all(&placeholder_path).unwrap();
        fs::create_dir_all(&registered_path).unwrap();

        let vfs = RealVfsFilesystem::new();
        let placeholder_dir = vfs
            .arc_open_directory(placeholder_path.to_str().unwrap(), OpenMode::READ_WRITE)
            .unwrap();
        let registered_dir = vfs
            .arc_open_directory(registered_path.to_str().unwrap(), OpenMode::READ_WRITE)
            .unwrap();
        let placeholder_cache = PlaceholderCache::new(placeholder_dir);
        let mut registered_cache = RegisteredCache::new(Arc::clone(&registered_dir));
        let placeholder_id = [0x11; 0x10];
        let content_id = [0x22; 0x10];
        let payload = vec![0x5a; 0x4000];

        assert!(placeholder_cache.create(&placeholder_id, payload.len() as u64));
        assert!(placeholder_cache.write(&placeholder_id, 0, &payload));
        assert_eq!(placeholder_cache.get_rights_id(&placeholder_id), None);
        assert!(placeholder_cache.register(&mut registered_cache, &placeholder_id, &content_id,));
        assert!(!placeholder_cache.exists(&placeholder_id));
        assert_eq!(
            registered_cache
                .get_file_at_id(&content_id)
                .unwrap()
                .read_all_bytes(),
            payload
        );

        let existing_record = ContentRecord {
            nca_id: content_id,
            record_type: ContentRecordType::Program,
            ..ContentRecord::default()
        };
        let missing_record = ContentRecord {
            nca_id: [0x33; 0x10],
            record_type: ContentRecordType::Data,
            ..ContentRecord::default()
        };
        let mut installed_header = crate::file_sys::nca_metadata::CNMTHeader::default();
        installed_header.title_id = 0x0500_0000_0000_1000;
        installed_header.title_type = TitleType::Application as u8;
        registered_cache.meta.insert(
            installed_header.title_id,
            CNMT::from_parts(
                installed_header,
                crate::file_sys::nca_metadata::OptionalHeader::default(),
                vec![existing_record, missing_record],
                Vec::new(),
            ),
        );
        let mut yuzu_header = installed_header;
        yuzu_header.title_id += 1;
        registered_cache.yuzu_meta.insert(
            yuzu_header.title_id,
            CNMT::from_parts(
                yuzu_header,
                crate::file_sys::nca_metadata::OptionalHeader::default(),
                vec![existing_record],
                Vec::new(),
            ),
        );

        let entries = registered_cache.list_entries_filter(None, None, None);
        assert_eq!(entries.len(), 3);
        assert!(entries.contains(&ContentProviderEntry {
            title_id: installed_header.title_id,
            record_type: ContentRecordType::Meta,
        }));
        assert!(entries.contains(&ContentProviderEntry {
            title_id: installed_header.title_id,
            record_type: ContentRecordType::Program,
        }));
        assert!(entries.contains(&ContentProviderEntry {
            title_id: yuzu_header.title_id,
            record_type: ContentRecordType::Program,
        }));
        assert!(!entries.contains(&ContentProviderEntry {
            title_id: installed_header.title_id,
            record_type: ContentRecordType::Data,
        }));
        assert!(!entries.contains(&ContentProviderEntry {
            title_id: yuzu_header.title_id,
            record_type: ContentRecordType::Meta,
        }));

        let parser_calls = Arc::new(AtomicUsize::new(0));
        let parser_calls_for_callback = Arc::clone(&parser_calls);
        let mut parsed_cache =
            RegisteredCache::new_with_parser(Arc::clone(&registered_dir), move |file, _id| {
                parser_calls_for_callback.fetch_add(1, Ordering::Relaxed);
                Some(Arc::clone(file))
            });
        parser_calls.store(0, Ordering::Relaxed);
        parsed_cache.meta.insert(
            installed_header.title_id,
            CNMT::from_parts(
                installed_header,
                OptionalHeader::default(),
                vec![existing_record],
                Vec::new(),
            ),
        );
        assert!(parsed_cache
            .get_entry_raw(installed_header.title_id, ContentRecordType::Program)
            .is_some());
        assert_eq!(parser_calls.load(Ordering::Relaxed), 1);

        assert!(registered_cache.delete(&content_id));
        assert!(registered_cache.get_file_at_id(&content_id).is_none());
        assert!(registered_cache.delete(&content_id));

        let copy_calls = AtomicUsize::new(0);
        let input: VirtualFile = Arc::new(VectorVfsFile::new(
            vec![0x7a; 0x2000],
            "libnx-homebrew.nca".to_owned(),
            None,
        ));
        let nca = NCA::new(input, None);
        let copy = |source: &dyn VfsFile, destination: &dyn VfsFile, block_size: usize| {
            copy_calls.fetch_add(1, Ordering::Relaxed);
            super::super::vfs::vfs::vfs_raw_copy(source, destination, block_size)
        };
        assert_eq!(
            registered_cache.install_entry_nca(&nca, TitleType::Application, false, &copy),
            InstallResult::Success
        );
        assert_eq!(copy_calls.load(Ordering::Relaxed), 1);

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn check_map_for_content_record_uses_id_offset_for_indexed_programs() {
        let mut map = BTreeMap::new();
        let mut header = CNMTHeader::default();
        header.title_id = 0x0100_0000_0001_0000;
        let base_record = ContentRecord {
            nca_id: [0x11; 0x10],
            record_type: ContentRecordType::Program,
            id_offset: 0,
            ..ContentRecord::default()
        };
        let indexed_record = ContentRecord {
            nca_id: [0x22; 0x10],
            record_type: ContentRecordType::Program,
            id_offset: 2,
            ..ContentRecord::default()
        };
        map.insert(
            header.title_id,
            CNMT::from_parts(
                header,
                OptionalHeader::default(),
                vec![base_record, indexed_record],
                Vec::new(),
            ),
        );

        assert_eq!(
            check_map_for_content_record(&map, header.title_id, ContentRecordType::Program),
            Some([0x11; 0x10])
        );
        assert_eq!(
            check_map_for_content_record(&map, header.title_id + 2, ContentRecordType::Program),
            Some([0x22; 0x10])
        );
        assert_eq!(
            check_map_for_content_record(&map, header.title_id + 3, ContentRecordType::Program),
            None
        );
    }
}

#[cfg(test)]
mod external_content_provider_tests {
    use std::sync::Arc;

    use super::*;
    use crate::file_sys::vfs::vfs_vector::VectorVfsDirectory;

    #[test]
    fn add_refresh_and_clear_preserve_upstream_directory_ownership() {
        let directory: VirtualDir = Arc::new(VectorVfsDirectory::new(
            Vec::new(),
            Vec::new(),
            "games".to_string(),
            None,
        ));
        let mut provider = ExternalContentProvider::default();
        provider.add_directory(directory);
        assert_eq!(provider.load_dirs.len(), 1);
        provider.refresh();
        assert_eq!(provider.load_dirs.len(), 1);
        provider.clear_directories();
        assert!(provider.load_dirs.is_empty());
        assert!(provider.versions.is_empty());
        assert!(provider.entries.multi_version_entries.is_empty());
    }
}

#[cfg(test)]
mod manual_content_provider_tests {
    use std::sync::Arc;

    use super::*;
    use crate::file_sys::vfs::vfs_vector::VectorVfsFile;

    fn file(name: &str) -> VirtualFile {
        Arc::new(VectorVfsFile::new(Vec::new(), name.to_owned(), None))
    }

    #[test]
    fn highest_update_version_owns_the_active_entry() {
        let mut provider = ManualContentProvider::new();
        let old = file("old.nca");
        let new = file("new.nca");
        let title_id = 0x0100_0000_0000_0800;

        provider.add_entry_with_version(
            TitleType::Update,
            ContentRecordType::Program,
            title_id,
            1,
            "1.0.0",
            old.clone(),
        );
        provider.add_entry_with_version(
            TitleType::Update,
            ContentRecordType::Program,
            title_id,
            2,
            "2.0.0",
            new.clone(),
        );
        provider.add_entry_with_version(
            TitleType::Update,
            ContentRecordType::Program,
            title_id,
            1,
            "1.0.0",
            old,
        );

        let selected = provider
            .get_entry_raw(title_id, ContentRecordType::Program)
            .unwrap();
        assert!(Arc::ptr_eq(&selected, &new));
        let versions = provider.list_update_versions(title_id);
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].version, 2);
        assert_eq!(versions[0].version_string, "2.0.0");
        assert_eq!(versions[1].version, 1);
    }

    #[test]
    fn clearing_manual_entries_also_clears_versioned_updates() {
        let mut provider = ManualContentProvider::new();
        provider.add_entry_with_version(
            TitleType::Update,
            ContentRecordType::Program,
            0x800,
            1,
            "1.0.0",
            file("update.nca"),
        );

        provider.clear_all_entries();

        assert!(!provider.has_entry(0x800, ContentRecordType::Program));
        assert!(provider.list_update_versions(0x800).is_empty());
    }

    #[test]
    fn frontend_manual_slot_exposes_versioned_updates_through_the_union() {
        let mut manual = ManualContentProvider::new();
        let title_id = 0x0100_0000_0000_0800;
        manual.add_entry_with_version(
            TitleType::Update,
            ContentRecordType::Program,
            title_id,
            0xB_0000,
            "1.4.3",
            file("update.nca"),
        );

        let mut union = ContentProviderUnion::new();
        unsafe {
            union.set_slot(
                ContentProviderUnionSlot::FrontendManual,
                (&mut manual as *mut ManualContentProvider) as *mut dyn ContentProvider,
            );
        }

        let versions = union.list_update_versions(title_id);
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version, 0xB_0000);
        assert!(union
            .get_entry_for_version(title_id, ContentRecordType::Program, 0xB_0000)
            .is_some());
        assert_eq!(
            union.get_slot_for_entry(title_id, ContentRecordType::Program),
            Some(ContentProviderUnionSlot::FrontendManual)
        );
    }
}

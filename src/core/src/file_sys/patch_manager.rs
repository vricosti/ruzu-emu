// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Ported from: core/file_sys/patch_manager.h / .cpp

use std::sync::Arc;

use super::common_funcs::get_base_title_id;
use super::content_archive::NCA;
use super::control_metadata::{LANGUAGE_NAMES, NACP};
use super::nca_metadata::{ContentRecordType, TitleType};
use super::registered_cache::{
    get_update_title_id, ContentProvider, ContentProviderEntry, ContentProviderUnionSlot,
};
use super::romfs::{create_romfs, extract_romfs};
use super::vfs::vfs::get_or_create_directory_relative;
use super::vfs::vfs_cached::CachedVfsDirectory;
use super::vfs::vfs_layered::LayeredVfsDirectory;
use super::vfs::vfs_types::{VirtualDir, VirtualFile};
use super::vfs::vfs_vector::VectorVfsFile;

use crate::hle::service::filesystem::filesystem::FileSystemController;
use crate::memory::cheat_engine::{CheatParser, TextCheatParser};
use crate::memory::dmnt_cheat_types::CheatEntry;

// ============================================================================
// Constants
// ============================================================================

const SINGLE_BYTE_MODULUS: u32 = 0x100;

/// The standard set of ExeFS file names.
/// Corresponds to upstream `EXEFS_FILE_NAMES`.
const EXEFS_FILE_NAMES: &[&str] = &[
    "main",
    "main.npdm",
    "rtld",
    "sdk",
    "subsdk0",
    "subsdk1",
    "subsdk2",
    "subsdk3",
    "subsdk4",
    "subsdk5",
    "subsdk6",
    "subsdk7",
    "subsdk8",
    "subsdk9",
];

// ============================================================================
// Title version formatting
// ============================================================================

/// Format a title version number into a human-readable string.
/// Corresponds to upstream `FormatTitleVersion`.
fn format_title_version(mut version: u32) -> String {
    let mut bytes = [0u8; 4];
    bytes[0] = (version % SINGLE_BYTE_MODULUS) as u8;
    for i in 1..4 {
        version /= SINGLE_BYTE_MODULUS;
        bytes[i] = (version % SINGLE_BYTE_MODULUS) as u8;
    }
    format!("v{}.{}.{}", bytes[3], bytes[2], bytes[1])
}

// ============================================================================
// Helper functions
// ============================================================================

/// Find a subdirectory case-insensitively.
/// Corresponds to upstream `FindSubdirectoryCaseless`.
fn find_subdirectory_caseless(dir: &VirtualDir, name: &str) -> Option<VirtualDir> {
    let name_lower = name.to_lowercase();
    for subdir in dir.get_subdirectories() {
        if subdir.get_name().to_lowercase() == name_lower {
            return Some(subdir);
        }
    }
    None
}

/// Read the cheat file matching the first 16 hexadecimal build-ID characters.
/// Corresponds to upstream `ReadCheatFileFromFolder`.
fn read_cheat_file_from_folder(
    title_id: u64,
    build_id: &BuildId,
    base_path: &VirtualDir,
    upper: bool,
) -> Option<Vec<CheatEntry>> {
    let build_id_raw = build_id
        .iter()
        .map(|byte| {
            if upper {
                format!("{byte:02X}")
            } else {
                format!("{byte:02x}")
            }
        })
        .collect::<String>();
    let short_build_id = &build_id_raw[..std::mem::size_of::<u64>() * 2];
    let file = match base_path.get_file(&format!("{short_build_id}.txt")) {
        Some(file) => file,
        None => {
            log::info!(
                "No cheats file found for title_id={title_id:016X}, build_id={short_build_id}"
            );
            return None;
        }
    };

    let mut data = vec![0; file.get_size()];
    let data_len = data.len();
    if file.read(&mut data, data_len, 0) != data_len {
        log::info!(
            "Failed to read cheats file for title_id={title_id:016X}, build_id={short_build_id}"
        );
        return None;
    }

    Some(TextCheatParser.parse(&String::from_utf8_lossy(&data)))
}

/// Append a string with comma separation if not empty.
/// Corresponds to upstream `AppendCommaIfNotEmpty`.
fn append_comma_if_not_empty(to: &mut String, with: &str) {
    if to.is_empty() {
        to.push_str(with);
    } else {
        to.push_str(", ");
        to.push_str(with);
    }
}

/// Check if a directory is valid and non-empty.
/// Corresponds to upstream `IsDirValidAndNonEmpty`.
fn is_dir_valid_and_non_empty(dir: &Option<VirtualDir>) -> bool {
    match dir {
        Some(d) => !d.get_files().is_empty() || !d.get_subdirectories().is_empty(),
        None => false,
    }
}

/// Get the disabled addons list for a title from settings.
/// Corresponds to upstream `Settings::values.disabled_addons[title_id]`.
fn get_disabled_addons(title_id: u64) -> Vec<String> {
    let settings = common::settings::values();
    settings
        .disabled_addons
        .get(&title_id)
        .cloned()
        .unwrap_or_default()
}

fn is_versioned_external_update_disabled(disabled: &[String], version: u32) -> bool {
    disabled
        .iter()
        .any(|entry| entry == &format!("Update@{version}"))
}

fn is_legacy_update_disabled(disabled: &[String]) -> bool {
    ["Update", "Update (NAND)", "Update (SDMC)"]
        .iter()
        .any(|name| disabled.iter().any(|entry| entry == name))
}

/// Apply LayeredFS patches to a RomFS file.
/// Corresponds to upstream static `ApplyLayeredFS`.
fn apply_layered_fs(
    romfs: &mut VirtualFile,
    title_id: u64,
    record_type: ContentRecordType,
    fs_controller: &FileSystemController,
) {
    let load_dir = fs_controller.get_modification_load_root(title_id);
    let sdmc_load_dir = fs_controller.get_sdmc_modification_load_root(title_id);

    if (record_type != ContentRecordType::Program
        && record_type != ContentRecordType::Data
        && record_type != ContentRecordType::HtmlDocument)
        || (load_dir.is_none() && sdmc_load_dir.is_none())
    {
        return;
    }

    let disabled = get_disabled_addons(title_id);

    let mut patch_dirs: Vec<VirtualDir> = if let Some(ref ld) = load_dir {
        ld.get_subdirectories()
    } else {
        Vec::new()
    };

    if let Some(sdmc_dir) = sdmc_load_dir {
        if !disabled.iter().any(|d| d == "SDMC") {
            patch_dirs.push(sdmc_dir);
        }
    }

    patch_dirs.sort_by(|l, r| l.get_name().cmp(&r.get_name()));

    let mut layers: Vec<VirtualDir> = Vec::new();
    let mut layers_ext: Vec<VirtualDir> = Vec::new();

    for subdir in &patch_dirs {
        if disabled.iter().any(|d| d == &subdir.get_name()) {
            continue;
        }

        if let Some(romfs_dir) = find_subdirectory_caseless(subdir, "romfs") {
            layers.push(Arc::new(CachedVfsDirectory::new(romfs_dir)));
        }

        if let Some(ext_dir) = find_subdirectory_caseless(subdir, "romfs_ext") {
            layers_ext.push(Arc::new(CachedVfsDirectory::new(ext_dir)));
        }

        if record_type == ContentRecordType::HtmlDocument {
            if let Some(manual_dir) = find_subdirectory_caseless(subdir, "manual_html") {
                layers.push(Arc::new(CachedVfsDirectory::new(manual_dir)));
            }
        }
    }

    // When there are no layers to apply, return early
    if layers.is_empty() && layers_ext.is_empty() {
        return;
    }

    let extracted = match extract_romfs(Some(romfs.clone())) {
        Some(dir) => dir,
        None => return,
    };

    layers.push(extracted);

    let layered = match LayeredVfsDirectory::make_layered_directory(layers, String::new()) {
        Some(dir) => dir,
        None => return,
    };

    let layered_ext = LayeredVfsDirectory::make_layered_directory(layers_ext, String::new());

    let packed = match create_romfs(Some(layered), layered_ext) {
        Some(f) => f,
        None => return,
    };

    log::info!("    RomFS: LayeredFS patches applied successfully");
    *romfs = packed;
}

// ============================================================================
// PatchType, Patch
// ============================================================================

/// Patch type.
/// Corresponds to upstream `PatchType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchType {
    Update,
    DLC,
    Mod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchSource {
    Unknown,
    NAND,
    SDMC,
    External,
    Packed,
}

/// A single patch entry.
/// Corresponds to upstream `Patch`.
#[derive(Debug, Clone)]
pub struct Patch {
    pub enabled: bool,
    pub name: String,
    pub version: String,
    pub patch_type: PatchType,
    pub program_id: u64,
    pub title_id: u64,
    pub source: PatchSource,
    pub location: String,
    pub numeric_version: u32,
}

/// Build ID — 0x20 bytes.
pub type BuildId = [u8; 0x20];

// ============================================================================
// PatchManager
// ============================================================================

/// Centralized manager for patches to games.
/// Corresponds to upstream `PatchManager`.
///
/// Upstream always takes `const FileSystemController&` and `const ContentProvider&`.
/// In Rust, callers that do not yet have these wired may pass `None`. Methods that
/// need them will gracefully no-op when the reference is absent.
pub struct PatchManager<'a> {
    title_id: u64,
    fs_controller: Option<&'a FileSystemController>,
    content_provider: Option<&'a dyn ContentProvider>,
}

impl<'a> PatchManager<'a> {
    /// Create a new PatchManager with all dependencies.
    /// Corresponds to upstream `PatchManager::PatchManager`.
    pub fn new(
        title_id: u64,
        fs_controller: &'a FileSystemController,
        content_provider: &'a dyn ContentProvider,
    ) -> Self {
        Self {
            title_id,
            fs_controller: Some(fs_controller),
            content_provider: Some(content_provider),
        }
    }

    /// Create a PatchManager without filesystem / content provider references.
    /// Methods requiring those references will return defaults / no-ops.
    pub fn new_without_deps(title_id: u64) -> Self {
        Self {
            title_id,
            fs_controller: None,
            content_provider: None,
        }
    }

    /// Get the title ID.
    /// Corresponds to upstream `PatchManager::GetTitleID`.
    pub fn get_title_id(&self) -> u64 {
        self.title_id
    }

    /// Patch ExeFS with updates and LayeredExeFS.
    /// Corresponds to upstream `PatchManager::PatchExeFS`.
    pub fn patch_exefs(&self, mut exefs: VirtualDir) -> VirtualDir {
        log::info!("Patching ExeFS for title_id={:016X}", self.title_id);

        let disabled = get_disabled_addons(self.title_id);
        let update_tid = get_update_title_id(self.title_id);
        let mut selected_update: Option<(VirtualFile, u32)> = None;
        if let Some(provider) = self.content_provider {
            let versions = provider.list_update_versions(update_tid);
            if let Some(version) = versions
                .iter()
                .find(|entry| !is_versioned_external_update_disabled(&disabled, entry.version))
                .map(|entry| entry.version)
            {
                selected_update = provider
                    .get_entry_for_version(update_tid, ContentRecordType::Program, version)
                    .map(|file| (file, version));
            }

            if selected_update.is_none() {
                for (slot, _) in provider.list_entries_filter_origin(
                    Some(TitleType::Update),
                    Some(ContentRecordType::Program),
                    Some(update_tid),
                ) {
                    if matches!(
                        slot,
                        ContentProviderUnionSlot::External
                            | ContentProviderUnionSlot::FrontendManual
                    ) {
                        continue;
                    }
                    let disabled_name = match slot {
                        ContentProviderUnionSlot::UserNAND | ContentProviderUnionSlot::SysNAND => {
                            "Update (NAND)"
                        }
                        ContentProviderUnionSlot::SDMC => "Update (SDMC)",
                        ContentProviderUnionSlot::External => continue,
                        ContentProviderUnionSlot::FrontendManual => continue,
                    };
                    if disabled.contains(&disabled_name.to_string()) {
                        continue;
                    }
                    if let Some(file) = provider.get_entry_raw_from_slot(
                        slot,
                        update_tid,
                        ContentRecordType::Program,
                    ) {
                        selected_update = Some((
                            file,
                            provider
                                .get_entry_version_from_slot(slot, update_tid)
                                .unwrap_or(0),
                        ));
                        break;
                    }
                }
            }

            if selected_update.is_none()
                && versions.is_empty()
                && !provider.supports_origin_tracking()
                && !is_legacy_update_disabled(&disabled)
            {
                selected_update = provider
                    .get_entry_raw(update_tid, ContentRecordType::Program)
                    .map(|file| (file, provider.get_entry_version(update_tid).unwrap_or(0)));
            }
        }

        if let Some((file, version)) = selected_update {
            let update = NCA::new(file, None);
            if let Some(update_exefs) = update.get_exefs() {
                log::info!(
                    "    ExeFS: Update ({}) applied successfully",
                    format_title_version(version)
                );
                exefs = update_exefs;
            }
        }

        // LayeredExeFS
        let load_dir = self
            .fs_controller
            .and_then(|fc| fc.get_modification_load_root(self.title_id));
        let sdmc_load_dir = self
            .fs_controller
            .and_then(|fc| fc.get_sdmc_modification_load_root(self.title_id));

        let mut patch_dirs: Vec<VirtualDir> = Vec::new();
        if let Some(ref sdmc_dir) = sdmc_load_dir {
            patch_dirs.push(sdmc_dir.clone());
        }
        if let Some(ref ld) = load_dir {
            let mut load_patch_dirs = ld.get_subdirectories();
            patch_dirs.append(&mut load_patch_dirs);
        }

        patch_dirs.sort_by(|l, r| l.get_name().cmp(&r.get_name()));

        let mut layers: Vec<VirtualDir> = Vec::new();
        for subdir in &patch_dirs {
            if disabled.iter().any(|d| d == &subdir.get_name()) {
                continue;
            }
            if let Some(exefs_dir) = find_subdirectory_caseless(subdir, "exefs") {
                layers.push(exefs_dir);
            }
        }
        layers.push(exefs.clone());

        if let Some(layered) = LayeredVfsDirectory::make_layered_directory(layers, String::new()) {
            log::info!("    ExeFS: LayeredExeFS patches applied successfully");
            exefs = layered;
        }

        if *common::settings::values().dump_exefs.get_value() {
            log::info!("Dumping ExeFS for title_id={:016X}", self.title_id);
            if let Some(dump_dir) = self
                .fs_controller
                .and_then(|fc| fc.get_modification_dump_root(self.title_id))
            {
                if let Some(exefs_dir) =
                    get_or_create_directory_relative(dump_dir.as_ref(), "/exefs")
                {
                    super::vfs::vfs::vfs_raw_copy_d(exefs.as_ref(), exefs_dir.as_ref(), 0x1000);
                }
            }
        }

        exefs
    }

    /// Collect IPS/IPSwitch patches from patch directories matching a build ID.
    /// Corresponds to upstream `PatchManager::CollectPatches`.
    fn collect_patches(&self, patch_dirs: &[VirtualDir], build_id: &str) -> Vec<VirtualFile> {
        let disabled = get_disabled_addons(self.title_id);
        let nso_build_id = format!("{:0<64}", build_id);
        let mut out = Vec::new();

        for subdir in patch_dirs {
            if disabled.iter().any(|d| d == &subdir.get_name()) {
                continue;
            }

            if let Some(exefs_dir) = find_subdirectory_caseless(subdir, "exefs") {
                for file in exefs_dir.get_files() {
                    let ext = file.get_extension();
                    if ext == "ips" {
                        let name = file.get_name();
                        let dot_pos = name.find('.').unwrap_or(name.len());
                        let this_build_id = format!("{:0<64}", &name[..dot_pos]);
                        if nso_build_id == this_build_id {
                            out.push(file);
                        }
                    } else if ext == "pchtxt" {
                        let compiler = super::ips_layer::IpSwitchCompiler::new(file.clone());
                        if !compiler.is_valid() {
                            continue;
                        }
                        let this_build_id: String = compiler
                            .get_build_id()
                            .iter()
                            .map(|b| format!("{:02x}", b))
                            .collect();
                        let this_build_id = format!("{:0<64}", this_build_id);
                        if nso_build_id == this_build_id {
                            out.push(file);
                        }
                    }
                }
            }
        }

        out
    }

    /// Patch NSO data with IPS and IPSwitch patches.
    /// Corresponds to upstream `PatchManager::PatchNSO`.
    pub fn patch_nso(&self, nso: Vec<u8>, name: &str) -> Vec<u8> {
        use super::super::loader::nso::NsoHeader;

        if nso.len() < std::mem::size_of::<NsoHeader>() {
            return nso;
        }

        let mut header = NsoHeader::default();
        let header_size = std::mem::size_of::<NsoHeader>();
        unsafe {
            std::ptr::copy_nonoverlapping(
                nso.as_ptr(),
                &mut header as *mut NsoHeader as *mut u8,
                header_size,
            );
        }

        use common::common_funcs::make_magic;
        if header.magic != make_magic(b'N', b'S', b'O', b'0') {
            return nso;
        }

        let build_id_raw: String = header
            .build_id
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect();
        let build_id = build_id_raw.trim_end_matches('0').to_string();

        if *common::settings::values().dump_nso.get_value() {
            log::info!(
                "Dumping NSO for name={}, build_id={}, title_id={:016X}",
                name,
                build_id,
                self.title_id
            );
            if let Some(dump_dir) = self
                .fs_controller
                .and_then(|fc| fc.get_modification_dump_root(self.title_id))
            {
                if let Some(nso_dir) = get_or_create_directory_relative(dump_dir.as_ref(), "/nso") {
                    let filename = format!("{}-{}.nso", name, build_id);
                    if let Some(file) = nso_dir.create_file(&filename) {
                        file.resize(nso.len());
                        file.write(&nso, nso.len(), 0);
                    }
                }
            }
        }

        log::info!("Patching NSO for name={}, build_id={}", name, build_id);

        let load_dir = match self
            .fs_controller
            .and_then(|fc| fc.get_modification_load_root(self.title_id))
        {
            Some(dir) => dir,
            None => {
                log::error!(
                    "Cannot load mods for invalid title_id={:016X}",
                    self.title_id
                );
                return nso;
            }
        };

        let mut patch_dirs = load_dir.get_subdirectories();
        patch_dirs.sort_by(|l, r| l.get_name().cmp(&r.get_name()));
        let patches = self.collect_patches(&patch_dirs, &build_id);

        let mut out = nso;
        for patch_file in &patches {
            if patch_file.get_extension() == "ips" {
                if let Some(containing) = patch_file.get_containing_directory() {
                    if let Some(parent) = containing.get_parent_directory() {
                        log::info!(
                            "    - Applying IPS patch from mod \"{}\"",
                            parent.get_name()
                        );
                    }
                }
                let source: VirtualFile =
                    Arc::new(VectorVfsFile::new(out.clone(), String::new(), None));
                if let Some(patched) = super::ips_layer::patch_ips(&source, patch_file) {
                    let size = patched.get_size();
                    let mut buf = vec![0u8; size];
                    patched.read(&mut buf, size, 0);
                    out = buf;
                }
            } else if patch_file.get_extension() == "pchtxt" {
                if let Some(containing) = patch_file.get_containing_directory() {
                    if let Some(parent) = containing.get_parent_directory() {
                        log::info!(
                            "    - Applying IPSwitch patch from mod \"{}\"",
                            parent.get_name()
                        );
                    }
                }
                let compiler = super::ips_layer::IpSwitchCompiler::new(patch_file.clone());
                let source: VirtualFile =
                    Arc::new(VectorVfsFile::new(out.clone(), String::new(), None));
                if let Some(patched) = compiler.apply(&source) {
                    let size = patched.get_size();
                    let mut buf = vec![0u8; size];
                    patched.read(&mut buf, size, 0);
                    out = buf;
                }
            }
        }

        if out.len() < std::mem::size_of::<NsoHeader>() {
            return out;
        }

        // Restore the original header (upstream does this to keep the original build ID etc.)
        unsafe {
            std::ptr::copy_nonoverlapping(
                &header as *const NsoHeader as *const u8,
                out.as_mut_ptr(),
                header_size,
            );
        }

        out
    }

    /// Check if PatchNSO would have any effect given the NSO's build ID.
    /// Corresponds to upstream `PatchManager::HasNSOPatch`.
    pub fn has_nso_patch(&self, build_id: &BuildId, name: &str) -> bool {
        let build_id_raw: String = build_id.iter().map(|b| format!("{:02x}", b)).collect();
        let build_id_str = build_id_raw.trim_end_matches('0').to_string();

        log::info!(
            "Querying NSO patch existence for build_id={}, name={}",
            build_id_str,
            name
        );

        let load_dir = match self
            .fs_controller
            .and_then(|fc| fc.get_modification_load_root(self.title_id))
        {
            Some(dir) => dir,
            None => {
                log::error!(
                    "Cannot load mods for invalid title_id={:016X}",
                    self.title_id
                );
                return false;
            }
        };

        let mut patch_dirs = load_dir.get_subdirectories();
        patch_dirs.sort_by(|l, r| l.get_name().cmp(&r.get_name()));

        !self.collect_patches(&patch_dirs, &build_id_str).is_empty()
    }

    /// Create the enabled cheat list for the supplied NSO build ID.
    /// Corresponds to upstream `PatchManager::CreateCheatList`.
    pub fn create_cheat_list(&self, build_id: &BuildId) -> Vec<CheatEntry> {
        let load_dir = match self
            .fs_controller
            .and_then(|controller| controller.get_modification_load_root(self.title_id))
        {
            Some(dir) => dir,
            None => {
                log::error!(
                    "Cannot load mods for invalid title_id={:016X}",
                    self.title_id
                );
                return Vec::new();
            }
        };

        let disabled = get_disabled_addons(self.title_id);
        let mut patch_dirs = load_dir.get_subdirectories();
        patch_dirs.sort_by(|left, right| left.get_name().cmp(&right.get_name()));

        // <mod dir> / cheats / <build id>.txt
        let mut out = Vec::new();
        for subdir in patch_dirs {
            if disabled.iter().any(|entry| entry == &subdir.get_name()) {
                continue;
            }
            if let Some(cheats_dir) = find_subdirectory_caseless(&subdir, "cheats") {
                if let Some(mut entries) =
                    read_cheat_file_from_folder(self.title_id, build_id, &cheats_dir, true)
                {
                    out.append(&mut entries);
                }
                if let Some(mut entries) =
                    read_cheat_file_from_folder(self.title_id, build_id, &cheats_dir, false)
                {
                    out.append(&mut entries);
                }
            }
        }

        // User-friendly loading of files whose names start with `cheat_`.
        // <mod dir> / <cheat file>.txt
        for file in load_dir.get_files() {
            let name = file.get_name();
            if !name.starts_with("cheat_") || disabled.iter().any(|entry| entry == &name) {
                continue;
            }

            let mut data = vec![0; file.get_size()];
            let data_len = data.len();
            if file.read(&mut data, data_len, 0) == data_len {
                out.extend(TextCheatParser.parse(&String::from_utf8_lossy(&data)));
            } else {
                log::info!(
                    "Failed to read cheats file for title_id={:016X}",
                    self.title_id
                );
            }
        }
        out
    }

    /// Get the list of patches available for this title.
    /// Corresponds to upstream `PatchManager::GetPatches`.
    pub fn get_patches(&self, update_raw: Option<VirtualFile>) -> Vec<Patch> {
        if self.title_id == 0 {
            return Vec::new();
        }

        let mut out = Vec::new();
        let disabled = get_disabled_addons(self.title_id);

        // Game Updates
        let update_tid = get_update_title_id(self.title_id);
        if self
            .content_provider
            .is_some_and(ContentProvider::supports_origin_tracking)
        {
            let provider = self.content_provider.unwrap();
            let mut external_updates: Vec<Patch> = provider
                .list_update_versions(update_tid)
                .into_iter()
                .map(|entry| Patch {
                    enabled: !is_versioned_external_update_disabled(&disabled, entry.version),
                    name: "Update".to_string(),
                    version: if entry.version_string.is_empty() {
                        format_title_version(entry.version)
                    } else {
                        entry.version_string
                    },
                    patch_type: PatchType::Update,
                    program_id: self.title_id,
                    title_id: update_tid,
                    source: PatchSource::External,
                    location: String::new(),
                    numeric_version: entry.version,
                })
                .collect();
            let mut found_enabled = false;
            for patch in &mut external_updates {
                if patch.enabled {
                    if found_enabled {
                        patch.enabled = false;
                    } else {
                        found_enabled = true;
                    }
                }
            }
            out.extend(external_updates);

            for (slot, _) in provider.list_entries_filter_origin(
                Some(TitleType::Update),
                Some(ContentRecordType::Program),
                Some(update_tid),
            ) {
                if matches!(
                    slot,
                    ContentProviderUnionSlot::External | ContentProviderUnionSlot::FrontendManual
                ) {
                    continue;
                }
                let (source, suffix) = match slot {
                    ContentProviderUnionSlot::UserNAND | ContentProviderUnionSlot::SysNAND => {
                        (PatchSource::NAND, " (NAND)")
                    }
                    ContentProviderUnionSlot::SDMC => (PatchSource::SDMC, " (SDMC)"),
                    ContentProviderUnionSlot::External => continue,
                    ContentProviderUnionSlot::FrontendManual => continue,
                };
                let numeric_version = provider
                    .get_entry_version_from_slot(slot, update_tid)
                    .unwrap_or(0);
                let name = format!("Update{suffix}");
                out.push(Patch {
                    enabled: !disabled.contains(&name),
                    name,
                    version: (numeric_version != 0)
                        .then(|| format_title_version(numeric_version))
                        .unwrap_or_default(),
                    patch_type: PatchType::Update,
                    program_id: self.title_id,
                    title_id: update_tid,
                    source,
                    location: String::new(),
                    numeric_version,
                });
            }
        } else {
            let update_pm = PatchManager {
                title_id: update_tid,
                fs_controller: self.fs_controller,
                content_provider: self.content_provider,
            };
            let nacp = update_pm.get_control_metadata().0;
            let update_disabled = disabled.iter().any(|d| d == "Update");
            let mut update_patch = Patch {
                enabled: !update_disabled,
                name: "Update".to_string(),
                version: String::new(),
                patch_type: PatchType::Update,
                program_id: self.title_id,
                title_id: self.title_id,
                source: PatchSource::Unknown,
                location: String::new(),
                numeric_version: 0,
            };

            if let Some(ref nacp_data) = nacp {
                update_patch.version = nacp_data.get_version_string();
                out.push(update_patch);
            } else if self
                .content_provider
                .map(|cp| cp.has_entry(update_tid, ContentRecordType::Program))
                .unwrap_or(false)
            {
                let meta_ver = self
                    .content_provider
                    .and_then(|cp| cp.get_entry_version(update_tid));
                if let Some(version) = meta_ver.filter(|version| *version != 0) {
                    update_patch.version = format_title_version(version);
                    update_patch.numeric_version = version;
                }
                out.push(update_patch);
            } else if update_raw.is_some() {
                update_patch.version = "PACKED".to_string();
                update_patch.source = PatchSource::Packed;
                out.push(update_patch);
            }
        }

        // General Mods (LayeredFS and IPS)
        if let Some(mod_dir) = self
            .fs_controller
            .and_then(|fc| fc.get_modification_load_root(self.title_id))
        {
            for file in mod_dir.get_files() {
                let name = file.get_name();
                if name.starts_with("cheat_") {
                    let mod_disabled = disabled.contains(&name);
                    out.push(Patch {
                        enabled: !mod_disabled,
                        name,
                        version: "Cheats".to_string(),
                        patch_type: PatchType::Mod,
                        program_id: self.title_id,
                        title_id: self.title_id,
                        source: PatchSource::Unknown,
                        location: file.get_full_path(),
                        numeric_version: 0,
                    });
                }
            }
            for mod_subdir in mod_dir.get_subdirectories() {
                let mut types = String::new();

                let exefs_dir = find_subdirectory_caseless(&mod_subdir, "exefs");
                if is_dir_valid_and_non_empty(&exefs_dir) {
                    let mut ips = false;
                    let mut ipswitch = false;
                    let mut layeredfs = false;

                    if let Some(ref exefs) = exefs_dir {
                        for file in exefs.get_files() {
                            if file.get_extension() == "ips" {
                                ips = true;
                            } else if file.get_extension() == "pchtxt" {
                                ipswitch = true;
                            } else if EXEFS_FILE_NAMES.contains(&file.get_name().as_str()) {
                                layeredfs = true;
                            }
                        }
                    }

                    if ips {
                        append_comma_if_not_empty(&mut types, "IPS");
                    }
                    if ipswitch {
                        append_comma_if_not_empty(&mut types, "IPSwitch");
                    }
                    if layeredfs {
                        append_comma_if_not_empty(&mut types, "LayeredExeFS");
                    }
                }

                if is_dir_valid_and_non_empty(&find_subdirectory_caseless(&mod_subdir, "romfs"))
                    || is_dir_valid_and_non_empty(&find_subdirectory_caseless(
                        &mod_subdir,
                        "romfslite",
                    ))
                {
                    append_comma_if_not_empty(&mut types, "LayeredFS");
                }
                if is_dir_valid_and_non_empty(&find_subdirectory_caseless(&mod_subdir, "romfs_ext"))
                {
                    append_comma_if_not_empty(&mut types, "ExtLayeredFS");
                }
                if is_dir_valid_and_non_empty(&find_subdirectory_caseless(&mod_subdir, "cheats")) {
                    append_comma_if_not_empty(&mut types, "Cheats");
                }

                if types.is_empty() {
                    continue;
                }

                let mod_disabled = disabled.iter().any(|d| d == &mod_subdir.get_name());
                out.push(Patch {
                    enabled: !mod_disabled,
                    name: mod_subdir.get_name(),
                    version: types,
                    patch_type: PatchType::Mod,
                    program_id: self.title_id,
                    title_id: self.title_id,
                    source: PatchSource::Unknown,
                    location: mod_subdir.get_full_path(),
                    numeric_version: 0,
                });
            }
        }

        // SDMC mod directory (RomFS LayeredFS)
        if let Some(sdmc_mod_dir) = self
            .fs_controller
            .and_then(|fc| fc.get_sdmc_modification_load_root(self.title_id))
        {
            let mut types = String::new();
            if is_dir_valid_and_non_empty(&find_subdirectory_caseless(&sdmc_mod_dir, "exefs")) {
                append_comma_if_not_empty(&mut types, "LayeredExeFS");
            }
            if is_dir_valid_and_non_empty(&find_subdirectory_caseless(&sdmc_mod_dir, "romfs"))
                || is_dir_valid_and_non_empty(&find_subdirectory_caseless(
                    &sdmc_mod_dir,
                    "romfslite",
                ))
            {
                append_comma_if_not_empty(&mut types, "LayeredFS");
            }
            if is_dir_valid_and_non_empty(&find_subdirectory_caseless(&sdmc_mod_dir, "romfs_ext")) {
                append_comma_if_not_empty(&mut types, "ExtLayeredFS");
            }

            if !types.is_empty() {
                let mod_disabled = disabled.iter().any(|d| d == "SDMC");
                out.push(Patch {
                    enabled: !mod_disabled,
                    name: "SDMC".to_string(),
                    version: types,
                    patch_type: PatchType::Mod,
                    program_id: self.title_id,
                    title_id: self.title_id,
                    source: PatchSource::Unknown,
                    location: String::new(),
                    numeric_version: 0,
                });
            }
        }

        // DLC
        let dlc_entries = self
            .content_provider
            .map(|cp| {
                cp.list_entries_filter(Some(TitleType::AOC), Some(ContentRecordType::Data), None)
            })
            .unwrap_or_default();
        let cp_ref = self.content_provider;
        let title_id_copy = self.title_id;
        let mut dlc_match: Vec<ContentProviderEntry> = dlc_entries
            .into_iter()
            .filter(|entry| {
                get_base_title_id(entry.title_id) == title_id_copy
                    && cp_ref
                        .and_then(|cp| cp.get_entry(entry.title_id, ContentRecordType::Data))
                        .map(|nca| {
                            nca.get_status() == super::partition_filesystem::ResultStatus::Success
                        })
                        .unwrap_or(false)
            })
            .collect();

        if !dlc_match.is_empty() {
            dlc_match.sort();

            let mut list = String::new();
            for (i, entry) in dlc_match.iter().enumerate() {
                if i > 0 {
                    list.push_str(", ");
                }
                list.push_str(&format!("{}", entry.title_id & 0x7FF));
            }

            let dlc_disabled = disabled.iter().any(|d| d == "DLC");
            out.push(Patch {
                enabled: !dlc_disabled,
                name: "DLC".to_string(),
                version: list,
                patch_type: PatchType::DLC,
                program_id: self.title_id,
                title_id: dlc_match.last().unwrap().title_id,
                source: PatchSource::Unknown,
                location: String::new(),
                numeric_version: 0,
            });
        }

        out
    }

    /// Get the game version from update meta NCA, falling back to base.
    /// Corresponds to upstream `PatchManager::GetGameVersion`.
    pub fn get_game_version(&self) -> Option<u32> {
        let cp = self.content_provider?;
        let update_tid = get_update_title_id(self.title_id);
        if cp.has_entry(update_tid, ContentRecordType::Program) {
            return cp.get_entry_version(update_tid);
        }
        cp.get_entry_version(self.title_id)
    }

    /// Get control metadata (NACP + icon) for this title.
    /// Corresponds to upstream `PatchManager::GetControlMetadata`.
    pub fn get_control_metadata(&self) -> (Option<NACP>, Option<VirtualFile>) {
        let cp = match self.content_provider {
            Some(cp) => cp,
            None => return (None, None),
        };
        let base_control_nca = match cp.get_entry(self.title_id, ContentRecordType::Control) {
            Some(nca) => nca,
            None => return (None, None),
        };
        self.parse_control_nca(&base_control_nca)
    }

    /// Parse a control NCA to extract NACP metadata and icon file.
    /// Corresponds to upstream `PatchManager::ParseControlNCA`.
    ///
    /// Returns `(Option<NACP>, Option<VirtualFile>)` — the NACP and icon file.
    pub fn parse_control_nca(&self, nca: &NCA) -> (Option<NACP>, Option<VirtualFile>) {
        let base_romfs = match nca.get_romfs() {
            Some(romfs) => romfs,
            None => return (None, None),
        };

        let romfs = self.patch_romfs(None, base_romfs, ContentRecordType::Control, None, true);

        let extracted = match extract_romfs(Some(romfs)) {
            Some(dir) => dir,
            None => return (None, None),
        };

        let nacp_file = extracted
            .get_file("control.nacp")
            .or_else(|| extracted.get_file("Control.nacp"));

        let nacp = nacp_file.map(|f| NACP::from_file(&f));

        // Determine icon language priority from settings.
        use crate::hle::service::ns::language::{
            convert_to_application_language, get_application_language_priority_list,
            ApplicationLanguage,
        };

        let language_index = *common::settings::values().language_index.get_value() as u32 as usize;
        let language_code =
            crate::hle::service::set::settings_server::get_language_code_from_index(language_index);
        let application_language = convert_to_application_language(language_code)
            .unwrap_or(ApplicationLanguage::AmericanEnglish);

        let mut priority_language_names: Vec<&str> = LANGUAGE_NAMES.to_vec();
        if let Some(priority_list) = get_application_language_priority_list(application_language) {
            for i in 0..priority_language_names.len().min(priority_list.len()) {
                let language_index = priority_list[i] as u8 as usize;
                if language_index < LANGUAGE_NAMES.len() {
                    priority_language_names[i] = LANGUAGE_NAMES[language_index];
                } else {
                    log::warn!("Invalid language index {}", language_index);
                }
            }
        }

        let mut icon_file: Option<VirtualFile> = None;
        for language in &priority_language_names {
            let filename = format!("icon_{}.dat", language);
            if let Some(file) = extracted.get_file(&filename) {
                icon_file = Some(file);
                break;
            }
        }

        (nacp, icon_file)
    }

    /// Get control metadata from the base title, falling back to the update title.
    /// Corresponds to upstream `PatchManager::GetMetadataFromBaseOrUpdate`.
    pub fn get_metadata_from_base_or_update(
        system: &crate::core::System,
        application_id: u64,
    ) -> (Option<NACP>, Option<VirtualFile>) {
        let fs_controller = system.get_filesystem_controller();
        let fs_controller = fs_controller.lock().unwrap();
        let Some(content_provider) = system.get_content_provider() else {
            return (None, None);
        };
        let content_provider = content_provider.lock().unwrap();

        let metadata = PatchManager::new(application_id, &fs_controller, &*content_provider)
            .get_control_metadata();
        if metadata.0.is_some() {
            return metadata;
        }

        PatchManager::new(
            get_update_title_id(application_id),
            &fs_controller,
            &*content_provider,
        )
        .get_control_metadata()
    }

    /// Patch RomFS with updates and LayeredFS.
    /// Corresponds to upstream `PatchManager::PatchRomFS`.
    pub fn patch_romfs(
        &self,
        base_nca: Option<&NCA>,
        base_romfs: VirtualFile,
        record_type: ContentRecordType,
        packed_update_raw: Option<VirtualFile>,
        apply_layeredfs: bool,
    ) -> VirtualFile {
        let log_string = format!(
            "Patching RomFS for title_id={:016X}, type={:02X}",
            self.title_id, record_type as u8
        );
        if record_type == ContentRecordType::Program || record_type == ContentRecordType::Data {
            log::info!("{}", log_string);
        } else {
            log::debug!("{}", log_string);
        }

        let mut romfs = base_romfs;

        // Game Updates
        let update_tid = get_update_title_id(self.title_id);
        let disabled = get_disabled_addons(self.title_id);
        let mut selected_update: Option<(VirtualFile, u32)> = None;
        if let Some(provider) = self.content_provider {
            let versions = provider.list_update_versions(update_tid);
            if let Some(version) = versions
                .iter()
                .find(|entry| !is_versioned_external_update_disabled(&disabled, entry.version))
                .map(|entry| entry.version)
            {
                selected_update = provider
                    .get_entry_for_version(update_tid, record_type, version)
                    .map(|file| (file, version));
            }
            if selected_update.is_none() {
                for (slot, _) in provider.list_entries_filter_origin(
                    Some(TitleType::Update),
                    Some(record_type),
                    Some(update_tid),
                ) {
                    if matches!(
                        slot,
                        ContentProviderUnionSlot::External
                            | ContentProviderUnionSlot::FrontendManual
                    ) {
                        continue;
                    }
                    let disabled_name = match slot {
                        ContentProviderUnionSlot::UserNAND | ContentProviderUnionSlot::SysNAND => {
                            "Update (NAND)"
                        }
                        ContentProviderUnionSlot::SDMC => "Update (SDMC)",
                        ContentProviderUnionSlot::External => continue,
                        ContentProviderUnionSlot::FrontendManual => continue,
                    };
                    if disabled.iter().any(|entry| entry == disabled_name) {
                        continue;
                    }
                    if let Some(file) =
                        provider.get_entry_raw_from_slot(slot, update_tid, record_type)
                    {
                        selected_update = Some((
                            file,
                            provider
                                .get_entry_version_from_slot(slot, update_tid)
                                .unwrap_or(0),
                        ));
                        break;
                    }
                }
            }
            if selected_update.is_none()
                && versions.is_empty()
                && !provider.supports_origin_tracking()
                && !is_legacy_update_disabled(&disabled)
            {
                selected_update = provider
                    .get_entry_raw(update_tid, record_type)
                    .map(|file| (file, provider.get_entry_version(update_tid).unwrap_or(0)));
            }
        }

        if let Some(base_nca) = base_nca {
            if let Some((raw, version)) = selected_update {
                let new_nca = NCA::new(raw, Some(base_nca));
                if new_nca.get_status() == super::partition_filesystem::ResultStatus::Success {
                    if let Some(new_romfs) = new_nca.get_romfs() {
                        log::info!(
                            "    RomFS: Update ({}) applied successfully",
                            format_title_version(version)
                        );
                        romfs = new_romfs;
                    }
                }
            } else if !disabled.iter().any(|entry| entry == "Update") {
                if let Some(packed_raw) = packed_update_raw {
                    let new_nca = NCA::new(packed_raw, Some(base_nca));
                    if new_nca.get_status() == super::partition_filesystem::ResultStatus::Success {
                        if let Some(new_romfs) = new_nca.get_romfs() {
                            log::info!("    RomFS: Update (PACKED) applied successfully");
                            romfs = new_romfs;
                        }
                    }
                }
            }
        }

        // LayeredFS
        if apply_layeredfs {
            if let Some(fc) = self.fs_controller {
                apply_layered_fs(&mut romfs, self.title_id, record_type, fc);
            }
        }

        romfs
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::super::vfs::vfs_vector::{VectorVfsDirectory, VectorVfsFile};
    use super::*;
    use std::sync::Mutex;

    struct RecordingContentProvider {
        control_requests: Mutex<Vec<u64>>,
    }

    impl ContentProvider for RecordingContentProvider {
        fn refresh(&mut self) {}

        fn has_entry(&self, _title_id: u64, _record_type: ContentRecordType) -> bool {
            false
        }

        fn get_entry_version(&self, _title_id: u64) -> Option<u32> {
            None
        }

        fn get_entry_unparsed(
            &self,
            _title_id: u64,
            _record_type: ContentRecordType,
        ) -> Option<VirtualFile> {
            None
        }

        fn get_entry_raw(
            &self,
            title_id: u64,
            record_type: ContentRecordType,
        ) -> Option<VirtualFile> {
            if record_type == ContentRecordType::Control {
                self.control_requests.lock().unwrap().push(title_id);
            }
            None
        }

        fn list_entries_filter(
            &self,
            _title_type: Option<TitleType>,
            _record_type: Option<ContentRecordType>,
            _title_id: Option<u64>,
        ) -> Vec<ContentProviderEntry> {
            Vec::new()
        }
    }

    #[test]
    fn test_format_title_version_three() {
        // Version 0x00010002 = bytes [2, 0, 1, 0] -> "v0.1.0"
        assert_eq!(format_title_version(0x00010002), "v0.1.0");
    }

    #[test]
    fn test_format_title_version_zero() {
        assert_eq!(format_title_version(0), "v0.0.0");
    }

    #[test]
    fn external_update_disabled_keys_include_numeric_version() {
        let disabled = vec!["Update@65536".to_string(), "Update".to_string()];
        assert!(is_versioned_external_update_disabled(&disabled, 65536));
        assert!(!is_versioned_external_update_disabled(&disabled, 65537));
        assert!(is_legacy_update_disabled(&["Update (NAND)".to_string()]));
        assert!(is_legacy_update_disabled(&["Update (SDMC)".to_string()]));
        assert!(!is_legacy_update_disabled(&["Update@65536".to_string()]));
    }

    #[test]
    fn test_append_comma_if_not_empty() {
        let mut s = String::new();
        append_comma_if_not_empty(&mut s, "IPS");
        assert_eq!(s, "IPS");
        append_comma_if_not_empty(&mut s, "LayeredFS");
        assert_eq!(s, "IPS, LayeredFS");
    }

    #[test]
    fn reads_cheat_file_using_uppercase_short_build_id() {
        let build_id = [0xAB; 0x20];
        let file: VirtualFile = Arc::new(VectorVfsFile::new(
            b"[Infinite lives]\n04000000 00000000 00000001\n".to_vec(),
            "ABABABABABABABAB.txt".to_string(),
            None,
        ));
        let directory: VirtualDir = Arc::new(VectorVfsDirectory::new(
            vec![file],
            Vec::new(),
            "cheats".to_string(),
            None,
        ));

        let cheats =
            read_cheat_file_from_folder(0x0100_0000_0000_1000, &build_id, &directory, true)
                .expect("matching build-ID cheat file");

        assert_eq!(cheats.len(), 2);
        assert!(cheats[1].enabled);
        assert_eq!(cheats[1].definition.num_opcodes, 3);
    }

    #[test]
    fn metadata_lookup_falls_back_from_base_to_update_title() {
        let application_id = 0x05AA_0000_0000_1000;
        let mut provider = Box::new(RecordingContentProvider {
            control_requests: Mutex::new(Vec::new()),
        });
        let mut union = super::super::registered_cache::ContentProviderUnion::new();
        unsafe {
            union.set_slot(
                ContentProviderUnionSlot::FrontendManual,
                &mut *provider as *mut dyn ContentProvider,
            );
        }
        let mut system = crate::core::System::new_for_test();
        system.set_content_provider(Arc::new(Mutex::new(union)));

        let metadata = PatchManager::get_metadata_from_base_or_update(&system, application_id);

        assert!(metadata.0.is_none());
        assert!(metadata.1.is_none());
        assert_eq!(
            *provider.control_requests.lock().unwrap(),
            vec![application_id, get_update_title_id(application_id)]
        );
    }
}

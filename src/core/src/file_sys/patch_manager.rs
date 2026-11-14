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
// Built-in patches
// ============================================================================
//
// Lichii extension (not in Eden), ported at the user's explicit request from
// Lichii `src/core/file_sys/patch_manager.cpp` (commits 99ac3d5955, 55532f2d81,
// 760183a2ad, cc3bfb9317, 12062785e3). Title-specific: every entry targets
// The Legend of Zelda: Ocarina of Time (Switch 2), title 0400486024DEC000,
// module `main`, build id E0F3CCFAF50065AE4EA01EB5AEB13B1A45DA3348. Authors
// and licence status of each byte sequence are recorded below and in DIFF.md
// (2026-11-14). Lichii's three "DLSS ... render scale (built-in)" entries are
// not ported: they only apply in an NVIDIA Streamline build with the
// `nvidia_dlss_quality` setting, which ruzu does not have; Lichii's other
// builds neither apply nor list them.

/// One write of a built-in patch, at an offset into the module image (after
/// the NSO header). Corresponds to Lichii `BuiltinPatchRecord`.
struct BuiltinPatchRecord {
    module_offset: usize,
    bytes: &'static [u8],
    /// Bytes required at `module_offset` in the unpatched module; empty means
    /// no check.
    original: &'static [u8],
    /// The record writes into the zero-filled padding between the end of
    /// `.text` and its page-rounded end (a code cave).
    executable_padding: bool,
}

/// Corresponds to Lichii `BuiltinPatch`.
struct BuiltinPatch {
    name: &'static str,
    title_id: u64,
    module: &'static str,
    build_id: &'static str,
    records: &'static [BuiltinPatchRecord],
    /// BSS storage the patch's routine uses; it must lie in BSS and be zero.
    state_offset: usize,
    state_size: usize,
    /// Another built-in this one contains; it is skipped once this one is
    /// applied. Empty when none.
    replaces: &'static str,
}

const MAX_RESOLUTION_NAME: &str = "Max Resolution (built-in)";

// Lichii 99ac3d5955 describes it as "the community Max Resolution exefs
// patch"; its author and licence are not named in Lichii. `tbz w8, #0, ...`
// becomes `b` to the game's own "dynamic resolution disabled" path.
const MAX_RESOLUTION_ORIGINAL: [u8; 4] = [0xE8, 0x41, 0x00, 0x36];
const MAX_RESOLUTION_BRANCH: [u8; 4] = [0x0F, 0x02, 0x00, 0x14];
const MAX_RESOLUTION_RECORDS: [BuiltinPatchRecord; 1] = [BuiltinPatchRecord {
    module_offset: 0x805BD0,
    bytes: &MAX_RESOLUTION_BRANCH,
    original: &MAX_RESOLUTION_ORIGINAL,
    executable_padding: false,
}];

// RsMan-Dev's texture pool patch: stp x8, x9, [sp, #0x10] stores the graphics allocator's
// initial pool size (x8); the call doubles it first (lsl x8, x8, #1 ; stp ; ret).
// (Lichii 12062785e3: "RsMan-Dev's patch as described in #247"; no licence stated.)
const TEXTURE_POOL_ORIGINAL: [u8; 4] = [0xE8, 0x27, 0x01, 0xA9];
const TEXTURE_POOL_CALL: [u8; 4] = [0xD5, 0x6F, 0xC0, 0x94];
const TEXTURE_POOL_ROUTINE: [u8; 12] = [
    0x08, 0xF9, 0x7F, 0xD3, 0xE8, 0x27, 0x01, 0xA9, 0xC0, 0x03, 0x5F, 0xD6,
];
const TEXTURE_POOL_RECORDS: [BuiltinPatchRecord; 2] = [
    BuiltinPatchRecord {
        module_offset: 0xC2A36C,
        bytes: &TEXTURE_POOL_CALL,
        original: &TEXTURE_POOL_ORIGINAL,
        executable_padding: false,
    },
    BuiltinPatchRecord {
        module_offset: 0x3C462C0,
        bytes: &TEXTURE_POOL_ROUTINE,
        original: &[],
        executable_padding: true,
    },
];

// Dynamic FPS routine written by Dark (an Eglantine developer). Lichii
// docs/nx2/ocarina-dynamic-fps.md: taken from Frizti's `oot-gather-fix` commit
// 765659af4ccb7201d1145c57f3b45ce84653cea0, which "explicitly states that Dark
// gave permission to bundle it"; no licence is named. CTGL's Lichii #299
// changed the delta cap; the FPS Unlock tail is atk3001's (Lichii cc3bfb9317).
const DYNAMIC_FPS_ORIGINAL: [u8; 4] = [0x28, 0x18, 0x20, 0x1E];
const DYNAMIC_FPS_CALL: [u8; 4] = [0x4F, 0x7D, 0xD5, 0x94];
// Dark's routine up to the step computation; the two built-ins differ only in the tail.
// (Lichii's `DYNAMIC_FPS_ROUTINE_HEAD` macro; it takes the tail bytes here.)
// The original routine caps the measured delta at CNTFRQ / 20 (50 ms); the head uses /10
// (100 ms) so Ocarina keeps real speed down to 10 fps.
macro_rules! dynamic_fps_routine_head {
    ($($tail:expr),* $(,)?) => {
        [
            0x28, 0x18, 0x20, 0x1E, 0xCA, 0x92, 0x00, 0xB0, 0x4A, 0x01, 0x38, 0x91, 0x2B, 0xE0,
            0x3B, 0xD5, 0x4C, 0x01, 0x40, 0xF9, 0x4B, 0x01, 0x00, 0xF9, 0xCC, 0x02, 0x00, 0xB4,
            0x6B, 0x01, 0x0C, 0xCB, 0x0D, 0xE0, 0x3B, 0xD5, 0x4E, 0x01, 0x80, 0xD2, 0xAE, 0x09,
            0xCE, 0x9A, 0x7F, 0x01, 0x0E, 0xEB, 0xCB, 0x81, 0x8B, 0x9A, 0x4C, 0x05, 0x40, 0xF9,
            0x4C, 0x00, 0x00, 0xB5, 0xEC, 0x03, 0x0B, 0xAA, 0x6E, 0x01, 0x0C, 0xCB, 0x8C, 0x0D,
            0x8E, 0x8B, 0x4C, 0x05, 0x00, 0xF9, 0x82, 0x01, 0x23, 0x9E, 0xA3, 0x01, 0x23, 0x9E,
            0x42, 0x18, 0x23, 0x1E, 0x42, 0x08,
            $($tail),*
        ]
    };
}
const DYNAMIC_FPS_ROUTINE: [u8; 120] = dynamic_fps_routine_head![
    // Dark's tail: the smoothed step if it is above original * 1.03, else the original step.
    0x21, 0x1E, 0xC3, 0x00, 0x00, 0x1C, 0x03, 0x09, 0x23, 0x1E, 0x40, 0x20, 0x23, 0x1E, 0x4D,
    0x00, 0x00, 0x54, 0x48, 0x40, 0x20, 0x1E, 0xC0, 0x03, 0x5F, 0xD6, 0x0A, 0xD7, 0x83, 0x3F,
];
const FPS_UNLOCK_ROUTINE: [u8; 128] = dynamic_fps_routine_head![
    // Unlock tail: the smoothed real-time step in 1/60-s units is clamped into [0.02, 6.0] and
    // always returned, so logic follows real time above 60 fps too (0.5 units/frame at 120 fps)
    // and down to 10 fps (6.0 matches the 100 ms delta cap). At 60 fps it equals Dark's tail.
    // 0x5C: ldr s3, +0x78 (0.02f) ; fmaxnm s2, s2, s3 ; 0x64: ldr s3, +0x7c (6.0f) ;
    // fminnm s2, s2, s3 ; fmov s8, s2 ; ret ; nop ; 0.02f ; 6.0f
    0x21, 0x1E, 0xE3, 0x00, 0x00, 0x1C, 0x42, 0x68, 0x23, 0x1E, 0xC3, 0x00, 0x00, 0x1C, 0x42,
    0x78, 0x23, 0x1E, 0x48, 0x40, 0x20, 0x1E, 0xC0, 0x03, 0x5F, 0xD6, 0x1F, 0x20, 0x03, 0xD5,
    0x0A, 0xD7, 0xA3, 0x3C, 0x00, 0x00, 0xC0, 0x40,
];
const DYNAMIC_FPS_RECORDS: [BuiltinPatchRecord; 2] = [
    BuiltinPatchRecord {
        module_offset: 0x6E6CC4,
        bytes: &DYNAMIC_FPS_CALL,
        original: &DYNAMIC_FPS_ORIGINAL,
        executable_padding: false,
    },
    BuiltinPatchRecord {
        module_offset: 0x3C46200,
        bytes: &DYNAMIC_FPS_ROUTINE,
        original: &[],
        executable_padding: true,
    },
];
const FPS_UNLOCK_RECORDS: [BuiltinPatchRecord; 2] = [
    BuiltinPatchRecord {
        module_offset: 0x6E6CC4,
        bytes: &DYNAMIC_FPS_CALL,
        original: &DYNAMIC_FPS_ORIGINAL,
        executable_padding: false,
    },
    BuiltinPatchRecord {
        module_offset: 0x3C46200,
        bytes: &FPS_UNLOCK_ROUTINE,
        original: &[],
        executable_padding: true,
    },
];

const BUILTIN_PATCHES: [BuiltinPatch; 4] = [
    // Ocarina of Time takes its "dynamic resolution disabled" path, so the 3D renders at full scale
    // instead of following the GPU-time controller down to its 480p floor.
    BuiltinPatch {
        name: MAX_RESOLUTION_NAME,
        title_id: 0x0400486024DEC000,
        module: "main",
        build_id: "E0F3CCFAF50065AE4EA01EB5AEB13B1A45DA3348",
        records: &MAX_RESOLUTION_RECORDS,
        state_offset: 0,
        state_size: 0,
        replaces: "",
    },
    // With Max Resolution the guard house outgrows the initial pool and dereferences a null
    // allocation.
    BuiltinPatch {
        name: "Texture Pool x2 (built-in)",
        title_id: 0x0400486024DEC000,
        module: "main",
        build_id: "E0F3CCFAF50065AE4EA01EB5AEB13B1A45DA3348",
        records: &TEXTURE_POOL_RECORDS,
        state_offset: 0,
        state_size: 0,
        replaces: "",
    },
    // Dark's routine with a tail that also lowers the step above 60 fps, so the game keeps real
    // speed at any frame rate limit (Settings frame_rate_limit).
    BuiltinPatch {
        name: "FPS Unlock (built-in)",
        title_id: 0x0400486024DEC000,
        module: "main",
        build_id: "E0F3CCFAF50065AE4EA01EB5AEB13B1A45DA3348",
        records: &FPS_UNLOCK_RECORDS,
        state_offset: 0x4E9FE00,
        state_size: 16,
        replaces: "Dynamic FPS by Dark (built-in)",
    },
    BuiltinPatch {
        name: "Dynamic FPS by Dark (built-in)",
        title_id: 0x0400486024DEC000,
        module: "main",
        build_id: "E0F3CCFAF50065AE4EA01EB5AEB13B1A45DA3348",
        records: &DYNAMIC_FPS_RECORDS,
        state_offset: 0x4E9FE00,
        state_size: 16,
        replaces: "",
    },
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

/// Upstream `GetUpdateVersionStringFromSlot`. The Rust provider exposes slot
/// access through the union instead of returning a borrowed slot provider.
fn get_update_version_string_from_slot(
    provider: &dyn ContentProvider,
    slot: ContentProviderUnionSlot,
    update_tid: u64,
) -> String {
    let Some(file) = provider.get_entry_raw_from_slot(slot, update_tid, ContentRecordType::Control) else {
        return String::new();
    };
    let control = NCA::new(file, None);
    if control.get_status() != super::partition_filesystem::ResultStatus::Success {
        return String::new();
    }
    read_update_version_from_romfs(control.get_romfs())
}

/// Mechanical extraction of the RomFS-reading portion of the upstream helper,
/// kept here so its filename rules can be tested without encryption keys.
fn read_update_version_from_romfs(romfs: Option<VirtualFile>) -> String {
    let Some(extracted) = extract_romfs(romfs) else {
        return String::new();
    };
    extracted
        .get_file("control.nacp")
        .or_else(|| extracted.get_file("Control.nacp"))
        .map(|file| NACP::from_file(&file).get_version_string())
        .unwrap_or_default()
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
    parent_title_id: Option<u64>,
    fs_controller: Option<&'a FileSystemController>,
    content_provider: Option<&'a dyn ContentProvider>,
}

impl<'a> PatchManager<'a> {
    /// Apply LayeredFS patches to a RomFS file.
    /// Corresponds to upstream `PatchManager::ApplyLayeredFS`.
    fn apply_layered_fs(
        &self,
        romfs: &mut VirtualFile,
        record_type: ContentRecordType,
    ) {
        let load_dir = self.get_modification_load_root(false);
        let sdmc_load_dir = self.get_modification_load_root(true);

        if (record_type != ContentRecordType::Program
            && record_type != ContentRecordType::Data
            && record_type != ContentRecordType::HtmlDocument)
            || (load_dir.is_none() && sdmc_load_dir.is_none())
        {
            return;
        }

        let disabled = self.get_disabled_addons();

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
            if let Some(romfslite_dir) = find_subdirectory_caseless(subdir, "romfslite") {
                layers.push(Arc::new(CachedVfsDirectory::new(romfslite_dir)));
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

    /// Create a new PatchManager with all dependencies.
    /// Corresponds to upstream `PatchManager::PatchManager`.
    pub fn new(
        title_id: u64,
        fs_controller: &'a FileSystemController,
        content_provider: &'a dyn ContentProvider,
    ) -> Self {
        Self {
            title_id,
            parent_title_id: content_provider.get_parent_application_id(title_id),
            fs_controller: Some(fs_controller),
            content_provider: Some(content_provider),
        }
    }

    /// Create a PatchManager without filesystem / content provider references.
    /// Methods requiring those references will return defaults / no-ops.
    pub fn new_without_deps(title_id: u64) -> Self {
        Self {
            title_id,
            parent_title_id: None,
            fs_controller: None,
            content_provider: None,
        }
    }

    /// Get the title ID.
    /// Corresponds to upstream `PatchManager::GetTitleID`.
    pub fn get_title_id(&self) -> u64 {
        self.title_id
    }

    fn get_modification_load_root(&self, sdmc: bool) -> Option<VirtualDir> {
        let controller = self.fs_controller?;
        let get_root = |id| if sdmc {
            controller.get_sdmc_modification_load_root(id)
        } else {
            controller.get_modification_load_root(id)
        };
        let root = get_root(self.title_id);
        let Some(parent) = self.parent_title_id else { return root };
        LayeredVfsDirectory::make_layered_directory(
            [root, get_root(parent)].into_iter().flatten().collect(), String::new())
    }

    fn get_disabled_addons(&self) -> Vec<String> {
        let mut disabled = get_disabled_addons(self.title_id);
        if let Some(parent) = self.parent_title_id {
            disabled.extend(get_disabled_addons(parent));
        }
        disabled
    }

    /// Patch ExeFS with updates and LayeredExeFS.
    /// Corresponds to upstream `PatchManager::PatchExeFS`.
    pub fn patch_exefs(&self, mut exefs: VirtualDir) -> VirtualDir {
        log::info!("Patching ExeFS for title_id={:016X}", self.title_id);

        let disabled = self.get_disabled_addons();
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
                && !is_legacy_update_disabled(&disabled)
            {
                // Eden falls back to ContentProvider::GetEntryRaw here even
                // when the provider is a ContentProviderUnion. This also
                // covers unversioned frontend-manual content discovered in a
                // game container.
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
        let load_dir = self.get_modification_load_root(false);
        let sdmc_load_dir = self.get_modification_load_root(true);

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
        let disabled = self.get_disabled_addons();
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
                            .map(|b| format!("{:02X}", b))
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
            .map(|b| format!("{:02X}", b))
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

        let load_dir = match self.get_modification_load_root(false)
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

        let mut out = nso.clone();
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

        // Lichii extension: built-in patches (see BUILTIN_PATCHES).
        let disabled = self.get_disabled_addons();
        let mut replaced: Vec<&str> = Vec::new();
        for patch in &BUILTIN_PATCHES {
            if patch.title_id != self.title_id || patch.module != name {
                continue;
            }
            if disabled.iter().any(|entry| entry == patch.name) {
                log::info!("    - Skipping \"{}\": disabled in Add-Ons", patch.name);
                continue;
            }
            if replaced.contains(&patch.name) {
                log::info!(
                    "    - Skipping \"{}\": included in an applied built-in patch",
                    patch.name
                );
                continue;
            }
            if patch.build_id != build_id {
                log::info!(
                    "    - Skipping \"{}\": build id {} is not {}",
                    patch.name,
                    build_id,
                    patch.build_id
                );
                continue;
            }
            let fits = |data: &[u8], offset: usize, size: usize| {
                data.len() >= header_size
                    && offset <= data.len() - header_size
                    && size <= data.len() - header_size - offset
            };
            if patch.state_size != 0 {
                let data = &header.segments[2];
                let bss_start = u64::from(data.location) + u64::from(data.size);
                let bss_end = bss_start + u64::from(data.alignment_or_bss_size);
                let state_offset = patch.state_offset as u64;
                if state_offset < bss_start
                    || state_offset > bss_end
                    || patch.state_size as u64 > bss_end - state_offset
                    || !fits(&nso, patch.state_offset, patch.state_size)
                    || !fits(&out, patch.state_offset, patch.state_size)
                {
                    log::warn!("    - Skipping \"{}\": timer state is outside BSS", patch.name);
                    continue;
                }
                let offset = header_size + patch.state_offset;
                let range = offset..offset + patch.state_size;
                if !nso[range.clone()].iter().all(|&byte| byte == 0)
                    || nso[range.clone()] != out[range]
                {
                    log::warn!(
                        "    - Skipping \"{}\": timer state is not zero-filled",
                        patch.name
                    );
                    continue;
                }
            }
            let mut apply = true;
            for record in patch.records {
                if !fits(&nso, record.module_offset, record.bytes.len())
                    || !fits(&out, record.module_offset, record.bytes.len())
                    || !fits(&nso, record.module_offset, record.original.len())
                {
                    log::warn!(
                        "    - Skipping \"{}\": {}+{:#x} is past the module's end",
                        patch.name,
                        name,
                        record.module_offset
                    );
                    apply = false;
                    break;
                }
                let offset = header_size + record.module_offset;
                if record.executable_padding {
                    let text = &header.segments[0];
                    let text_end = u64::from(text.location) + u64::from(text.size);
                    let executable_end = (text_end + 0xFFF) & !0xFFFu64;
                    let record_end = (record.module_offset + record.bytes.len()) as u64;
                    if (record.module_offset as u64) < text_end
                        || record_end > executable_end
                        || record_end > u64::from(header.segments[1].location)
                        || !nso[offset..offset + record.bytes.len()]
                            .iter()
                            .all(|&byte| byte == 0)
                    {
                        log::warn!(
                            "    - Skipping \"{}\": code cave is not executable padding",
                            patch.name
                        );
                        apply = false;
                        break;
                    }
                }
                if nso[offset..offset + record.original.len()] != *record.original {
                    log::warn!(
                        "    - Skipping \"{}\": unexpected original bytes at {}+{:#x}",
                        patch.name,
                        name,
                        record.module_offset
                    );
                    apply = false;
                    break;
                }
                let range = offset..offset + record.bytes.len();
                if nso[range.clone()] != out[range] {
                    log::info!(
                        "    - Skipping \"{}\": a user mod already patched {}+{:#x}",
                        patch.name,
                        name,
                        record.module_offset
                    );
                    apply = false;
                    break;
                }
            }
            if !apply {
                continue;
            }
            for record in patch.records {
                let offset = header_size + record.module_offset;
                out[offset..offset + record.bytes.len()].copy_from_slice(record.bytes);
            }
            log::info!(
                "    - Applying built-in patch \"{}\" ({} records) to {}",
                patch.name,
                patch.records.len(),
                name
            );
            if !patch.replaces.is_empty() {
                replaced.push(patch.replaces);
            }
        }

        if out.len() < std::mem::size_of::<NsoHeader>() {
            return nso;
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
        let build_id_raw: String = build_id.iter().map(|b| format!("{:02X}", b)).collect();
        let build_id_str = build_id_raw.trim_end_matches('0').to_string();

        log::info!(
            "Querying NSO patch existence for build_id={}, name={}",
            build_id_str,
            name
        );

        // Lichii extension: a built-in patch for this module makes PatchNSO run.
        if BUILTIN_PATCHES
            .iter()
            .any(|patch| patch.title_id == self.title_id && patch.module == name)
        {
            return true;
        }

        let load_dir = match self.get_modification_load_root(false)
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
        let load_dir = match self.get_modification_load_root(false)
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

        let disabled = self.get_disabled_addons();
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
                let mut version = get_update_version_string_from_slot(provider, slot, update_tid);
                let numeric_version = provider
                    .get_entry_version_from_slot(slot, update_tid)
                    .unwrap_or(0);
                if version.is_empty() && numeric_version != 0 {
                    version = format_title_version(numeric_version);
                }
                let name = format!("Update{suffix}");
                out.push(Patch {
                    enabled: !disabled.contains(&name),
                    name,
                    version,
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
                parent_title_id: self.content_provider.and_then(|cp| cp.get_parent_application_id(update_tid)),
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

        // Lichii extension: built-in patches are listed as mods and toggled
        // through the disabled add-ons like one.
        let is_disabled = |name: &str| disabled.iter().any(|entry| entry == name);
        for patch in &BUILTIN_PATCHES {
            if patch.title_id != self.title_id {
                continue;
            }
            // Name an enabled built-in that contains this one, so unchecking only this one is not
            // mistaken for turning the change off. Whether it applies is only known in PatchNSO (a
            // user mod can make it skip), hence "when it applies".
            let replacing = BUILTIN_PATCHES.iter().find(|other| {
                other.title_id == self.title_id
                    && other.replaces == patch.name
                    && !is_disabled(other.name)
            });
            out.push(Patch {
                enabled: !is_disabled(patch.name),
                name: patch.name.to_string(),
                version: match replacing {
                    Some(replacing) => {
                        format!("IPS, replaced by {} when it applies", replacing.name)
                    }
                    None => "IPS".to_string(),
                },
                patch_type: PatchType::Mod,
                program_id: self.title_id,
                title_id: self.title_id,
                source: PatchSource::Unknown,
                location: String::new(),
                numeric_version: 0,
            });
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
            return cp.get_entry_version(update_tid).or_else(||
                self.parent_title_id.and_then(|parent| cp.get_entry_version(get_update_title_id(parent))));
        }
        cp.get_entry_version(self.title_id).or_else(||
            self.parent_title_id.and_then(|parent| cp.get_entry_version(parent)))
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
            None => {
                let Some(parent) = self.parent_title_id else { return (None, None) };
                let control_id = if cp.has_entry(parent, ContentRecordType::Control) {
                    parent
                } else {
                    get_update_title_id(parent)
                };
                let parent = PatchManager {
                    title_id: control_id,
                    parent_title_id: cp.get_parent_application_id(control_id),
                    fs_controller: self.fs_controller,
                    content_provider: self.content_provider,
                };
                return parent.get_control_metadata();
            }
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

        let romfs = self.patch_romfs(Some(nca), base_romfs, ContentRecordType::Control, None, true);

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
        let disabled = self.get_disabled_addons();
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
                && !is_legacy_update_disabled(&disabled)
            {
                // Match Eden's union-wide GetEntryRaw fallback. In
                // particular, an update Control NCA can come from the
                // frontend-manual provider even when no versioned entry was
                // registered for this record type.
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
            self.apply_layered_fs(&mut romfs, record_type);
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

    #[derive(Default)]
    struct ParentTestProvider {
        entries: std::collections::BTreeMap<(TitleType, ContentRecordType, u64), ()>,
        versions: std::collections::BTreeMap<u64, u32>,
        requests: Mutex<Vec<u64>>,
    }

    impl ContentProvider for ParentTestProvider {
        fn refresh(&mut self) {}
        fn has_entry(&self, id: u64, record: ContentRecordType) -> bool {
            self.entries.keys().any(|&(_, r, t)| r == record && t == id)
        }
        fn get_entry_version(&self, id: u64) -> Option<u32> { self.versions.get(&id).copied() }
        fn get_entry_unparsed(&self, _: u64, _: ContentRecordType) -> Option<VirtualFile> { None }
        fn get_entry_raw(&self, id: u64, record: ContentRecordType) -> Option<VirtualFile> {
            if record == ContentRecordType::Control { self.requests.lock().unwrap().push(id); }
            None
        }
        fn list_entries_filter(&self, kind: Option<TitleType>, record: Option<ContentRecordType>, id: Option<u64>) -> Vec<ContentProviderEntry> {
            self.entries.keys().filter(|&&(k, r, t)|
                kind.is_none_or(|v| v == k) && record.is_none_or(|v| v == r) && id.is_none_or(|v| v == t))
                .map(|&(_, record_type, title_id)| ContentProviderEntry { title_id, record_type }).collect()
        }
    }

    #[test]
    fn bundled_program_versions_and_control_lookup_follow_parent_fallbacks() {
        let base = 0x0100_0000_0000_0000;
        let child = base + 1;
        let controller = FileSystemController::new();
        for update in [false, true] {
            for own_version in [None, Some(0), Some(42)] {
                let mut provider = ParentTestProvider::default();
                provider.entries.insert((TitleType::Application, ContentRecordType::Meta, base), ());
                provider.entries.insert((TitleType::Application, ContentRecordType::Program, child), ());
                let queried_id = if update { get_update_title_id(child) } else { child };
                let parent_id = if update { get_update_title_id(base) } else { base };
                if update { provider.entries.insert((TitleType::Update, ContentRecordType::Program, queried_id), ()); }
                provider.versions.insert(parent_id, 77);
                if let Some(version) = own_version { provider.versions.insert(queried_id, version); }
                let manager = PatchManager::new(child, &controller, &provider);
                assert_eq!(manager.parent_title_id, Some(base));
                assert_eq!(manager.get_game_version(), own_version.or(Some(77)));
                assert!(manager.get_control_metadata().0.is_none());
                assert_eq!(*provider.requests.lock().unwrap(), vec![child, get_update_title_id(base)]);
                provider.requests.lock().unwrap().clear();
                provider.entries.insert((TitleType::Application, ContentRecordType::Control, base), ());
                PatchManager::new(child, &controller, &provider).get_control_metadata();
                assert_eq!(*provider.requests.lock().unwrap(), vec![child, base]);
                provider.versions.remove(&parent_id);
                provider.versions.remove(&queried_id);
                assert_eq!(PatchManager::new(child, &controller, &provider).get_game_version(), None);
            }
        }
    }

    #[test]
    fn bundled_program_mod_roots_prioritize_child_and_inherit_disabled_names() {
        const CHILD_PROCESS: &str = "RUZU_TEST_PARENT_MODS";
        if std::env::var_os(CHILD_PROCESS).is_none() {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", std::thread::current().name().unwrap()])
                .env(CHILD_PROCESS, "1").status().unwrap().success());
            return;
        }
        let base = 0x0100_0000_0000_0000;
        let child = base + 1;
        let dir = |name: &str, files: Vec<VirtualFile>, dirs: Vec<VirtualDir>| -> VirtualDir {
            Arc::new(VectorVfsDirectory::new(files, dirs, name.to_owned(), None))
        };
        let file = |name: &str, byte| -> VirtualFile {
            Arc::new(VectorVfsFile::new(vec![byte], name.to_owned(), None))
        };
        let parent_mod = dir("shared", vec![], vec![
            dir("exefs", vec![file("main", 1), file("rtld", 2)], vec![]),
            dir("romfs", vec![file("same", 1)], vec![]),
            dir("romfslite", vec![file("same", 5), file("lite", 6)], vec![]),
        ]);
        let child_mod = dir("shared", vec![], vec![
            dir("exefs", vec![file("main", 3)], vec![]),
            dir("romfs", vec![file("same", 3)], vec![]),
        ]);
        let load = dir("load", vec![], vec![
            dir(&format!("{base:016X}"), vec![], vec![parent_mod]),
            dir(&format!("{child:016X}"), vec![], vec![child_mod]),
        ]);
        let empty = dir("empty", vec![], vec![]);
        let mut controller = FileSystemController::new();
        controller.set_bis_factory(crate::file_sys::bis_factory::BisFactory::new(empty.clone(), load, empty));
        let mut provider = ParentTestProvider::default();
        provider.entries.insert((TitleType::Application, ContentRecordType::Meta, base), ());
        provider.entries.insert((TitleType::Application, ContentRecordType::Program, child), ());
        let manager = PatchManager::new(child, &controller, &provider);
        let exefs = dir("exefs", vec![file("main", 9), file("sdk", 8)], vec![]);
        let patched = manager.patch_exefs(exefs.clone());
        assert_eq!(patched.get_file("main").unwrap().read_all_bytes(), vec![3]);
        assert_eq!(patched.get_file("rtld").unwrap().read_all_bytes(), vec![2]);
        assert_eq!(patched.get_file("sdk").unwrap().read_all_bytes(), vec![8]);
        let original_romfs = create_romfs(Some(dir("romfs", vec![file("same", 9), file("base", 8)], vec![])), None).unwrap();
        let patched_romfs = manager.patch_romfs(None, original_romfs.clone(), ContentRecordType::Program, None, true);
        let patched_romfs = extract_romfs(Some(patched_romfs)).unwrap();
        assert_eq!(patched_romfs.get_file("same").unwrap().read_all_bytes(), vec![3]);
        assert_eq!(patched_romfs.get_file("lite").unwrap().read_all_bytes(), vec![6]);
        assert_eq!(patched_romfs.get_file("base").unwrap().read_all_bytes(), vec![8]);
        {
            let mut settings = common::settings::values_mut();
            settings.disabled_addons.insert(child, vec!["child-only".into(), "duplicate".into()]);
            settings.disabled_addons.insert(base, vec!["shared".into(), "duplicate".into()]);
        }
        assert_eq!(manager.get_disabled_addons(), vec!["child-only", "duplicate", "shared", "duplicate"]);
        let patched = manager.patch_exefs(exefs);
        assert_eq!(patched.get_file("main").unwrap().read_all_bytes(), vec![9]);
        assert!(patched.get_file("rtld").is_none());
        let patched_romfs = manager.patch_romfs(None, original_romfs, ContentRecordType::Program, None, true);
        let patched_romfs = extract_romfs(Some(patched_romfs)).unwrap();
        assert_eq!(patched_romfs.get_file("same").unwrap().read_all_bytes(), vec![9]);
        assert!(patched_romfs.get_file("lite").is_none());
    }

    #[test]
    fn update_display_version_reads_control_metadata_from_romfs() {
        use super::super::control_metadata::RawNACP;
        let make_control = |name: &str, version: &[u8]| -> VirtualFile {
            let mut data = vec![0; std::mem::size_of::<RawNACP>()];
            let offset = std::mem::offset_of!(RawNACP, version_string);
            data[offset..offset + version.len()].copy_from_slice(version);
            Arc::new(VectorVfsFile::new(data, name.to_owned(), None))
        };
        for files in [
            vec![make_control("control.nacp", b"3.2.1")],
            vec![make_control("Control.nacp", b"3.2.1")],
            vec![make_control("control.nacp", b"3.2.1"), make_control("Control.nacp", b"9.0.0")],
        ] {
            let directory: VirtualDir = Arc::new(VectorVfsDirectory::new(
                files, Vec::new(), "metadata".to_owned(), None,
            ));
            let romfs = create_romfs(Some(directory), None).expect("synthetic RomFS");
            assert_eq!(read_update_version_from_romfs(Some(romfs)), "3.2.1");
        }
        assert_eq!(read_update_version_from_romfs(None), "");
        let directory: VirtualDir = Arc::new(VectorVfsDirectory::new(
            Vec::new(), Vec::new(), "metadata".to_owned(), None,
        ));
        assert_eq!(read_update_version_from_romfs(create_romfs(Some(directory), None)), "");
    }

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

        fn supports_origin_tracking(&self) -> bool {
            true
        }
    }

    #[test]
    fn dump_settings_export_exefs_after_layers_and_nso_before_patches() {
        const ROOT: &str = "RUZU_TEST_PATCH_DUMP_ROOT";
        let root = match std::env::var_os(ROOT) {
            Some(root) => std::path::PathBuf::from(root),
            None => {
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let root = std::env::temp_dir().join(format!(
                    "ruzu-patch-dump-{}-{nonce}",
                    std::process::id()
                ));
                std::fs::create_dir(&root).unwrap();
                let status = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", std::thread::current().name().unwrap()])
                    .env(ROOT, &root)
                    .status()
                    .unwrap();
                std::fs::remove_dir_all(&root).unwrap();
                assert!(status.success());
                return;
            }
        };
        use crate::file_sys::vfs::vfs_real::RealVfsFilesystem;
        use crate::file_sys::{bis_factory::BisFactory, fs_filesystem::OpenMode};

        let filesystem = RealVfsFilesystem::new();
        let directory = |name: &str| {
            filesystem
                .arc_create_directory(root.join(name).to_str().unwrap(), OpenMode::READ_WRITE)
                .unwrap()
        };
        let mut controller = FileSystemController::new();
        controller.set_bis_factory(BisFactory::new(
            directory("nand"),
            directory("load"),
            directory("dump"),
        ));
        let provider = RecordingContentProvider {
            control_requests: Mutex::new(Vec::new()),
        };
        let manager = PatchManager::new(42, &controller, &provider);
        let exefs = directory("base");
        std::fs::write(root.join("base/main"), b"original executable").unwrap();
        std::fs::write(root.join("base/main.npdm"), b"original metadata").unwrap();
        let mod_path = root.join("load/000000000000002A/synthetic/exefs");
        std::fs::create_dir_all(&mod_path).unwrap();
        std::fs::write(mod_path.join("main"), b"replacement executable").unwrap();
        std::fs::write(
            mod_path.join("ABC.ips"),
            b"PATCH\x00\x01\x00\x00\x01\xAAEOF",
        )
        .unwrap();
        let dump = root.join("dump/000000000000002A");

        let mut nso = vec![0; 0x101];
        nso[..4].copy_from_slice(b"NSO0");
        let offset = std::mem::offset_of!(crate::loader::nso::NsoHeader, build_id);
        nso[offset..offset + 2].copy_from_slice(&[0xAB, 0xC0]);
        for enabled in [false, true, false] {
            {
                let mut values = common::settings::values_mut();
                values.dump_exefs.set_value(enabled);
                values.dump_nso.set_value(enabled);
            }
            let layered = manager.patch_exefs(exefs.clone());
            assert_eq!(
                layered.get_file("main").unwrap().read_all_bytes(),
                b"replacement executable"
            );
            assert_eq!(manager.patch_nso(nso.clone(), "main")[0x100], 0xAA);
            if enabled {
                assert_eq!(
                    std::fs::read(dump.join("exefs/main")).unwrap(),
                    b"replacement executable"
                );
                assert_eq!(
                    std::fs::read(dump.join("exefs/main.npdm")).unwrap(),
                    b"original metadata"
                );
                assert_eq!(std::fs::read(dump.join("nso/main-ABC.nso")).unwrap(), nso);
                // Case-insensitive filesystems resolve the lowercase spelling
                // to the same file. Inspect the stored name, not path existence.
                let names: Vec<_> = std::fs::read_dir(dump.join("nso")).unwrap()
                    .map(|entry| entry.unwrap().file_name()).collect();
                assert_eq!(names, vec![std::ffi::OsString::from("main-ABC.nso")]);
                // Re-disabling must stop exports, without disabling patch application.
                std::fs::remove_dir_all(&dump).unwrap();
            } else {
                assert!(!dump.exists());
            }
        }
    }

    #[test]
    fn nso_patch_build_ids_use_upstream_uppercase() {
        const CHILD: &str = "RUZU_TEST_NSO_PATCH_BUILD_IDS";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", std::thread::current().name().unwrap()])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }

        common::settings::values_mut().dump_nso.set_value(false);
        let directory = |name: &str, files: Vec<VirtualFile>, dirs: Vec<VirtualDir>| -> VirtualDir {
            Arc::new(VectorVfsDirectory::new(files, dirs, name.into(), None))
        };
        let ips: VirtualFile = Arc::new(VectorVfsFile::new(
            b"PATCH\x00\x01\x00\x00\x01\xAAEOF".to_vec(),
            "ABC.ips".into(),
            None,
        ));
        let text: VirtualFile = Arc::new(VectorVfsFile::new(
            format!("@nsobid-{:0<64}\n@enabled\n00000100 BB\n@stop\n", "ABC").into_bytes(),
            "synthetic.pchtxt".into(),
            None,
        ));
        let exefs = directory("exefs", vec![ips, text], vec![]);
        let patch = directory("synthetic", vec![], vec![exefs]);
        let load = directory(
            "load",
            vec![],
            vec![directory("000000000000002A", vec![], vec![patch.clone()])],
        );
        let mut controller = FileSystemController::new();
        controller.set_bis_factory(super::super::bis_factory::BisFactory::new(
            directory("nand", vec![], vec![]),
            load,
            directory("dump", vec![], vec![]),
        ));
        let provider = RecordingContentProvider {
            control_requests: Mutex::new(Vec::new()),
        };
        let manager = PatchManager::new(42, &controller, &provider);
        assert_eq!(manager.collect_patches(&[patch], "ABC").len(), 2);
        let mut build_id = [0; 32];
        build_id[..2].copy_from_slice(&[0xAB, 0xC0]);
        assert!(manager.has_nso_patch(&build_id, "main"));
        let mut nso = vec![0; 0x101];
        nso[..4].copy_from_slice(b"NSO0");
        let offset = std::mem::offset_of!(crate::loader::nso::NsoHeader, build_id);
        nso[offset..offset + 32].copy_from_slice(&build_id);
        assert_eq!(manager.patch_nso(nso, "main")[0x100], 0xBB);
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

    #[test]
    fn romfs_uses_raw_fallback_with_an_origin_tracking_provider() {
        let application_id = 0x05AA_0000_0000_1000;
        let controller = crate::hle::service::filesystem::filesystem::FileSystemController::new();
        let provider = RecordingContentProvider {
            control_requests: Mutex::new(Vec::new()),
        };
        let patch_manager = PatchManager::new(application_id, &controller, &provider);
        let base_romfs: VirtualFile = Arc::new(VectorVfsFile::new(
            Vec::new(),
            "base.romfs".to_owned(),
            None,
        ));

        let _ =
            patch_manager.patch_romfs(None, base_romfs, ContentRecordType::Control, None, false);

        assert_eq!(
            *provider.control_requests.lock().unwrap(),
            vec![get_update_title_id(application_id)]
        );
    }

    // ------------------------------------------------------------------------
    // Built-in patches (Lichii src/tests/core/builtin_patches.cpp)
    // ------------------------------------------------------------------------

    const OCARINA_TITLE_ID: u64 = 0x0400486024DEC000;
    const OTHER_TITLE_ID: u64 = 0x0100000000010000;
    const MAX_RESOLUTION: &str = "Max Resolution (built-in)";
    const DYNAMIC_FPS: &str = "Dynamic FPS by Dark (built-in)";
    const FPS_UNLOCK: &str = "FPS Unlock (built-in)";
    const TEXTURE_POOL: &str = "Texture Pool x2 (built-in)";
    const OCARINA_BUILD_ID_HEX: &str = "E0F3CCFAF50065AE4EA01EB5AEB13B1A45DA3348";
    const OCARINA_BUILD_ID: [u8; 20] = [
        0xE0, 0xF3, 0xCC, 0xFA, 0xF5, 0x00, 0x65, 0xAE, 0x4E, 0xA0, 0x1E, 0xB5, 0xAE, 0xB1, 0x3B,
        0x1A, 0x45, 0xDA, 0x33, 0x48,
    ];
    const NSO_HEADER_SIZE: usize = std::mem::size_of::<crate::loader::nso::NsoHeader>();
    const PATCH_OFFSET: usize = NSO_HEADER_SIZE + 0x805BD0;
    const TBZ_INSTRUCTION: u32 = 0x360041E8;
    const BRANCH_INSTRUCTION: u32 = 0x1400020F;
    const NOP_INSTRUCTION: u32 = 0xD503201F;
    const DYNAMIC_CALL_OFFSET: usize = NSO_HEADER_SIZE + 0x6E6CC4;
    const DYNAMIC_ROUTINE_OFFSET: usize = NSO_HEADER_SIZE + 0x3C46200;
    const DYNAMIC_STATE_OFFSET: usize = NSO_HEADER_SIZE + 0x4E9FE00;
    const DYNAMIC_ROUTINE_SIZE: usize = 120;
    const UNLOCK_ROUTINE_SIZE: usize = 128;
    const DYNAMIC_CALL_INSTRUCTION: u32 = 0x94D57D4F;
    const DYNAMIC_ORIGINAL_INSTRUCTION: u32 = 0x1E201828;

    /// Settings are process-global: each built-in test runs alone in a child
    /// process. Returns true in the parent once the child has passed.
    fn ran_in_child_process(variable: &str) -> bool {
        if std::env::var_os(variable).is_some() {
            common::settings::values_mut().dump_nso.set_value(false);
            return false;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", std::thread::current().name().unwrap()])
            .env(variable, "1")
            .status()
            .unwrap();
        assert!(status.success());
        true
    }

    /// Lichii `WriteIps`.
    fn write_ips(word: u32, offset: usize) -> Vec<u8> {
        let ips32 = offset > 0xFFFFFF;
        let mut ips = if ips32 { b"IPS32".to_vec() } else { b"PATCH".to_vec() };
        if ips32 {
            ips.push((offset >> 24) as u8);
        }
        ips.extend([(offset >> 16) as u8, (offset >> 8) as u8, offset as u8, 0, 4]);
        ips.extend(word.to_le_bytes());
        if ips32 {
            ips.push(b'E');
        }
        ips.extend(b"EOF");
        ips
    }

    /// Lichii `PatchManagerTestContext`: a load root with an Ocarina title
    /// directory, optionally holding a user IPS mod.
    fn builtin_test_controller(user_mod: Option<(u32, usize)>) -> FileSystemController {
        let directory = |name: &str, files: Vec<VirtualFile>, dirs: Vec<VirtualDir>| -> VirtualDir {
            Arc::new(VectorVfsDirectory::new(files, dirs, name.into(), None))
        };
        let mut mods = Vec::new();
        if let Some((word, offset)) = user_mod {
            let ips: VirtualFile = Arc::new(VectorVfsFile::new(
                write_ips(word, offset),
                format!("{OCARINA_BUILD_ID_HEX}.ips"),
                None,
            ));
            mods.push(directory("UserMod", vec![], vec![directory("exefs", vec![ips], vec![])]));
        }
        let load = directory(
            "load",
            vec![],
            vec![
                directory(&format!("{OCARINA_TITLE_ID:016X}"), vec![], mods),
                directory(&format!("{OTHER_TITLE_ID:016X}"), vec![], vec![]),
            ],
        );
        let mut controller = FileSystemController::new();
        controller.set_bis_factory(super::super::bis_factory::BisFactory::new(
            directory("nand", vec![], vec![]),
            load,
            directory("dump", vec![], vec![]),
        ));
        controller
    }

    fn builtin_test_provider() -> RecordingContentProvider {
        RecordingContentProvider { control_requests: Mutex::new(Vec::new()) }
    }

    fn set_disabled(names: &[&str]) {
        common::settings::values_mut()
            .disabled_addons
            .insert(OCARINA_TITLE_ID, names.iter().map(|name| name.to_string()).collect());
    }

    fn update_header(nso: &mut [u8], update: impl FnOnce(&mut crate::loader::nso::NsoHeader)) {
        let mut header = crate::loader::nso::NsoHeader::default();
        unsafe {
            std::ptr::copy_nonoverlapping(
                nso.as_ptr(),
                &mut header as *mut _ as *mut u8,
                NSO_HEADER_SIZE,
            );
        }
        update(&mut header);
        let bytes = unsafe {
            std::slice::from_raw_parts(&header as *const _ as *const u8, NSO_HEADER_SIZE)
        };
        nso[..NSO_HEADER_SIZE].copy_from_slice(bytes);
    }

    /// Lichii `MakeMain`.
    fn make_main(build_id: &[u8; 20], word: u32) -> Vec<u8> {
        let mut nso = vec![0; PATCH_OFFSET + 0x100];
        update_header(&mut nso, |header| {
            header.magic = common::common_funcs::make_magic(b'N', b'S', b'O', b'0');
            header.build_id[..20].copy_from_slice(build_id);
        });
        nso[PATCH_OFFSET..PATCH_OFFSET + 4].copy_from_slice(&word.to_le_bytes());
        nso
    }

    /// Lichii `MakeDynamicMain`: the real segment layout of Ocarina's main.
    fn make_dynamic_main() -> Vec<u8> {
        let mut nso = make_main(&OCARINA_BUILD_ID, TBZ_INSTRUCTION);
        nso.resize(DYNAMIC_STATE_OFFSET + 16, 0);
        update_header(&mut nso, |header| {
            header.segments[0].size = 63201744;
            header.segments[1].location = 63205376;
            header.segments[1].size = 12603520;
            header.segments[2].location = 75812864;
            header.segments[2].size = 5217152;
            header.segments[2].alignment_or_bss_size = 1414272;
        });
        nso[DYNAMIC_CALL_OFFSET..DYNAMIC_CALL_OFFSET + 4]
            .copy_from_slice(&DYNAMIC_ORIGINAL_INSTRUCTION.to_le_bytes());
        nso
    }

    fn word_at(nso: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(nso[offset..offset + 4].try_into().unwrap())
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02X}")).collect()
    }

    fn listed(manager: &PatchManager, name: &str) -> Patch {
        let patch = manager
            .get_patches(None)
            .into_iter()
            .find(|patch| patch.name == name)
            .unwrap_or_else(|| panic!("{name} is not listed"));
        assert_eq!(patch.patch_type, PatchType::Mod);
        patch
    }

    #[test]
    fn builtin_max_resolution_is_gated_on_build_id_and_original_word() {
        if ran_in_child_process("RUZU_TEST_BUILTIN_MAX_RESOLUTION_GATES") {
            return;
        }
        let controller = builtin_test_controller(None);
        let provider = builtin_test_provider();
        let patch_main = |build_id: &[u8; 20], word, name: &str, title_id| {
            let manager = PatchManager::new(title_id, &controller, &provider);
            word_at(&manager.patch_nso(make_main(build_id, word), name), PATCH_OFFSET)
        };

        // Matching build id and original word applies.
        assert_eq!(
            patch_main(&OCARINA_BUILD_ID, TBZ_INSTRUCTION, "main", OCARINA_TITLE_ID),
            BRANCH_INSTRUCTION
        );
        // Wrong original word skips.
        assert_eq!(
            patch_main(&OCARINA_BUILD_ID, NOP_INSTRUCTION, "main", OCARINA_TITLE_ID),
            NOP_INSTRUCTION
        );
        // Other build id skips.
        let mut other = OCARINA_BUILD_ID;
        other[0] ^= 1;
        assert_eq!(
            patch_main(&other, TBZ_INSTRUCTION, "main", OCARINA_TITLE_ID),
            TBZ_INSTRUCTION
        );
        // Other module skips.
        assert_eq!(
            patch_main(&OCARINA_BUILD_ID, TBZ_INSTRUCTION, "sdk", OCARINA_TITLE_ID),
            TBZ_INSTRUCTION
        );
        // Other title skips.
        assert_eq!(
            patch_main(&OCARINA_BUILD_ID, TBZ_INSTRUCTION, "main", OTHER_TITLE_ID),
            TBZ_INSTRUCTION
        );
        // Disabled in Add-Ons skips.
        set_disabled(&[MAX_RESOLUTION]);
        assert_eq!(
            patch_main(&OCARINA_BUILD_ID, TBZ_INSTRUCTION, "main", OCARINA_TITLE_ID),
            TBZ_INSTRUCTION
        );
    }

    #[test]
    fn builtin_max_resolution_defers_to_a_user_mod_at_the_same_offset() {
        if ran_in_child_process("RUZU_TEST_BUILTIN_MAX_RESOLUTION_USER_MOD") {
            return;
        }
        let provider = builtin_test_provider();
        let patch_main = |mod_word| {
            let controller = builtin_test_controller(Some((mod_word, PATCH_OFFSET)));
            let manager = PatchManager::new(OCARINA_TITLE_ID, &controller, &provider);
            word_at(
                &manager.patch_nso(make_main(&OCARINA_BUILD_ID, TBZ_INSTRUCTION), "main"),
                PATCH_OFFSET,
            )
        };
        // A different word from the mod is kept.
        assert_eq!(patch_main(NOP_INSTRUCTION), NOP_INSTRUCTION);
        // The same word from the mod is applied once.
        assert_eq!(patch_main(BRANCH_INSTRUCTION), BRANCH_INSTRUCTION);
        // Disabling the built-in patch keeps the mod.
        set_disabled(&[MAX_RESOLUTION]);
        assert_eq!(patch_main(BRANCH_INSTRUCTION), BRANCH_INSTRUCTION);
    }

    #[test]
    fn builtin_max_resolution_is_listed_and_loaded_for_ocarina_main() {
        if ran_in_child_process("RUZU_TEST_BUILTIN_MAX_RESOLUTION_LISTED") {
            return;
        }
        let controller = builtin_test_controller(None);
        let provider = builtin_test_provider();
        let manager = PatchManager::new(OCARINA_TITLE_ID, &controller, &provider);
        let mut build_id = [0u8; 0x20];
        build_id[..20].copy_from_slice(&OCARINA_BUILD_ID);

        assert!(manager.has_nso_patch(&build_id, "main"));
        assert!(!manager.has_nso_patch(&build_id, "sdk"));
        assert!(!PatchManager::new(OTHER_TITLE_ID, &controller, &provider)
            .has_nso_patch(&build_id, "main"));

        assert!(listed(&manager, MAX_RESOLUTION).enabled);
        set_disabled(&[MAX_RESOLUTION]);
        assert!(!listed(&manager, MAX_RESOLUTION).enabled);
    }

    #[test]
    fn builtin_fps_unlock_defaults_on_and_replaces_dynamic_fps() {
        if ran_in_child_process("RUZU_TEST_BUILTIN_FPS_UNLOCK") {
            return;
        }
        let controller = builtin_test_controller(None);
        let provider = builtin_test_provider();
        let manager = PatchManager::new(OCARINA_TITLE_ID, &controller, &provider);
        let routine = |nso: &[u8], size| {
            hex(&nso[DYNAMIC_ROUTINE_OFFSET..DYNAMIC_ROUTINE_OFFSET + size])
        };
        const ROUTINE_HEAD: &str = concat!(
            "2818201ECA9200B04A0138912BE03BD54C0140F94B0100F9CC0200B46B010CCB0DE03BD5",
            "4E0180D2AE09CE9A7F010EEBCB818B9A4C0540F94C0000B5EC030BAA6E010CCB8C0D8E8B",
            "4C0500F98201239EA301239E4218231E4208211E"
        );
        let nso = make_dynamic_main();

        // Default: FPS Unlock applies, Dynamic FPS is listed as replaced by it when it applies.
        assert!(listed(&manager, FPS_UNLOCK).enabled);
        assert!(listed(&manager, DYNAMIC_FPS).enabled);
        assert_eq!(
            listed(&manager, DYNAMIC_FPS).version,
            format!("IPS, replaced by {FPS_UNLOCK} when it applies")
        );
        assert_eq!(listed(&manager, FPS_UNLOCK).version, "IPS");
        let patched = manager.patch_nso(nso.clone(), "main");
        assert_eq!(word_at(&patched, DYNAMIC_CALL_OFFSET), DYNAMIC_CALL_INSTRUCTION);
        assert_eq!(
            routine(&patched, UNLOCK_ROUTINE_SIZE),
            format!(
                "{ROUTINE_HEAD}E300001C4268231EC300001C4278231E4840201EC0035FD61F2003D50AD7A33C0000C040"
            )
        );
        assert_eq!(word_at(&patched, PATCH_OFFSET), BRANCH_INSTRUCTION);
        let unlocked = routine(&patched, UNLOCK_ROUTINE_SIZE);

        // Unchecking only Dynamic FPS keeps FPS Unlock.
        set_disabled(&[DYNAMIC_FPS]);
        assert!(!listed(&manager, DYNAMIC_FPS).enabled);
        assert_eq!(
            routine(&manager.patch_nso(nso.clone(), "main"), UNLOCK_ROUTINE_SIZE),
            unlocked
        );

        // Without FPS Unlock, Dark's routine applies unchanged.
        set_disabled(&[FPS_UNLOCK]);
        assert!(!listed(&manager, FPS_UNLOCK).enabled);
        assert_eq!(listed(&manager, DYNAMIC_FPS).version, "IPS");
        let patched = manager.patch_nso(nso.clone(), "main");
        assert_eq!(word_at(&patched, DYNAMIC_CALL_OFFSET), DYNAMIC_CALL_INSTRUCTION);
        assert_eq!(
            routine(&patched, DYNAMIC_ROUTINE_SIZE),
            format!("{ROUTINE_HEAD}C300001C0309231E4020231E4D0000544840201EC0035FD60AD7833F")
        );
        assert_eq!(word_at(&patched, DYNAMIC_ROUTINE_OFFSET + DYNAMIC_ROUTINE_SIZE), 0);

        // Both unchecked: hook and code cave stay untouched.
        set_disabled(&[FPS_UNLOCK, DYNAMIC_FPS]);
        let patched = manager.patch_nso(nso.clone(), "main");
        assert_eq!(word_at(&patched, DYNAMIC_CALL_OFFSET), DYNAMIC_ORIGINAL_INSTRUCTION);
        assert_eq!(word_at(&patched, DYNAMIC_ROUTINE_OFFSET), 0);

        set_disabled(&[MAX_RESOLUTION]);
        let patched = manager.patch_nso(nso, "main");
        assert_eq!(word_at(&patched, DYNAMIC_CALL_OFFSET), DYNAMIC_CALL_INSTRUCTION);
        assert_eq!(word_at(&patched, PATCH_OFFSET), TBZ_INSTRUCTION);

        let unrelated = PatchManager::new(OTHER_TITLE_ID, &controller, &provider).get_patches(None);
        assert!(unrelated.iter().all(|patch| patch.name != DYNAMIC_FPS && patch.name != FPS_UNLOCK));
    }

    #[test]
    fn builtin_dynamic_fps_rejects_mismatched_or_truncated_modules_transactionally() {
        if ran_in_child_process("RUZU_TEST_BUILTIN_DYNAMIC_FPS_REJECTS") {
            return;
        }
        let controller = builtin_test_controller(None);
        let provider = builtin_test_provider();
        set_disabled(&[MAX_RESOLUTION]);
        let build_id_offset = std::mem::offset_of!(crate::loader::nso::NsoHeader, build_id);
        let cases: Vec<(&str, Box<dyn Fn(&mut Vec<u8>, &mut &str, &mut u64)>)> = vec![
            ("wrong build", Box::new(move |nso, _, _| nso[build_id_offset] ^= 1)),
            ("wrong module", Box::new(|_, module, _| *module = "sdk")),
            ("wrong title", Box::new(|_, _, title| *title = OTHER_TITLE_ID)),
            ("incomplete header", Box::new(|nso, _, _| nso.truncate(NSO_HEADER_SIZE - 1))),
            ("incomplete call", Box::new(|nso, _, _| nso.truncate(DYNAMIC_CALL_OFFSET + 3))),
            (
                "incomplete routine",
                Box::new(|nso, _, _| {
                    nso.truncate(DYNAMIC_ROUTINE_OFFSET + DYNAMIC_ROUTINE_SIZE - 1)
                }),
            ),
            ("incomplete timer state", Box::new(|nso, _, _| nso.truncate(DYNAMIC_STATE_OFFSET + 15))),
            (
                "timer state outside BSS",
                Box::new(|nso, _, _| {
                    update_header(nso, |header| header.segments[2].alignment_or_bss_size = 15)
                }),
            ),
            ("wrong original hook", Box::new(|nso, _, _| nso[DYNAMIC_CALL_OFFSET] ^= 1)),
            (
                "nonzero code cave",
                Box::new(|nso, _, _| nso[DYNAMIC_ROUTINE_OFFSET + DYNAMIC_ROUTINE_SIZE - 1] = 1),
            ),
            ("nonzero timer state", Box::new(|nso, _, _| nso[DYNAMIC_STATE_OFFSET + 15] = 1)),
            (
                "code cave outside executable padding",
                Box::new(|nso, _, _| update_header(nso, |header| header.segments[0].size = 0x3C46000)),
            ),
            (
                "code cave overlaps read-only segment",
                Box::new(|nso, _, _| {
                    update_header(nso, |header| header.segments[1].location = 0x3C46200)
                }),
            ),
        ];
        for (section, mutate) in cases {
            let mut nso = make_dynamic_main();
            let mut module = "main";
            let mut title = OCARINA_TITLE_ID;
            mutate(&mut nso, &mut module, &mut title);
            let manager = PatchManager::new(title, &controller, &provider);
            assert!(manager.patch_nso(nso.clone(), module) == nso, "{section}");
        }
    }

    #[test]
    fn builtin_dynamic_fps_leaves_both_records_alone_when_a_user_mod_changes_either() {
        if ran_in_child_process("RUZU_TEST_BUILTIN_DYNAMIC_FPS_USER_MOD") {
            return;
        }
        let provider = builtin_test_provider();
        set_disabled(&[MAX_RESOLUTION]);
        for (section, offset, mod_word) in [
            ("user call", DYNAMIC_CALL_OFFSET, 0x11223344),
            ("identical call", DYNAMIC_CALL_OFFSET, DYNAMIC_CALL_INSTRUCTION),
            ("user routine", DYNAMIC_ROUTINE_OFFSET, 0x11223344),
            (
                "last routine word",
                DYNAMIC_ROUTINE_OFFSET + DYNAMIC_ROUTINE_SIZE - 4,
                0x11223344,
            ),
            ("user timer state", DYNAMIC_STATE_OFFSET, 0x11223344),
        ] {
            let controller = builtin_test_controller(Some((mod_word, offset)));
            let manager = PatchManager::new(OCARINA_TITLE_ID, &controller, &provider);
            let nso = make_dynamic_main();
            let mut expected = nso.clone();
            expected[offset..offset + 4].copy_from_slice(&u32::to_le_bytes(mod_word));
            assert!(manager.patch_nso(nso, "main") == expected, "{section}");
        }
    }

    #[test]
    fn builtin_texture_pool_doubles_the_allocators_initial_pool_size() {
        if ran_in_child_process("RUZU_TEST_BUILTIN_TEXTURE_POOL") {
            return;
        }
        const CALL_OFFSET: usize = NSO_HEADER_SIZE + 0xC2A36C;
        const ROUTINE_OFFSET: usize = NSO_HEADER_SIZE + 0x3C462C0;
        const STP_INSTRUCTION: u32 = 0xA90127E8;
        const CALL_INSTRUCTION: u32 = 0x94C06FD5;
        let controller = builtin_test_controller(None);
        let provider = builtin_test_provider();
        let manager = PatchManager::new(OCARINA_TITLE_ID, &controller, &provider);
        let mut nso = make_dynamic_main();
        nso[CALL_OFFSET..CALL_OFFSET + 4].copy_from_slice(&STP_INSTRUCTION.to_le_bytes());

        let patched = manager.patch_nso(nso.clone(), "main");
        assert_eq!(word_at(&patched, CALL_OFFSET), CALL_INSTRUCTION);
        assert_eq!(hex(&patched[ROUTINE_OFFSET..ROUTINE_OFFSET + 12]), "08F97FD3E82701A9C0035FD6");
        assert_eq!(word_at(&patched, ROUTINE_OFFSET + 12), 0);
        // The Texture Pool cave sits right after the FPS Unlock routine.
        assert_eq!(DYNAMIC_ROUTINE_OFFSET + UNLOCK_ROUTINE_SIZE, ROUTINE_OFFSET - 0x40);
        assert!(listed(&manager, TEXTURE_POOL).enabled);

        set_disabled(&[TEXTURE_POOL]);
        let patched = manager.patch_nso(nso.clone(), "main");
        assert_eq!(word_at(&patched, CALL_OFFSET), STP_INSTRUCTION);
        assert_eq!(word_at(&patched, ROUTINE_OFFSET), 0);

        common::settings::values_mut().disabled_addons.remove(&OCARINA_TITLE_ID);
        nso[CALL_OFFSET..CALL_OFFSET + 4].copy_from_slice(&NOP_INSTRUCTION.to_le_bytes());
        let patched = manager.patch_nso(nso, "main");
        assert_eq!(word_at(&patched, CALL_OFFSET), NOP_INSTRUCTION);
        assert_eq!(word_at(&patched, ROUTINE_OFFSET), 0);
    }
}

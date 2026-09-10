// SPDX-License-Identifier: GPL-3.0-or-later
// Installed-title portion of qt_common/game_list/worker.{h,cpp}.

use super::*;
use ruzu_core::file_sys::nca_metadata::ContentRecordType;

fn installed_origin(path: &str) -> Option<ContentProviderUnionSlot> {
    match path {
        "SDMC" => Some(ContentProviderUnionSlot::SDMC),
        "UserNAND" => Some(ContentProviderUnionSlot::UserNAND),
        "SysNAND" => Some(ContentProviderUnionSlot::SysNAND),
        _ => None,
    }
}

/// AddTitlesToGameList: filter installed Application/Program entries by storage
/// origin; do not interpret NAND folders as ordinary recursive host paths.
pub(super) fn add_titles_to_game_list(path: &str, reader: &mut MetadataReader) -> Vec<GameFile> {
    let entries = installed_program_entries(
        path,
        &reader
            .content_provider
            .lock()
            .unwrap_or_else(|e| e.into_inner()),
    );
    let mut games = Vec::new();
    for (slot, entry) in entries {
        if slot == ContentProviderUnionSlot::FrontendManual {
            continue;
        }
        let file = reader
            .content_provider
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_entry_unparsed(entry.title_id, entry.record_type);
        let Some(file) = file else {
            continue;
        };
        let Some(mut metadata) = reader.read_virtual_file(file.clone(), true) else {
            continue;
        };
        {
            let controller = reader.controller.lock().unwrap_or_else(|e| e.into_inner());
            let provider = reader
                .content_provider
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let patch = PatchManager::new(metadata.program_id, &controller, &*provider);
            // Installed titles use their Control NCA rather than relying on the
            // program loader's title/icon methods (which can be unsupported).
            metadata.title = None;
            metadata.icon = None;
            if let Some(control) = provider.get_entry(entry.title_id, ContentRecordType::Control) {
                let cache =
                    common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::CacheDir)
                        .join("game_list");
                let (icon, name) = get_game_list_cached_object(
                    &cache,
                    patch.get_title_id(),
                    uisettings::with(|v| *v.cache_game_list.get_value()),
                    || {
                        let (nacp, icon) = patch.parse_control_nca(&control);
                        (
                            icon.map(|file| file.read_all_bytes()).unwrap_or_default(),
                            nacp.map(|nacp| nacp.get_application_name())
                                .unwrap_or_default(),
                        )
                    },
                );
                metadata.title = Some(name);
                metadata.icon = Some(icon);
            }
        }
        games.push(GameFile {
            name: metadata.title.unwrap_or_default(),
            developer: metadata.developer,
            version: metadata.version,
            kind: ruzu_core::loader::loader::get_file_type_string(identify_file(&file)).into(),
            architecture: metadata.architecture,
            size: file.get_size() as u64,
            path: PathBuf::from(file.get_full_path()),
            program_id: metadata.program_id,
            add_ons: metadata.add_ons,
            icon: metadata.icon,
        });
    }
    games
}

// The entry selection portion of AddTitlesToGameList, separated mechanically
// to test origin/content filtering without encrypted content fixtures.
fn installed_program_entries(
    path: &str,
    provider: &ContentProviderUnion,
) -> Vec<(ContentProviderUnionSlot, ContentProviderEntry)> {
    let Some(origin) = installed_origin(path) else {
        return Vec::new();
    };
    provider.list_entries_filter_origin(
        Some(origin),
        Some(TitleType::Application),
        Some(ContentRecordType::Program),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_entries_filter_origin_and_content_kind() {
        use ruzu_core::file_sys::vfs::vfs_vector::VectorVfsFile;
        let mut user = Box::new(ManualContentProvider::new());
        let mut sd = Box::new(ManualContentProvider::new());
        let mut manual = Box::new(ManualContentProvider::new());
        let file: VirtualFile = Arc::new(VectorVfsFile::new(vec![], "synthetic".into(), None));
        user.add_entry(
            TitleType::Application,
            ContentRecordType::Program,
            42,
            file.clone(),
        );
        user.add_entry(
            TitleType::Application,
            ContentRecordType::Control,
            42,
            file.clone(),
        );
        user.add_entry(
            TitleType::Update,
            ContentRecordType::Program,
            43,
            file.clone(),
        );
        sd.add_entry(
            TitleType::Application,
            ContentRecordType::Program,
            44,
            file.clone(),
        );
        manual.add_entry(TitleType::Application, ContentRecordType::Program, 45, file);
        let mut provider = ContentProviderUnion::new();
        // All boxed providers outlive the union and are not moved out of their boxes.
        unsafe {
            provider.set_slot(ContentProviderUnionSlot::UserNAND, &mut *user);
            provider.set_slot(ContentProviderUnionSlot::SDMC, &mut *sd);
            provider.set_slot(ContentProviderUnionSlot::FrontendManual, &mut *manual);
        }
        let titles = installed_program_entries("UserNAND", &provider);
        assert_eq!(titles.len(), 1);
        assert_eq!(titles[0].1.title_id, 42);
        assert_eq!(
            installed_program_entries("SDMC", &provider)[0].1.title_id,
            44
        );
        assert!(installed_program_entries("SysNAND", &provider).is_empty());
        assert!(installed_program_entries("/homebrew", &provider).is_empty());
    }

    #[test]
    fn installed_roots_map_to_distinct_storage_origins() {
        assert_eq!(
            installed_origin("SDMC"),
            Some(ContentProviderUnionSlot::SDMC)
        );
        assert_eq!(
            installed_origin("UserNAND"),
            Some(ContentProviderUnionSlot::UserNAND)
        );
        assert_eq!(
            installed_origin("SysNAND"),
            Some(ContentProviderUnionSlot::SysNAND)
        );
        assert_eq!(installed_origin("/homebrew"), None);
    }

    #[test]
    fn metadata_pair_cache_honors_toggle_and_requires_both_files() {
        let directory = super::super::tests::make_temp_dir();
        let calls = std::cell::Cell::new(0);
        let generate = || {
            calls.set(calls.get() + 1);
            (vec![1, 2, 3], format!("Homebrew {}", calls.get()))
        };
        let first = get_game_list_cached_object(&directory, 42, true, generate);
        assert_eq!(calls.get(), 1);
        assert_eq!(
            get_game_list_cached_object(&directory, 42, true, generate),
            first
        );
        assert_eq!(calls.get(), 1);
        assert_eq!(
            get_game_list_cached_object(&directory, 42, false, generate).1,
            "Homebrew 2"
        );
        assert_eq!(
            get_game_list_cached_object(&directory, 42, true, generate),
            first
        );
        // An icon alone is not a complete cache entry.
        std::fs::remove_file(directory.join("000000000000002A.appname.txt")).unwrap();
        assert_eq!(
            get_game_list_cached_object(&directory, 42, true, generate).1,
            "Homebrew 3"
        );
        get_game_list_cached_object(&directory, 0, true, generate);
        assert!(!directory.join("0000000000000000.jpeg").exists());
        // Upstream retries generation if the icon cache cannot be written.
        std::fs::create_dir(directory.join("000000000000002B.jpeg")).unwrap();
        let before = calls.get();
        get_game_list_cached_object(&directory, 43, true, generate);
        assert_eq!(calls.get(), before + 2);
        std::fs::remove_dir_all(directory).unwrap();
    }
}

/// GetGameListCachedObject's icon/name pair. Cache location and enable flag are
/// explicit inputs for deterministic tests; the caller supplies the UI setting.
fn get_game_list_cached_object(
    directory: &Path,
    program_id: u64,
    enabled: bool,
    mut generator: impl FnMut() -> (Vec<u8>, String),
) -> (Vec<u8>, String) {
    if !enabled || program_id == 0 {
        return generator();
    }
    let icon_path = directory.join(format!("{program_id:016X}.jpeg"));
    let name_path = directory.join(format!("{program_id:016X}.appname.txt"));
    let _ = std::fs::create_dir_all(directory);
    if !icon_path.exists() || !name_path.exists() {
        let (icon, name) = generator();
        if let Err(error) = std::fs::write(&icon_path, &icon) {
            log::error!(
                "Failed to write icon cache {}: {error}",
                icon_path.display()
            );
            return generator();
        }
        // Upstream returns the generated pair even if writing appname fails.
        let _ = std::fs::write(&name_path, name.as_bytes());
        return (icon, name);
    }
    match (std::fs::read(icon_path), std::fs::read(name_path)) {
        (Ok(icon), Ok(name)) => (icon, String::from_utf8_lossy(&name).into_owned()),
        _ => generator(),
    }
}

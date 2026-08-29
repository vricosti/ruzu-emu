// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK counterpart of Eden's `src/yuzu/migration_worker.{h,cpp}`.
//
// Eden offers copy, move, and link strategies for whole emulator directories.
// Ruzu exposes the non-destructive copy and link strategies for individually
// selected trees. Copies are verified byte-for-byte and links are verified
// against their canonical target before transactional activation. Legacy
// source paths are never renamed or deleted.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};

use tempfile::TempDir;

/// Upstream `Emulator` from `migration_worker.h`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Emulator {
    pub name: &'static str,
    pub directory_name: &'static str,
    pub user_dir: PathBuf,
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl Emulator {
    pub fn get_user_dir(&self) -> &Path {
        &self.user_dir
    }

    pub fn get_config_dir(&self) -> &Path {
        &self.config_dir
    }

    #[allow(dead_code)]
    pub fn get_cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    #[allow(dead_code)]
    pub fn lower_name(&self) -> String {
        self.name.to_lowercase()
    }
}

/// Upstream `legacy_emus`, represented as names plus legacy directory keys;
/// platform-specific absolute paths are resolved by the frontend detector.
pub const LEGACY_EMULATORS: [(&str, &str); 4] = [
    ("Citron", "citron"),
    ("Sudachi", "sudachi"),
    ("Suyu", "suyu"),
    ("Yuzu", "yuzu"),
];

/// Non-destructive subset of upstream `MigrationWorker::MigrationStrategy`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MigrationStrategy {
    #[default]
    Copy,
    Link,
}

/// Categories exposed independently by the migration dialog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationSelection {
    pub firmware: bool,
    pub configuration: bool,
    /// Import only the source frontend's configured game-folder paths. The
    /// frontend owns the INI merge; no ROM directory or full config is copied.
    pub game_directories: bool,
    pub nand: bool,
    pub sdmc: bool,
    pub keys: bool,
    pub save_games: Vec<u64>,
    pub mod_games: Vec<u64>,
}

impl MigrationSelection {
    pub fn any(&self) -> bool {
        self.tree_data_selected() || self.game_directories
    }

    /// Whether the filesystem worker has a whole directory tree to process.
    /// Game-directory paths are merged selectively by the frontend.
    pub fn tree_data_selected(&self) -> bool {
        self.firmware
            || self.configuration
            || self.nand
            || self.sdmc
            || self.keys
            || !self.save_games.is_empty()
            || !self.mod_games.is_empty()
    }
}

/// A title for which legacy save data and/or load-directory content can be
/// associated reliably without parsing installed NCA metadata.
/// Retained while the per-game migration tab is intentionally hidden.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MigratableGame {
    pub title_id: u64,
    pub save_bytes: u64,
    pub mod_bytes: u64,
    pub has_saves: bool,
    pub has_mods: bool,
}

#[allow(dead_code)]
impl MigratableGame {
    pub fn has_saves(self) -> bool {
        self.has_saves
    }

    pub fn has_mods(self) -> bool {
        self.has_mods
    }
}

/// All source and destination paths needed by the worker.
#[derive(Debug, Clone)]
pub struct MigrationPlan {
    pub source_name: String,
    pub source_user_dir: PathBuf,
    pub source_config_dir: PathBuf,
    pub destination_config_dir: PathBuf,
    pub destination_nand_dir: PathBuf,
    pub destination_sdmc_dir: PathBuf,
    pub destination_load_dir: PathBuf,
    pub destination_keys_dir: PathBuf,
    pub strategy: MigrationStrategy,
    pub selection: MigrationSelection,
    /// Populated only with destinations whose replacement was disclosed by
    /// the confirmation dialog. This prevents a newly appeared destination
    /// from being removed under a broader boolean authorization.
    pub confirmed_mode_conversion_destinations: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationModeConversions {
    pub copies_to_links: usize,
    pub links_to_copies: usize,
    destinations: Vec<PathBuf>,
}

impl MigrationModeConversions {
    pub fn authorize(&self, plan: &mut MigrationPlan) {
        plan.confirmed_mode_conversion_destinations = self.destinations.clone();
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MigrationReport {
    pub trees: usize,
    pub files: u64,
    pub bytes: u64,
    pub game_directories: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CopyStats {
    files: u64,
    bytes: u64,
}

struct TreeSpec {
    source: PathBuf,
    destination: PathBuf,
    staging_parent: PathBuf,
    excluded: Vec<PathBuf>,
}

struct PreparedTree {
    _temporary: TempDir,
    payload: PathBuf,
    destination: PathBuf,
    backup: PathBuf,
    had_destination: bool,
    activated: bool,
    stats: CopyStats,
}

/// Copy or share all selected data without renaming or deleting the legacy
/// installation.
pub fn process(plan: &MigrationPlan) -> io::Result<MigrationReport> {
    if !plan.selection.any() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no migration category was selected",
        ));
    }

    let specs = tree_specs(plan)?;
    let mut prepared = Vec::new();
    for spec in specs {
        if !spec.source.is_dir() {
            log::info!(
                "Skipping missing {} migration source {}",
                plan.source_name,
                spec.source.display()
            );
            continue;
        }
        let mode_conversion_confirmed = plan
            .confirmed_mode_conversion_destinations
            .contains(&spec.destination);
        prepared.push(match plan.strategy {
            MigrationStrategy::Copy => prepare_copy_tree(&spec, mode_conversion_confirmed)?,
            MigrationStrategy::Link => prepare_link_tree(&spec, mode_conversion_confirmed)?,
        });
    }

    if prepared.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "none of the selected source directories exists",
        ));
    }

    for index in 0..prepared.len() {
        if let Err(error) = activate_tree(&mut prepared[index]) {
            for tree in prepared[..index].iter_mut().rev() {
                if let Err(rollback_error) = rollback_tree(tree) {
                    log::error!(
                        "Migration rollback failed for {}: {rollback_error}",
                        tree.destination.display()
                    );
                }
            }
            return Err(error);
        }
    }

    let mut report = MigrationReport {
        trees: prepared.len(),
        ..MigrationReport::default()
    };
    for tree in &prepared {
        report.files += tree.stats.files;
        report.bytes += tree.stats.bytes;
    }
    // Dropping each TempDir removes its retained pre-migration backup only
    // after every selected destination has been activated successfully.
    Ok(report)
}

/// Inspect selected destinations without modifying them so the frontend can
/// disclose mode conversions before authorizing the worker to replace them.
pub fn inspect_mode_conversions(plan: &MigrationPlan) -> io::Result<MigrationModeConversions> {
    let mut conversions = MigrationModeConversions::default();
    for spec in tree_specs(plan)? {
        if !spec.source.is_dir() {
            continue;
        }
        let metadata = match fs::symlink_metadata(&spec.destination) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if directory_link_target(&spec.destination)?.is_some() {
            if plan.strategy == MigrationStrategy::Copy {
                conversions.links_to_copies += 1;
                conversions.destinations.push(spec.destination);
            }
        } else if plan.strategy == MigrationStrategy::Link
            && metadata.file_type().is_dir()
            && fs::read_dir(&spec.destination)?
                .next()
                .transpose()?
                .is_some()
        {
            conversions.copies_to_links += 1;
            conversions.destinations.push(spec.destination);
        }
    }
    Ok(conversions)
}

fn tree_specs(plan: &MigrationPlan) -> io::Result<Vec<TreeSpec>> {
    let mut specs = Vec::new();

    if plan.selection.configuration {
        specs.push(TreeSpec {
            source: plan.source_config_dir.clone(),
            destination: plan.destination_config_dir.clone(),
            staging_parent: stable_parent(&plan.destination_config_dir),
            excluded: Vec::new(),
        });
    }
    if plan.selection.keys {
        specs.push(TreeSpec {
            source: plan.source_user_dir.join("keys"),
            destination: plan.destination_keys_dir.clone(),
            staging_parent: stable_parent(&plan.destination_keys_dir),
            excluded: Vec::new(),
        });
    }
    if plan.selection.sdmc {
        specs.push(TreeSpec {
            source: plan.source_user_dir.join("sdmc"),
            destination: plan.destination_sdmc_dir.clone(),
            staging_parent: stable_parent(&plan.destination_sdmc_dir),
            excluded: Vec::new(),
        });
    }

    if plan.selection.nand {
        let mut excluded = vec![PathBuf::from("user/save")];
        if !plan.selection.firmware {
            excluded.push(PathBuf::from("system/Contents"));
        }
        specs.push(TreeSpec {
            source: plan.source_user_dir.join("nand"),
            destination: plan.destination_nand_dir.clone(),
            staging_parent: stable_parent(&plan.destination_nand_dir),
            excluded,
        });
    } else if plan.selection.firmware {
        specs.push(TreeSpec {
            source: plan.source_user_dir.join("nand/system/Contents"),
            destination: plan.destination_nand_dir.join("system/Contents"),
            staging_parent: stable_parent(&plan.destination_nand_dir),
            excluded: Vec::new(),
        });
    }

    for title_id in sorted_unique(&plan.selection.save_games) {
        for relative in save_directories_for_title(&plan.source_user_dir, title_id)? {
            specs.push(TreeSpec {
                source: plan.source_user_dir.join("nand/user/save").join(&relative),
                destination: plan.destination_nand_dir.join("user/save").join(relative),
                staging_parent: stable_parent(&plan.destination_nand_dir),
                excluded: Vec::new(),
            });
        }
    }

    for title_id in sorted_unique(&plan.selection.mod_games) {
        let directory = format!("{title_id:016X}");
        specs.push(TreeSpec {
            source: plan.source_user_dir.join("load").join(&directory),
            destination: plan.destination_load_dir.join(directory),
            staging_parent: stable_parent(&plan.destination_load_dir),
            excluded: Vec::new(),
        });
    }

    Ok(specs)
}

fn stable_parent(destination_root: &Path) -> PathBuf {
    destination_root
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| destination_root.to_path_buf())
}

/// Estimate the bytes selected for migration. This scans metadata only and is
/// used by the confirmation page before any destination is modified.
pub fn estimate_selection_bytes(plan: &MigrationPlan) -> io::Result<u64> {
    tree_specs(plan)?
        .into_iter()
        .try_fold(0_u64, |total, spec| {
            Ok(total.saturating_add(tree_size(&spec.source, &spec.excluded)?))
        })
}

/// Discover per-title save and mod sizes using the directory ownership used by
/// upstream: saves are nested below an account id and mods are keyed directly
/// by their 16-digit title id. Installed update/DLC NCAs are intentionally not
/// guessed here because their title ownership requires CNMT parsing.
#[allow(dead_code)]
pub fn discover_migratable_games(user_dir: &Path) -> io::Result<Vec<MigratableGame>> {
    let mut games = BTreeMap::<u64, MigratableGame>::new();
    for relative in save_title_directories(user_dir)? {
        let Some(name) = relative.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(title_id) = parse_title_id(name) else {
            continue;
        };
        let bytes = tree_size(&user_dir.join("nand/user/save").join(relative), &[])?;
        let game = games.entry(title_id).or_insert(MigratableGame {
            title_id,
            ..MigratableGame::default()
        });
        game.has_saves = true;
        game.save_bytes = game.save_bytes.saturating_add(bytes);
    }

    let load = user_dir.join("load");
    if load.is_dir() {
        for entry in fs::read_dir(load)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(title_id) = parse_title_id(&name) else {
                continue;
            };
            let game = games.entry(title_id).or_insert(MigratableGame {
                title_id,
                ..MigratableGame::default()
            });
            game.has_mods = true;
            game.mod_bytes = tree_size(&entry.path(), &[])?;
        }
    }

    Ok(games.into_values().collect())
}

fn sorted_unique(title_ids: &[u64]) -> Vec<u64> {
    let mut title_ids = title_ids.to_vec();
    title_ids.sort_unstable();
    title_ids.dedup();
    title_ids
}

fn parse_title_id(value: &str) -> Option<u64> {
    (value.len() == 16)
        .then(|| u64::from_str_radix(value, 16).ok())
        .flatten()
        .filter(|title_id| *title_id != 0)
}

fn save_title_directories(user_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let save_root = user_dir.join("nand/user/save");
    let mut titles = Vec::new();
    if !save_root.is_dir() {
        return Ok(titles);
    }
    for space in fs::read_dir(&save_root)? {
        let space = space?;
        if !space.file_type()?.is_dir() {
            continue;
        }
        for account in fs::read_dir(space.path())? {
            let account = account?;
            if !account.file_type()?.is_dir() {
                continue;
            }
            let account_name = account.file_name();
            let Some(account_name) = account_name.to_str() else {
                continue;
            };
            if account_name.len() != 32
                || !account_name.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                continue;
            }
            for title in fs::read_dir(account.path())? {
                let title = title?;
                if !title.file_type()?.is_dir() {
                    continue;
                }
                let title_name = title.file_name();
                let Some(title_name) = title_name.to_str() else {
                    continue;
                };
                if parse_title_id(title_name).is_some() {
                    titles.push(
                        PathBuf::from(space.file_name())
                            .join(account.file_name())
                            .join(title_name),
                    );
                }
            }
        }
    }
    titles.sort();
    Ok(titles)
}

fn save_directories_for_title(user_dir: &Path, title_id: u64) -> io::Result<Vec<PathBuf>> {
    Ok(save_title_directories(user_dir)?
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(parse_title_id)
                == Some(title_id)
        })
        .collect())
}

fn tree_size(source: &Path, excluded: &[PathBuf]) -> io::Result<u64> {
    if !source.is_dir() {
        return Ok(0);
    }
    tree_size_directory(source, source, excluded)
}

fn tree_size_directory(root: &Path, source: &Path, excluded: &[PathBuf]) -> io::Result<u64> {
    let mut bytes = 0_u64;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(root).map_err(io::Error::other)?;
        if excluded
            .iter()
            .any(|excluded| relative.starts_with(excluded))
        {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
            bytes = bytes.saturating_add(tree_size_directory(root, &path, excluded)?);
        } else if metadata.file_type().is_file() {
            bytes = bytes.saturating_add(metadata.len());
        }
    }
    Ok(bytes)
}

fn prepare_copy_tree(spec: &TreeSpec, allow_mode_conversion: bool) -> io::Result<PreparedTree> {
    reject_aliasing(&spec.source, &spec.destination)?;
    spec.destination.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("destination has no parent: {}", spec.destination.display()),
        )
    })?;
    fs::create_dir_all(&spec.staging_parent)?;

    let temporary = tempfile::Builder::new()
        .prefix(".ruzu-migration-")
        .tempdir_in(&spec.staging_parent)?;
    let payload = temporary.path().join("payload");
    let backup = temporary.path().join("backup");
    fs::create_dir(&payload)?;

    let destination_metadata = match fs::symlink_metadata(&spec.destination) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };

    // Start from the current Ruzu tree so activating the migration is a merge,
    // not an implicit deletion of data the user may already have. A final
    // symlink is an existing destination entry, but its target must never be
    // traversed: activation replaces the Ruzu-owned link with the verified
    // real directory and leaves the legacy target untouched.
    if let Some(metadata) = destination_metadata.as_ref() {
        if directory_link_target(&spec.destination)?.is_some() {
            if !allow_mode_conversion {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "replacing a shared link requires confirmed mode conversion: {}",
                        spec.destination.display()
                    ),
                ));
            }
            // The link or junction itself is retained as the rollback backup.
        } else if !metadata.file_type().is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "migration destination is not a real directory: {}",
                    spec.destination.display()
                ),
            ));
        } else {
            copy_tree_contents(&spec.destination, &payload, &[], false)?;
        }
    }

    let stats = copy_tree_contents(&spec.source, &payload, &spec.excluded, true)?;
    verify_tree_contents(&spec.source, &payload, &spec.excluded)?;

    Ok(PreparedTree {
        _temporary: temporary,
        payload,
        destination: spec.destination.clone(),
        backup,
        had_destination: destination_metadata.is_some(),
        activated: false,
        stats,
    })
}

fn prepare_link_tree(spec: &TreeSpec, allow_mode_conversion: bool) -> io::Result<PreparedTree> {
    if !spec.excluded.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "cannot share only part of a directory: {}",
                spec.source.display()
            ),
        ));
    }
    reject_aliasing(&spec.source, &spec.destination)?;
    let destination_parent = spec.destination.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("destination has no parent: {}", spec.destination.display()),
        )
    })?;
    fs::create_dir_all(destination_parent)?;
    fs::create_dir_all(&spec.staging_parent)?;

    let destination_metadata = match fs::symlink_metadata(&spec.destination) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if let Some(metadata) = destination_metadata.as_ref() {
        if let Some(target) = directory_link_target(&spec.destination)? {
            let source = fs::canonicalize(&spec.source)?;
            let target = canonicalize_link_target(&spec.destination, &target)?;
            if source != target {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "migration destination already links to another directory: {}",
                        spec.destination.display()
                    ),
                ));
            }
        } else if metadata.file_type().is_dir() {
            let destination_is_nonempty = fs::read_dir(&spec.destination)?
                .next()
                .transpose()?
                .is_some();
            if destination_is_nonempty && !allow_mode_conversion {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "replacing a local copy requires confirmed mode conversion: {}",
                        spec.destination.display()
                    ),
                ));
            }
        } else {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "migration destination is not a directory: {}",
                    spec.destination.display()
                ),
            ));
        }
    }

    let temporary = tempfile::Builder::new()
        .prefix(".ruzu-migration-")
        .tempdir_in(&spec.staging_parent)?;
    let payload = temporary.path().join("payload");
    let backup = temporary.path().join("backup");
    create_directory_link(&spec.source, &payload)?;
    verify_directory_link(&spec.source, &payload)?;

    Ok(PreparedTree {
        _temporary: temporary,
        payload,
        destination: spec.destination.clone(),
        backup,
        had_destination: destination_metadata.is_some(),
        activated: false,
        stats: CopyStats::default(),
    })
}

fn activate_tree(tree: &mut PreparedTree) -> io::Result<()> {
    if let Some(parent) = tree.destination.parent() {
        fs::create_dir_all(parent)?;
    }
    if tree.had_destination {
        fs::rename(&tree.destination, &tree.backup)?;
    }
    if let Err(error) = fs::rename(&tree.payload, &tree.destination) {
        if tree.had_destination {
            if let Err(rollback_error) = fs::rename(&tree.backup, &tree.destination) {
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "activation failed ({error}) and rollback failed ({rollback_error}) for {}",
                        tree.destination.display()
                    ),
                ));
            }
        }
        return Err(error);
    }
    tree.activated = true;
    Ok(())
}

fn rollback_tree(tree: &mut PreparedTree) -> io::Result<()> {
    if !tree.activated {
        return Ok(());
    }
    remove_entry(&tree.destination)?;
    if tree.had_destination {
        fs::rename(&tree.backup, &tree.destination)?;
    }
    tree.activated = false;
    Ok(())
}

fn reject_aliasing(source: &Path, destination: &Path) -> io::Result<()> {
    let source = fs::canonicalize(source)?;

    // Resolve the destination parent, but deliberately do not follow the
    // final component. Replacing a Ruzu-owned symlink that points at the
    // legacy source is safe; resolving that link would incorrectly report the
    // two trees as identical. A symlinked parent is still resolved so a
    // destination entry physically nested inside the source remains blocked.
    let destination_entry = destination
        .parent()
        .and_then(|parent| fs::canonicalize(parent).ok())
        .and_then(|parent| {
            destination
                .file_name()
                .map(|file_name| parent.join(file_name))
        })
        .unwrap_or_else(|| destination.to_path_buf());

    if paths_overlap(&source, &destination_entry) {
        return aliasing_error(&source, &destination_entry);
    }

    if directory_link_target(destination)?.is_some() {
        return Ok(());
    }

    if let Ok(destination) = fs::canonicalize(destination) {
        if paths_overlap(&source, &destination) {
            return aliasing_error(&source, &destination);
        }
    }
    Ok(())
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

fn aliasing_error(source: &Path, destination: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "source and destination overlap: {} and {}",
            source.display(),
            destination.display()
        ),
    ))
}

fn copy_tree_contents(
    source: &Path,
    destination: &Path,
    excluded: &[PathBuf],
    count: bool,
) -> io::Result<CopyStats> {
    let mut stats = CopyStats::default();
    copy_directory(source, source, destination, excluded, count, &mut stats)?;
    Ok(stats)
}

fn copy_directory(
    root: &Path,
    source: &Path,
    destination: &Path,
    excluded: &[PathBuf],
    count: bool,
    stats: &mut CopyStats,
) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let relative = source_path.strip_prefix(root).map_err(io::Error::other)?;
        if excluded.iter().any(|path| relative.starts_with(path)) {
            continue;
        }
        let destination_path = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            replace_with_symlink(&source_path, &destination_path)?;
            if count {
                stats.files += 1;
            }
        } else if file_type.is_dir() {
            if let Ok(destination_metadata) = fs::symlink_metadata(&destination_path) {
                if !destination_metadata.file_type().is_dir()
                    || destination_metadata.file_type().is_symlink()
                {
                    remove_entry(&destination_path)?;
                }
            }
            copy_directory(
                root,
                &source_path,
                &destination_path,
                excluded,
                count,
                stats,
            )?;
        } else if file_type.is_file() {
            if let Ok(destination_metadata) = fs::symlink_metadata(&destination_path) {
                if destination_metadata.file_type().is_dir()
                    || destination_metadata.file_type().is_symlink()
                {
                    remove_entry(&destination_path)?;
                }
            }
            fs::copy(&source_path, &destination_path)?;
            if count {
                stats.files += 1;
                stats.bytes += metadata.len();
            }
        }
    }
    Ok(())
}

fn verify_tree_contents(source: &Path, destination: &Path, excluded: &[PathBuf]) -> io::Result<()> {
    verify_directory(source, source, destination, excluded)
}

fn verify_directory(
    root: &Path,
    source: &Path,
    destination: &Path,
    excluded: &[PathBuf],
) -> io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let relative = source_path.strip_prefix(root).map_err(io::Error::other)?;
        if excluded.iter().any(|path| relative.starts_with(path)) {
            continue;
        }
        let destination_path = destination.join(entry.file_name());
        let source_metadata = fs::symlink_metadata(&source_path)?;
        let destination_metadata = fs::symlink_metadata(&destination_path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "verification failed for {}: {error}",
                    destination_path.display()
                ),
            )
        })?;

        let source_type = source_metadata.file_type();
        let destination_type = destination_metadata.file_type();
        if source_type.is_symlink() {
            if !destination_type.is_symlink()
                || fs::read_link(&source_path)? != fs::read_link(&destination_path)?
            {
                return verification_error(&source_path);
            }
        } else if source_type.is_dir() {
            if !destination_type.is_dir() {
                return verification_error(&source_path);
            }
            verify_directory(root, &source_path, &destination_path, excluded)?;
        } else if source_type.is_file() {
            if !destination_type.is_file()
                || source_metadata.len() != destination_metadata.len()
                || !files_equal(&source_path, &destination_path)?
            {
                return verification_error(&source_path);
            }
        }
    }
    Ok(())
}

fn verification_error(path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("migration verification failed for {}", path.display()),
    ))
}

fn files_equal(left: &Path, right: &Path) -> io::Result<bool> {
    let mut left = BufReader::new(File::open(left)?);
    let mut right = BufReader::new(File::open(right)?);
    let mut left_buffer = [0_u8; 64 * 1024];
    let mut right_buffer = [0_u8; 64 * 1024];
    loop {
        let left_count = left.read(&mut left_buffer)?;
        let right_count = right.read(&mut right_buffer)?;
        if left_count != right_count || left_buffer[..left_count] != right_buffer[..right_count] {
            return Ok(false);
        }
        if left_count == 0 {
            return Ok(true);
        }
    }
}

fn replace_with_symlink(source: &Path, destination: &Path) -> io::Result<()> {
    if fs::symlink_metadata(destination).is_ok() {
        remove_entry(destination)?;
    }
    let target = fs::read_link(source)?;
    create_symlink(source, &target, destination)
}

fn canonicalize_link_target(link: &Path, target: &Path) -> io::Result<PathBuf> {
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        link.parent()
            .map(|parent| parent.join(target))
            .unwrap_or_else(|| target.to_path_buf())
    };
    fs::canonicalize(target)
}

fn verify_directory_link(source: &Path, link: &Path) -> io::Result<()> {
    let expected = fs::canonicalize(source)?;
    let target = directory_link_target(link)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "shared destination is not a directory link: {}",
                link.display()
            ),
        )
    })?;
    let actual = canonicalize_link_target(link, &target)?;
    if actual != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "shared destination points to {} instead of {}",
                actual.display(),
                expected.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn create_directory_link(source: &Path, destination: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(source, destination)
}

#[cfg(windows)]
fn create_directory_link(source: &Path, destination: &Path) -> io::Result<()> {
    junction::create(source, destination)
}

#[cfg(unix)]
fn directory_link_target(path: &Path) -> io::Result<Option<PathBuf>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() {
        fs::read_link(path).map(Some)
    } else {
        Ok(None)
    }
}

#[cfg(windows)]
fn junction_exists(path: &Path) -> io::Result<bool> {
    const ERROR_NOT_A_REPARSE_POINT: i32 = 4390;

    match junction::exists(path) {
        Err(error) if error.raw_os_error() == Some(ERROR_NOT_A_REPARSE_POINT) => Ok(false),
        result => result,
    }
}

#[cfg(windows)]
fn directory_link_target(path: &Path) -> io::Result<Option<PathBuf>> {
    if junction_exists(path)? {
        return junction::get_target(path).map(Some);
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() {
        fs::read_link(path).map(Some)
    } else {
        Ok(None)
    }
}

#[cfg(unix)]
fn create_symlink(_source: &Path, target: &Path, destination: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, destination)
}

#[cfg(windows)]
fn create_symlink(source: &Path, target: &Path, destination: &Path) -> io::Result<()> {
    if fs::metadata(source).is_ok_and(|metadata| metadata.is_dir()) {
        std::os::windows::fs::symlink_dir(target, destination)
    } else {
        std::os::windows::fs::symlink_file(target, destination)
    }
}

fn remove_entry(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    if junction_exists(path)? {
        return junction::delete(path);
    }

    let metadata = fs::symlink_metadata(path)?;
    #[cfg(windows)]
    if metadata.file_type().is_symlink() && fs::metadata(path).is_ok_and(|target| target.is_dir()) {
        return fs::remove_dir(path);
    }
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic homebrew fixture ids reserved for tests.
    const SYNTHETIC_HOMEBREW_TITLE_ID: u64 = 0x05AA_0000_0000_1000;
    const OTHER_SYNTHETIC_HOMEBREW_TITLE_ID: u64 = 0x05AA_0000_0000_2000;

    fn plan(root: &Path, selection: MigrationSelection) -> MigrationPlan {
        let source = root.join("source");
        let target = root.join("target");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        MigrationPlan {
            source_name: "Yuzu".to_owned(),
            source_user_dir: source.clone(),
            source_config_dir: root.join("source-config"),
            destination_config_dir: root.join("target-config"),
            destination_nand_dir: target.join("nand"),
            destination_sdmc_dir: target.join("sdmc"),
            destination_load_dir: target.join("load"),
            destination_keys_dir: target.join("keys"),
            strategy: MigrationStrategy::Copy,
            selection,
            confirmed_mode_conversion_destinations: Vec::new(),
        }
    }

    #[test]
    fn migration_copies_selected_trees_and_preserves_every_source() {
        let root = tempfile::tempdir().unwrap();
        let selection = MigrationSelection {
            configuration: true,
            keys: true,
            ..MigrationSelection::default()
        };
        let plan = plan(root.path(), selection);
        fs::create_dir_all(&plan.source_config_dir).unwrap();
        fs::create_dir_all(plan.source_user_dir.join("keys")).unwrap();
        fs::write(plan.source_config_dir.join("qt-config.ini"), b"settings").unwrap();
        fs::write(plan.source_user_dir.join("keys/prod.keys"), b"secret").unwrap();

        let report = process(&plan).unwrap();

        assert_eq!(report.trees, 2);
        assert_eq!(report.files, 2);
        assert_eq!(
            fs::read(plan.destination_config_dir.join("qt-config.ini")).unwrap(),
            b"settings"
        );
        assert_eq!(
            fs::read(plan.destination_keys_dir.join("prod.keys")).unwrap(),
            b"secret"
        );
        assert_eq!(
            fs::read(plan.source_config_dir.join("qt-config.ini")).unwrap(),
            b"settings"
        );
        assert_eq!(
            fs::read(plan.source_user_dir.join("keys/prod.keys")).unwrap(),
            b"secret"
        );
    }

    #[test]
    fn game_directory_paths_are_a_frontend_selection_not_a_worker_tree() {
        let selection = MigrationSelection {
            game_directories: true,
            ..MigrationSelection::default()
        };
        assert!(selection.any());
        assert!(!selection.tree_data_selected());
    }

    #[cfg(windows)]
    #[test]
    fn regular_directory_is_not_reported_as_a_directory_link() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("regular-directory");
        fs::create_dir(&directory).unwrap();

        assert_eq!(directory_link_target(&directory).unwrap(), None);
    }

    #[test]
    fn firmware_only_migration_does_not_copy_the_rest_of_nand() {
        let root = tempfile::tempdir().unwrap();
        let plan = plan(
            root.path(),
            MigrationSelection {
                firmware: true,
                ..MigrationSelection::default()
            },
        );
        fs::create_dir_all(plan.source_user_dir.join("nand/user/Contents/registered")).unwrap();
        fs::create_dir_all(plan.source_user_dir.join("nand/system/Contents")).unwrap();
        fs::write(
            plan.source_user_dir
                .join("nand/user/Contents/registered/update.nca"),
            b"update",
        )
        .unwrap();
        fs::write(
            plan.source_user_dir.join("nand/system/Contents/firmware"),
            b"fw",
        )
        .unwrap();

        process(&plan).unwrap();

        assert_eq!(
            fs::read(plan.destination_nand_dir.join("system/Contents/firmware")).unwrap(),
            b"fw"
        );
        assert!(!plan
            .destination_nand_dir
            .join("user/Contents/registered/update.nca")
            .exists());
    }

    #[cfg(unix)]
    #[test]
    fn confirmed_copy_replaces_destination_symlink_without_touching_source() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let mut plan = plan(
            root.path(),
            MigrationSelection {
                firmware: true,
                ..MigrationSelection::default()
            },
        );
        let source = plan.source_user_dir.join("nand/system/Contents");
        let destination = plan.destination_nand_dir.join("system/Contents");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(source.join("homebrew-firmware.bin"), b"firmware").unwrap();
        symlink(&source, &destination).unwrap();

        let conversions = inspect_mode_conversions(&plan).unwrap();
        assert_eq!(conversions.copies_to_links, 0);
        assert_eq!(conversions.links_to_copies, 1);
        let error = process(&plan).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(fs::symlink_metadata(&destination)
            .unwrap()
            .file_type()
            .is_symlink());

        conversions.authorize(&mut plan);
        process(&plan).unwrap();

        assert_eq!(
            fs::read(source.join("homebrew-firmware.bin")).unwrap(),
            b"firmware"
        );
        assert!(!fs::symlink_metadata(&destination)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read(destination.join("homebrew-firmware.bin")).unwrap(),
            b"firmware"
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn link_strategy_shares_firmware_and_keys_without_copying_files() {
        let root = tempfile::tempdir().unwrap();
        let mut plan = plan(
            root.path(),
            MigrationSelection {
                firmware: true,
                keys: true,
                ..MigrationSelection::default()
            },
        );
        plan.strategy = MigrationStrategy::Link;
        let firmware_source = plan.source_user_dir.join("nand/system/Contents");
        let keys_source = plan.source_user_dir.join("keys");
        fs::create_dir_all(&firmware_source).unwrap();
        fs::create_dir_all(&keys_source).unwrap();
        fs::write(firmware_source.join("homebrew-firmware.bin"), b"firmware").unwrap();
        fs::write(keys_source.join("homebrew.keys"), b"keys").unwrap();

        let report = process(&plan).unwrap();

        let firmware_destination = plan.destination_nand_dir.join("system/Contents");
        assert!(directory_link_target(&firmware_destination)
            .unwrap()
            .is_some());
        assert!(directory_link_target(&plan.destination_keys_dir)
            .unwrap()
            .is_some());
        assert_eq!(
            fs::canonicalize(firmware_destination).unwrap(),
            fs::canonicalize(firmware_source).unwrap()
        );
        assert_eq!(
            fs::canonicalize(&plan.destination_keys_dir).unwrap(),
            fs::canonicalize(keys_source).unwrap()
        );
        assert_eq!(report.trees, 2);
        assert_eq!(report.files, 0);
        assert_eq!(report.bytes, 0);
    }

    #[test]
    fn link_strategy_requires_confirmation_to_replace_nonempty_ruzu_copy() {
        let root = tempfile::tempdir().unwrap();
        let mut plan = plan(
            root.path(),
            MigrationSelection {
                keys: true,
                ..MigrationSelection::default()
            },
        );
        plan.strategy = MigrationStrategy::Link;
        fs::create_dir_all(plan.source_user_dir.join("keys")).unwrap();
        fs::write(plan.source_user_dir.join("keys/homebrew.keys"), b"source").unwrap();
        fs::create_dir_all(&plan.destination_keys_dir).unwrap();
        fs::write(plan.destination_keys_dir.join("ruzu.keys"), b"keep").unwrap();

        let conversions = inspect_mode_conversions(&plan).unwrap();
        assert_eq!(conversions.copies_to_links, 1);
        assert_eq!(conversions.links_to_copies, 0);
        let error = process(&plan).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            fs::read(plan.destination_keys_dir.join("ruzu.keys")).unwrap(),
            b"keep"
        );
        assert_eq!(
            fs::read(plan.source_user_dir.join("keys/homebrew.keys")).unwrap(),
            b"source"
        );

        conversions.authorize(&mut plan);
        process(&plan).unwrap();

        assert!(directory_link_target(&plan.destination_keys_dir)
            .unwrap()
            .is_some());
        assert!(!plan.destination_keys_dir.join("ruzu.keys").exists());
        assert_eq!(
            fs::read(plan.destination_keys_dir.join("homebrew.keys")).unwrap(),
            b"source"
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn confirmation_authorizes_only_the_destinations_that_were_disclosed() {
        let root = tempfile::tempdir().unwrap();
        let mut plan = plan(
            root.path(),
            MigrationSelection {
                firmware: true,
                keys: true,
                ..MigrationSelection::default()
            },
        );
        plan.strategy = MigrationStrategy::Link;
        let firmware_source = plan.source_user_dir.join("nand/system/Contents");
        let firmware_destination = plan.destination_nand_dir.join("system/Contents");
        fs::create_dir_all(&firmware_source).unwrap();
        fs::create_dir_all(plan.source_user_dir.join("keys")).unwrap();
        fs::write(firmware_source.join("homebrew-firmware.bin"), b"source").unwrap();
        fs::write(plan.source_user_dir.join("keys/homebrew.keys"), b"source").unwrap();
        fs::create_dir_all(&plan.destination_keys_dir).unwrap();
        fs::write(plan.destination_keys_dir.join("ruzu.keys"), b"keep").unwrap();

        let conversions = inspect_mode_conversions(&plan).unwrap();
        assert_eq!(conversions.copies_to_links, 1);
        conversions.authorize(&mut plan);

        // This destination appeared after the confirmation snapshot and must
        // therefore not inherit authorization intended for the keys path.
        fs::create_dir_all(&firmware_destination).unwrap();
        fs::write(firmware_destination.join("ruzu-firmware.bin"), b"keep").unwrap();

        let error = process(&plan).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            fs::read(plan.destination_keys_dir.join("ruzu.keys")).unwrap(),
            b"keep"
        );
        assert_eq!(
            fs::read(firmware_destination.join("ruzu-firmware.bin")).unwrap(),
            b"keep"
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn confirmed_copy_share_copy_round_trip_preserves_the_source() {
        let root = tempfile::tempdir().unwrap();
        let mut plan = plan(
            root.path(),
            MigrationSelection {
                keys: true,
                ..MigrationSelection::default()
            },
        );
        let source = plan.source_user_dir.join("keys");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("homebrew.keys"), b"source").unwrap();

        process(&plan).unwrap();
        assert!(directory_link_target(&plan.destination_keys_dir)
            .unwrap()
            .is_none());

        plan.strategy = MigrationStrategy::Link;
        let conversions = inspect_mode_conversions(&plan).unwrap();
        assert_eq!(conversions.copies_to_links, 1);
        conversions.authorize(&mut plan);
        process(&plan).unwrap();
        assert!(directory_link_target(&plan.destination_keys_dir)
            .unwrap()
            .is_some());

        plan.strategy = MigrationStrategy::Copy;
        plan.confirmed_mode_conversion_destinations.clear();
        let conversions = inspect_mode_conversions(&plan).unwrap();
        assert_eq!(conversions.links_to_copies, 1);
        conversions.authorize(&mut plan);
        process(&plan).unwrap();

        assert!(directory_link_target(&plan.destination_keys_dir)
            .unwrap()
            .is_none());
        assert_eq!(
            fs::read(plan.destination_keys_dir.join("homebrew.keys")).unwrap(),
            b"source"
        );
        assert_eq!(fs::read(source.join("homebrew.keys")).unwrap(), b"source");
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn link_strategy_replaces_an_empty_ruzu_directory() {
        let root = tempfile::tempdir().unwrap();
        let mut plan = plan(
            root.path(),
            MigrationSelection {
                keys: true,
                ..MigrationSelection::default()
            },
        );
        plan.strategy = MigrationStrategy::Link;
        fs::create_dir_all(plan.source_user_dir.join("keys")).unwrap();
        fs::write(plan.source_user_dir.join("keys/homebrew.keys"), b"source").unwrap();
        fs::create_dir_all(&plan.destination_keys_dir).unwrap();

        process(&plan).unwrap();

        assert!(directory_link_target(&plan.destination_keys_dir)
            .unwrap()
            .is_some());
        assert_eq!(
            fs::read(plan.destination_keys_dir.join("homebrew.keys")).unwrap(),
            b"source"
        );
    }

    #[test]
    fn link_strategy_rejects_a_partial_directory_selection() {
        let root = tempfile::tempdir().unwrap();
        let mut plan = plan(
            root.path(),
            MigrationSelection {
                nand: true,
                ..MigrationSelection::default()
            },
        );
        plan.strategy = MigrationStrategy::Link;
        fs::create_dir_all(plan.source_user_dir.join("nand/user/Contents/registered")).unwrap();

        let error = process(&plan).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("cannot share only part"));
    }

    #[test]
    fn nand_without_firmware_excludes_saves_and_firmware() {
        let root = tempfile::tempdir().unwrap();
        let plan = plan(
            root.path(),
            MigrationSelection {
                nand: true,
                ..MigrationSelection::default()
            },
        );
        fs::create_dir_all(plan.source_user_dir.join(
            "nand/user/save/0000000000000000/00112233445566778899AABBCCDDEEFF/0100000000001000",
        ))
        .unwrap();
        fs::create_dir_all(plan.source_user_dir.join("nand/user/Contents/registered")).unwrap();
        fs::create_dir_all(plan.source_user_dir.join("nand/system/Contents")).unwrap();
        fs::write(
            plan.source_user_dir.join(
                "nand/user/save/0000000000000000/00112233445566778899AABBCCDDEEFF/0100000000001000/data",
            ),
            b"save",
        )
        .unwrap();
        fs::write(
            plan.source_user_dir
                .join("nand/user/Contents/registered/update.nca"),
            b"update",
        )
        .unwrap();
        fs::write(
            plan.source_user_dir.join("nand/system/Contents/firmware"),
            b"fw",
        )
        .unwrap();

        process(&plan).unwrap();

        assert!(!plan.destination_nand_dir.join("user/save").exists());
        assert!(!plan
            .destination_nand_dir
            .join("system/Contents/firmware")
            .exists());
        assert_eq!(
            fs::read(
                plan.destination_nand_dir
                    .join("user/Contents/registered/update.nca")
            )
            .unwrap(),
            b"update"
        );
    }

    #[test]
    fn selected_save_migration_preserves_account_layout_and_ignores_other_titles() {
        let root = tempfile::tempdir().unwrap();
        let selected = SYNTHETIC_HOMEBREW_TITLE_ID;
        let ignored = OTHER_SYNTHETIC_HOMEBREW_TITLE_ID;
        let plan = plan(
            root.path(),
            MigrationSelection {
                save_games: vec![selected],
                ..MigrationSelection::default()
            },
        );
        let account = "00112233445566778899AABBCCDDEEFF";
        let save_root = plan
            .source_user_dir
            .join("nand/user/save/0000000000000000")
            .join(account);
        fs::create_dir_all(save_root.join(format!("{selected:016X}"))).unwrap();
        fs::create_dir_all(save_root.join(format!("{ignored:016X}"))).unwrap();
        fs::write(save_root.join(format!("{selected:016X}/data")), b"selected").unwrap();
        fs::write(save_root.join(format!("{ignored:016X}/data")), b"ignored").unwrap();

        process(&plan).unwrap();

        let target = plan
            .destination_nand_dir
            .join("user/save/0000000000000000")
            .join(account);
        assert_eq!(
            fs::read(target.join(format!("{selected:016X}/data"))).unwrap(),
            b"selected"
        );
        assert!(!target.join(format!("{ignored:016X}/data")).exists());
    }

    #[test]
    fn nand_and_selected_save_use_independent_staging_outside_nand() {
        let root = tempfile::tempdir().unwrap();
        let plan = plan(
            root.path(),
            MigrationSelection {
                nand: true,
                save_games: vec![SYNTHETIC_HOMEBREW_TITLE_ID],
                ..MigrationSelection::default()
            },
        );
        let relative = format!(
            "nand/user/save/0000000000000000/00112233445566778899AABBCCDDEEFF/{SYNTHETIC_HOMEBREW_TITLE_ID:016X}"
        );
        fs::create_dir_all(plan.source_user_dir.join(&relative)).unwrap();
        fs::create_dir_all(plan.source_user_dir.join("nand/user/Contents/registered")).unwrap();
        fs::write(
            plan.source_user_dir.join(&relative).join("save.bin"),
            b"save",
        )
        .unwrap();
        fs::write(
            plan.source_user_dir
                .join("nand/user/Contents/registered/update.nca"),
            b"update",
        )
        .unwrap();

        process(&plan).unwrap();

        assert_eq!(
            fs::read(
                plan.destination_nand_dir
                    .join(relative.strip_prefix("nand/").unwrap())
                    .join("save.bin")
            )
            .unwrap(),
            b"save"
        );
        assert_eq!(
            fs::read(
                plan.destination_nand_dir
                    .join("user/Contents/registered/update.nca")
            )
            .unwrap(),
            b"update"
        );
    }

    #[test]
    fn activation_merges_selected_mod_with_existing_ruzu_data() {
        let root = tempfile::tempdir().unwrap();
        let selected = SYNTHETIC_HOMEBREW_TITLE_ID;
        let plan = plan(
            root.path(),
            MigrationSelection {
                mod_games: vec![selected],
                ..MigrationSelection::default()
            },
        );
        let source = plan.source_user_dir.join(format!("load/{selected:016X}"));
        let destination = plan.destination_load_dir.join(format!("{selected:016X}"));
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(source.join("shared"), b"legacy").unwrap();
        fs::write(destination.join("shared"), b"ruzu").unwrap();
        fs::write(destination.join("ruzu-only"), b"keep").unwrap();

        process(&plan).unwrap();

        assert_eq!(fs::read(destination.join("shared")).unwrap(), b"legacy");
        assert_eq!(fs::read(destination.join("ruzu-only")).unwrap(), b"keep");
    }

    #[test]
    fn discovers_per_title_save_and_mod_sizes() {
        let root = tempfile::tempdir().unwrap();
        let user = root.path().join("yuzu");
        let title = SYNTHETIC_HOMEBREW_TITLE_ID;
        let save = user.join(format!(
            "nand/user/save/0000000000000000/00112233445566778899AABBCCDDEEFF/{title:016X}"
        ));
        let mods = user.join(format!("load/{title:016x}"));
        fs::create_dir_all(&save).unwrap();
        fs::create_dir_all(&mods).unwrap();
        fs::write(save.join("save.bin"), vec![0_u8; 7]).unwrap();
        fs::write(mods.join("mod.bin"), vec![0_u8; 11]).unwrap();

        let games = discover_migratable_games(&user).unwrap();

        assert_eq!(
            games,
            vec![MigratableGame {
                title_id: title,
                save_bytes: 7,
                mod_bytes: 11,
                has_saves: true,
                has_mods: true,
            }]
        );
        assert!(games[0].has_saves());
        assert!(games[0].has_mods());
    }

    #[test]
    fn shader_cache_is_not_a_migration_category() {
        let root = tempfile::tempdir().unwrap();
        let plan = plan(
            root.path(),
            MigrationSelection {
                configuration: true,
                ..MigrationSelection::default()
            },
        );
        fs::create_dir_all(&plan.source_config_dir).unwrap();
        fs::create_dir_all(plan.source_user_dir.join("shader")).unwrap();
        fs::write(plan.source_config_dir.join("qt-config.ini"), b"settings").unwrap();
        fs::write(plan.source_user_dir.join("shader/cache.bin"), b"cache").unwrap();

        process(&plan).unwrap();

        assert!(!root.path().join("target/shader").exists());
        assert!(plan.source_user_dir.join("shader/cache.bin").exists());
    }
}

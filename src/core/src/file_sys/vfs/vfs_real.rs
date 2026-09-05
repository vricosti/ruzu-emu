// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/core/file_sys/vfs/vfs_real.h and vfs_real.cpp
//! RealVfsFilesystem, RealVfsFile, RealVfsDirectory: VFS backed by the real filesystem.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};

use common::fs::file::{IOFile, SeekOrigin};
use common::fs::fs as fs_ops;
use common::fs::fs_types::{FileAccessMode, FileShareFlag, FileType};
use common::fs::path_util;

use super::vfs::{VfsDirectory, VfsEntryType, VfsFile, VfsFilesystem};
use super::vfs_types::{FileTimeStampRaw, VirtualDir, VirtualFile};

#[cfg(windows)]
fn windows_file_time_to_unix_seconds(file_time: u64) -> u64 {
    const WINDOWS_TO_UNIX_EPOCH_100NS: i128 = 116_444_736_000_000_000;
    const TICKS_PER_SECOND: i128 = 10_000_000;
    let seconds = (i128::from(file_time) - WINDOWS_TO_UNIX_EPOCH_100NS) / TICKS_PER_SECOND;
    seconds as i64 as u64
}
use crate::file_sys::fs_filesystem::OpenMode;

// ============================================================================
// Constants
// ============================================================================

/// Maximum number of concurrently open file handles.
///
/// Maps to upstream `MaxOpenFiles`.
const MAX_OPEN_FILES: usize = 8192;

fn is_within_root(root: &str, full_path: &str) -> bool {
    root.is_empty()
        || (full_path.starts_with(root)
            && (full_path.len() == root.len()
                || full_path.as_bytes().get(root.len()) == Some(&b'/')
                || full_path.as_bytes().get(root.len()) == Some(&b'\\')))
}

// ============================================================================
// Helper: convert OpenMode to FileAccessMode
// ============================================================================

/// Maps to upstream `ModeFlagsToFileAccessMode`.
fn mode_flags_to_file_access_mode(mode: OpenMode) -> FileAccessMode {
    if mode == OpenMode::READ {
        FileAccessMode::Read
    } else {
        FileAccessMode::ReadWrite
    }
}

// ============================================================================
// FileReference
// ============================================================================

/// Tracks an open file handle with reference counting.
///
/// Maps to upstream `FileReference` (intrusive list node).
struct FileReference {
    file: Option<IOFile>,
}

struct RealVfsFilesystemState {
    cache: BTreeMap<String, Weak<dyn VfsFile>>,
    references: BTreeMap<u64, FileReference>,
    open_references: VecDeque<u64>,
    closed_references: VecDeque<u64>,
    num_open_files: usize,
    next_reference_id: u64,
}

impl RealVfsFilesystemState {
    fn new() -> Self {
        Self {
            cache: BTreeMap::new(),
            references: BTreeMap::new(),
            open_references: VecDeque::new(),
            closed_references: VecDeque::new(),
            num_open_files: 0,
            next_reference_id: 0,
        }
    }

    fn insert_reference(&mut self) -> u64 {
        let id = self.next_reference_id;
        self.next_reference_id = self.next_reference_id.wrapping_add(1);
        self.references.insert(id, FileReference::new());
        self.closed_references.push_front(id);
        id
    }

    fn remove_reference_from_lists(&mut self, id: u64) {
        self.open_references.retain(|candidate| *candidate != id);
        self.closed_references.retain(|candidate| *candidate != id);
    }

    fn evict_single_reference(&mut self) {
        if self.num_open_files < MAX_OPEN_FILES || self.open_references.is_empty() {
            return;
        }
        let id = self.open_references.pop_back().unwrap();
        if let Some(reference) = self.references.get_mut(&id) {
            if reference.file.take().is_some() {
                self.num_open_files -= 1;
            }
            self.closed_references.push_front(id);
        }
    }

    fn drop_reference(&mut self, id: u64) {
        self.remove_reference_from_lists(id);
        if self
            .references
            .remove(&id)
            .and_then(|reference| reference.file)
            .is_some()
        {
            self.num_open_files -= 1;
        }
    }
}

impl FileReference {
    fn new() -> Self {
        Self { file: None }
    }
}

// ============================================================================
// RealVfsFilesystem
// ============================================================================

/// VFS implementation backed by the real filesystem. Manages a cache of open
/// file handles with LRU eviction.
///
/// Maps to upstream `RealVfsFilesystem`.
pub struct RealVfsFilesystem {
    state: Mutex<RealVfsFilesystemState>,
    self_weak: Weak<RealVfsFilesystem>,
}

impl RealVfsFilesystem {
    pub fn new() -> Arc<Self> {
        Arc::new_cyclic(|self_weak| Self {
            state: Mutex::new(RealVfsFilesystemState::new()),
            self_weak: self_weak.clone(),
        })
    }

    /// Opens a file, using the cache if possible.
    ///
    /// Maps to upstream `RealVfsFilesystem::OpenFileFromEntry`.
    fn open_file_from_entry(
        self: &Arc<Self>,
        path: &str,
        size: Option<u64>,
        parent_path: Option<String>,
        perms: OpenMode,
    ) -> Option<VirtualFile> {
        let sanitized =
            path_util::sanitize_path(path, path_util::DirectorySeparator::PlatformDefault);

        let mut state = self.state.lock().unwrap();
        if let Some(weak) = state.cache.get(&sanitized) {
            if let Some(file) = weak.upgrade() {
                return Some(file);
            }
        }

        if size.is_none() && !fs_ops::is_file(Path::new(&sanitized)) {
            return None;
        }

        let reference_id = state.insert_reference();
        let file: Arc<dyn VfsFile> = Arc::new(RealVfsFile::new(
            Arc::clone(self),
            reference_id,
            sanitized.clone(),
            perms,
            size,
            parent_path.unwrap_or_else(|| path_util::get_parent_path(&sanitized)),
        ));

        state.cache.insert(sanitized, Arc::downgrade(&file));

        Some(file)
    }

    fn with_reference<R>(
        &self,
        reference_id: u64,
        path: &str,
        perms: OpenMode,
        operation: impl FnOnce(&mut IOFile) -> R,
    ) -> Option<R> {
        let mut state = self.state.lock().unwrap();
        state.remove_reference_from_lists(reference_id);

        let needs_open = state
            .references
            .get(&reference_id)
            .is_none_or(|reference| reference.file.is_none());
        if needs_open {
            state.evict_single_reference();
            let file = IOFile::new(
                Path::new(path),
                mode_flags_to_file_access_mode(perms),
                FileType::BinaryFile,
                FileShareFlag::ShareReadOnly,
            );
            if file.is_open() {
                state.references.get_mut(&reference_id)?.file = Some(file);
                state.num_open_files += 1;
            }
        }

        let is_open = state.references.get(&reference_id)?.file.is_some();
        if is_open {
            state.open_references.push_front(reference_id);
        } else {
            state.closed_references.push_front(reference_id);
        }
        state
            .references
            .get_mut(&reference_id)?
            .file
            .as_mut()
            .map(operation)
    }
}

impl VfsFilesystem for RealVfsFilesystem {
    fn get_name(&self) -> String {
        "Real".to_string()
    }

    fn is_readable(&self) -> bool {
        true
    }

    fn is_writable(&self) -> bool {
        true
    }

    fn get_entry_type(&self, path: &str) -> VfsEntryType {
        let sanitized =
            path_util::sanitize_path(path, path_util::DirectorySeparator::PlatformDefault);
        let p = Path::new(&sanitized);
        if !fs_ops::exists(p) {
            return VfsEntryType::None;
        }
        if fs_ops::is_dir(p) {
            return VfsEntryType::Directory;
        }
        VfsEntryType::File
    }

    fn open_file(&self, path: &str, perms: OpenMode) -> Option<VirtualFile> {
        self.self_weak.upgrade()?.arc_open_file(path, perms)
    }

    fn create_file(&self, path: &str, perms: OpenMode) -> Option<VirtualFile> {
        self.self_weak.upgrade()?.arc_create_file(path, perms)
    }

    fn copy_file(&self, _old_path: &str, _new_path: &str) -> Option<VirtualFile> {
        // Unused in upstream
        None
    }

    fn move_file(&self, old_path: &str, new_path: &str) -> Option<VirtualFile> {
        self.self_weak.upgrade()?.arc_move_file(old_path, new_path)
    }

    fn delete_file(&self, path: &str) -> bool {
        let sanitized =
            path_util::sanitize_path(path, path_util::DirectorySeparator::PlatformDefault);
        {
            self.state.lock().unwrap().cache.remove(&sanitized);
        }
        fs_ops::remove_file(Path::new(&sanitized))
    }

    fn open_directory(&self, path: &str, perms: OpenMode) -> Option<VirtualDir> {
        self.self_weak.upgrade()?.arc_open_directory(path, perms)
    }

    fn create_directory(&self, path: &str, perms: OpenMode) -> Option<VirtualDir> {
        let sanitized =
            path_util::sanitize_path(path, path_util::DirectorySeparator::PlatformDefault);
        self.self_weak
            .upgrade()?
            .arc_create_directory(&sanitized, perms)
    }

    fn copy_directory(&self, _old_path: &str, _new_path: &str) -> Option<VirtualDir> {
        // Unused in upstream
        None
    }

    fn move_directory(&self, old_path: &str, new_path: &str) -> Option<VirtualDir> {
        let old =
            path_util::sanitize_path(old_path, path_util::DirectorySeparator::PlatformDefault);
        let new =
            path_util::sanitize_path(new_path, path_util::DirectorySeparator::PlatformDefault);
        if !fs_ops::rename_dir(Path::new(&old), Path::new(&new)) {
            return None;
        }
        self.self_weak
            .upgrade()?
            .arc_open_directory(&new, OpenMode::READ_WRITE)
    }

    fn delete_directory(&self, path: &str) -> bool {
        let sanitized =
            path_util::sanitize_path(path, path_util::DirectorySeparator::PlatformDefault);
        fs_ops::remove_dir_recursively(Path::new(&sanitized))
    }
}

/// Arc-based API for RealVfsFilesystem, providing the full set of operations.
///
/// These methods require Arc<Self> because they create RealVfsFile/RealVfsDirectory instances
/// that hold a reference back to the filesystem.
impl RealVfsFilesystem {
    pub fn arc_open_file(self: &Arc<Self>, path: &str, perms: OpenMode) -> Option<VirtualFile> {
        self.open_file_from_entry(path, None, None, perms)
    }

    pub fn arc_create_file(self: &Arc<Self>, path: &str, perms: OpenMode) -> Option<VirtualFile> {
        let sanitized =
            path_util::sanitize_path(path, path_util::DirectorySeparator::PlatformDefault);
        {
            self.state.lock().unwrap().cache.remove(&sanitized);
        }

        let p = Path::new(&sanitized);

        // Current usages of CreateFile expect to delete the contents of an existing file.
        if fs_ops::is_file(p) {
            let temp = IOFile::new(
                p,
                FileAccessMode::Write,
                FileType::BinaryFile,
                FileShareFlag::ShareReadOnly,
            );
            if !temp.is_open() {
                return None;
            }
            drop(temp);
            return self.arc_open_file(&sanitized, perms);
        }

        if !fs_ops::new_file(p, 0) {
            return None;
        }
        self.arc_open_file(&sanitized, perms)
    }

    pub fn arc_move_file(self: &Arc<Self>, old_path: &str, new_path: &str) -> Option<VirtualFile> {
        let old =
            path_util::sanitize_path(old_path, path_util::DirectorySeparator::PlatformDefault);
        let new =
            path_util::sanitize_path(new_path, path_util::DirectorySeparator::PlatformDefault);
        {
            let mut state = self.state.lock().unwrap();
            state.cache.remove(&old);
            state.cache.remove(&new);
        }
        if !fs_ops::rename_file(Path::new(&old), Path::new(&new)) {
            return None;
        }
        self.arc_open_file(&new, OpenMode::READ_WRITE)
    }

    pub fn arc_open_directory(self: &Arc<Self>, path: &str, perms: OpenMode) -> Option<VirtualDir> {
        let sanitized =
            path_util::sanitize_path(path, path_util::DirectorySeparator::PlatformDefault);
        Some(Arc::new(RealVfsDirectory::new(
            Arc::clone(self),
            sanitized,
            perms,
        )))
    }

    pub fn arc_create_directory(
        self: &Arc<Self>,
        path: &str,
        perms: OpenMode,
    ) -> Option<VirtualDir> {
        let sanitized =
            path_util::sanitize_path(path, path_util::DirectorySeparator::PlatformDefault);
        if !fs_ops::create_dirs(Path::new(&sanitized)) {
            return None;
        }
        Some(Arc::new(RealVfsDirectory::new(
            Arc::clone(self),
            sanitized,
            perms,
        )))
    }
}

// ============================================================================
// RealVfsFile
// ============================================================================

/// An implementation of VfsFile that represents a file on the user's computer.
///
/// Maps to upstream `RealVfsFile`.
pub struct RealVfsFile {
    base: Arc<RealVfsFilesystem>,
    reference_id: u64,
    path: String,
    parent_path: String,
    path_components: Vec<String>,
    size: Mutex<Option<u64>>,
    perms: OpenMode,
}

impl RealVfsFile {
    fn new(
        base: Arc<RealVfsFilesystem>,
        reference_id: u64,
        path: String,
        perms: OpenMode,
        size: Option<u64>,
        parent_path: String,
    ) -> Self {
        let path_components = path_util::split_path_components_copy(&path);
        Self {
            base,
            reference_id,
            path,
            parent_path,
            path_components,
            size: Mutex::new(size),
            perms,
        }
    }
}

impl Drop for RealVfsFile {
    fn drop(&mut self) {
        self.base
            .state
            .lock()
            .unwrap()
            .drop_reference(self.reference_id);
    }
}

impl VfsFile for RealVfsFile {
    fn get_name(&self) -> String {
        if self.path_components.is_empty() {
            String::new()
        } else {
            self.path_components.last().unwrap().clone()
        }
    }

    fn get_size(&self) -> usize {
        let size_opt = *self.size.lock().unwrap();
        if let Some(s) = size_opt {
            return s as usize;
        }

        self.base
            .with_reference(self.reference_id, &self.path, self.perms, |file| {
                file.get_size() as usize
            })
            .unwrap_or(0)
    }

    fn resize(&self, new_size: usize) -> bool {
        *self.size.lock().unwrap() = None;
        self.base
            .with_reference(self.reference_id, &self.path, self.perms, |file| {
                file.set_size(new_size as u64)
            })
            .unwrap_or(false)
    }

    fn get_containing_directory(&self) -> Option<VirtualDir> {
        self.base.arc_open_directory(&self.parent_path, self.perms)
    }

    fn is_writable(&self) -> bool {
        self.perms.contains(OpenMode::WRITE)
    }

    fn is_readable(&self) -> bool {
        self.perms.contains(OpenMode::READ)
    }

    fn read(&self, data: &mut [u8], length: usize, offset: usize) -> usize {
        self.base
            .with_reference(self.reference_id, &self.path, self.perms, |file| {
                if !file.seek(offset as i64, SeekOrigin::SetOrigin) {
                    return 0;
                }
                file.read_bytes(&mut data[..length])
            })
            .unwrap_or(0)
    }

    fn write(&self, data: &[u8], length: usize, offset: usize) -> usize {
        *self.size.lock().unwrap() = None;
        self.base
            .with_reference(self.reference_id, &self.path, self.perms, |file| {
                if !file.seek(offset as i64, SeekOrigin::SetOrigin) {
                    return 0;
                }
                file.write_bytes(&data[..length])
            })
            .unwrap_or(0)
    }

    fn rename(&self, name: &str) -> bool {
        self.base
            .arc_move_file(&self.path, &format!("{}/{}", self.parent_path, name))
            .is_some()
    }
}

// ============================================================================
// RealVfsDirectory
// ============================================================================

/// An implementation of VfsDirectory that represents a directory on the user's computer.
///
/// Maps to upstream `RealVfsDirectory`.
pub struct RealVfsDirectory {
    base: Arc<RealVfsFilesystem>,
    path: String,
    parent_path: String,
    path_components: Vec<String>,
    perms: OpenMode,
}

impl RealVfsDirectory {
    pub fn new(base: Arc<RealVfsFilesystem>, path: String, perms: OpenMode) -> Self {
        let cleaned = path_util::remove_trailing_slash(&path).to_string();
        let parent_path = path_util::get_parent_path(&cleaned);
        let path_components = path_util::split_path_components_copy(&cleaned);

        if !fs_ops::exists(Path::new(&cleaned)) && perms.contains(OpenMode::WRITE) {
            let _ = fs_ops::create_dirs(Path::new(&cleaned));
        }

        Self {
            base,
            path: cleaned,
            parent_path,
            path_components,
            perms,
        }
    }
}

impl VfsDirectory for RealVfsDirectory {
    fn get_file_relative(&self, relative_path: &str) -> Option<VirtualFile> {
        let full_path = path_util::sanitize_path(
            &format!("{}/{}", self.path, relative_path),
            path_util::DirectorySeparator::PlatformDefault,
        );
        let p = Path::new(&full_path);
        let root =
            path_util::sanitize_path(&self.path, path_util::DirectorySeparator::PlatformDefault);
        if !fs_ops::exists(p) || fs_ops::is_dir(p) || !is_within_root(&root, &full_path) {
            return None;
        }
        self.base.arc_open_file(&full_path, self.perms)
    }

    fn get_directory_relative(&self, relative_path: &str) -> Option<VirtualDir> {
        let full_path = path_util::sanitize_path(
            &format!("{}/{}", self.path, relative_path),
            path_util::DirectorySeparator::PlatformDefault,
        );
        let p = Path::new(&full_path);
        let root =
            path_util::sanitize_path(&self.path, path_util::DirectorySeparator::PlatformDefault);
        if !fs_ops::exists(p) || !fs_ops::is_dir(p) || !is_within_root(&root, &full_path) {
            return None;
        }
        self.base.arc_open_directory(&full_path, self.perms)
    }

    fn get_file(&self, name: &str) -> Option<VirtualFile> {
        self.get_file_relative(name)
    }

    fn get_subdirectory(&self, name: &str) -> Option<VirtualDir> {
        self.get_directory_relative(name)
    }

    fn create_file_relative(&self, relative_path: &str) -> Option<VirtualFile> {
        let full_path = path_util::sanitize_path(
            &format!("{}/{}", self.path, relative_path),
            path_util::DirectorySeparator::PlatformDefault,
        );
        let root =
            path_util::sanitize_path(&self.path, path_util::DirectorySeparator::PlatformDefault);
        if !fs_ops::create_parent_dirs(Path::new(&full_path)) || !is_within_root(&root, &full_path)
        {
            return None;
        }
        self.base.arc_create_file(&full_path, self.perms)
    }

    fn create_directory_relative(&self, relative_path: &str) -> Option<VirtualDir> {
        let full_path = path_util::sanitize_path(
            &format!("{}/{}", self.path, relative_path),
            path_util::DirectorySeparator::PlatformDefault,
        );
        self.base.arc_create_directory(&full_path, self.perms)
    }

    fn delete_subdirectory_recursive(&self, name: &str) -> bool {
        let full_path = path_util::sanitize_path(
            &format!("{}/{}", self.path, name),
            path_util::DirectorySeparator::PlatformDefault,
        );
        self.base.delete_directory(&full_path)
    }

    fn get_files(&self) -> Vec<VirtualFile> {
        if self.perms == OpenMode::ALLOW_APPEND {
            return Vec::new();
        }

        let mut out = Vec::new();
        let entries = match std::fs::read_dir(&self.path) {
            Ok(e) => e,
            Err(_) => return out,
        };

        for entry_result in entries {
            let entry = match entry_result {
                Ok(e) => e,
                Err(_) => continue,
            };

            let entry_path = entry.path();
            if entry_path.is_file() {
                let full = entry_path.to_string_lossy().to_string();
                let size = fs_ops::get_size(&entry_path);
                if let Some(file) = self.base.open_file_from_entry(
                    &full,
                    Some(size),
                    Some(self.path.clone()),
                    self.perms,
                ) {
                    out.push(file);
                }
            }
        }

        out
    }

    fn get_file_time_stamp(&self, path: &str) -> FileTimeStampRaw {
        let full_path = path_util::sanitize_path(
            &format!("{}/{}", self.path, path),
            path_util::DirectorySeparator::PlatformDefault,
        );

        #[cfg(unix)]
        {
            use std::ffi::CString;

            let c_path = match CString::new(full_path.as_bytes()) {
                Ok(p) => p,
                Err(_) => return FileTimeStampRaw::default(),
            };

            unsafe {
                let mut stat: libc::stat = std::mem::zeroed();
                if libc::stat(c_path.as_ptr(), &mut stat) != 0 {
                    return FileTimeStampRaw::default();
                }

                FileTimeStampRaw {
                    created: stat.st_ctime as u64,
                    accessed: stat.st_atime as u64,
                    modified: stat.st_mtime as u64,
                    padding: 0,
                }
            }
        }

        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;

            let metadata = match std::fs::metadata(&full_path) {
                Ok(metadata) => metadata,
                Err(_) => return FileTimeStampRaw::default(),
            };
            FileTimeStampRaw {
                created: windows_file_time_to_unix_seconds(metadata.creation_time()),
                accessed: windows_file_time_to_unix_seconds(metadata.last_access_time()),
                modified: windows_file_time_to_unix_seconds(metadata.last_write_time()),
                padding: 0,
            }
        }

        #[cfg(not(any(unix, windows)))]
        {
            let _ = full_path;
            FileTimeStampRaw::default()
        }
    }

    fn get_subdirectories(&self) -> Vec<VirtualDir> {
        if self.perms == OpenMode::ALLOW_APPEND {
            return Vec::new();
        }

        let mut out = Vec::new();
        let entries = match std::fs::read_dir(&self.path) {
            Ok(e) => e,
            Err(_) => return out,
        };

        for entry_result in entries {
            let entry = match entry_result {
                Ok(e) => e,
                Err(_) => continue,
            };

            let entry_path = entry.path();
            if entry_path.is_dir() {
                let full = entry_path.to_string_lossy().to_string();
                if let Some(dir) = self.base.arc_open_directory(&full, self.perms) {
                    out.push(dir);
                }
            }
        }

        out
    }

    fn is_writable(&self) -> bool {
        self.perms.contains(OpenMode::WRITE)
    }

    fn is_readable(&self) -> bool {
        self.perms.contains(OpenMode::READ)
    }

    fn get_name(&self) -> String {
        if self.path_components.is_empty() {
            String::new()
        } else {
            self.path_components.last().unwrap().clone()
        }
    }

    fn get_parent_directory(&self) -> Option<VirtualDir> {
        if self.path_components.len() <= 1 {
            return None;
        }
        self.base.arc_open_directory(&self.parent_path, self.perms)
    }

    fn create_subdirectory(&self, name: &str) -> Option<VirtualDir> {
        let subdir_path = format!("{}/{}", self.path, name);
        self.base.arc_create_directory(&subdir_path, self.perms)
    }

    fn create_file(&self, name: &str) -> Option<VirtualFile> {
        let file_path = format!("{}/{}", self.path, name);
        self.base.arc_create_file(&file_path, self.perms)
    }

    fn delete_subdirectory(&self, name: &str) -> bool {
        let subdir_path = format!("{}/{}", self.path, name);
        self.base.delete_directory(&subdir_path)
    }

    fn delete_file(&self, name: &str) -> bool {
        let file_path = format!("{}/{}", self.path, name);
        self.base.delete_file(&file_path)
    }

    fn rename(&self, name: &str) -> bool {
        let new_name = format!("{}/{}", self.parent_path, name);
        self.base.arc_move_file(&self.path, &new_name).is_some()
    }

    fn get_full_path(&self) -> String {
        self.path.replace('\\', "/")
    }

    fn get_entries(&self) -> BTreeMap<String, VfsEntryType> {
        if self.perms == OpenMode::ALLOW_APPEND {
            return BTreeMap::new();
        }

        let mut out = BTreeMap::new();
        let entries = match std::fs::read_dir(&self.path) {
            Ok(e) => e,
            Err(_) => return out,
        };

        for entry_result in entries {
            let entry = match entry_result {
                Ok(e) => e,
                Err(_) => continue,
            };

            let entry_path = entry.path();
            if let Some(filename) = entry_path.file_name() {
                let name = filename.to_string_lossy().to_string();
                let entry_type = if entry_path.is_dir() {
                    VfsEntryType::Directory
                } else {
                    VfsEntryType::File
                };
                out.insert(name, entry_type);
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_directory() -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("ruzu-vfs-real-{}-{unique}", std::process::id()))
    }

    #[test]
    fn filesystem_trait_reuses_and_drops_file_reference() {
        let directory = test_directory();
        std::fs::create_dir_all(&directory).unwrap();
        let file_path = directory.join("homebrew.bin");
        std::fs::write(&file_path, b"free-data").unwrap();

        let filesystem = RealVfsFilesystem::new();
        let file = VfsFilesystem::open_file(
            filesystem.as_ref(),
            &file_path.to_string_lossy(),
            OpenMode::READ,
        )
        .unwrap();
        let mut first = [0; 4];
        let mut second = [0; 4];
        assert_eq!(file.read(&mut first, 4, 0), 4);
        assert_eq!(file.read(&mut second, 4, 4), 4);
        assert_eq!(&first, b"free");
        assert_eq!(&second, b"-dat");

        {
            let state = filesystem.state.lock().unwrap();
            assert_eq!(state.num_open_files, 1);
            assert_eq!(state.open_references.len(), 1);
            assert!(state.closed_references.is_empty());
        }

        drop(file);
        {
            let state = filesystem.state.lock().unwrap();
            assert_eq!(state.num_open_files, 0);
            assert!(state.references.is_empty());
            assert!(state.open_references.is_empty());
        }

        std::fs::remove_file(file_path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn relative_lookup_cannot_escape_directory_root() {
        let directory = test_directory();
        let root = directory.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let inside = root.join("inside.bin");
        std::fs::write(&inside, b"inside").unwrap();
        let outside = directory.join("outside.bin");
        std::fs::write(&outside, b"outside").unwrap();

        let filesystem = RealVfsFilesystem::new();
        let root_directory = RealVfsDirectory::new(
            filesystem,
            root.to_string_lossy().into_owned(),
            OpenMode::READ,
        );
        assert!(root_directory.get_file_relative("inside.bin").is_some());
        assert!(root_directory.get_file_relative("../outside.bin").is_none());

        std::fs::remove_file(inside).unwrap();
        std::fs::remove_file(outside).unwrap();
        std::fs::remove_dir(root).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_file_time_conversion_matches_unix_epoch_seconds() {
        const EPOCH: u64 = 116_444_736_000_000_000;
        assert_eq!(windows_file_time_to_unix_seconds(EPOCH), 0);
        assert_eq!(windows_file_time_to_unix_seconds(EPOCH + 30_000_000), 3);
        assert_eq!(
            windows_file_time_to_unix_seconds(EPOCH - 10_000_000),
            u64::MAX
        );
    }
}

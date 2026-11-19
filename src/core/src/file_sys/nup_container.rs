// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Ported from: Fritchii (Eden) core/file_sys/nup_container.h and
// nup_container.cpp — the NUP (NX Package) container used by Switch 2 dumps.
//
// A NUP stores loose ExeFS/RomFS trees for a base program, its updates and
// add-on content in one file:
//
//   Superblock (0x80 bytes, magic "NUP\x01")
//   Table of Contents (packages, dir/file nodes, children array, string pool)
//   Blob region (per-file extents, raw or zstd codec, 16-byte aligned)
//
// Per-package meta TLVs carry NPDM bytes, title names (default + localized),
// icons and a display version. All integers little-endian.

use std::collections::{HashMap, VecDeque};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

use super::vfs::vfs::VfsFile;
use super::vfs::vfs_offset::OffsetVfsFile;
use super::vfs::vfs_types::{VirtualDir, VirtualFile};
use super::vfs::vfs_vector::{VectorVfsDirectory, VectorVfsFile};
use crate::loader::loader::ResultStatus;

// ── Layout constants ────────────────────────────────────────────────────────

const NO_DIR: u32 = 0xFFFFFFFF;
const SUPERBLOCK_SIZE: usize = 0x80;
const PACKAGE_RECORD_SIZE: usize = 0x40;
const DIR_RECORD_SIZE: usize = 0x18;
const FILE_RECORD_SIZE: usize = 0x50;
const TOC_HEADER_SIZE: usize = 0x30;
const TLV_HEADER_SIZE: usize = 0x8;
const TOC_MAGIC: u32 = 0x01434F54; // "TOC\x01" little-endian
const CODEC_RAW: u8 = 0;
const CODEC_ZSTD: u8 = 1;
const PACKAGE_FLAG_STANDALONE_UPDATE: u8 = 0x01;
const MAX_DIR_DEPTH: u32 = 128;

/// Magic in the superblock, `"NUP\x01"`.
///
/// Maps to upstream `NupContainer::HeaderMagic`.
pub const HEADER_MAGIC: [u8; 4] = [b'N', b'U', b'P', 0x01];

// ── Small little-endian readers ─────────────────────────────────────────────

fn read_u16_le(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn read_u64_le(data: &[u8], offset: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&data[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}

fn sha256(data: &[u8]) -> [u8; 0x20] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

// ── Metadata TLV types ──────────────────────────────────────────────────────

/// Per-package meta TLV payload types. Maps to upstream `MetaType`.
enum MetaType {
    Npdm = 0x0001,
    Icon = 0x0003,
    DisplayVersion = 0x0004,
    TitleName = 0x0005,
    Publisher = 0x0006,
    SourceDump = 0x0008,
    LocalizedTitles = 0x0009,
    LocalizedIcons = 0x000A,
}

// ── Package kind / info ─────────────────────────────────────────────────────

/// Maps to upstream `NupContainer::PackageKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PackageKind {
    Base = 0,
    Update = 1,
    Addon = 2,
}

impl Default for PackageKind {
    fn default() -> Self {
        Self::Base
    }
}

/// Maps to upstream `NupContainer::PackageInfo`.
#[derive(Debug, Default, Clone)]
pub struct PackageInfo {
    pub title_id: u64,
    pub version: u32,
    pub kind: PackageKind,
    pub standalone_update: bool,
    pub root_dir: u32,
    pub exefs_dir: u32,
    pub romfs_dir: u32,
    pub meta_offset: u64,
    pub meta_size: u32,

    pub npdm: Vec<u8>,
    pub title_name: String,
    pub publisher: String,
    pub source_dump: String,
    pub localized_titles: Vec<(String, String)>,
    pub icon: Vec<u8>,
    pub localized_icons: Vec<(String, Vec<u8>)>,
    pub display_version: String,
}

// ── TOC nodes ───────────────────────────────────────────────────────────────

/// Maps to upstream `NupContainer::DirNode`.
#[derive(Debug, Default, Clone)]
struct DirNode {
    name: String,
    first_child: u32,
    child_count: u32,
}

/// Maps to upstream `NupContainer::FileNode`.
#[derive(Debug, Default, Clone)]
struct FileNode {
    name: String,
    size: u64,
    sha256: [u8; 0x20],
    extent_offset: u64,
    extent_size: u64,
    codec: u8,
}

// ── Decompression cache ─────────────────────────────────────────────────────

/// Default cap for the shared LRU over decompressed zstd extents.
///
/// Maps to upstream `NupDecompressCache::DefaultCap`.
pub const DEFAULT_CACHE_CAP: usize = 512 * 1024 * 1024;

/// A zstd member too large for the in-memory cache, decompressed once into a
/// temporary file.
///
/// Maps to upstream `NupSpillFile`.
struct NupSpillFile {
    file: Mutex<Option<std::fs::File>>,
    attempted: bool,
}

/// LRU over decompressed zstd extents, shared by the files of one container.
///
/// Maps to upstream `NupDecompressCache`.
///
/// Rust adaptation: the C++ list+iterator LRU becomes a deque of the most
/// recently used entries plus a map; spill files stay open on their handle
/// instead of being closed and reopened (Windows share-mode).
pub struct NupDecompressCache {
    inner: Mutex<CacheInner>,
    cap: usize,
    spill_dir: PathBuf,
}

struct CacheInner {
    // Back = most recently used.
    lru: VecDeque<(usize, Arc<Vec<u8>>)>,
    index: HashMap<usize, Arc<Vec<u8>>>,
    bytes: usize,
    spills: HashMap<usize, Arc<Mutex<NupSpillFile>>>,
}

impl NupDecompressCache {
    fn new(cap: usize, spill_dir: PathBuf) -> Self {
        Self {
            inner: Mutex::new(CacheInner {
                lru: VecDeque::new(),
                index: HashMap::new(),
                bytes: 0,
                spills: HashMap::new(),
            }),
            cap,
            spill_dir,
        }
    }

    /// Returns the cached decompressed bytes for `file_index`, marking the
    /// entry as most recently used. Maps to upstream `NupDecompressCache::Find`.
    fn find(&self, file_index: usize) -> Option<Arc<Vec<u8>>> {
        let mut inner = self.inner.lock().unwrap();
        let entry = inner.index.get(&file_index)?.clone();
        if let Some(pos) = inner.lru.iter().position(|(index, _)| *index == file_index) {
            if let Some(moved) = inner.lru.remove(pos) {
                inner.lru.push_back(moved);
            }
        }
        Some(entry)
    }

    /// Takes ownership of `data` only when it is inserted.
    /// Maps to upstream `NupDecompressCache::Insert`.
    fn insert(&self, file_index: usize, data: Arc<Vec<u8>>) {
        let mut inner = self.inner.lock().unwrap();
        if inner.index.contains_key(&file_index) {
            return;
        }
        let size = data.len();
        while inner.bytes + size > self.cap && !inner.lru.is_empty() {
            if let Some(evicted) = inner.lru.pop_front() {
                inner.bytes -= evicted.1.len();
                inner.index.remove(&evicted.0);
            }
        }
        if inner.bytes + size > self.cap {
            return;
        }
        let shared = data;
        inner.bytes += size;
        inner.index.insert(file_index, Arc::clone(&shared));
        inner.lru.push_back((file_index, shared));
    }

    /// Maps to upstream `NupDecompressCache::GetSpill`.
    fn get_spill(&self, file_index: usize) -> Arc<Mutex<NupSpillFile>> {
        let mut inner = self.inner.lock().unwrap();
        inner
            .spills
            .entry(file_index)
            .or_insert_with(|| {
                Arc::new(Mutex::new(NupSpillFile {
                    file: Mutex::new(None),
                    attempted: false,
                }))
            })
            .clone()
    }

    fn spill_dir(&self) -> &Path {
        &self.spill_dir
    }
}

/// Streaming VFS adapter for NupVfsFile::DecompressToFile's bounded input reads.
struct NupExtentReader<'a> {
    backing: &'a VirtualFile,
    offset: u64,
    remaining: u64,
}
impl std::io::Read for NupExtentReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let count = (out.len() as u64).min(self.remaining) as usize;
        if count == 0 {
            return Ok(0);
        }
        let read = self.backing.read(out, count, self.offset as usize);
        if read != count {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "short NUP extent read",
            ));
        }
        self.offset += read as u64;
        self.remaining -= read as u64;
        Ok(read)
    }
}

/// Decompresses a whole zstd extent with an exact output-size bound.
///
/// Maps to upstream `NupVfsFile::Decompress` (the in-memory path).
fn decompress_extent(
    backing: &VirtualFile,
    extent_offset: u64,
    extent_size: u64,
    uncompressed_size: u64,
    name: &str,
) -> Option<Vec<u8>> {
    let backing_size = backing.get_size() as u64;
    if extent_size > backing_size || extent_offset > backing_size - extent_size {
        return None;
    }
    let compressed = backing.read_bytes(extent_size as usize, extent_offset as usize);
    if compressed.len() != extent_size as usize {
        return None;
    }
    let mut reader = zstd::stream::Decoder::new(std::io::Cursor::new(&compressed)).ok()?;
    let mut out: Vec<u8> = Vec::with_capacity(uncompressed_size as usize);
    let mut buffer = [0u8; 65536];
    loop {
        let remaining = uncompressed_size as usize - out.len();
        if remaining == 0 {
            // The frame must now be exhausted; any further output overflows
            // the recorded size.
            let overflow = reader.read(&mut buffer).ok()?;
            if overflow != 0 {
                log::error!(
                    "NUP zstd decompression of {} overflows the recorded size",
                    name
                );
                return None;
            }
            break;
        }
        let want = remaining.min(buffer.len());
        let read = reader.read(&mut buffer[..want]).ok()?;
        if read == 0 {
            break;
        }
        out.extend_from_slice(&buffer[..read]);
    }
    if out.len() != uncompressed_size as usize {
        log::error!("NUP zstd decompression failed for {}", name);
        return None;
    }
    Some(out)
}

// ── NupVfsFile ──────────────────────────────────────────────────────────────

/// A member file whose extent is zstd-compressed. Decompresses on first read
/// and caches the result (LRU-evicted); a file over half the cache cap is
/// decompressed once to a temporary file.
///
/// Maps to upstream `NupVfsFile`.
struct NupVfsFile {
    backing: VirtualFile,
    extent_offset: u64,
    extent_size: u64,
    uncompressed_size: u64,
    name: Mutex<String>,
    cache: Arc<NupDecompressCache>,
    cache_index: usize,
    cache_enabled: bool,
}

impl NupVfsFile {
    /// Port of NupVfsFile::ReadSpilled; one decompression attempt per member.
    fn read_spilled(&self, data_out: &mut [u8], length: usize, offset: usize) -> usize {
        let spill = self.cache.get_spill(self.cache_index);
        let mut spill = spill.lock().unwrap();
        if !spill.attempted {
            spill.attempted = true;
            self.decompress_to_file(&mut spill);
        }
        let mut lock = spill.file.lock().unwrap();
        let Some(file) = lock.as_mut() else {
            return 0;
        };
        use std::io::{Read, Seek, SeekFrom};
        if file.seek(SeekFrom::Start(offset as u64)).is_err() {
            return 0;
        }
        file.read(&mut data_out[..length]).unwrap_or(0)
    }

    /// Port of NupVfsFile::DecompressToFile. Bounded input/output buffers.
    fn decompress_to_file(&self, spill: &mut NupSpillFile) {
        let backing = &self.backing;
        let extent_offset = self.extent_offset;
        let extent_size = self.extent_size;
        let uncompressed_size = self.uncompressed_size;
        let name = self.name.lock().unwrap().clone();
        let backing_size = backing.get_size() as u64;
        if extent_size > backing_size || extent_offset > backing_size - extent_size {
            return;
        }
        let _ = std::fs::create_dir_all(self.cache.spill_dir());
        let mut temp = match tempfile::Builder::new()
            .prefix("nup-")
            .suffix(".tmp")
            .tempfile_in(self.cache.spill_dir())
        {
            Ok(temp) => temp,
            Err(error) => {
                log::error!(
                    "NUP zstd decompression of {} to a temporary file failed: {}",
                    name,
                    error
                );
                return;
            }
        };
        // Fritchii DecompressToFile streams both sides; never materialize
        // the compressed or decompressed extent in RAM on the spill path.
        let result = (|| -> std::io::Result<()> {
            let source = NupExtentReader {
                backing,
                offset: extent_offset,
                remaining: extent_size,
            };
            let mut decoder = zstd::stream::Decoder::new(source)?.single_frame();
            let mut buffer = [0u8; 65536];
            let mut written = 0u64;
            loop {
                let count = decoder.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                if count as u64 > uncompressed_size - written {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "oversized zstd extent",
                    ));
                }
                temp.write_all(&buffer[..count])?;
                written += count as u64;
            }
            if written != uncompressed_size {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "zstd extent size mismatch",
                ));
            }
            temp.flush()
        })();
        if let Err(error) = result {
            log::error!(
                "NUP zstd decompression of {} to a temporary file failed: {}",
                name,
                error
            );
            return;
        }
        *spill.file.lock().unwrap() = Some(temp.into_file());
    }

    fn new(
        backing: VirtualFile,
        extent_offset: u64,
        extent_size: u64,
        uncompressed_size: u64,
        name: String,
        cache: Arc<NupDecompressCache>,
        cache_index: usize,
    ) -> Self {
        let cache_enabled = (uncompressed_size as usize) <= cache.cap / 2;
        Self {
            backing,
            extent_offset,
            extent_size,
            uncompressed_size,
            name: Mutex::new(name),
            cache,
            cache_index,
            cache_enabled,
        }
    }
}

impl VfsFile for NupVfsFile {
    fn get_name(&self) -> String {
        self.name.lock().unwrap().clone()
    }

    fn get_size(&self) -> usize {
        self.uncompressed_size as usize
    }

    fn resize(&self, _new_size: usize) -> bool {
        false
    }

    fn get_containing_directory(&self) -> Option<VirtualDir> {
        None
    }

    fn is_writable(&self) -> bool {
        false
    }

    fn is_readable(&self) -> bool {
        true
    }

    /// Maps to upstream `NupVfsFile::Read` (cache hit, then decompress-and-
    /// publish, then spill-to-temporary-file for oversized members).
    fn read(&self, data_out: &mut [u8], length: usize, offset: usize) -> usize {
        if offset as u64 >= self.uncompressed_size {
            return 0;
        }
        let length = length.min(self.uncompressed_size as usize - offset);

        if !self.cache_enabled {
            return self.read_spilled(data_out, length, offset);
        }

        if let Some(cached) = self.cache.find(self.cache_index) {
            data_out[..length].copy_from_slice(&cached[offset..offset + length]);
            return length;
        }

        // Miss: decompress without holding the lock, then publish and copy.
        let name = self.name.lock().unwrap().clone();
        let Some(scratch) = decompress_extent(
            &self.backing,
            self.extent_offset,
            self.extent_size,
            self.uncompressed_size,
            &name,
        ) else {
            return 0;
        };
        let scratch = Arc::new(scratch);
        self.cache.insert(self.cache_index, scratch.clone());
        let cached = self.cache.find(self.cache_index);
        match cached {
            Some(cached) => {
                data_out[..length].copy_from_slice(&cached[offset..offset + length]);
            }
            // The entry was rejected by the cache cap; serve from scratch.
            None => {
                data_out[..length].copy_from_slice(&scratch[offset..offset + length]);
            }
        }
        length
    }

    fn write(&self, _data_in: &[u8], _length: usize, _offset: usize) -> usize {
        0
    }

    fn rename(&self, new_name: &str) -> bool {
        *self.name.lock().unwrap() = new_name.to_string();
        true
    }
}

// ── NupContainer ────────────────────────────────────────────────────────────

/// NUP (NX Package) container reader.
///
/// Maps to upstream `FileSys::NupContainer`.
pub struct NupContainer {
    file: Option<VirtualFile>,
    status: ResultStatus,

    toc_offset: u64,
    toc_size: u64,
    blob_offset: u64,
    file_size: u64,

    dirs: Vec<DirNode>,
    files: Vec<FileNode>,
    children: Vec<u32>,
    packages: Vec<PackageInfo>,

    exefs_dir_cache: Mutex<Vec<Option<VirtualDir>>>,
    romfs_dir_cache: Mutex<Vec<Option<VirtualDir>>>,

    cache: Arc<NupDecompressCache>,
}

impl NupContainer {
    /// Quick magic check without a full parse.
    ///
    /// Maps to upstream `NupContainer::IsNupFile`.
    pub fn is_nup_file(file: &VirtualFile) -> bool {
        if file.get_size() < SUPERBLOCK_SIZE {
            return false;
        }
        let mut magic = [0u8; 4];
        file.read(&mut magic, 4, 0) == 4 && magic == HEADER_MAGIC
    }

    /// Creates a container with the default cache cap and the default spill
    /// directory (the ruzu cache directory).
    ///
    /// Maps to upstream `NupContainer::NupContainer(VirtualFile, ...)` with
    /// default arguments.
    pub fn new(file: Option<VirtualFile>) -> Self {
        let spill_dir =
            common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::CacheDir)
                .join("nup");
        Self::new_with_config(file, DEFAULT_CACHE_CAP, spill_dir)
    }

    /// Maps to the upstream constructor with explicit cache configuration.
    pub fn new_with_config(
        file: Option<VirtualFile>,
        memory_cache_cap: usize,
        spill_dir: PathBuf,
    ) -> Self {
        let cache = Arc::new(NupDecompressCache::new(memory_cache_cap, spill_dir));
        let mut this = Self {
            file,
            status: ResultStatus::ErrorNullFile,
            toc_offset: 0,
            toc_size: 0,
            blob_offset: 0,
            file_size: 0,
            dirs: Vec::new(),
            files: Vec::new(),
            children: Vec::new(),
            packages: Vec::new(),
            exefs_dir_cache: Mutex::new(Vec::new()),
            romfs_dir_cache: Mutex::new(Vec::new()),
            cache,
        };
        if this.file.is_none() {
            return this;
        }
        if this.parse() {
            this.status = ResultStatus::Success;
        } else if this.status == ResultStatus::ErrorNullFile {
            this.status = ResultStatus::ErrorNUPBadFormat;
        }
        this
    }

    pub fn get_status(&self) -> ResultStatus {
        self.status
    }

    pub fn get_packages(&self) -> &[PackageInfo] {
        &self.packages
    }

    pub fn get_file_count(&self) -> usize {
        self.files.len()
    }

    /// Maps to upstream `NupContainer::FindPackage`.
    pub fn find_package(&self, kind: PackageKind, title_id: Option<u64>) -> Option<usize> {
        self.packages.iter().position(|package| {
            package.kind == kind && title_id.is_none_or(|expected| package.title_id == expected)
        })
    }

    /// Maps to upstream `NupContainer::FindBasePackage`.
    pub fn find_base_package(&self) -> Option<usize> {
        self.find_package(PackageKind::Base, None)
    }

    /// Highest version among Update packages, if any.
    /// Maps to upstream `NupContainer::FindBestUpdatePackage`.
    pub fn find_best_update_package(&self) -> Option<usize> {
        let mut best: Option<usize> = None;
        for (i, package) in self.packages.iter().enumerate() {
            if package.kind != PackageKind::Update {
                continue;
            }
            if best.is_none_or(|b| package.version > self.packages[b].version) {
                best = Some(i);
            }
        }
        best
    }

    /// Opens a member file by node index; raw extents are zero-copy views into
    /// the container, zstd extents are lazily decompressed, cached in memory
    /// or, when large, in a temporary file.
    ///
    /// Maps to upstream `NupContainer::OpenFile`.
    pub fn open_file(&self, file_index: usize) -> Option<VirtualFile> {
        let node = self.files.get(file_index)?;
        if node.size == 0 {
            return Some(Arc::new(VectorVfsFile::new(
                Vec::new(),
                node.name.clone(),
                None,
            )));
        }
        if node.codec == CODEC_RAW {
            return Some(Arc::new(OffsetVfsFile::new(
                self.file.clone()?,
                node.extent_size as usize,
                node.extent_offset as usize,
                node.name.clone(),
            )));
        }
        Some(Arc::new(NupVfsFile::new(
            self.file.clone()?,
            node.extent_offset,
            node.extent_size,
            node.size,
            node.name.clone(),
            Arc::clone(&self.cache),
            file_index,
        )))
    }

    /// Maps to upstream `NupContainer::GetExeFSDir`.
    pub fn get_exefs_dir(&self, package_index: usize) -> Option<VirtualDir> {
        let package = self.packages.get(package_index)?;
        if package.exefs_dir == NO_DIR {
            return None;
        }
        let dir_index = package.exefs_dir;
        let mut cache = self.exefs_dir_cache.lock().unwrap();
        if cache.len() <= package_index {
            cache.resize(self.packages.len(), None);
        }
        if cache[package_index].is_none() {
            cache[package_index] = self.build_package_dir(dir_index);
        }
        cache[package_index].clone()
    }

    /// Maps to upstream `NupContainer::GetRomFSDir`.
    pub fn get_romfs_dir(&self, package_index: usize) -> Option<VirtualDir> {
        let package = self.packages.get(package_index)?;
        if package.romfs_dir == NO_DIR {
            return None;
        }
        let dir_index = package.romfs_dir;
        let mut cache = self.romfs_dir_cache.lock().unwrap();
        if cache.len() <= package_index {
            cache.resize(self.packages.len(), None);
        }
        if cache[package_index].is_none() {
            cache[package_index] = self.build_package_dir(dir_index);
        }
        cache[package_index].clone()
    }

    /// Maps to upstream `NupContainer::GetPackageDir`.
    pub fn get_package_dir(&self, package_index: usize) -> Option<VirtualDir> {
        let dir_index = self.packages.get(package_index)?.root_dir;
        self.build_package_dir(dir_index)
    }

    /// Hashes each complete member file against its stored SHA-256.
    ///
    /// Maps to upstream `NupContainer::VerifyIntegrity`.
    pub fn verify_integrity(
        &self,
        progress_callback: Option<&dyn Fn(usize, usize) -> bool>,
    ) -> ResultStatus {
        let total = self.files.len();
        for (i, node) in self.files.iter().enumerate() {
            let content = if node.size == 0 || node.codec == CODEC_RAW {
                self.read_extent_raw(node)
            } else {
                let Some(file) = &self.file else {
                    return ResultStatus::ErrorNullFile;
                };
                let view = NupVfsFile::new(
                    file.clone(),
                    node.extent_offset,
                    node.extent_size,
                    node.size,
                    node.name.clone(),
                    Arc::clone(&self.cache),
                    i,
                );
                view.read_all_bytes()
            };
            if content.len() as u64 != node.size {
                log::error!("NUP integrity check failed for {}: short read", node.name);
                return ResultStatus::ErrorIntegrityVerificationFailed;
            }
            if sha256(&content) != node.sha256 {
                log::error!(
                    "NUP integrity check failed for {}: SHA-256 mismatch",
                    node.name
                );
                return ResultStatus::ErrorIntegrityVerificationFailed;
            }
            if let Some(progress_callback) = progress_callback {
                if !progress_callback(i + 1, total) {
                    return ResultStatus::ErrorIntegrityVerificationFailed;
                }
            }
        }
        ResultStatus::Success
    }

    /// Recursively merges overlay into a copy of base; overlay paths win.
    ///
    /// Maps to upstream `NupContainer::MergeShadowDirs`.
    pub fn merge_shadow_dirs(
        base: Option<&VirtualDir>,
        overlay: Option<&VirtualDir>,
    ) -> Option<VirtualDir> {
        let Some(base) = base else {
            return overlay.cloned();
        };
        let Some(overlay) = overlay else {
            return Some(base.clone());
        };

        let mut file_map: std::collections::BTreeMap<String, VirtualFile> =
            std::collections::BTreeMap::new();
        for entry in base.get_files() {
            file_map.insert(entry.get_name(), entry);
        }
        for entry in overlay.get_files() {
            file_map.insert(entry.get_name(), entry);
        }

        let mut dir_map: std::collections::BTreeMap<String, VirtualDir> =
            std::collections::BTreeMap::new();
        for entry in base.get_subdirectories() {
            dir_map.insert(entry.get_name(), entry);
        }
        for entry in overlay.get_subdirectories() {
            let merged = match dir_map.get(entry.get_name().as_str()) {
                Some(existing) => Self::merge_shadow_dirs(Some(existing), Some(&entry)),
                None => Some(entry.clone()),
            };
            if let Some(merged) = merged {
                dir_map.insert(entry.get_name(), merged);
            }
        }

        let merged_files: Vec<VirtualFile> = file_map.into_values().collect();
        let merged_dirs: Vec<VirtualDir> = dir_map.into_values().collect();
        Some(Arc::new(VectorVfsDirectory::new(
            merged_files,
            merged_dirs,
            base.get_name(),
            None,
        )))
    }

    // ── Parsing ─────────────────────────────────────────────────────────────

    /// Maps to upstream `NupContainer::Parse`.
    fn parse(&mut self) -> bool {
        let Some(file) = &self.file else {
            return false;
        };
        if (file.get_size() as u64) < SUPERBLOCK_SIZE as u64 {
            self.status = ResultStatus::ErrorNUPBadFormat;
            return false;
        }
        let superblock = file.read_bytes(SUPERBLOCK_SIZE, 0);
        if superblock.len() != SUPERBLOCK_SIZE {
            self.status = ResultStatus::ErrorNUPBadFormat;
            return false;
        }
        if superblock[0..4] != HEADER_MAGIC
            || (read_u16_le(&superblock, 4) as usize) < SUPERBLOCK_SIZE
            || read_u32_le(&superblock, 0x4C) != 0
        {
            self.status = ResultStatus::ErrorNUPBadFormat;
            return false;
        }
        self.toc_offset = read_u64_le(&superblock, 0x08);
        self.toc_size = read_u64_le(&superblock, 0x10);
        self.blob_offset = read_u64_le(&superblock, 0x38);
        self.file_size = read_u64_le(&superblock, 0x40);
        let package_count = read_u32_le(&superblock, 0x48);
        if self.file_size != file.get_size() as u64
            || self.toc_offset < SUPERBLOCK_SIZE as u64
            || self.blob_offset > self.file_size
            || self.toc_offset > self.blob_offset
            || self.toc_size > self.blob_offset - self.toc_offset
            || self.blob_offset % 0x10 != 0
        {
            self.status = ResultStatus::ErrorNUPBadFormat;
            return false;
        }

        let toc = file.read_bytes(self.toc_size as usize, self.toc_offset as usize);
        if toc.len() as u64 != self.toc_size {
            self.status = ResultStatus::ErrorNUPBadFormat;
            return false;
        }
        if sha256(&toc)[..] != superblock[0x18..0x38] {
            log::error!("NUP TOC SHA-256 mismatch, the file is corrupt or was modified");
            self.status = ResultStatus::ErrorNUPBadFormat;
            return false;
        }

        if toc.len() < TOC_HEADER_SIZE {
            self.status = ResultStatus::ErrorNUPBadFormat;
            return false;
        }
        if read_u32_le(&toc, 0x00) != TOC_MAGIC
            || package_count == 0
            || read_u32_le(&toc, 0x04) != package_count
        {
            self.status = ResultStatus::ErrorNUPBadFormat;
            return false;
        }
        let dir_count = read_u64_le(&toc, 0x08);
        let file_count = read_u64_le(&toc, 0x10);
        let pool_offset = read_u64_le(&toc, 0x18);
        let pool_size = read_u64_le(&toc, 0x20);

        // The counts come from the file, so bound them by the TOC size before
        // multiplying.
        if dir_count > (toc.len() / DIR_RECORD_SIZE) as u64
            || file_count > (toc.len() / FILE_RECORD_SIZE) as u64
        {
            self.status = ResultStatus::ErrorNUPBadFormat;
            return false;
        }
        let needed = TOC_HEADER_SIZE as u64
            + package_count as u64 * PACKAGE_RECORD_SIZE as u64
            + dir_count * DIR_RECORD_SIZE as u64
            + file_count * FILE_RECORD_SIZE as u64;
        if needed > toc.len() as u64
            || pool_offset < needed
            || pool_offset > toc.len() as u64
            || pool_size > toc.len() as u64 - pool_offset
            || (pool_offset - needed) % std::mem::size_of::<u32>() as u64 != 0
        {
            self.status = ResultStatus::ErrorNUPBadFormat;
            return false;
        }
        let children_offset = needed;
        let children_bytes = pool_offset - needed;
        let children_count = (children_bytes / std::mem::size_of::<u32>() as u64) as usize;

        // Package records.
        let mut pos = TOC_HEADER_SIZE;
        self.packages.reserve(package_count as usize);
        for _ in 0..package_count {
            if pos + PACKAGE_RECORD_SIZE > toc.len() {
                self.status = ResultStatus::ErrorNUPBadFormat;
                return false;
            }
            let record = &toc[pos..pos + PACKAGE_RECORD_SIZE];
            pos += PACKAGE_RECORD_SIZE;
            let kind = record[0x18];
            let flags = record[0x19];
            let root_dir = read_u32_le(record, 0x08);
            let exefs_dir = read_u32_le(record, 0x0C);
            let romfs_dir = read_u32_le(record, 0x10);
            if kind > PackageKind::Addon as u8
                || (flags & !PACKAGE_FLAG_STANDALONE_UPDATE) != 0
                || root_dir as u64 >= dir_count
                || (exefs_dir != NO_DIR && exefs_dir as u64 >= dir_count)
                || (romfs_dir != NO_DIR && romfs_dir as u64 >= dir_count)
            {
                self.status = ResultStatus::ErrorNUPBadFormat;
                return false;
            }
            let meta_offset = read_u64_le(record, 0x20);
            let meta_size = read_u32_le(record, 0x28);
            let mut package = PackageInfo {
                title_id: read_u64_le(record, 0x00),
                version: read_u32_le(record, 0x14),
                kind: match kind {
                    0 => PackageKind::Base,
                    1 => PackageKind::Update,
                    _ => PackageKind::Addon,
                },
                standalone_update: (flags & PACKAGE_FLAG_STANDALONE_UPDATE) != 0,
                root_dir,
                exefs_dir,
                romfs_dir,
                meta_offset: 0,
                meta_size: 0,
                ..PackageInfo::default()
            };
            if meta_size > 0
                && meta_offset != 0
                && meta_offset <= self.file_size
                && (meta_size as u64) <= self.file_size - meta_offset
            {
                package.meta_offset = meta_offset;
                package.meta_size = meta_size;
            }
            self.packages.push(package);
        }

        // Directory records.
        self.dirs = vec![DirNode::default(); dir_count as usize];
        for node in self.dirs.iter_mut() {
            if pos + DIR_RECORD_SIZE > toc.len() {
                self.status = ResultStatus::ErrorNUPBadFormat;
                return false;
            }
            let record = &toc[pos..pos + DIR_RECORD_SIZE];
            pos += DIR_RECORD_SIZE;
            let name_offset = read_u32_le(record, 0x00) as u64;
            let name_length = read_u32_le(record, 0x04) as u64;
            if read_u32_le(record, 0x08) as u64 + read_u32_le(record, 0x0C) as u64
                > children_bytes / std::mem::size_of::<u32>() as u64
                || name_offset + name_length > pool_size
            {
                self.status = ResultStatus::ErrorNUPBadFormat;
                return false;
            }
            node.name = string_from_pool(&toc, pool_offset as usize, name_offset, name_length);
            node.first_child = read_u32_le(record, 0x08);
            node.child_count = read_u32_le(record, 0x0C);
        }

        // File records.
        self.files = vec![FileNode::default(); file_count as usize];
        for node in self.files.iter_mut() {
            if pos + FILE_RECORD_SIZE > toc.len() {
                self.status = ResultStatus::ErrorNUPBadFormat;
                return false;
            }
            let record = &toc[pos..pos + FILE_RECORD_SIZE];
            pos += FILE_RECORD_SIZE;
            let name_offset = read_u32_le(record, 0x00) as u64;
            let name_length = read_u32_le(record, 0x04) as u64;
            let size = read_u64_le(record, 0x08);
            let extent_offset = read_u64_le(record, 0x10);
            let extent_size = read_u64_le(record, 0x18);
            let codec = record[0x40];
            let codec_flags = record[0x41];
            if codec > CODEC_ZSTD
                || codec_flags != 0
                || (size == 0 && (extent_offset != 0 || extent_size != 0))
                || (size != 0
                    && (extent_size == 0
                        || extent_offset < self.blob_offset
                        || extent_offset % 0x10 != 0
                        || extent_offset > self.file_size
                        || extent_size > self.file_size - extent_offset))
                || name_offset + name_length > pool_size
            {
                self.status = ResultStatus::ErrorNUPBadFormat;
                return false;
            }
            node.name = string_from_pool(&toc, pool_offset as usize, name_offset, name_length);
            node.size = size;
            node.sha256.copy_from_slice(&record[0x20..0x40]);
            node.extent_offset = extent_offset;
            node.extent_size = extent_size;
            node.codec = codec;
        }

        // Children array.
        self.children = vec![0u32; children_count];
        let children_slice = &toc[children_offset as usize..pool_offset as usize];
        for (i, child) in self.children.iter_mut().enumerate() {
            *child = read_u32_le(children_slice, i * std::mem::size_of::<u32>());
            if *child as u64 >= dir_count + file_count {
                self.status = ResultStatus::ErrorNUPBadFormat;
                return false;
            }
        }

        if !self.validate_dir_tree() {
            self.status = ResultStatus::ErrorNUPBadFormat;
            return false;
        }

        for index in 0..self.packages.len() {
            if !self.parse_meta(index) {
                self.status = ResultStatus::ErrorNUPBadFormat;
                return false;
            }
        }
        true
    }

    /// Iterative cycle/depth validation of the directory tree.
    /// Maps to upstream `NupContainer::ValidateDirTree`.
    fn validate_dir_tree(&self) -> bool {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum State {
            Unvisited,
            InProgress,
            Done,
        }
        struct Frame {
            dir: usize,
            next: u32,
        }

        let mut state = vec![State::Unvisited; self.dirs.len()];
        let mut height = vec![0u32; self.dirs.len()];
        let mut stack: Vec<Frame> = Vec::new();
        for start in 0..self.dirs.len() {
            if state[start] != State::Unvisited {
                continue;
            }
            state[start] = State::InProgress;
            stack.push(Frame {
                dir: start,
                next: 0,
            });
            while !stack.is_empty() {
                let node = &self.dirs[stack.last().expect("stack non-empty").dir];
                if stack.last().expect("stack non-empty").next == node.child_count {
                    let done = stack.pop().expect("stack non-empty");
                    state[done.dir] = State::Done;
                    if let Some(parent) = stack.last() {
                        let child_height = height[done.dir] + 1;
                        let parent_dir = parent.dir;
                        height[parent_dir] = height[parent_dir].max(child_height);
                    }
                    continue;
                }
                let frame = stack.last_mut().expect("stack non-empty");
                let child = self.children[(self.dirs[frame.dir].first_child + frame.next) as usize];
                frame.next += 1;
                if child as usize >= self.dirs.len() {
                    continue;
                }
                if state[child as usize] == State::InProgress {
                    log::error!("NUP directory tree contains a cycle");
                    return false;
                }
                if stack.len() as u32 + height[child as usize] + 1 > MAX_DIR_DEPTH {
                    log::error!("NUP directory tree is nested too deeply");
                    return false;
                }
                if state[child as usize] == State::Done {
                    let frame_dir = stack.last().expect("stack non-empty").dir;
                    let child_height = height[child as usize] + 1;
                    height[frame_dir] = height[frame_dir].max(child_height);
                } else {
                    state[child as usize] = State::InProgress;
                    stack.push(Frame {
                        dir: child as usize,
                        next: 0,
                    });
                }
            }
        }
        true
    }

    /// Parses the meta TLV block of one package (NPDM, names, icons, version).
    /// Maps to upstream `NupContainer::ParseMeta`.
    fn parse_meta(&mut self, package_index: usize) -> bool {
        let meta_offset = self.packages[package_index].meta_offset;
        let meta_size = self.packages[package_index].meta_size;
        if meta_offset == 0 {
            return true;
        }
        let Some(file) = &self.file else {
            return false;
        };
        let block = file.read_bytes(meta_size as usize, meta_offset as usize);
        if block.len() != meta_size as usize {
            return false;
        }
        let mut pos = 0usize;
        while pos + TLV_HEADER_SIZE <= block.len() {
            let tlv_type = read_u16_le(&block, pos);
            let size = read_u32_le(&block, pos + 4) as usize;
            pos += TLV_HEADER_SIZE;
            if pos + size > block.len() {
                return false;
            }
            let payload = block[pos..pos + size].to_vec();
            pos += size;
            match tlv_type {
                t if t == MetaType::Npdm as u16 => {
                    self.packages[package_index].npdm = payload;
                }
                t if t == MetaType::Icon as u16 => {
                    self.packages[package_index].icon = payload;
                }
                t if t == MetaType::TitleName as u16 => {
                    self.packages[package_index].title_name =
                        String::from_utf8_lossy(&payload).into_owned();
                }
                t if t == MetaType::Publisher as u16 => {
                    self.packages[package_index].publisher =
                        String::from_utf8_lossy(&payload).into_owned();
                }
                t if t == MetaType::SourceDump as u16 => {
                    self.packages[package_index].source_dump =
                        String::from_utf8_lossy(&payload).into_owned();
                }
                t if t == MetaType::DisplayVersion as u16 => {
                    self.packages[package_index].display_version =
                        String::from_utf8_lossy(&payload).into_owned();
                }
                t if t == MetaType::LocalizedTitles as u16
                    || t == MetaType::LocalizedIcons as u16 =>
                {
                    let is_title = t == MetaType::LocalizedTitles as u16;
                    let mut record_pos = 0usize;
                    while record_pos < size {
                        let lang_length = payload[record_pos] as usize;
                        record_pos += 1;
                        if record_pos + lang_length + std::mem::size_of::<u32>() > size {
                            return false;
                        }
                        let lang =
                            String::from_utf8_lossy(&payload[record_pos..record_pos + lang_length])
                                .into_owned();
                        record_pos += lang_length;
                        let data_length = read_u32_le(&payload, record_pos) as usize;
                        record_pos += std::mem::size_of::<u32>();
                        if record_pos + data_length > size {
                            return false;
                        }
                        if is_title {
                            let title = String::from_utf8_lossy(
                                &payload[record_pos..record_pos + data_length],
                            )
                            .into_owned();
                            self.packages[package_index]
                                .localized_titles
                                .push((lang, title));
                        } else {
                            self.packages[package_index].localized_icons.push((
                                lang,
                                payload[record_pos..record_pos + data_length].to_vec(),
                            ));
                        }
                        record_pos += data_length;
                    }
                }
                _ => {}
            }
        }
        true
    }

    // ── Tree building ───────────────────────────────────────────────────────

    /// Maps to upstream `NupContainer::BuildDirTree`.
    fn build_dir_tree(&self, dir_index: usize, out: &VectorVfsDirectory) {
        let node = &self.dirs[dir_index];
        let end = node.first_child as u64 + node.child_count as u64;
        for slot in node.first_child as u64..end {
            if slot as usize >= self.children.len() {
                break;
            }
            let child = self.children[slot as usize];
            if child as usize >= self.dirs.len() {
                if let Some(file) = self.open_file(child as usize - self.dirs.len()) {
                    out.add_file(file);
                }
            } else {
                let sub = Arc::new(VectorVfsDirectory::new(
                    Vec::new(),
                    Vec::new(),
                    self.dirs[child as usize].name.clone(),
                    None,
                ));
                self.build_dir_tree(child as usize, &sub);
                out.add_directory(sub);
            }
        }
    }

    /// Maps to upstream `NupContainer::BuildPackageDir`.
    fn build_package_dir(&self, dir_index: u32) -> Option<VirtualDir> {
        let dir_index = dir_index as usize;
        if dir_index >= self.dirs.len() {
            return None;
        }
        let out = Arc::new(VectorVfsDirectory::new(
            Vec::new(),
            Vec::new(),
            self.dirs[dir_index].name.clone(),
            None,
        ));
        self.build_dir_tree(dir_index, &out);
        Some(out)
    }

    /// Maps to upstream `NupContainer::ReadExtentRaw`.
    fn read_extent_raw(&self, node: &FileNode) -> Vec<u8> {
        self.file
            .as_ref()
            .map(|file| file.read_bytes(node.extent_size as usize, node.extent_offset as usize))
            .unwrap_or_default()
    }
}

/// Reads a NUL-free name out of the TOC string pool.
///
/// The writer never stores NUL terminators (names keep their exact length),
/// matching upstream `std::string(base + pool_offset + name_offset, length)`.
fn string_from_pool(toc: &[u8], pool_offset: usize, name_offset: u64, name_length: u64) -> String {
    let start = pool_offset.saturating_add(name_offset as usize);
    let end = start.saturating_add(name_length as usize);
    if end > toc.len() {
        return String::new();
    }
    String::from_utf8_lossy(&toc[start..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_sys::vfs::vfs_vector::VectorVfsFile;

    const MAIN_NPDM: &[u8] = b"npdm-bytes";
    const MAIN_NSO: &[u8] = b"nso-bytes";
    const DATA_BIN: &[u8] = b"romfs-data";
    const TITLE_NAME: &str = "Test Title";
    const DISPLAY_VERSION: &str = "1.2.3";
    const TITLE_ID: u64 = 0x0100_4860_24DE_C000;
    const VERSION: u32 = 0x0001_0000; // 1.0.0

    /// Builds a minimal but valid NUP container: one base package with an
    /// ExeFS dir ("main", "main.npdm") and a RomFS dir ("data.bin"), one zstd
    /// or raw member, and meta TLVs.
    fn build_test_container(zstd_data_file: bool) -> Vec<u8> {
        // ── Layout constants for the synthetic file ──
        let dir_count = 3u64;
        let file_count = 3u64;
        let package_count = 1u32;

        // String pool.
        let mut pool: Vec<u8> = Vec::new();
        let push_name = |pool: &mut Vec<u8>, name: &str| -> (u32, u32) {
            let offset = pool.len() as u32;
            pool.extend_from_slice(name.as_bytes());
            (offset, name.len() as u32)
        };
        let (root_off, root_len) = push_name(&mut pool, "root");
        let (exefs_off, exefs_len) = push_name(&mut pool, "exefs");
        let (romfs_off, romfs_len) = push_name(&mut pool, "romfs");
        let (main_off, main_len) = push_name(&mut pool, "main");
        let (npdm_off, npdm_len) = push_name(&mut pool, "main.npdm");
        let (data_off, data_len) = push_name(&mut pool, "data.bin");
        let pool_size = pool.len() as u64;

        let needed = TOC_HEADER_SIZE as u64
            + package_count as u64 * PACKAGE_RECORD_SIZE as u64
            + dir_count * DIR_RECORD_SIZE as u64
            + file_count * FILE_RECORD_SIZE as u64;
        let children_offset = needed as usize;
        let pool_offset = children_offset as u64 + 5 * std::mem::size_of::<u32>() as u64;

        // ── TOC ── (extents are placeholders, patched once blob_offset is known)
        let mut toc: Vec<u8> = Vec::new();
        toc.extend_from_slice(&TOC_MAGIC.to_le_bytes());
        toc.extend_from_slice(&package_count.to_le_bytes());
        toc.extend_from_slice(&dir_count.to_le_bytes());
        toc.extend_from_slice(&file_count.to_le_bytes());
        toc.extend_from_slice(&pool_offset.to_le_bytes());
        toc.extend_from_slice(&pool_size.to_le_bytes());
        toc.extend_from_slice(&0u64.to_le_bytes()); // reserved

        // Package record: base package.
        toc.extend_from_slice(&TITLE_ID.to_le_bytes());
        toc.extend_from_slice(&0u32.to_le_bytes()); // root_dir = 0
        toc.extend_from_slice(&1u32.to_le_bytes()); // exefs_dir = 1
        toc.extend_from_slice(&2u32.to_le_bytes()); // romfs_dir = 2
        toc.extend_from_slice(&VERSION.to_le_bytes());
        toc.push(PackageKind::Base as u8);
        toc.push(0); // flags
        toc.extend_from_slice(&[0; 6]); // reserved
        toc.extend_from_slice(&0u64.to_le_bytes()); // meta_offset, patched below
        toc.extend_from_slice(&0u32.to_le_bytes()); // meta_size, patched below
        toc.extend_from_slice(&[0; 20]); // reserved

        // Dir records. Child ids: dirs 0..2, files 3..5.
        let dir_record =
            |name_offset: u32, name_length: u32, first_child: u32, child_count: u32| {
                let mut record = Vec::new();
                record.extend_from_slice(&name_offset.to_le_bytes());
                record.extend_from_slice(&name_length.to_le_bytes());
                record.extend_from_slice(&first_child.to_le_bytes());
                record.extend_from_slice(&child_count.to_le_bytes());
                record.extend_from_slice(&0u64.to_le_bytes()); // reserved
                record
            };
        toc.extend_from_slice(&dir_record(root_off, root_len, 0, 2)); // root: [dir1, dir2]
        toc.extend_from_slice(&dir_record(exefs_off, exefs_len, 2, 2)); // exefs: [file3, file4]
        toc.extend_from_slice(&dir_record(romfs_off, romfs_len, 4, 1)); // romfs: [file5]

        // File records.
        let file_record = |name_offset: u32, name_length: u32| {
            let mut record = Vec::new();
            record.extend_from_slice(&name_offset.to_le_bytes());
            record.extend_from_slice(&name_length.to_le_bytes());
            record.extend_from_slice(&0u64.to_le_bytes()); // size, patched below
            record.extend_from_slice(&0u64.to_le_bytes()); // extent_offset, patched below
            record.extend_from_slice(&0u64.to_le_bytes()); // extent_size, patched below
            record.extend_from_slice(&[0; 0x20]); // sha256, patched below
            record.push(0); // codec, patched below
            record.push(0); // codec_flags
            record.extend_from_slice(&[0; 14]); // reserved
            record
        };
        toc.extend_from_slice(&file_record(main_off, main_len));
        toc.extend_from_slice(&file_record(npdm_off, npdm_len));
        toc.extend_from_slice(&file_record(data_off, data_len));

        // Children array: root=[1,2], exefs=[3,4], romfs=[5].
        for child in [1u32, 2u32, 3u32, 4u32, 5u32] {
            toc.extend_from_slice(&child.to_le_bytes());
        }

        // String pool.
        toc.extend_from_slice(&pool);

        // Blob region starts right after the TOC, 16-byte aligned. Pad the TOC
        // so the assembled file really places the blob at that offset.
        let blob_offset = super_align(SUPERBLOCK_SIZE + toc.len());
        while toc.len() < blob_offset - SUPERBLOCK_SIZE {
            toc.push(0);
        }
        // Every member extent is 16-byte aligned, like the real writer.
        let main_extent = blob_offset as u64;
        let npdm_extent = super_align(main_extent as usize + MAIN_NSO.len()) as u64;
        let data_extent = super_align(npdm_extent as usize + MAIN_NPDM.len()) as u64;

        let (data_member, data_size, data_extent_size, data_codec, data_sha) = if zstd_data_file {
            let compressed = zstd_encode(DATA_BIN);
            (
                compressed.clone(),
                DATA_BIN.len() as u64,
                compressed.len() as u64,
                CODEC_ZSTD,
                sha256(DATA_BIN),
            )
        } else {
            (
                DATA_BIN.to_vec(),
                DATA_BIN.len() as u64,
                DATA_BIN.len() as u64,
                CODEC_RAW,
                sha256(DATA_BIN),
            )
        };

        // Patch the file records with the real extents.
        let files_base = TOC_HEADER_SIZE + PACKAGE_RECORD_SIZE + 3 * DIR_RECORD_SIZE;
        let patch_file = |bytes: &mut Vec<u8>,
                          index: usize,
                          size: u64,
                          extent_offset: u64,
                          extent_size: u64,
                          codec: u8,
                          sha: &[u8; 0x20]| {
            let at = files_base + index * FILE_RECORD_SIZE;
            bytes[at + 0x08..at + 0x10].copy_from_slice(&size.to_le_bytes());
            bytes[at + 0x10..at + 0x18].copy_from_slice(&extent_offset.to_le_bytes());
            bytes[at + 0x18..at + 0x20].copy_from_slice(&extent_size.to_le_bytes());
            bytes[at + 0x20..at + 0x40].copy_from_slice(sha);
            bytes[at + 0x40] = codec;
        };
        patch_file(
            &mut toc,
            0,
            MAIN_NSO.len() as u64,
            main_extent,
            MAIN_NSO.len() as u64,
            CODEC_RAW,
            &sha256(MAIN_NSO),
        );
        patch_file(
            &mut toc,
            1,
            MAIN_NPDM.len() as u64,
            npdm_extent,
            MAIN_NPDM.len() as u64,
            CODEC_RAW,
            &sha256(MAIN_NPDM),
        );
        patch_file(
            &mut toc,
            2,
            data_size,
            data_extent,
            data_extent_size,
            data_codec,
            &data_sha,
        );

        // Meta TLVs live at an absolute file offset after the blob.
        let meta = build_meta();

        // ── Blob region ──
        let mut blob: Vec<u8> = Vec::new();
        blob.extend_from_slice(MAIN_NSO);
        while blob.len() < npdm_extent as usize - blob_offset {
            blob.push(0);
        }
        blob.extend_from_slice(MAIN_NPDM);
        while blob.len() < data_extent as usize - blob_offset {
            blob.push(0);
        }
        blob.extend_from_slice(&data_member);

        // ── Superblock ──
        let toc_offset = SUPERBLOCK_SIZE as u64;
        let toc_size = blob_offset as u64 - toc_offset;
        let file_size = blob_offset as u64 + blob.len() as u64 + meta.len() as u64;
        let meta_offset = blob_offset as u64 + blob.len() as u64;

        let mut superblock: Vec<u8> = Vec::new();
        superblock.extend_from_slice(&HEADER_MAGIC);
        superblock.extend_from_slice(&(SUPERBLOCK_SIZE as u16).to_le_bytes());
        superblock.extend_from_slice(&1u16.to_le_bytes()); // header_version
        superblock.extend_from_slice(&toc_offset.to_le_bytes());
        superblock.extend_from_slice(&toc_size.to_le_bytes());
        superblock.extend_from_slice(&[0; 0x20]); // toc_sha256, patched below
        superblock.extend_from_slice(&(blob_offset as u64).to_le_bytes());
        superblock.extend_from_slice(&file_size.to_le_bytes());
        superblock.extend_from_slice(&package_count.to_le_bytes());
        superblock.extend_from_slice(&0u32.to_le_bytes()); // flags
        superblock.extend_from_slice(&[0; 0x10]); // uuid
        superblock.extend_from_slice(&[0; 0x20]); // reserved
        assert_eq!(superblock.len(), SUPERBLOCK_SIZE);

        // Patch the package record meta_offset/meta_size.
        let package_meta_at = TOC_HEADER_SIZE + 0x20;
        toc[package_meta_at..package_meta_at + 8].copy_from_slice(&meta_offset.to_le_bytes());
        toc[package_meta_at + 8..package_meta_at + 12]
            .copy_from_slice(&(meta.len() as u32).to_le_bytes());
        // The TOC bytes changed, so the hash must be taken now.
        let toc_hash = sha256(&toc);
        superblock[0x18..0x38].copy_from_slice(&toc_hash);

        // ── Assemble ──
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&superblock);
        bytes.extend_from_slice(&toc);
        bytes.extend_from_slice(&blob);
        bytes.extend_from_slice(&meta);
        assert_eq!(bytes.len() as u64, file_size);
        bytes
    }

    fn super_align(value: usize) -> usize {
        (value + 0xF) & !0xF
    }

    fn build_meta() -> Vec<u8> {
        let mut meta = Vec::new();
        let push_tlv = |meta: &mut Vec<u8>, tlv_type: u16, payload: &[u8]| {
            meta.extend_from_slice(&tlv_type.to_le_bytes());
            meta.extend_from_slice(&0u16.to_le_bytes());
            meta.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            meta.extend_from_slice(payload);
        };
        push_tlv(&mut meta, MetaType::Npdm as u16, MAIN_NPDM);
        push_tlv(&mut meta, MetaType::TitleName as u16, TITLE_NAME.as_bytes());
        push_tlv(
            &mut meta,
            MetaType::DisplayVersion as u16,
            DISPLAY_VERSION.as_bytes(),
        );
        meta
    }

    fn zstd_encode(data: &[u8]) -> Vec<u8> {
        zstd::stream::encode_all(std::io::Cursor::new(data), 3).expect("zstd encode")
    }

    fn container_bytes() -> VirtualFile {
        Arc::new(VectorVfsFile::new(
            build_test_container(true),
            "test.nup".to_owned(),
            None,
        ))
    }

    #[test]
    fn test_is_nup_file_rejects_non_nup_and_accepts_magic() {
        let small: VirtualFile = Arc::new(VectorVfsFile::new(
            vec![0; 0x80],
            "small.nup".to_owned(),
            None,
        ));
        assert!(!NupContainer::is_nup_file(&small));
        assert!(NupContainer::is_nup_file(&container_bytes()));
    }

    #[test]
    fn null_backing_preserves_null_file_status() {
        assert_eq!(
            NupContainer::new(None).get_status(),
            ResultStatus::ErrorNullFile
        );
    }

    #[test]
    fn shared_directory_is_not_a_cycle_but_back_edges_are() {
        let mut container = NupContainer::new(Some(container_bytes()));
        container.dirs = vec![
            DirNode {
                name: "root".into(),
                first_child: 0,
                child_count: 2,
            },
            DirNode {
                name: "shared".into(),
                first_child: 2,
                child_count: 0,
            },
            DirNode {
                name: "second".into(),
                first_child: 2,
                child_count: 1,
            },
        ];
        container.children = vec![1, 2, 1];
        assert!(container.validate_dir_tree());
        container.children[2] = 0;
        assert!(!container.validate_dir_tree());
    }

    #[test]
    fn oversized_member_spills_and_rejects_size_mismatch() {
        let directory = tempfile::tempdir().unwrap();
        let expected = vec![0x5a; 200_000];
        let compressed = zstd::stream::encode_all(expected.as_slice(), 1).unwrap();
        let backing: VirtualFile = Arc::new(VectorVfsFile::new(
            compressed.clone(),
            "compressed".into(),
            None,
        ));
        for size in [expected.len() - 1, expected.len(), expected.len() + 1] {
            let cache = Arc::new(NupDecompressCache::new(1, directory.path().to_owned()));
            let file = NupVfsFile::new(
                backing.clone(),
                0,
                compressed.len() as u64,
                size as u64,
                "large".into(),
                cache,
                0,
            );
            let mut actual = [0; 32];
            let read = file.read(&mut actual, 32, 100_000);
            if size == expected.len() {
                assert_eq!(read, 32);
                assert_eq!(actual, [0x5a; 32]);
            } else {
                assert_eq!(read, 0);
            }
        }
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn test_parse_reads_package_records_and_meta_tlvs() {
        let container = NupContainer::new_with_config(
            Some(container_bytes()),
            DEFAULT_CACHE_CAP,
            std::env::temp_dir().join("ruzu-nup-test"),
        );
        assert_eq!(container.get_status(), ResultStatus::Success);
        let packages = container.get_packages();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].title_id, TITLE_ID);
        assert_eq!(packages[0].kind, PackageKind::Base);
        assert_eq!(packages[0].version, VERSION);
        assert_eq!(packages[0].title_name, TITLE_NAME);
        assert_eq!(packages[0].display_version, DISPLAY_VERSION);
        assert_eq!(packages[0].npdm, MAIN_NPDM);
        assert_eq!(container.find_base_package(), Some(0));
        assert_eq!(container.find_best_update_package(), None);
    }

    #[test]
    fn test_member_files_expose_exefs_and_romfs_trees() {
        let container = NupContainer::new_with_config(
            Some(container_bytes()),
            DEFAULT_CACHE_CAP,
            std::env::temp_dir().join("ruzu-nup-test"),
        );
        let exefs = container.get_exefs_dir(0).expect("exefs dir");
        assert_eq!(exefs.get_name(), "exefs");
        let main = exefs.get_file("main").expect("main");
        assert_eq!(main.read_all_bytes(), MAIN_NSO);
        let npdm = exefs.get_file("main.npdm").expect("main.npdm");
        assert_eq!(npdm.read_all_bytes(), MAIN_NPDM);

        // data.bin is zstd-compressed: the VFS view must decompress lazily.
        let romfs = container.get_romfs_dir(0).expect("romfs dir");
        assert_eq!(romfs.get_name(), "romfs");
        let data = romfs.get_file("data.bin").expect("data.bin");
        assert_eq!(data.get_size(), DATA_BIN.len());
        assert_eq!(data.read_all_bytes(), DATA_BIN);
    }

    #[test]
    fn test_verify_integrity_passes_and_detects_corruption() {
        let mut bytes = build_test_container(true);
        let container = NupContainer::new_with_config(
            Some(Arc::new(VectorVfsFile::new(
                bytes.clone(),
                "test.nup".to_owned(),
                None,
            ))),
            DEFAULT_CACHE_CAP,
            std::env::temp_dir().join("ruzu-nup-test"),
        );
        assert_eq!(
            container.verify_integrity(Some(&|_, _| true)),
            ResultStatus::Success
        );

        // Corrupt one blob byte without touching the TOC hash.
        let blob_start = super_align(
            SUPERBLOCK_SIZE
                + 0x30
                + PACKAGE_RECORD_SIZE
                + 3 * DIR_RECORD_SIZE
                + 3 * FILE_RECORD_SIZE
                + 20
                + pool_len(),
        );
        bytes[blob_start] ^= 0xFF;
        let corrupt = NupContainer::new_with_config(
            Some(Arc::new(VectorVfsFile::new(
                bytes,
                "corrupt.nup".to_owned(),
                None,
            ))),
            DEFAULT_CACHE_CAP,
            std::env::temp_dir().join("ruzu-nup-test"),
        );
        assert_eq!(
            corrupt.verify_integrity(Some(&|_, _| true)),
            ResultStatus::ErrorIntegrityVerificationFailed
        );
    }

    fn pool_len() -> usize {
        4 + 5 + 5 + 4 + 9 + 8
    }

    #[test]
    fn test_bad_toc_hash_is_rejected() {
        let mut bytes = build_test_container(false);
        // Flip a byte inside the TOC string pool region.
        let toc_start = SUPERBLOCK_SIZE + TOC_HEADER_SIZE;
        bytes[toc_start + 0x100] ^= 0xFF;
        let container = NupContainer::new_with_config(
            Some(Arc::new(VectorVfsFile::new(
                bytes,
                "bad.nup".to_owned(),
                None,
            ))),
            DEFAULT_CACHE_CAP,
            std::env::temp_dir().join("ruzu-nup-test"),
        );
        assert_eq!(container.get_status(), ResultStatus::ErrorNUPBadFormat);
    }

    #[test]
    fn test_merge_shadow_dirs_lets_overlay_win() {
        let make_file = |name: &str, data: u8| -> VirtualFile {
            Arc::new(VectorVfsFile::new(vec![data], name.to_owned(), None))
        };
        let base: VirtualDir = Arc::new(VectorVfsDirectory::new(
            vec![make_file("shared.bin", 1), make_file("base-only.bin", 2)],
            vec![],
            "romfs".to_owned(),
            None,
        ));
        let overlay: VirtualDir = Arc::new(VectorVfsDirectory::new(
            vec![make_file("shared.bin", 9), make_file("update-only.bin", 3)],
            vec![],
            "romfs".to_owned(),
            None,
        ));

        let merged = NupContainer::merge_shadow_dirs(Some(&base), Some(&overlay)).expect("merged");
        assert_eq!(merged.get_files().len(), 3);
        assert_eq!(
            merged
                .get_file("shared.bin")
                .expect("shared")
                .read_all_bytes(),
            vec![9]
        );
        assert_eq!(
            merged
                .get_file("base-only.bin")
                .expect("base-only")
                .read_all_bytes(),
            vec![2]
        );
        assert_eq!(
            merged
                .get_file("update-only.bin")
                .expect("update-only")
                .read_all_bytes(),
            vec![3]
        );

        // Single-sided merges return the other tree.
        let only_base = NupContainer::merge_shadow_dirs(Some(&base), None).expect("base only");
        assert_eq!(only_base.get_files().len(), 2);
        assert!(
            NupContainer::merge_shadow_dirs(None, None).is_none(),
            "no trees at all"
        );
    }
}

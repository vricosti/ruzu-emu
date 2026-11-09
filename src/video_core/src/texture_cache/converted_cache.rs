// SPDX-FileCopyrightText: 2026 ruzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Disk cache for converted ASTC textures (ruzu extension, no Eden
//! counterpart).
//!
//! ASTC decode plus BCn recompression runs on the GPU command thread inside
//! the texture upload path and was measured as the dominant first-encounter
//! stall source (fill-image-views bursts of 280-546 ms/s while driving).
//! The emulator pays that conversion again every session because nothing
//! persists the result. This cache stores the converted BCn payload keyed by
//! a stable hash of the ASTC source bytes, so later sessions upload straight
//! from disk.
//!
//! Layout: two append-only files next to the title's shader cache
//! (`cache/shader/<title_id>/converted_textures.{idx,bin}`). The index holds
//! fixed-size records; the data file holds the converted payloads. Records
//! are written by a dedicated thread so lookups and conversion stay off the
//! file-write path. A version header invalidates both files wholesale.
//! `RUZU_TEXTURE_CONVERSION_CACHE=0` disables the cache.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};

const INDEX_MAGIC: &[u8; 4] = b"RZTC";
const INDEX_VERSION: u32 = 1;
const HEADER_LEN: u64 = 8;
/// Stop appending beyond this much converted data per title. Mario Kart
/// World filled 3.5 GiB in a single race, so the bound covers a full game's
/// texture set while still capping runaway growth.
const MAX_DATA_BYTES: u64 = 16 << 30;

/// Cache key: identity of one converted copy region.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub source_hash: u64,
    pub source_len: u32,
    pub width: u32,
    pub height: u32,
    pub depth_layers: u32,
    /// `AstcRecompression` discriminant of the produced payload.
    pub target: u32,
}

impl Key {
    const RECORD_LEN: usize = 8 + 4 + 4 + 4 + 4 + 4 + 8 + 4;

    fn write_record(&self, out: &mut Vec<u8>, offset: u64, len: u32) {
        out.extend_from_slice(&self.source_hash.to_le_bytes());
        out.extend_from_slice(&self.source_len.to_le_bytes());
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.extend_from_slice(&self.depth_layers.to_le_bytes());
        out.extend_from_slice(&self.target.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
    }

    fn read_record(record: &[u8]) -> (Self, u64, u32) {
        let u32_at = |i: usize| u32::from_le_bytes(record[i..i + 4].try_into().unwrap());
        let u64_at = |i: usize| u64::from_le_bytes(record[i..i + 8].try_into().unwrap());
        (
            Self {
                source_hash: u64_at(0),
                source_len: u32_at(8),
                width: u32_at(12),
                height: u32_at(16),
                depth_layers: u32_at(20),
                target: u32_at(24),
            },
            u64_at(28),
            u32_at(36),
        )
    }
}

struct Store {
    index: Mutex<HashMap<Key, (u64, u32)>>,
    data_read: Mutex<File>,
    writer: Mutex<mpsc::Sender<(Key, Vec<u8>)>>,
    data_len: AtomicU64,
    full_reported: AtomicBool,
    title_id: u64,
}

static STORE: OnceLock<Option<Store>> = OnceLock::new();

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("RUZU_TEXTURE_CONVERSION_CACHE")
            .is_none_or(|value| !value.to_string_lossy().starts_with('0'))
    })
}

fn cache_paths(title_id: u64) -> (PathBuf, PathBuf) {
    let base = common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::ShaderDir)
        .join(format!("{title_id:016x}"));
    (
        base.join("converted_textures.idx"),
        base.join("converted_textures.bin"),
    )
}

fn open_store() -> Option<Store> {
    if !enabled() {
        return None;
    }
    let title_id = common::settings::get_current_program_id();
    if title_id == 0 {
        return None;
    }
    let (idx_path, bin_path) = cache_paths(title_id);
    if let Some(parent) = idx_path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }

    let mut index = HashMap::new();
    let mut valid = false;
    let mut data_len = 0u64;
    if let (Ok(mut idx), Ok(bin_meta)) = (File::open(&idx_path), std::fs::metadata(&bin_path)) {
        let mut header = [0u8; HEADER_LEN as usize];
        if idx.read_exact(&mut header).is_ok()
            && &header[0..4] == INDEX_MAGIC
            && u32::from_le_bytes(header[4..8].try_into().unwrap()) == INDEX_VERSION
        {
            valid = true;
            let mut record = [0u8; Key::RECORD_LEN];
            while idx.read_exact(&mut record).is_ok() {
                let (key, offset, len) = Key::read_record(&record);
                // Ignore index entries whose data never finished writing.
                if offset + u64::from(len) <= bin_meta.len() {
                    index.insert(key, (offset, len));
                    data_len = data_len.max(offset + u64::from(len));
                }
            }
        }
    }
    if !valid {
        // Recreate both files with a fresh header.
        let mut idx = File::create(&idx_path).ok()?;
        idx.write_all(INDEX_MAGIC).ok()?;
        idx.write_all(&INDEX_VERSION.to_le_bytes()).ok()?;
        File::create(&bin_path).ok()?;
        data_len = 0;
        index.clear();
    }

    let data_read = File::open(&bin_path).ok()?;
    let mut data_append = OpenOptions::new().append(true).open(&bin_path).ok()?;
    let mut idx_append = OpenOptions::new().append(true).open(&idx_path).ok()?;

    let (sender, receiver) = mpsc::channel::<(Key, Vec<u8>)>();
    let mut write_offset = data_len;
    std::thread::Builder::new()
        .name("TextureConvCache".into())
        .spawn(move || {
            while let Ok((key, bytes)) = receiver.recv() {
                if data_append.write_all(&bytes).is_err() {
                    continue;
                }
                if data_append.flush().is_err() {
                    continue;
                }
                let mut record = Vec::with_capacity(Key::RECORD_LEN);
                key.write_record(&mut record, write_offset, bytes.len() as u32);
                write_offset += bytes.len() as u64;
                let _ = idx_append.write_all(&record);
                let _ = idx_append.flush();
            }
        })
        .ok()?;

    log::info!(
        "Converted texture cache: {} entries, {} MiB (title {title_id:016X})",
        index.len(),
        data_len / (1024 * 1024),
    );
    Some(Store {
        index: Mutex::new(index),
        data_read: Mutex::new(data_read),
        writer: Mutex::new(sender),
        data_len: AtomicU64::new(data_len),
        full_reported: AtomicBool::new(false),
        title_id,
    })
}

fn store() -> Option<&'static Store> {
    let store = STORE.get_or_init(open_store).as_ref()?;
    // A new title in the same process (session restart) must not reuse the
    // previous title's cache; fall back to conversion-only in that case.
    if store.title_id != common::settings::get_current_program_id() {
        return None;
    }
    Some(store)
}

/// Builds the key for one conversion input.
pub fn key_for(
    source: &[u8],
    width: u32,
    height: u32,
    depth_layers: u32,
    target: common::settings_enums::AstcRecompression,
) -> Key {
    Key {
        source_hash: common::cityhash::city_hash64(source),
        source_len: source.len() as u32,
        width,
        height,
        depth_layers,
        target: target as u32,
    }
}

/// Copies the cached converted payload into `output`, returning its length.
pub fn get(key: &Key, output: &mut [u8]) -> Option<usize> {
    let store = store()?;
    let (offset, len) = *store.index.lock().unwrap().get(key)?;
    let len = len as usize;
    if output.len() < len {
        return None;
    }
    let mut file = store.data_read.lock().unwrap();
    file.seek(SeekFrom::Start(offset)).ok()?;
    file.read_exact(&mut output[..len]).ok()?;
    Some(len)
}

/// Stores a freshly converted payload.
pub fn put(key: Key, payload: &[u8]) {
    let Some(store) = store() else { return };
    let projected = store.data_len.load(Ordering::Relaxed) + payload.len() as u64;
    if projected > MAX_DATA_BYTES {
        if !store.full_reported.swap(true, Ordering::Relaxed) {
            log::warn!("Converted texture cache reached its size cap; further conversions are not persisted");
        }
        return;
    }
    {
        let mut index = store.index.lock().unwrap();
        if index.contains_key(&key) {
            return;
        }
        // Reserve the slot optimistically; the writer thread appends in the
        // same order the channel receives, so offsets stay consistent.
        let offset = store.data_len.fetch_add(payload.len() as u64, Ordering::Relaxed);
        index.insert(key, (offset, payload.len() as u32));
    }
    let _ = store
        .writer
        .lock()
        .unwrap()
        .send((key, payload.to_vec()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_records_round_trip() {
        let key = Key {
            source_hash: 0x0123_4567_89ab_cdef,
            source_len: 42,
            width: 1920,
            height: 1080,
            depth_layers: 6,
            target: 3,
        };
        let mut record = Vec::new();
        key.write_record(&mut record, 0xdead_beef_cafe, 512);
        assert_eq!(record.len(), Key::RECORD_LEN);
        let (parsed, offset, len) = Key::read_record(&record);
        assert!(parsed == key);
        assert_eq!(offset, 0xdead_beef_cafe);
        assert_eq!(len, 512);
    }
}

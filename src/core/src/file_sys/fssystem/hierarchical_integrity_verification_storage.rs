// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Ported from: core/file_sys/fssystem/fssystem_hierarchical_integrity_verification_storage.h / .cpp

use super::fs_types::{HashSalt, Int64, INTEGRITY_MAX_LAYER_COUNT, INTEGRITY_MIN_LAYER_COUNT};
use super::integrity_verification_storage::IntegrityVerificationStorage;
use crate::file_sys::errors;
use crate::file_sys::vfs::vfs::VfsFile;
use crate::file_sys::vfs::vfs_offset::OffsetVfsFile;
use crate::file_sys::vfs::vfs_types::VirtualFile;
use common::ResultCode;
use std::sync::Arc;

/// Level information for hierarchical integrity verification.
/// Corresponds to upstream `HierarchicalIntegrityVerificationLevelInformation`.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct HierarchicalIntegrityVerificationLevelInformation {
    pub offset: Int64,
    pub size: Int64,
    pub block_order: i32,
    pub reserved: [u8; 4],
}

const _: () =
    assert!(std::mem::size_of::<HierarchicalIntegrityVerificationLevelInformation>() == 0x18);
const _: () =
    assert!(std::mem::align_of::<HierarchicalIntegrityVerificationLevelInformation>() == 0x4);

/// Hierarchical integrity verification information.
/// Corresponds to upstream `HierarchicalIntegrityVerificationInformation`.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct HierarchicalIntegrityVerificationInformation {
    pub max_layers: u32,
    pub info: [HierarchicalIntegrityVerificationLevelInformation; INTEGRITY_MAX_LAYER_COUNT - 1],
    pub seed: HashSalt,
}

impl HierarchicalIntegrityVerificationInformation {
    /// Get the layered hash size.
    /// Corresponds to upstream `GetLayeredHashSize`.
    pub fn get_layered_hash_size(&self) -> i64 {
        let idx = (self.max_layers as usize).saturating_sub(2);
        self.info[idx].offset.get()
    }

    /// Get the data offset.
    /// Corresponds to upstream `GetDataOffset`.
    pub fn get_data_offset(&self) -> i64 {
        let idx = (self.max_layers as usize).saturating_sub(2);
        self.info[idx].offset.get()
    }

    /// Get the data size.
    /// Corresponds to upstream `GetDataSize`.
    pub fn get_data_size(&self) -> i64 {
        let idx = (self.max_layers as usize).saturating_sub(2);
        self.info[idx].size.get()
    }
}

/// Meta information for hierarchical integrity verification.
/// Corresponds to upstream `HierarchicalIntegrityVerificationMetaInformation`.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct HierarchicalIntegrityVerificationMetaInformation {
    pub magic: u32,
    pub version: u32,
    pub master_hash_size: u32,
    pub level_hash_info: HierarchicalIntegrityVerificationInformation,
}

/// Size set for hierarchical integrity verification.
/// Corresponds to upstream `HierarchicalIntegrityVerificationSizeSet`.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct HierarchicalIntegrityVerificationSizeSet {
    pub control_size: i64,
    pub master_hash_size: i64,
    pub layered_hash_sizes: [i64; INTEGRITY_MAX_LAYER_COUNT - 2],
}

/// Hierarchical storage information -- array of storages for each layer.
/// Corresponds to upstream `HierarchicalIntegrityVerificationStorage::HierarchicalStorageInformation`.
pub struct HierarchicalStorageInformation {
    pub storages: [Option<VirtualFile>; Self::DATA_STORAGE + 1],
}

impl HierarchicalStorageInformation {
    pub const MASTER_STORAGE: usize = 0;
    pub const LAYER1_STORAGE: usize = 1;
    pub const LAYER2_STORAGE: usize = 2;
    pub const LAYER3_STORAGE: usize = 3;
    pub const LAYER4_STORAGE: usize = 4;
    pub const LAYER5_STORAGE: usize = 5;
    pub const DATA_STORAGE: usize = 6;

    pub fn new() -> Self {
        Self {
            storages: Default::default(),
        }
    }

    pub fn set_master_hash_storage(&mut self, s: VirtualFile) {
        self.storages[Self::MASTER_STORAGE] = Some(s);
    }
    pub fn set_layer1_hash_storage(&mut self, s: VirtualFile) {
        self.storages[Self::LAYER1_STORAGE] = Some(s);
    }
    pub fn set_layer2_hash_storage(&mut self, s: VirtualFile) {
        self.storages[Self::LAYER2_STORAGE] = Some(s);
    }
    pub fn set_layer3_hash_storage(&mut self, s: VirtualFile) {
        self.storages[Self::LAYER3_STORAGE] = Some(s);
    }
    pub fn set_layer4_hash_storage(&mut self, s: VirtualFile) {
        self.storages[Self::LAYER4_STORAGE] = Some(s);
    }
    pub fn set_layer5_hash_storage(&mut self, s: VirtualFile) {
        self.storages[Self::LAYER5_STORAGE] = Some(s);
    }
    pub fn set_data_storage(&mut self, s: VirtualFile) {
        self.storages[Self::DATA_STORAGE] = Some(s);
    }

    /// Index into the storages array.
    /// Corresponds to upstream `operator[]`.
    pub fn get(&self, index: usize) -> Option<&VirtualFile> {
        assert!(index <= Self::DATA_STORAGE);
        self.storages[index].as_ref()
    }

    /// Take a storage by index, leaving None in its place.
    pub fn take(&mut self, index: usize) -> Option<VirtualFile> {
        assert!(index <= Self::DATA_STORAGE);
        self.storages[index].take()
    }
}

impl Default for HierarchicalStorageInformation {
    fn default() -> Self {
        Self::new()
    }
}

/// Hierarchical integrity verification storage.
/// Corresponds to upstream `HierarchicalIntegrityVerificationStorage`.
pub struct HierarchicalIntegrityVerificationStorage {
    verify_storages: Vec<Arc<IntegrityVerificationStorage>>,
    buffer_storages: Vec<Option<VirtualFile>>,
    data_size: i64,
    max_layers: i32,
}

impl HierarchicalIntegrityVerificationStorage {
    pub const HASH_SIZE: i64 = 256 / 8;
    pub const MAX_LAYERS: usize = INTEGRITY_MAX_LAYER_COUNT;

    /// Create a new uninitialized storage.
    /// Corresponds to upstream `HierarchicalIntegrityVerificationStorage::HierarchicalIntegrityVerificationStorage`.
    pub fn new() -> Self {
        let mut verify_storages = Vec::with_capacity(Self::MAX_LAYERS - 1);
        for _ in 0..Self::MAX_LAYERS - 1 {
            verify_storages.push(Arc::new(IntegrityVerificationStorage::new()));
        }

        Self {
            verify_storages,
            buffer_storages: vec![None; Self::MAX_LAYERS - 1],
            data_size: -1,
            max_layers: 0,
        }
    }

    /// Initialize the storage with integrity verification layers.
    /// Corresponds to upstream `HierarchicalIntegrityVerificationStorage::Initialize`.
    pub fn initialize(
        &mut self,
        info: &HierarchicalIntegrityVerificationInformation,
        mut storage: HierarchicalStorageInformation,
        _max_data_cache_entries: i32,
        _max_hash_cache_entries: i32,
        _buffer_level: i8,
    ) -> Result<(), ResultCode> {
        // Validate preconditions.
        assert!(
            INTEGRITY_MIN_LAYER_COUNT <= info.max_layers as usize
                && (info.max_layers as usize) <= INTEGRITY_MAX_LAYER_COUNT
        );

        // Set member variables.
        self.max_layers = info.max_layers as i32;

        let result = (|| -> Result<(), ResultCode> {
            // Initialize the top level verification storage.
            let master_storage = storage
                .take(HierarchicalStorageInformation::MASTER_STORAGE)
                .ok_or(errors::RESULT_ALLOCATION_MEMORY_FAILED_ALLOCATE_SHARED)?;
            let layer1_storage = storage
                .take(HierarchicalStorageInformation::LAYER1_STORAGE)
                .ok_or(errors::RESULT_ALLOCATION_MEMORY_FAILED_ALLOCATE_SHARED)?;

            {
                let vs = Arc::get_mut(&mut self.verify_storages[0]).unwrap();
                vs.initialize(
                    master_storage,
                    layer1_storage,
                    1i64 << info.info[0].block_order,
                    Self::HASH_SIZE,
                    false,
                );
            }

            // Initialize the top level buffer storage.
            self.buffer_storages[0] = Some(self.verify_storages[0].clone() as VirtualFile);

            // Initialize the intermediate level storages.
            let mut level: i32 = 0;
            while level < self.max_layers - 3 {
                let buffer_storage = self.buffer_storages[level as usize]
                    .clone()
                    .ok_or(errors::RESULT_ALLOCATION_MEMORY_FAILED_ALLOCATE_SHARED)?;

                let offset_storage: VirtualFile = Arc::new(OffsetVfsFile::new(
                    buffer_storage,
                    info.info[level as usize].size.get() as usize,
                    0,
                    String::new(),
                ));

                let next_storage = storage
                    .take(level as usize + 2)
                    .ok_or(errors::RESULT_ALLOCATION_MEMORY_FAILED_ALLOCATE_SHARED)?;

                {
                    let vs = Arc::get_mut(&mut self.verify_storages[(level + 1) as usize]).unwrap();
                    vs.initialize(
                        offset_storage,
                        next_storage,
                        1i64 << info.info[(level + 1) as usize].block_order,
                        1i64 << info.info[level as usize].block_order,
                        false,
                    );
                }

                self.buffer_storages[(level + 1) as usize] =
                    Some(self.verify_storages[(level + 1) as usize].clone() as VirtualFile);

                level += 1;
            }

            // Initialize the final level storage.
            {
                let buffer_storage = self.buffer_storages[level as usize]
                    .clone()
                    .ok_or(errors::RESULT_ALLOCATION_MEMORY_FAILED_ALLOCATE_SHARED)?;

                let offset_storage: VirtualFile = Arc::new(OffsetVfsFile::new(
                    buffer_storage,
                    info.info[level as usize].size.get() as usize,
                    0,
                    String::new(),
                ));

                let next_storage = storage
                    .take(level as usize + 2)
                    .ok_or(errors::RESULT_ALLOCATION_MEMORY_FAILED_ALLOCATE_SHARED)?;

                {
                    let vs = Arc::get_mut(&mut self.verify_storages[(level + 1) as usize]).unwrap();
                    vs.initialize(
                        offset_storage,
                        next_storage,
                        1i64 << info.info[(level + 1) as usize].block_order,
                        1i64 << info.info[level as usize].block_order,
                        true,
                    );
                }

                self.buffer_storages[(level + 1) as usize] =
                    Some(self.verify_storages[(level + 1) as usize].clone() as VirtualFile);
            }

            // Set the data size.
            self.data_size = info.info[(level + 1) as usize].size.get();

            Ok(())
        })();

        if result.is_err() {
            self.data_size = 0;
            self.finalize();
        }

        result
    }

    /// Finalize the storage, releasing all references.
    /// Corresponds to upstream `HierarchicalIntegrityVerificationStorage::Finalize`.
    pub fn finalize(&mut self) {
        if self.data_size >= 0 {
            self.data_size = 0;

            for level in (0..=(self.max_layers - 2) as usize).rev() {
                self.buffer_storages[level] = None;
                if let Some(vs) = Arc::get_mut(&mut self.verify_storages[level]) {
                    vs.finalize();
                }
            }

            self.data_size = -1;
        }
    }

    /// Check if the storage is initialized.
    /// Corresponds to upstream `HierarchicalIntegrityVerificationStorage::IsInitialized`.
    pub fn is_initialized(&self) -> bool {
        self.data_size >= 0
    }

    /// Get the size of the data layer.
    /// Corresponds to upstream `HierarchicalIntegrityVerificationStorage::GetSize`.
    pub fn get_size(&self) -> usize {
        self.data_size as usize
    }

    /// Read data from the storage.
    /// Corresponds to upstream `HierarchicalIntegrityVerificationStorage::Read`.
    pub fn read(&self, buffer: &mut [u8], size: usize, offset: usize) -> usize {
        // Validate preconditions.
        assert!(self.data_size >= 0);

        // Succeed if zero-size.
        if size == 0 {
            return 0;
        }

        // Read the data from the buffer storage of the data layer.
        let data_layer_idx = (self.max_layers - 2) as usize;
        self.buffer_storages[data_layer_idx]
            .as_ref()
            .expect("initialized hierarchy has a data-layer buffer")
            .read(buffer, size, offset)
    }

    /// Get the L1 hash verification block size.
    /// Corresponds to upstream `GetL1HashVerificationBlockSize`.
    pub fn get_l1_hash_verification_block_size(&self) -> i64 {
        let idx = (self.max_layers - 2) as usize;
        self.verify_storages[idx].get_block_size()
    }

    /// Get the L1 hash storage as a VirtualFile.
    /// Corresponds to upstream `GetL1HashStorage`.
    pub fn get_l1_hash_storage(&self) -> Option<VirtualFile> {
        let layer_idx = (self.max_layers - 3) as usize;
        let buffer_storage = self.buffer_storages[layer_idx].clone()?;
        let block_size = self.get_l1_hash_verification_block_size();
        let size = if block_size > 0 {
            ((self.data_size + block_size - 1) / block_size) as usize
        } else {
            0
        };
        Some(Arc::new(OffsetVfsFile::new(
            buffer_storage,
            size,
            0,
            String::new(),
        )))
    }

    /// Get the default data cache buffer level.
    /// Corresponds to upstream `GetDefaultDataCacheBufferLevel`.
    pub fn get_default_data_cache_buffer_level(max_layers: u32) -> i8 {
        (16 + max_layers as i8 - 2) as i8
    }
}

/// Implement VfsFile so HierarchicalIntegrityVerificationStorage can be used as a VirtualFile.
impl VfsFile for HierarchicalIntegrityVerificationStorage {
    fn get_name(&self) -> String {
        String::from("HierarchicalIntegrityVerificationStorage")
    }

    fn get_size(&self) -> usize {
        self.get_size()
    }

    fn resize(&self, _new_size: usize) -> bool {
        false
    }

    fn get_containing_directory(&self) -> Option<crate::file_sys::vfs::vfs_types::VirtualDir> {
        None
    }

    fn is_writable(&self) -> bool {
        false
    }

    fn is_readable(&self) -> bool {
        true
    }

    fn read(&self, data: &mut [u8], length: usize, offset: usize) -> usize {
        let actual = length.min(data.len());
        if actual == 0 {
            return 0;
        }
        self.read(&mut data[..actual], actual, offset)
    }

    fn write(&self, _data: &[u8], _length: usize, _offset: usize) -> usize {
        0
    }

    fn rename(&self, _new_name: &str) -> bool {
        false
    }
}

impl Default for HierarchicalIntegrityVerificationStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for HierarchicalIntegrityVerificationStorage {
    fn drop(&mut self) {
        self.finalize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_sys::vfs::vfs_vector::VectorVfsFile;

    fn three_layer_info() -> HierarchicalIntegrityVerificationInformation {
        let mut levels = [HierarchicalIntegrityVerificationLevelInformation::default();
            INTEGRITY_MAX_LAYER_COUNT - 1];
        levels[0].size = Int64::from_i64(32);
        levels[0].block_order = 5;
        levels[1].size = Int64::from_i64(64);
        levels[1].block_order = 5;
        HierarchicalIntegrityVerificationInformation {
            max_layers: 3,
            info: levels,
            seed: HashSalt::default(),
        }
    }

    fn three_layer_storage(include_data: bool) -> HierarchicalStorageInformation {
        let mut storage = HierarchicalStorageInformation::new();
        storage.set_master_hash_storage(Arc::new(VectorVfsFile::new(
            vec![0; 32],
            "master".to_owned(),
            None,
        )));
        storage.set_layer1_hash_storage(Arc::new(VectorVfsFile::new(
            vec![0; 32],
            "layer1".to_owned(),
            None,
        )));
        if include_data {
            storage.set_layer2_hash_storage(Arc::new(VectorVfsFile::new(
                vec![0; 64],
                "data".to_owned(),
                None,
            )));
        }
        storage
    }

    #[test]
    fn test_level_information_size() {
        assert_eq!(
            std::mem::size_of::<HierarchicalIntegrityVerificationLevelInformation>(),
            0x18
        );
    }

    #[test]
    fn test_hierarchical_storage_information_new() {
        let info = HierarchicalStorageInformation::new();
        for s in &info.storages {
            assert!(s.is_none());
        }
    }

    #[test]
    fn test_hierarchical_storage_information_constants() {
        assert_eq!(HierarchicalStorageInformation::MASTER_STORAGE, 0);
        assert_eq!(HierarchicalStorageInformation::DATA_STORAGE, 6);
    }

    #[test]
    fn test_hierarchical_integrity_constants() {
        assert_eq!(HierarchicalIntegrityVerificationStorage::HASH_SIZE, 32);
        assert_eq!(HierarchicalIntegrityVerificationStorage::MAX_LAYERS, 7);
    }

    #[test]
    fn test_new_not_initialized() {
        let storage = HierarchicalIntegrityVerificationStorage::new();
        assert!(!storage.is_initialized());
    }

    #[test]
    fn failed_initialization_cleans_layers_and_allows_retry() {
        let info = three_layer_info();
        let mut storage = HierarchicalIntegrityVerificationStorage::new();

        assert!(storage
            .initialize(&info, three_layer_storage(false), 0, 0, 0)
            .is_err());
        assert!(!storage.is_initialized());

        assert!(storage
            .initialize(&info, three_layer_storage(true), 0, 0, 0)
            .is_ok());
        assert!(storage.is_initialized());
        assert_eq!(storage.get_size(), 64);
    }

    #[test]
    fn get_size_preserves_uninitialized_signed_bit_pattern() {
        assert_eq!(
            HierarchicalIntegrityVerificationStorage::new().get_size(),
            usize::MAX
        );
    }

    #[test]
    fn test_get_default_data_cache_buffer_level() {
        // max_layers = 7: 16 + 7 - 2 = 21
        assert_eq!(
            HierarchicalIntegrityVerificationStorage::get_default_data_cache_buffer_level(7),
            21
        );
    }
}

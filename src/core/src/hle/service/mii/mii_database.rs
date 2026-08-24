// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/mii/mii_database.h
//! Port of zuyu/src/core/hle/service/mii/mii_database.cpp
//!
//! NintendoFigurineDatabase: the on-disk Mii database format.

use super::mii_result::{
    RESULT_INVALID_DATABASE_CHECKSUM, RESULT_INVALID_DATABASE_LENGTH,
    RESULT_INVALID_DATABASE_SIGNATURE, RESULT_INVALID_DATABASE_VERSION, RESULT_NOT_UPDATED,
};
use super::mii_types::DatabaseSessionMetadata;
use super::types::store_data::StoreData;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::mii::mii_util;

pub const MAX_MII_COUNT: usize = 100;
pub const MII_MAGIC: u32 = 0xA523_B78F;
pub const DATABASE_MAGIC: u32 = 0x4244_464E; // NFDB

#[derive(Clone, Copy)]
#[repr(C)]
pub struct NintendoFigurineDatabase {
    pub magic: u32,
    pub miis: [StoreData; MAX_MII_COUNT],
    pub version: u8,
    pub database_length: u8,
    pub crc: u16,
}

impl NintendoFigurineDatabase {
    pub fn new() -> Self {
        let mut database = Self {
            magic: 0,
            miis: [StoreData::new(); MAX_MII_COUNT],
            version: 0,
            database_length: 0,
            crc: 0,
        };
        database.clean_database();
        database
    }

    pub fn get_database_length(&self) -> u8 {
        self.database_length
    }

    pub fn is_full(&self) -> bool {
        self.database_length as usize >= MAX_MII_COUNT
    }

    pub fn get(&self, index: usize) -> StoreData {
        let mut store_data = self.miis[index];
        // Matches upstream compatibility fix for external dumps.
        store_data.set_device_checksum();
        store_data
    }

    pub fn get_count(&self, metadata: &DatabaseSessionMetadata) -> u32 {
        if metadata.magic == MII_MAGIC {
            return self.get_database_length() as u32;
        }

        let mut mii_count = 0u32;
        for index in 0..self.database_length as usize {
            if !self.get(index).is_special() {
                mii_count += 1;
            }
        }

        mii_count
    }

    pub fn get_index_by_creator_id(&self, create_id: [u8; 16]) -> Option<u32> {
        for index in 0..self.database_length as usize {
            if self.miis[index].get_create_id() == create_id {
                return Some(index as u32);
            }
        }
        None
    }

    pub fn add(&mut self, store_data: StoreData) {
        let index = self.database_length as usize;
        self.miis[index] = store_data;
        self.database_length = self.database_length.saturating_add(1);
        self.crc = self.generate_database_crc();
    }

    pub fn replace(&mut self, index: u32, store_data: StoreData) {
        self.miis[index as usize] = store_data;
        self.crc = self.generate_database_crc();
    }

    pub fn move_entry(&mut self, current_index: u32, new_index: u32) -> ResultCode {
        if current_index == new_index {
            return RESULT_NOT_UPDATED;
        }

        let store_data = self.miis[current_index as usize];
        if new_index > current_index {
            let index_diff = (new_index - current_index) as usize;
            for i in 0..index_diff {
                self.miis[current_index as usize + i] = self.miis[current_index as usize + i + 1];
            }
        } else {
            let index_diff = (current_index - new_index) as usize;
            for i in 0..index_diff {
                self.miis[current_index as usize - i] = self.miis[current_index as usize - i - 1];
            }
        }

        self.miis[new_index as usize] = store_data;
        self.crc = self.generate_database_crc();
        RESULT_SUCCESS
    }

    pub fn delete(&mut self, index: u32) {
        let new_database_size = self.database_length as i32 - 1;
        if (index as i32) < new_database_size {
            for i in index as usize..new_database_size as usize {
                self.miis[i] = self.miis[i + 1];
            }
        }

        self.database_length = new_database_size as u8;
        self.crc = self.generate_database_crc();
    }

    pub fn clean_database(&mut self) {
        self.miis = [StoreData::new(); MAX_MII_COUNT];
        self.version = 1;
        self.magic = DATABASE_MAGIC;
        self.database_length = 0;
        self.crc = self.generate_database_crc();
    }

    pub fn corrupt_crc(&mut self) {
        self.crc = !self.generate_database_crc();
    }

    pub fn check_integrity(&self) -> ResultCode {
        if self.magic != DATABASE_MAGIC {
            return RESULT_INVALID_DATABASE_SIGNATURE;
        }
        if self.version != 1 {
            return RESULT_INVALID_DATABASE_VERSION;
        }
        if self.crc != self.generate_database_crc() {
            return RESULT_INVALID_DATABASE_CHECKSUM;
        }
        if self.database_length as usize >= MAX_MII_COUNT {
            return RESULT_INVALID_DATABASE_LENGTH;
        }
        RESULT_SUCCESS
    }

    fn generate_database_crc(&self) -> u16 {
        let data = unsafe {
            core::slice::from_raw_parts(
                (self as *const NintendoFigurineDatabase).cast::<u8>(),
                core::mem::size_of::<NintendoFigurineDatabase>() - core::mem::size_of::<u16>(),
            )
        };
        mii_util::calculate_crc16(data)
    }
}

impl Default for NintendoFigurineDatabase {
    fn default() -> Self {
        Self::new()
    }
}

const _: () = assert!(core::mem::size_of::<NintendoFigurineDatabase>() == 0x1A98);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::service::mii::mii_result::RESULT_NOT_FOUND;
    use crate::hle::service::mii::mii_types::DatabaseSessionMetadata;

    #[test]
    fn clean_database_sets_upstream_header_and_crc() {
        let database = NintendoFigurineDatabase::new();
        assert_eq!(database.magic, DATABASE_MAGIC);
        assert_eq!(database.version, 1);
        assert_eq!(database.database_length, 0);
        assert_eq!(database.check_integrity(), RESULT_SUCCESS);
    }

    #[test]
    fn get_index_by_creator_id_finds_added_entry() {
        let mut database = NintendoFigurineDatabase::new();
        let mut store_data = StoreData::new();
        store_data.build_default(0);
        let create_id = store_data.get_create_id();
        database.add(store_data);
        assert_eq!(database.get_index_by_creator_id(create_id), Some(0));
    }

    #[test]
    fn move_entry_returns_not_updated_when_indices_match() {
        let mut database = NintendoFigurineDatabase::new();
        let mut store_data = StoreData::new();
        store_data.build_default(0);
        database.add(store_data);
        assert_eq!(database.move_entry(0, 0), RESULT_NOT_UPDATED);
    }

    #[test]
    fn missing_creator_id_returns_none() {
        let database = NintendoFigurineDatabase::new();
        assert_eq!(database.get_index_by_creator_id([0; 16]), None);
        assert_eq!(RESULT_NOT_FOUND.is_error(), true);
    }

    #[test]
    fn get_count_skips_special_entries_for_non_mii_magic_metadata() {
        let mut database = NintendoFigurineDatabase::new();
        let mut special = StoreData::new();
        special.build_default(0);
        let mut normal = special;
        normal.set_type(0);
        database.add(special);
        database.add(normal);

        let metadata = DatabaseSessionMetadata::default();
        assert_eq!(database.get_count(&metadata), 1);
    }
}

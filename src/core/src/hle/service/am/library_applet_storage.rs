// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/library_applet_storage.h
//! Port of zuyu/src/core/hle/service/am/library_applet_storage.cpp

use super::am_results;
use crate::hle::kernel::k_transfer_memory::KTransferMemory;
use crate::hle::result::{ResultCode, RESULT_UNKNOWN};
use crate::memory::memory::Memory;
use std::sync::{Arc, Mutex};

fn validate_offset(offset: i64, size: usize, data_size: usize) -> Result<(), ResultCode> {
    if offset < 0 {
        return Err(am_results::RESULT_INVALID_OFFSET);
    }
    let begin = offset as usize;
    let end = begin + size;
    if begin > end || end > data_size {
        return Err(am_results::RESULT_INVALID_OFFSET);
    }
    Ok(())
}

/// Port of LibraryAppletStorage (abstract base)
pub trait LibraryAppletStorage: Send + Sync {
    fn read(&self, offset: i64, buffer: &mut [u8]) -> Result<(), ResultCode>;
    fn write(&mut self, offset: i64, buffer: &[u8]) -> Result<(), ResultCode>;
    fn get_size(&self) -> i64;
    fn get_handle_object_id(&self) -> Option<u64> {
        None
    }

    fn get_data(&self) -> Vec<u8> {
        let size = self.get_size() as usize;
        let mut data = vec![0u8; size];
        let _ = self.read(0, &mut data);
        data
    }
}

/// Port of BufferLibraryAppletStorage
pub struct BufferLibraryAppletStorage {
    data: Vec<u8>,
}

impl BufferLibraryAppletStorage {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }
}

impl LibraryAppletStorage for BufferLibraryAppletStorage {
    fn read(&self, offset: i64, buffer: &mut [u8]) -> Result<(), ResultCode> {
        validate_offset(offset, buffer.len(), self.data.len())?;
        let start = offset as usize;
        buffer.copy_from_slice(&self.data[start..start + buffer.len()]);
        Ok(())
    }

    fn write(&mut self, offset: i64, buffer: &[u8]) -> Result<(), ResultCode> {
        validate_offset(offset, buffer.len(), self.data.len())?;
        let start = offset as usize;
        self.data[start..start + buffer.len()].copy_from_slice(buffer);
        Ok(())
    }

    fn get_size(&self) -> i64 {
        self.data.len() as i64
    }
}

/// Create a buffer-backed storage.
pub fn create_storage(data: Vec<u8>) -> Box<dyn LibraryAppletStorage> {
    Box::new(BufferLibraryAppletStorage::new(data))
}

/// Port of upstream `TransferMemoryLibraryAppletStorage`.
pub struct TransferMemoryLibraryAppletStorage {
    memory: Arc<Mutex<Memory>>,
    transfer_memory: Arc<Mutex<KTransferMemory>>,
    object_id: u64,
    is_writable: bool,
    size: i64,
}

impl TransferMemoryLibraryAppletStorage {
    pub fn new(
        memory: Arc<Mutex<Memory>>,
        transfer_memory: Arc<Mutex<KTransferMemory>>,
        object_id: u64,
        is_writable: bool,
        size: i64,
    ) -> Self {
        Self {
            memory,
            transfer_memory,
            object_id,
            is_writable,
            size,
        }
    }

    fn source_address(&self) -> u64 {
        self.transfer_memory.lock().unwrap().get_source_address()
    }
}

impl LibraryAppletStorage for TransferMemoryLibraryAppletStorage {
    fn read(&self, offset: i64, buffer: &mut [u8]) -> Result<(), ResultCode> {
        validate_offset(offset, buffer.len(), self.size as usize)?;
        self.memory
            .lock()
            .unwrap()
            .read_block(self.source_address() + offset as u64, buffer);
        Ok(())
    }

    fn write(&mut self, offset: i64, buffer: &[u8]) -> Result<(), ResultCode> {
        if !self.is_writable {
            return Err(RESULT_UNKNOWN);
        }
        validate_offset(offset, buffer.len(), self.size as usize)?;
        self.memory
            .lock()
            .unwrap()
            .write_block(self.source_address() + offset as u64, buffer);
        Ok(())
    }

    fn get_size(&self) -> i64 {
        self.size
    }
}

/// Port of upstream `HandleLibraryAppletStorage`.
pub struct HandleLibraryAppletStorage {
    inner: TransferMemoryLibraryAppletStorage,
}

impl HandleLibraryAppletStorage {
    pub fn new(
        memory: Arc<Mutex<Memory>>,
        transfer_memory: Arc<Mutex<KTransferMemory>>,
        object_id: u64,
        size: i64,
    ) -> Self {
        Self {
            inner: TransferMemoryLibraryAppletStorage::new(
                memory,
                transfer_memory,
                object_id,
                true,
                size,
            ),
        }
    }
}

impl LibraryAppletStorage for HandleLibraryAppletStorage {
    fn read(&self, offset: i64, buffer: &mut [u8]) -> Result<(), ResultCode> {
        self.inner.read(offset, buffer)
    }

    fn write(&mut self, offset: i64, buffer: &[u8]) -> Result<(), ResultCode> {
        self.inner.write(offset, buffer)
    }

    fn get_size(&self) -> i64 {
        self.inner.get_size()
    }

    fn get_handle_object_id(&self) -> Option<u64> {
        Some(self.inner.object_id)
    }
}

pub fn create_transfer_memory_storage(
    memory: Arc<Mutex<Memory>>,
    transfer_memory: Arc<Mutex<KTransferMemory>>,
    object_id: u64,
    is_writable: bool,
    size: i64,
) -> Arc<Mutex<dyn LibraryAppletStorage>> {
    Arc::new(Mutex::new(TransferMemoryLibraryAppletStorage::new(
        memory,
        transfer_memory,
        object_id,
        is_writable,
        size,
    )))
}

pub fn create_handle_storage(
    memory: Arc<Mutex<Memory>>,
    transfer_memory: Arc<Mutex<KTransferMemory>>,
    object_id: u64,
    size: i64,
) -> Arc<Mutex<dyn LibraryAppletStorage>> {
    Arc::new(Mutex::new(HandleLibraryAppletStorage::new(
        memory,
        transfer_memory,
        object_id,
        size,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::SystemRef;
    use crate::device_memory::DeviceMemory;

    #[test]
    fn only_handle_storage_exposes_the_transfer_memory_handle() {
        let device_memory = Box::new(DeviceMemory::with_size(0x1000));
        let memory = Arc::new(Mutex::new(unsafe {
            Memory::new(
                SystemRef::null(),
                device_memory.as_ref() as *const _,
                &device_memory.buffer as *const _,
            )
        }));
        let transfer_memory = Arc::new(Mutex::new(KTransferMemory::new()));

        let transfer = TransferMemoryLibraryAppletStorage::new(
            Arc::clone(&memory),
            Arc::clone(&transfer_memory),
            42,
            true,
            0x1000,
        );
        assert_eq!(transfer.get_handle_object_id(), None);

        let handle = HandleLibraryAppletStorage::new(memory, transfer_memory, 42, 0x1000);
        assert_eq!(handle.get_handle_object_id(), Some(42));
    }
}

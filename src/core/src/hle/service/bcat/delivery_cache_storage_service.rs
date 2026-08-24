// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/core/hle/service/bcat/delivery_cache_storage_service.h
//! Port of zuyu/src/core/hle/service/bcat/delivery_cache_storage_service.cpp

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::bcat_types::DirectoryName;
use super::delivery_cache_directory_service::IDeliveryCacheDirectoryService;
use super::delivery_cache_file_service::IDeliveryCacheFileService;
use crate::file_sys::vfs::vfs_types::VirtualDir;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::cmif_serialization::{CmifOutArrayBuffer, CmifResponse};
use crate::hle::service::cmif_types::buffer_attr;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command IDs for IDeliveryCacheStorageService
pub mod commands {
    pub const CREATE_FILE_SERVICE: u32 = 0;
    pub const CREATE_DIRECTORY_SERVICE: u32 = 1;
    pub const ENUMERATE_DELIVERY_CACHE_DIRECTORY: u32 = 10;
}

/// IDeliveryCacheStorageService corresponds to upstream `IDeliveryCacheStorageService`.
pub struct IDeliveryCacheStorageService {
    root: VirtualDir,
    entries: Mutex<Vec<DirectoryName>>,
    next_read_index: Mutex<usize>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IDeliveryCacheStorageService {
    pub fn new(root: VirtualDir) -> Self {
        let handlers = build_handler_map(&[
            (
                commands::CREATE_FILE_SERVICE,
                Some(Self::create_file_service_handler),
                "CreateFileService",
            ),
            (
                commands::CREATE_DIRECTORY_SERVICE,
                Some(Self::create_directory_service_handler),
                "CreateDirectoryService",
            ),
            (
                commands::ENUMERATE_DELIVERY_CACHE_DIRECTORY,
                Some(Self::enumerate_delivery_cache_directory_handler),
                "EnumerateDeliveryCacheDirectory",
            ),
        ]);

        Self {
            root,
            entries: Mutex::new(Vec::new()),
            next_read_index: Mutex::new(0),
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    pub fn create_file_service(&self) -> (ResultCode, Arc<IDeliveryCacheFileService>) {
        log::debug!("IDeliveryCacheStorageService::create_file_service called");
        let service = Arc::new(IDeliveryCacheFileService::new(self.root.clone()));
        (RESULT_SUCCESS, service)
    }

    fn create_file_service_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let (result, file_service) = service.create_file_service();

        let mut response = CmifResponse::new(ctx, 2, 0, 1);
        response.push_result(result);
        response.push_interface(file_service);
    }

    pub fn create_directory_service(&self) -> (ResultCode, Arc<IDeliveryCacheDirectoryService>) {
        log::debug!("IDeliveryCacheStorageService::create_directory_service called");
        let service = Arc::new(IDeliveryCacheDirectoryService::new(self.root.clone()));
        (RESULT_SUCCESS, service)
    }

    fn create_directory_service_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let (result, directory_service) = service.create_directory_service();

        let mut response = CmifResponse::new(ctx, 2, 0, 1);
        response.push_result(result);
        response.push_interface(directory_service);
    }

    pub fn enumerate_delivery_cache_directory(
        &self,
        out_directories: &mut [DirectoryName],
    ) -> (ResultCode, i32) {
        log::debug!(
            "IDeliveryCacheStorageService::enumerate_delivery_cache_directory called, size={:016X}",
            out_directories.len()
        );

        let entries = self.entries.lock().unwrap();
        let mut next_read_index = self.next_read_index.lock().unwrap();
        let count = std::cmp::min(out_directories.len(), entries.len() - *next_read_index);
        for i in 0..count {
            out_directories[i] = entries[*next_read_index + i];
        }
        *next_read_index += count;
        (RESULT_SUCCESS, count as i32)
    }

    fn enumerate_delivery_cache_directory_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let mut out_storage = CmifOutArrayBuffer::<
            DirectoryName,
            { buffer_attr::BufferAttr_HipcMapAlias },
        >::from_ctx(ctx, 0);
        let mut out_directories = out_storage.as_out_array();
        let (result, count) = service.enumerate_delivery_cache_directory(&mut out_directories);
        out_storage.write_back(ctx, 0, count as usize);

        let mut response = CmifResponse::new(ctx, 3, 0, 0);
        response.push_result(result);
        response.push_i32(count);
    }
}

impl SessionRequestHandler for IDeliveryCacheStorageService {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "IDeliveryCacheStorageService"
    }
}

impl ServiceFramework for IDeliveryCacheStorageService {
    fn get_service_name(&self) -> &str {
        "IDeliveryCacheStorageService"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_sys::vfs::vfs_vector::VectorVfsDirectory;

    #[test]
    fn enumerate_delivery_cache_directory_is_wired_and_empty_initially() {
        let root = Arc::new(VectorVfsDirectory::new(
            Vec::new(),
            Vec::new(),
            String::new(),
            None,
        ));
        let service = IDeliveryCacheStorageService::new(root);
        assert!(
            service.handlers[&commands::ENUMERATE_DELIVERY_CACHE_DIRECTORY]
                .handler_callback
                .is_some()
        );
        assert!(service.handlers[&commands::CREATE_FILE_SERVICE]
            .handler_callback
            .is_some());
        assert!(service.handlers[&commands::CREATE_DIRECTORY_SERVICE]
            .handler_callback
            .is_some());

        let mut output = [DirectoryName::default(); 2];
        assert_eq!(
            service.enumerate_delivery_cache_directory(&mut output),
            (RESULT_SUCCESS, 0)
        );
    }
}

// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ns/document_interface.h
//! Port of zuyu/src/core/hle/service/ns/document_interface.cpp
//!
//! IDocumentInterface — document-related operations for NS.

use std::collections::BTreeMap;

use crate::file_sys::nca_metadata::ContentRecordType;
use crate::file_sys::registered_cache::{get_update_title_id, ContentProvider, ContentProviderUnion};

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use super::ns_types::ContentPath;
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for IDocumentInterface.
///
/// Corresponds to the function table in upstream document_interface.cpp.
pub mod commands {
    pub const GET_APPLICATION_CONTENT_PATH: u32 = 21;
    pub const RESOLVE_APPLICATION_CONTENT_PATH: u32 = 23;
    pub const GET_RUNNING_APPLICATION_PROGRAM_ID: u32 = 92;
}

/// IDocumentInterface.
///
/// Corresponds to `IDocumentInterface` in upstream.
pub struct IDocumentInterface {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    system: crate::core::SystemRef,
}

impl IDocumentInterface {
    pub fn new(system: crate::core::SystemRef) -> Self {
        let handlers = build_handler_map(&[
            (
                commands::GET_APPLICATION_CONTENT_PATH,
                Some(Self::get_application_content_path_handler),
                "GetApplicationContentPath",
            ),
            (
                commands::RESOLVE_APPLICATION_CONTENT_PATH,
                Some(Self::resolve_application_content_path_handler),
                "ResolveApplicationContentPath",
            ),
            (
                commands::GET_RUNNING_APPLICATION_PROGRAM_ID,
                Some(Self::get_running_application_program_id_handler),
                "GetRunningApplicationProgramId",
            ),
        ]);
        Self {
            handlers,
            handlers_tipc: BTreeMap::new(),
            system,
        }
    }

    /// Extension beyond Eden's null command 21. Switchbrew NS/NCM documents
    /// a ContentType (not NCAContentType), application ID and a 0x300-byte path.
    /// Selection belongs to NS; the provider translates only catalogued content
    /// into guest mount paths, consumed by FSP commands 8/10.
    fn application_content_path(provider: &ContentProviderUnion, content_type: u8, application_id: u64)
        -> Result<String, ResultCode>
    {
        let invalid = ResultCode::new(crate::file_sys::errors::RESULT_INVALID_ARGUMENT.raw());
        let missing = ResultCode::new(crate::file_sys::errors::RESULT_TARGET_NOT_FOUND.raw());
        let record_type = match content_type {
            0 => ContentRecordType::Meta,
            1 => ContentRecordType::Program,
            2 => ContentRecordType::Data,
            3 => ContentRecordType::Control,
            4 => ContentRecordType::HtmlDocument,
            5 => ContentRecordType::LegalInformation,
            6 => ContentRecordType::DeltaFragment,
            _ => return Err(invalid),
        };
        if application_id == 0 { return Err(invalid); }
        let update_id = get_update_title_id(application_id);
        // NS uses patch content when a patch is installed, not a mixture of
        // old base documents and a different installed application version.
        let title_id = if !provider.list_entries_filter(None, None, Some(update_id)).is_empty() {
            update_id
        } else { application_id };
        let slot = provider.get_slot_for_entry(title_id, record_type).ok_or(missing)?;
        provider.get_entry_content_path(slot, title_id, record_type).ok_or(missing)
    }

    fn get_application_content_path_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = this.as_any().downcast_ref::<Self>().unwrap();
        // Same explicit 7-byte padding and u64 offset as Resolve's ContentPath.
        let input = RequestParser::new(ctx).pop_raw::<ContentPath>();
        let missing = ResultCode::new(crate::file_sys::errors::RESULT_TARGET_NOT_FOUND.raw());
        let result = if service.system.is_null() { Err(missing) } else {
            service.system.get().get_content_provider().ok_or(missing).and_then(|provider| {
                Self::application_content_path(&provider.lock().unwrap(), input.file_system_proxy_type, input.program_id)
            })
        };
        let result = result.and_then(|path| {
            if path.len() >= 0x300 || ctx.get_write_buffer_size(0) < path.len() + 1 {
                return Err(ResultCode::new(crate::file_sys::errors::RESULT_INVALID_ARGUMENT.raw()));
            }
            let mut output = [0u8; 0x300];
            output[..path.len()].copy_from_slice(path.as_bytes());
            ctx.write_buffer(&output, 0);
            log::info!("GetApplicationContentPath: type={}, application_id={:016x}, path={path}", input.file_system_proxy_type, input.program_id);
            Ok(())
        });
        if let Err(error) = result {
            log::warn!("GetApplicationContentPath failed: type={}, application_id={:016x}, result={error:?}", input.file_system_proxy_type, input.program_id);
        }
        // The firmware document client also consumes an Out<u8> after Result
        // and forwards it to FSP OpenFileSystemWithId as ContentAttributes.
        // Catalogue entries expose full NCAs: ContentAttributes::None (0).
        // Include the extra word in the wire size, otherwise stale TLS bytes
        // become attributes even though ResponseBuilder zeroes its local buffer.
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result.map_or_else(|error| error, |_| RESULT_SUCCESS));
        rb.push_u8(0);
    }

    /// ResolveApplicationContentPath (cmd 23).
    ///
    /// Corresponds to upstream `IDocumentInterface::ResolveApplicationContentPath`.
    pub fn resolve_application_content_path(
        &self,
        file_system_proxy_type: u8,
        program_id: u64,
    ) -> Result<(), ResultCode> {
        log::warn!(
            "(STUBBED) ResolveApplicationContentPath called, file_system_proxy_type={}, program_id={:016x}",
            file_system_proxy_type,
            program_id,
        );
        Ok(())
    }

    /// GetRunningApplicationProgramId (cmd 92).
    ///
    /// Corresponds to upstream `IDocumentInterface::GetRunningApplicationProgramId`.
    pub fn get_running_application_program_id(
        &self,
        _caller_program_id: u64,
    ) -> Result<u64, ResultCode> {
        log::warn!("(STUBBED) GetRunningApplicationProgramId called");
        Ok(self.system.get().get_application_process_program_id())
    }

    fn resolve_application_content_path_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = this.as_any().downcast_ref::<Self>().unwrap();
        let path = RequestParser::new(ctx).pop_raw::<ContentPath>();
        let result = service.resolve_application_content_path(path.file_system_proxy_type, path.program_id);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result.map_or_else(|error| error, |_| RESULT_SUCCESS));
    }

    fn get_running_application_program_id_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = this.as_any().downcast_ref::<Self>().unwrap();
        let caller = RequestParser::new(ctx).pop_u64();
        match service.get_running_application_program_id(caller) {
            Ok(program_id) => {
                let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u64(program_id);
            }
            Err(error) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(error);
            }
        }
    }
}

impl SessionRequestHandler for IDocumentInterface {
    fn as_any(&self) -> &dyn std::any::Any { self }
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ns::IDocumentInterface"
    }
}

impl ServiceFramework for IDocumentInterface {
    fn get_service_name(&self) -> &str {
        "ns::IDocumentInterface"
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
    use crate::file_sys::nca_metadata::TitleType;
    use crate::file_sys::registered_cache::{ContentProviderUnionSlot, ManualContentProvider};
    use crate::file_sys::vfs::vfs_vector::VectorVfsFile;
    use std::sync::Arc;

    #[test]
    fn document_path_reply_overwrites_stale_tls_content_attributes() {
        use crate::core::{System, SystemRef};
        use crate::device_memory::DeviceMemory;
        use crate::hle::kernel::k_process::{KProcess, ProcessLock};
        use crate::hle::kernel::k_thread::{KThread, KThreadLock};
        use crate::hle::ipc;
        use crate::memory::memory::Memory;
        use common::page_table::{PageTable, PageType};
        use std::sync::Mutex;

        let device = Box::new(DeviceMemory::new());
        let memory = Arc::new(Mutex::new(unsafe {
            Memory::new(SystemRef::null(), device.as_ref(), &device.buffer)
        }));
        let mut page_table = Box::new(PageTable::new());
        page_table.resize(32, 12);
        page_table.entries.get_and_fault(3).store(
            false, PageType::Memory, 1, device.buffer.backing_base_pointer() as usize);
        memory.lock().unwrap().set_current_page_table(page_table.as_mut(), true);
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        process.lock().unwrap().page_table.set_memory(memory.clone());
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));

        let application_id = 0x0100123400000000u64;
        let mut manual = ManualContentProvider::new();
        manual.add_entry(TitleType::Application, ContentRecordType::LegalInformation, application_id,
            Arc::new(VectorVfsFile::new(Vec::new(), "11111111111111111111111111111111.nca".into(), None)));
        let mut provider = ContentProviderUnion::new();
        unsafe { provider.set_slot(ContentProviderUnionSlot::UserNAND, &mut manual); }
        let expected_path = IDocumentInterface::application_content_path(&provider, 5, application_id).unwrap();
        let mut system = Box::new(System::new());
        system.set_content_provider(Arc::new(Mutex::new(provider)));
        let service = IDocumentInterface::new(SystemRef::from_ref(&system));

        // Exercise both success and error with the same output ABI.
        for content_type in [5, 255] {
            let mut ctx = HLERequestContext::new_with_thread(thread.clone(), 0x3000);
            ctx.command_buffer_mut()[2] = content_type;
            ctx.command_buffer_mut()[4] = application_id as u32;
            ctx.command_buffer_mut()[5] = (application_id >> 32) as u32;
            ctx.set_buffer_b_descriptors_for_test(vec![ipc::BufferDescriptorABW {
                size_bits_0_31: 0x300, address_bits_0_31: 0x3100, raw_word2: 0,
            }]);
            memory.lock().unwrap().write_32(0x3020, 0x53535353);
            service.handlers()[&21].handler_callback.unwrap()(&service, &mut ctx);
            let offset = ctx.get_data_payload_offset() as usize;
            assert_eq!(ctx.command_buffer()[offset] == 0, content_type == 5);
            assert_eq!(ctx.write_size as usize, offset + 3);
            assert_eq!(ctx.write_to_outgoing_command_buffer(), RESULT_SUCCESS);
            let mem = memory.lock().unwrap();
            assert_eq!(mem.read_32(0x3000 + ((offset + 2) * 4) as u64), 0);
            if content_type == 5 {
                let mut output = [0u8; 0x300];
                mem.read_block(0x3100, &mut output);
                assert_eq!(&output[..expected_path.len()], expected_path.as_bytes());
                assert!(output[expected_path.len()..].iter().all(|byte| *byte == 0));
            }
        }
    }

    #[test]
    fn document_path_selects_installed_patch_and_preserves_content_type() {
        let application_id = 0x0100123400000000;
        let mut manual = ManualContentProvider::new();
        manual.add_entry(TitleType::Application, ContentRecordType::LegalInformation, application_id,
            Arc::new(VectorVfsFile::new(Vec::new(), "11111111111111111111111111111111.nca".into(), None)));
        let mut provider = ContentProviderUnion::new();
        unsafe { provider.set_slot(ContentProviderUnionSlot::UserNAND, &mut manual); }
        let path = IDocumentInterface::application_content_path(&provider, 5, application_id).unwrap();
        assert!(path.ends_with("11111111111111111111111111111111.nca"));
        assert!(IDocumentInterface::application_content_path(&provider, 4, application_id).is_err());
        assert!(IDocumentInterface::application_content_path(&provider, 255, application_id).is_err());
        assert!(IDocumentInterface::application_content_path(&provider, 5, 0).is_err());
        assert!(IDocumentInterface::application_content_path(&provider, 5, application_id + 1).is_err());
        manual.add_entry(TitleType::Update, ContentRecordType::LegalInformation, get_update_title_id(application_id),
            Arc::new(VectorVfsFile::new(Vec::new(), "22222222222222222222222222222222.nca".into(), None)));
        let path = IDocumentInterface::application_content_path(&provider, 5, application_id).unwrap();
        assert!(path.ends_with("22222222222222222222222222222222.nca"));
        assert!(IDocumentInterface::application_content_path(&provider, 4, application_id).is_err());
    }
}

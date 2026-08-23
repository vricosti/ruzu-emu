// SPDX-FileCopyrightText: 2016 Citra Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ipc_helpers.h
//! Status: Structural port
//!
//! Contains:
//! - ResultSessionClosed: IPC result code for closed sessions
//! - RequestHelperBase: base for request parsing/building
//! - ResponseBuilder: builds IPC responses
//! - RequestParser: parses IPC requests

use crate::hle::ipc;
use crate::hle::result::{ErrorModule, ResultCode};
use crate::hle::service::hle_ipc::HLERequestContext;

/// Result code indicating a session has been closed.
pub const RESULT_SESSION_CLOSED: ResultCode =
    ResultCode::from_module_description(ErrorModule::HIPC, 301);

/// Flags used for customizing the behavior of ResponseBuilder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ResponseBuilderFlags {
    None = 0,
    /// Uses move handles to move objects in the response, even when in a domain.
    AlwaysMoveHandles = 1,
}

/// Builds IPC response messages.
///
/// Corresponds to upstream `IPC::ResponseBuilder`.
pub struct ResponseBuilder<'a> {
    context: &'a mut HLERequestContext,
    index: usize,
    // Eden retains these constructor values as ResponseBuilder members even
    // though no method reads them after construction. Keep the exact state
    // boundary for structural parity instead of deleting or renaming it.
    #[allow(dead_code)]
    normal_params_size: u32,
    #[allow(dead_code)]
    num_handles_to_copy: u32,
    #[allow(dead_code)]
    num_objects_to_move: u32,
    #[allow(dead_code)]
    data_payload_index: usize,
}

impl<'a> ResponseBuilder<'a> {
    /// Creates a new ResponseBuilder.
    ///
    /// # Arguments
    /// * `ctx` - The HLE request context.
    /// * `normal_params_size` - Number of normal parameter words.
    /// * `num_handles_to_copy` - Number of handles to copy.
    /// * `num_objects_to_move` - Number of objects to move (domain objects or move handles).
    pub fn new(
        ctx: &'a mut HLERequestContext,
        normal_params_size: u32,
        num_handles_to_copy: u32,
        num_objects_to_move: u32,
    ) -> Self {
        Self::new_with_flags(
            ctx,
            normal_params_size,
            num_handles_to_copy,
            num_objects_to_move,
            ResponseBuilderFlags::None,
        )
    }

    /// Creates a new ResponseBuilder with flags.
    pub fn new_with_flags(
        ctx: &'a mut HLERequestContext,
        normal_params_size: u32,
        num_handles_to_copy: u32,
        num_objects_to_move: u32,
        flags: ResponseBuilderFlags,
    ) -> Self {
        // Zero the command buffer.
        for i in 0..ipc::COMMAND_BUFFER_LENGTH {
            ctx.cmd_buf[i] = 0;
        }

        let is_tipc = ctx.is_tipc();
        let is_domain = ctx
            .get_manager()
            .map_or(false, |m| m.lock().unwrap().is_domain());

        let mut raw_data_size = if is_tipc {
            normal_params_size.wrapping_sub(1)
        } else {
            normal_params_size
        };
        ctx.write_size = raw_data_size;

        let always_move = (flags as u32 & ResponseBuilderFlags::AlwaysMoveHandles as u32) != 0;

        let num_handles_to_move;
        let num_domain_objects;
        if !is_domain || always_move {
            num_handles_to_move = num_objects_to_move;
            num_domain_objects = 0u32;
        } else {
            num_handles_to_move = 0;
            num_domain_objects = num_objects_to_move;
        }

        if is_domain {
            raw_data_size += (core::mem::size_of::<ipc::DomainMessageHeader>() as u32)
                / (core::mem::size_of::<u32>() as u32)
                + num_domain_objects;
            ctx.write_size += num_domain_objects;
        }

        if !is_tipc {
            raw_data_size += (core::mem::size_of::<ipc::DataPayloadHeader>() as u32)
                / (core::mem::size_of::<u32>() as u32)
                + 4
                + normal_params_size;
        }

        let mut index = 0usize;

        // Build CommandHeader.
        let mut header = ipc::CommandHeader {
            raw_low: 0,
            raw_high: 0,
        };

        if is_tipc {
            // Assign the original command type back.
            let raw_type = ctx.get_command_type() as u32;
            header.raw_low = assign_bits(header.raw_low, raw_type, 0, 16);
        }

        // data_size in bits [0, 10) of raw_high.
        header.raw_high = assign_bits(header.raw_high, raw_data_size, 0, 10);

        if num_handles_to_copy > 0 || num_handles_to_move > 0 {
            // enable_handle_descriptor at bit 31 of raw_high.
            header.raw_high |= 1 << 31;
        }

        ctx.cmd_buf[index] = header.raw_low;
        ctx.cmd_buf[index + 1] = header.raw_high;
        index += 2;

        if header.enable_handle_descriptor() {
            let mut hdh_raw = 0u32;
            hdh_raw = assign_bits(hdh_raw, num_handles_to_copy, 1, 4);
            hdh_raw = assign_bits(hdh_raw, num_handles_to_move, 5, 4);
            ctx.cmd_buf[index] = hdh_raw;
            index += 1;

            ctx.handles_offset = index as u32;

            // Skip space for handles.
            let total_handles = (num_handles_to_copy + num_handles_to_move) as usize;
            index += total_handles;
        }

        if !is_tipc {
            // Align to 16 bytes (4 words).
            if index & 3 != 0 {
                let padding = 4 - (index & 3);
                index += padding;
            }

            // Domain message header.
            if is_domain && ctx.has_domain_message_header() {
                let mut domain_header = ipc::DomainMessageHeader::default();
                domain_header.set_num_objects(num_domain_objects);
                ctx.cmd_buf[index] = domain_header.raw[0];
                ctx.cmd_buf[index + 1] = domain_header.raw[1];
                ctx.cmd_buf[index + 2] = domain_header.raw[2];
                ctx.cmd_buf[index + 3] = domain_header.raw[3];
                index += 4;
            }

            // Data payload header.
            // SFCO magic = 'S','F','C','O'
            let sfco_magic = u32::from_le_bytes([b'S', b'F', b'C', b'O']);
            ctx.cmd_buf[index] = sfco_magic;
            ctx.cmd_buf[index + 1] = 0; // padding
            index += 2;
        }

        let data_payload_index = index;
        ctx.data_payload_offset = index as u32;
        ctx.write_size += index as u32;
        ctx.domain_offset = (index as u32) + raw_data_size / (core::mem::size_of::<u32>() as u32);

        if std::env::var_os("RUZU_TRACE_RB").is_some() {
            eprintln!(
                "[RB] N={} hc={} mv={} tipc={} domain={} raw_data_size={} index={} write_size={} data_payload_offset={}",
                normal_params_size,
                num_handles_to_copy,
                num_objects_to_move,
                is_tipc,
                is_domain,
                raw_data_size,
                index,
                ctx.write_size,
                ctx.data_payload_offset
            );
        }

        Self {
            context: ctx,
            index,
            normal_params_size,
            num_handles_to_copy,
            num_objects_to_move,
            data_payload_index,
        }
    }

    /// Push a ResultCode value. Result codes are 64-bit in the IPC buffer (high part is discarded).
    pub fn push_result(&mut self, value: ResultCode) {
        self.push_u32(value.get_inner_value());
        self.push_u32(0);
    }

    /// Push a copy handle into the outgoing copy objects list.
    ///
    /// Matches upstream `ResponseBuilder::PushCopyObjects(O* ptr)`.
    /// Upstream stores the KAutoObject* pointer; handle creation is deferred
    /// to WriteToOutgoingCommandBuffer() where handle_table.Add() is called.
    /// We store a KAutoObjectRef; handle resolution happens in WriteToOutgoing.
    pub fn push_copy_objects(&mut self, handle: u32) {
        use super::hle_ipc::KAutoObjectRef;
        self.context
            .outgoing_copy_objects
            .push(KAutoObjectRef::Handle(handle));
    }

    /// Push a copy object by kernel object id.
    ///
    /// Matches upstream `PushCopyObjects(O* ptr)` more closely than pre-resolving
    /// a process handle in the service implementation.
    pub fn push_copy_object_id(&mut self, object_id: u64) {
        use super::hle_ipc::KAutoObjectRef;
        self.context
            .outgoing_copy_objects
            .push(KAutoObjectRef::ObjectId(object_id));
    }

    /// Push a move handle into the outgoing move objects list.
    ///
    /// Matches upstream `ResponseBuilder::PushMoveObjects(O* ptr)`.
    /// Upstream stores the KAutoObject* pointer and calls object->Close()
    /// after handle_table.Add() in WriteToOutgoingCommandBuffer().
    pub fn push_move_objects(&mut self, handle: u32) {
        use super::hle_ipc::KAutoObjectRef;
        self.context
            .outgoing_move_objects
            .push(KAutoObjectRef::Handle(handle));
    }

    /// Push a move object by kernel object id.
    ///
    /// Matches the upstream ownership model more closely than pre-resolving a
    /// handle in the service implementation: the response stores the object,
    /// and the final handle translation happens when writing the outgoing IPC
    /// buffer.
    pub fn push_move_object_id(&mut self, object_id: u64) {
        use super::hle_ipc::KAutoObjectRef;
        self.context
            .outgoing_move_objects
            .push(KAutoObjectRef::ObjectId(object_id));
    }

    /// Push an IPC interface (service object) as a move handle or domain object.
    ///
    /// Matches upstream `ResponseBuilder::PushIpcInterface<T>(shared_ptr<T>)`.
    ///
    /// In non-domain mode: creates a new KSession with a SessionRequestManager linked
    /// to the parent's ServerManager, registers it, and adds the client session as a
    /// move handle.
    ///
    /// In domain mode: adds the service object as a domain object.
    pub fn push_ipc_interface(
        &mut self,
        iface: std::sync::Arc<dyn super::hle_ipc::SessionRequestHandler>,
    ) {
        let is_domain = self
            .context
            .get_manager()
            .map_or(false, |m| m.lock().unwrap().is_domain());

        if is_domain {
            self.context.add_domain_object(iface);
        } else {
            // Non-domain: create a new session matching upstream PushIpcInterface exactly.
            //
            // Upstream:
            //   auto next_manager = make_shared<SessionRequestManager>(kernel, manager->GetServerManager());
            //   next_manager->SetSessionHandler(iface);
            //   manager->GetServerManager().RegisterSession(&session->GetServerSession(), next_manager);
            //   context->AddMoveObject(&session->GetClientSession());

            // Get the parent manager's ServerManager reference + pending-
            // registration queue + wakeup_event, all in one lock acquisition.
            let (parent_server_manager, parent_queue, parent_wakeup) = self
                .context
                .get_manager()
                .map(|m| {
                    let guard = m.lock().unwrap();
                    (
                        guard.get_server_manager(),
                        guard.pending_registrations().cloned(),
                        guard.server_wakeup().cloned(),
                    )
                })
                .unwrap_or((None, None, None));

            // Create child manager linked to same ServerManager. Propagate
            // the queue + wakeup so this child can register grandchild
            // sessions without locking `Mutex<ServerManager>`.
            let child_manager = std::sync::Arc::new(std::sync::Mutex::new(
                super::hle_ipc::SessionRequestManager::from_parent_server_manager_endpoints(
                    parent_server_manager.clone(),
                    parent_queue.clone(),
                    parent_wakeup.clone(),
                ),
            ));
            child_manager.lock().unwrap().set_session_handler(iface);

            // Create session and keep the client-session object unresolved until
            // WriteToOutgoingCommandBuffer(), matching upstream PushIpcInterface.
            let object_id = self
                .context
                .create_session_with_manager_object_id(child_manager.clone())
                .unwrap_or(0);

            self.context
                .register_last_created_session_with_manager(child_manager);
            // Suppress warning about unused parent_server_manager — we read it
            // from the parent's SessionRequestManager above but the local
            // variable computed earlier is no longer needed.
            let _ = parent_server_manager;

            self.push_move_object_id(object_id);
        }
    }

    /// Push a u32 value.
    pub fn push_u32(&mut self, value: u32) {
        if self.index < ipc::COMMAND_BUFFER_LENGTH {
            self.context.cmd_buf[self.index] = value;
            self.index += 1;
        }
    }

    /// Push a u64 value (little-endian, low word first).
    pub fn push_u64(&mut self, value: u64) {
        self.push_u32(value as u32);
        self.push_u32((value >> 32) as u32);
    }

    /// Push a u16 value (occupies one word, upper bits zeroed).
    pub fn push_u16(&mut self, value: u16) {
        self.push_raw_bytes(&value.to_le_bytes());
    }

    /// Push a u8 value.
    pub fn push_u8(&mut self, value: u8) {
        self.push_raw_bytes(&[value]);
    }

    /// Push a bool value (as u8).
    pub fn push_bool(&mut self, value: bool) {
        self.push_u8(value as u8);
    }

    /// Push a f32 value.
    pub fn push_f32(&mut self, value: f32) {
        self.push_u32(value.to_bits());
    }

    /// Push a i32 value.
    pub fn push_i32(&mut self, value: i32) {
        self.push_u32(value as u32);
    }

    /// Push a i64 value.
    pub fn push_i64(&mut self, value: i64) {
        self.push_u64(value as u64);
    }

    /// Push raw bytes, advancing the index by ceil(len/4) words.
    pub fn push_raw_bytes(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let start = self.index;
        let words_needed = (data.len() + 3) / 4;
        if start + words_needed > ipc::COMMAND_BUFFER_LENGTH {
            return;
        }
        // Zero the target words first.
        for i in 0..words_needed {
            self.context.cmd_buf[start + i] = 0;
        }
        // Copy byte-by-byte into the command buffer.
        let buf_bytes: &mut [u8] = unsafe {
            core::slice::from_raw_parts_mut(
                self.context.cmd_buf[start..].as_mut_ptr() as *mut u8,
                words_needed * 4,
            )
        };
        buf_bytes[..data.len()].copy_from_slice(data);
        self.index += words_needed;
    }

    /// Push a trivially-copyable struct as raw data.
    pub fn push_raw<T: Copy>(&mut self, value: &T) {
        let bytes = unsafe {
            core::slice::from_raw_parts(value as *const T as *const u8, core::mem::size_of::<T>())
        };
        self.push_raw_bytes(bytes);
    }

    /// Returns the current write offset (in words) into the command buffer.
    pub fn get_current_offset(&self) -> usize {
        self.index
    }
}

/// Parses IPC request messages.
///
/// Corresponds to upstream `IPC::RequestParser`.
pub struct RequestParser<'a> {
    context: &'a HLERequestContext,
    index: usize,
}

impl<'a> RequestParser<'a> {
    /// Creates a new RequestParser from an HLERequestContext.
    pub fn new(ctx: &'a HLERequestContext) -> Self {
        let mut index = 0usize;

        // TIPC does not have data payload offset.
        if !ctx.is_tipc() {
            let offset = ctx.get_data_payload_offset() as usize;
            index = offset;
        }

        // Skip the u64 command id (2 words).
        index += 2;

        Self {
            context: ctx,
            index,
        }
    }

    /// Creates a new RequestParser from a raw command buffer.
    pub fn from_buffer(ctx: &'a HLERequestContext) -> Self {
        Self {
            context: ctx,
            index: 0,
        }
    }

    /// Pop a u32 value.
    pub fn pop_u32(&mut self) -> u32 {
        let value = self.context.cmd_buf[self.index];
        self.index += 1;
        value
    }

    /// Pop a u64 value.
    pub fn pop_u64(&mut self) -> u64 {
        let lsw = self.pop_u32() as u64;
        let msw = self.pop_u32() as u64;
        (msw << 32) | lsw
    }

    /// Pop a u16 value.
    pub fn pop_u16(&mut self) -> u16 {
        self.pop_raw::<u16>()
    }

    /// Pop a u8 value.
    pub fn pop_u8(&mut self) -> u8 {
        self.pop_raw::<u8>()
    }

    /// Pop a bool value.
    pub fn pop_bool(&mut self) -> bool {
        self.pop_u8() != 0
    }

    /// Pop a i32 value.
    pub fn pop_i32(&mut self) -> i32 {
        self.pop_u32() as i32
    }

    /// Pop a i64 value.
    pub fn pop_i64(&mut self) -> i64 {
        self.pop_u64() as i64
    }

    /// Pop a f32 value.
    pub fn pop_f32(&mut self) -> f32 {
        let bits = self.pop_u32();
        f32::from_bits(bits)
    }

    /// Pop a f64 value.
    pub fn pop_f64(&mut self) -> f64 {
        let bits = self.pop_u64();
        f64::from_bits(bits)
    }

    /// Pop a Result value.
    pub fn pop_result(&mut self) -> ResultCode {
        ResultCode::new(self.pop_u32())
    }

    /// Pop a raw trivially-copyable struct.
    pub fn pop_raw<T: Copy + Default>(&mut self) -> T {
        let size = core::mem::size_of::<T>();
        let words = (size + 3) / 4;
        let mut value = T::default();
        if self.index + words <= ipc::COMMAND_BUFFER_LENGTH {
            unsafe {
                let src = self.context.cmd_buf[self.index..].as_ptr() as *const u8;
                let dst = &mut value as *mut T as *mut u8;
                core::ptr::copy_nonoverlapping(src, dst, size);
            }
        }
        self.index += words;
        value
    }

    /// Skip a number of words.
    pub fn skip(&mut self, count: usize) {
        self.index += count;
    }

    /// Get the current offset.
    pub fn get_current_offset(&self) -> usize {
        self.index
    }

    /// Set the current offset.
    pub fn set_current_offset(&mut self, offset: usize) {
        self.index = offset;
    }

    /// Align the current position forward to a 16-byte boundary.
    pub fn align_with_padding(&mut self) {
        if self.index & 3 != 0 {
            self.index += 4 - (self.index & 3);
        }
    }

    /// Align the current raw-data position forward to the natural alignment of `T`.
    ///
    /// CMIF raw input/output data is laid out with per-argument natural alignment, not
    /// just packed word-by-word. This helper matches the upstream serialization rule used
    /// by `cmif_serialization.h`.
    pub fn align_for<T>(&mut self) {
        let align_words = core::mem::align_of::<T>() / core::mem::size_of::<u32>();
        if align_words > 1 {
            let rem = self.index % align_words;
            if rem != 0 {
                self.index += align_words - rem;
            }
        }
    }
}

/// Assign bits into a u32 value at position with given width.
fn assign_bits(value: u32, field: u32, position: usize, bits: usize) -> u32 {
    let mask = ((1u32 << bits) - 1) << position;
    (value & !mask) | ((field << position) & mask)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::SystemRef;
    use crate::device_memory::DeviceMemory;
    use crate::hle::kernel::k_process::{KProcess, ProcessLock};
    use crate::hle::kernel::k_thread::{KThread, KThreadLock};
    use crate::hle::result::{ResultCode, RESULT_SUCCESS};
    use crate::hle::service::hle_ipc::{SessionRequestHandler, SessionRequestManager};
    use crate::memory::memory::Memory;
    use common::page_table::{PageTable, PageType};
    use std::sync::{Arc, Mutex};

    struct MappedResponseMemory {
        _device_memory: Box<DeviceMemory>,
        _page_table: Box<PageTable>,
        memory: Arc<Mutex<Memory>>,
    }

    impl MappedResponseMemory {
        fn new(mapped_start: u64, mapped_size: usize) -> Self {
            const PAGE_BITS: usize = 12;
            const PAGE_SIZE: usize = 1 << PAGE_BITS;

            let device_memory = Box::new(DeviceMemory::new());
            let buffer_ptr = &device_memory.buffer as *const common::host_memory::HostMemory;
            let memory = Arc::new(Mutex::new(unsafe {
                Memory::new(
                    SystemRef::null(),
                    device_memory.as_ref() as *const _,
                    buffer_ptr,
                )
            }));

            let mut page_table = Box::new(PageTable::new());
            page_table.resize(32, PAGE_BITS);
            let base_page = (mapped_start as usize) / PAGE_SIZE;
            let num_pages = mapped_size.div_ceil(PAGE_SIZE);
            let host_base = device_memory.buffer.backing_base_pointer() as usize;
            for page in base_page..base_page + num_pages {
                let target = (page * PAGE_SIZE) as u64;
                page_table.map_pages(
                    page,
                    1,
                    target,
                    PageType::Memory,
                    host_base + target as usize,
                );
            }

            memory
                .lock()
                .unwrap()
                .set_current_page_table(page_table.as_mut() as *mut PageTable, true);

            Self {
                _device_memory: device_memory,
                _page_table: page_table,
                memory,
            }
        }
    }

    struct TestHandler;

    impl SessionRequestHandler for TestHandler {
        fn handle_sync_request(&self, _context: &mut HLERequestContext) -> ResultCode {
            RESULT_SUCCESS
        }

        fn service_name(&self) -> &str {
            "TestHandler"
        }
    }

    #[test]
    fn test_result_session_closed() {
        assert!(RESULT_SESSION_CLOSED.is_error());
        assert_eq!(RESULT_SESSION_CLOSED.get_module(), ErrorModule::HIPC);
        assert_eq!(RESULT_SESSION_CLOSED.get_description(), 301);
    }

    #[test]
    fn test_assign_bits() {
        let value = 0u32;
        let result = assign_bits(value, 5, 4, 4);
        assert_eq!(result, 5 << 4);
    }

    #[test]
    fn test_response_builder_basic() {
        let mut ctx = HLERequestContext::new();
        {
            let mut rb = ResponseBuilder::new(&mut ctx, 2, 0, 0);
            rb.push_result(RESULT_SUCCESS);
        }
        // The command buffer should have been written to.
        // Basic smoke test that it doesn't panic.
    }

    #[test]
    fn push_ipc_interface_writes_move_handle_into_response() {
        let fixture = MappedResponseMemory::new(0x4000, 0x1000);
        let memory = fixture.memory.clone();

        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        {
            let mut process_guard = process.lock().unwrap();
            assert_eq!(
                process_guard.ensure_handle_table_initialized(),
                RESULT_SUCCESS.get_inner_value()
            );
            process_guard.page_table.set_memory(memory.clone());
        }

        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));

        let tls_address = 0x4000;

        let mut ctx = HLERequestContext::new_with_thread(thread, tls_address);
        let manager = Arc::new(Mutex::new(SessionRequestManager::new()));
        ctx.set_session_request_manager(manager);

        let object_id;
        {
            let mut rb = ResponseBuilder::new(&mut ctx, 2, 0, 1);
            rb.push_result(RESULT_SUCCESS);
            rb.push_ipc_interface(Arc::new(TestHandler));
        }

        assert_eq!(ctx.outgoing_move_objects.len(), 1);
        object_id = match &ctx.outgoing_move_objects[0] {
            crate::hle::service::hle_ipc::KAutoObjectRef::ObjectId(object_id) => *object_id,
            _ => panic!("expected object-backed move object"),
        };
        assert_ne!(object_id, 0);

        assert_eq!(ctx.write_to_outgoing_command_buffer(), RESULT_SUCCESS);

        let mem = memory.lock().unwrap();
        let handle_word = mem.read_32(tls_address + 12);
        assert_ne!(handle_word, 0);
        assert!(process
            .lock()
            .unwrap()
            .handle_table
            .get_object(handle_word)
            .is_some());
        assert_eq!(
            process.lock().unwrap().handle_table.get_object(handle_word),
            Some(object_id)
        );
    }

    #[test]
    fn push_copy_object_id_writes_copy_handle_into_response() {
        use crate::hle::kernel::k_readable_event::KReadableEvent;
        use crate::hle::kernel::k_typed_address::KProcessAddress;

        let fixture = MappedResponseMemory::new(0x4000, 0x1000);
        let memory = fixture.memory.clone();

        let mut process = KProcess::new();
        process.process_id = 1;
        assert_eq!(
            process.ensure_handle_table_initialized(),
            RESULT_SUCCESS.get_inner_value()
        );
        process.page_table.set_memory(memory);
        let process = Arc::new(ProcessLock::from_value(process));

        let tls_address = 0x4000;
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut thread = thread.lock().unwrap();
            thread.tls_address = KProcessAddress::new(tls_address);
            thread.parent = Some(Arc::downgrade(&process));
        }

        let mut ctx = HLERequestContext::new_with_thread(Arc::clone(&thread), tls_address);

        let readable_event = Arc::new(Mutex::new(KReadableEvent::new()));
        readable_event.lock().unwrap().object_id = 0x2000_0001;
        let object_id = ctx
            .register_readable_event_object(Arc::clone(&readable_event))
            .unwrap();

        {
            let mut rb = ResponseBuilder::new(&mut ctx, 2, 1, 0);
            rb.push_result(RESULT_SUCCESS);
            rb.push_copy_object_id(object_id);
        }

        assert_eq!(ctx.outgoing_copy_objects.len(), 1);
        assert!(matches!(
            ctx.outgoing_copy_objects[0],
            crate::hle::service::hle_ipc::KAutoObjectRef::ObjectId(id) if id == object_id
        ));

        assert_eq!(ctx.write_to_outgoing_command_buffer(), RESULT_SUCCESS);

        let mem = ctx
            .owner_process_arc()
            .unwrap()
            .lock()
            .unwrap()
            .get_memory()
            .unwrap();
        let mem = mem.lock().unwrap();
        let handle_word = mem.read_32(tls_address + 12);
        assert_ne!(handle_word, 0);
        assert_eq!(
            process.lock().unwrap().handle_table.get_object(handle_word),
            Some(object_id)
        );
    }

    #[test]
    fn request_parser_align_for_u64_skips_single_word_after_0x34_blob() {
        let mut ctx = HLERequestContext::new();
        let start = 12usize;
        ctx.cmd_buf[start + 13] = 0;
        ctx.cmd_buf[start + 14] = 0x0005_B000;
        ctx.cmd_buf[start + 15] = 0;
        ctx.cmd_buf[start + 16] = 0x51;
        ctx.cmd_buf[start + 17] = 0;

        #[derive(Clone, Copy)]
        struct RawBlob([u8; 0x34]);

        impl Default for RawBlob {
            fn default() -> Self {
                Self([0; 0x34])
            }
        }

        let mut rp = RequestParser::from_buffer(&ctx);
        rp.set_current_offset(start);
        let blob: RawBlob = rp.pop_raw();
        assert_eq!(blob.0, [0; 0x34]);
        assert_eq!(rp.get_current_offset(), start + 13);
        rp.align_for::<u64>();
        assert_eq!(rp.get_current_offset(), start + 14);
        assert_eq!(rp.pop_u64(), 0x0005_B000);
        assert_eq!(rp.pop_u64(), 0x51);
    }
}

// SPDX-FileCopyrightText: 2021 yuzu Emulator Project
// SPDX-FileCopyrightText: 2021 Skyline Team and Contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/core/hle/service/nvdrv/nvdrv_interface.h
//! Port of zuyu/src/core/hle/service/nvdrv/nvdrv_interface.cpp

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use super::core::container::SessionId;
use super::nvdata::*;
use super::nvdrv::Module;
use crate::hle::kernel::k_process::ProcessLock;
use crate::hle::kernel::svc_common::PseudoHandle;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for NVDRV:
/// - 0: Open
/// - 1: Ioctl
/// - 2: Close
/// - 3: Initialize
/// - 4: QueryEvent
/// - 5: MapSharedMem (nullptr upstream)
/// - 6: GetStatus
/// - 7: SetAruidForTest (nullptr upstream)
/// - 8: SetAruid
/// - 9: DumpGraphicsMemoryInfo
/// - 10: InitializeDevtools (nullptr upstream)
/// - 11: Ioctl2
/// - 12: Ioctl3
/// - 13: SetGraphicsFirmwareMemoryMarginEnabled
pub struct NvdrvInterface {
    nvdrv: Arc<Module>,
    pid: u64,
    is_initialized: bool,
    session_id: SessionId,
    output_buffer: Vec<u8>,
    inline_output_buffer: Vec<u8>,
}

impl NvdrvInterface {
    fn should_trace_lifecycle() -> bool {
        std::env::var_os("RUZU_NVDRV_TRACE").is_some_and(|value| value != std::ffi::OsStr::new("0"))
    }

    fn should_trace_ioctl_fp() -> bool {
        std::env::var_os("RUZU_TRACE_IOCTL_FP")
            .is_some_and(|value| value != std::ffi::OsStr::new("0"))
    }

    fn should_trace_ioctl_payload(command: Ioctl) -> bool {
        matches!(
            command.raw,
            0xC0B0_4705
                | 0xC018_4706
                | 0x8004_4701
                | 0x8028_4702
                | 0x4028_4109
                | 0xC040_4108
                | 0xC018_4102
                | 0xC028_4106
                | 0xC028_4808
                | 0xC010_480B
                | 0xC008_0101
                | 0xC008_0103
                | 0xC008_010E
                | 0xC020_0104
        )
    }

    fn format_prefix_hex(bytes: &[u8], limit: usize) -> String {
        bytes
            .iter()
            .take(limit)
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn format_ioctl_fp_hex(bytes: &[u8]) -> String {
        let mut hex = String::new();
        for (i, byte) in bytes.iter().take(0x40).enumerate() {
            if i > 0 && (i & 3) == 0 {
                hex.push(' ');
            }
            hex.push_str(&format!("{:02x}", byte));
        }
        hex
    }

    fn read_le_u64_prefix(bytes: &[u8], offset: usize) -> Option<u64> {
        let end = offset.checked_add(8)?;
        let slice = bytes.get(offset..end)?;
        Some(u64::from_le_bytes(slice.try_into().ok()?))
    }

    fn read_le_u32_prefix(bytes: &[u8], offset: usize) -> u32 {
        let Some(slice) = bytes.get(offset..offset.saturating_add(4)) else {
            return 0;
        };
        if slice.len() != 4 {
            return 0;
        }
        u32::from_le_bytes(slice.try_into().unwrap())
    }

    pub fn new(nvdrv: Arc<Module>) -> Self {
        Self {
            nvdrv,
            pid: 0,
            is_initialized: false,
            session_id: SessionId { id: 0 },
            output_buffer: Vec::new(),
            inline_output_buffer: Vec::new(),
        }
    }

    pub fn get_module(&self) -> Arc<Module> {
        Arc::clone(&self.nvdrv)
    }

    pub fn is_initialized(&self) -> bool {
        self.is_initialized
    }

    /// Port of NVDRV::Open
    pub fn open(&mut self, device_name: &str) -> (DeviceFD, NvResult) {
        if Self::should_trace_lifecycle() {
            log::info!(
                "NVDRV::Open state self_ptr={:p} service_session_id={} initialized={} device={}",
                self,
                self.session_id.id,
                self.is_initialized,
                device_name
            );
        }
        if !self.is_initialized {
            log::error!("NvServices is not initialized!");
            return (0, NvResult::NotInitialized);
        }

        if device_name == "/dev/nvhost-prof-gpu" {
            log::warn!("/dev/nvhost-prof-gpu cannot be opened in production");
            return (0, NvResult::NotSupported);
        }

        let fd = self.nvdrv.open(device_name, self.session_id);
        let result = if fd != INVALID_NVDRV_FD {
            NvResult::Success
        } else {
            NvResult::FileOperationFailed
        };
        if Self::should_trace_lifecycle() {
            log::info!(
                "NVDRV::Open device_name={} -> fd={} nv_result={:?}",
                device_name,
                fd,
                result
            );
        }
        (fd, result)
    }

    /// Port of NVDRV::Ioctl1
    pub fn ioctl1(
        &mut self,
        fd: DeviceFD,
        command: Ioctl,
        input: &[u8],
        output_size: usize,
    ) -> NvResult {
        if Self::should_trace_lifecycle() {
            log::info!(
                "NVDRV::Ioctl1 state self_ptr={:p} service_session_id={} initialized={} fd={} ioctl=0x{:08X}",
                self,
                self.session_id.id,
                self.is_initialized,
                fd,
                command.raw
            );
        }
        log::trace!("Ioctl1 called fd={}, ioctl=0x{:08X}", fd, command.raw);
        if !self.is_initialized {
            log::error!("NvServices is not initialized!");
            return NvResult::NotInitialized;
        }

        self.output_buffer.resize(output_size, 0);
        self.output_buffer.fill(0);

        if Self::should_trace_ioctl_payload(command) {
            log::trace!(
                "Ioctl1 input fd={} ioctl=0x{:08X} len=0x{:X} bytes=[{}]",
                fd,
                command.raw,
                input.len(),
                Self::format_prefix_hex(input, 0x40)
            );
        }
        let nv_result = self
            .nvdrv
            .ioctl1(fd, command, input, &mut self.output_buffer);
        log::trace!(
            "Ioctl1 result fd={}, ioctl=0x{:08X}, nv_result={:?}",
            fd,
            command.raw,
            nv_result
        );
        if Self::should_trace_ioctl_fp() {
            eprintln!(
                "[IOCTL_FP] fd={} ioctl=0x{:08X} out_sz=0x{:x} nv_result=0x{:x} head={}",
                fd,
                command.raw,
                self.output_buffer.len(),
                nv_result as u32,
                Self::format_ioctl_fp_hex(&self.output_buffer)
            );
        }
        if Self::should_trace_ioctl_payload(command) {
            log::trace!(
                "Ioctl1 output fd={} ioctl=0x{:08X} len=0x{:X} bytes=[{}]",
                fd,
                command.raw,
                self.output_buffer.len(),
                Self::format_prefix_hex(&self.output_buffer, 0x40)
            );
            if std::env::var_os("RUZU_IOCTL_PAYLOAD_DUMP").is_some() {
                log::info!(
                    "IOCTL1_OUTPUT fd={} ioctl=0x{:08X} len=0x{:X} bytes=[{}]",
                    fd,
                    command.raw,
                    self.output_buffer.len(),
                    Self::format_prefix_hex(&self.output_buffer, 0x40)
                );
            }
        }
        nv_result
    }

    /// Port of NVDRV::Ioctl2
    pub fn ioctl2(
        &mut self,
        fd: DeviceFD,
        command: Ioctl,
        input: &[u8],
        inline_input: &[u8],
        output_size: usize,
    ) -> NvResult {
        if Self::should_trace_lifecycle() {
            log::info!(
                "NVDRV::Ioctl2 state self_ptr={:p} service_session_id={} initialized={} fd={} ioctl=0x{:08X}",
                self,
                self.session_id.id,
                self.is_initialized,
                fd,
                command.raw
            );
        }
        log::trace!("Ioctl2 called fd={}, ioctl=0x{:08X}", fd, command.raw);
        if !self.is_initialized {
            log::error!("NvServices is not initialized!");
            return NvResult::NotInitialized;
        }

        self.output_buffer.resize(output_size, 0);
        self.output_buffer.fill(0);

        if Self::should_trace_ioctl_payload(command) {
            log::trace!(
                "Ioctl2 input fd={} ioctl=0x{:08X} len0=0x{:X} len1=0x{:X} bytes0=[{}] bytes1=[{}]",
                fd,
                command.raw,
                input.len(),
                inline_input.len(),
                Self::format_prefix_hex(input, 0x40),
                Self::format_prefix_hex(inline_input, 0x40)
            );
        }
        let nv_result =
            self.nvdrv
                .ioctl2(fd, command, input, inline_input, &mut self.output_buffer);
        log::trace!(
            "Ioctl2 result fd={}, ioctl=0x{:08X}, nv_result={:?}",
            fd,
            command.raw,
            nv_result
        );
        if Self::should_trace_ioctl_payload(command) {
            log::trace!(
                "Ioctl2 output fd={} ioctl=0x{:08X} len=0x{:X} bytes=[{}]",
                fd,
                command.raw,
                self.output_buffer.len(),
                Self::format_prefix_hex(&self.output_buffer, 0x40)
            );
        }
        nv_result
    }

    /// Port of NVDRV::Ioctl3
    pub fn ioctl3(
        &mut self,
        fd: DeviceFD,
        command: Ioctl,
        input: &[u8],
        output_size: usize,
        inline_output_size: usize,
    ) -> NvResult {
        if Self::should_trace_lifecycle() {
            log::info!(
                "NVDRV::Ioctl3 state self_ptr={:p} service_session_id={} initialized={} fd={} ioctl=0x{:08X}",
                self,
                self.session_id.id,
                self.is_initialized,
                fd,
                command.raw
            );
        }
        log::trace!("Ioctl3 called fd={}, ioctl=0x{:08X}", fd, command.raw);
        if !self.is_initialized {
            log::error!("NvServices is not initialized!");
            return NvResult::NotInitialized;
        }

        self.output_buffer.resize(output_size, 0);
        self.output_buffer.fill(0);
        self.inline_output_buffer.resize(inline_output_size, 0);
        self.inline_output_buffer.fill(0);

        if Self::should_trace_ioctl_payload(command) {
            log::trace!(
                "Ioctl3 input fd={} ioctl=0x{:08X} len=0x{:X} bytes=[{}]",
                fd,
                command.raw,
                input.len(),
                Self::format_prefix_hex(input, 0x40)
            );
        }
        let nv_result = self.nvdrv.ioctl3(
            fd,
            command,
            input,
            &mut self.output_buffer,
            &mut self.inline_output_buffer,
        );
        log::trace!(
            "Ioctl3 result fd={}, ioctl=0x{:08X}, nv_result={:?}",
            fd,
            command.raw,
            nv_result
        );
        if Self::should_trace_ioctl_payload(command) {
            log::trace!(
                "Ioctl3 output fd={} ioctl=0x{:08X} len0=0x{:X} len1=0x{:X} bytes0=[{}] bytes1=[{}]",
                fd,
                command.raw,
                self.output_buffer.len(),
                self.inline_output_buffer.len(),
                Self::format_prefix_hex(&self.output_buffer, 0x40),
                Self::format_prefix_hex(&self.inline_output_buffer, 0x40)
            );
        }
        nv_result
    }

    /// Port of NVDRV::Close
    pub fn close(&mut self, fd: DeviceFD) -> NvResult {
        if Self::should_trace_lifecycle() {
            log::info!(
                "NVDRV::Close state self_ptr={:p} service_session_id={} initialized={} fd={}",
                self,
                self.session_id.id,
                self.is_initialized,
                fd
            );
        }
        log::debug!("Close called");
        if !self.is_initialized {
            log::error!("NvServices is not initialized!");
            return NvResult::NotInitialized;
        }

        self.nvdrv.close(fd)
    }

    /// Port of NVDRV::Initialize
    pub fn initialize(&mut self, process: &Arc<ProcessLock>) -> NvResult {
        log::warn!("(STUBBED) Initialize called");
        if Self::should_trace_lifecycle() {
            let process_id = process.lock().unwrap().process_id;
            log::info!(
                "NVDRV::Initialize begin self_ptr={:p} service_session_id={} initialized={} process_id={}",
                self,
                self.session_id.id,
                self.is_initialized,
                process_id
            );
        }
        if self.is_initialized {
            return NvResult::Success;
        }

        let container = self.nvdrv.get_container();
        self.session_id = container.open_session(process);
        self.is_initialized = true;
        if Self::should_trace_lifecycle() {
            log::info!(
                "NVDRV::Initialize end self_ptr={:p} service_session_id={} initialized={}",
                self,
                self.session_id.id,
                self.is_initialized
            );
        }
        NvResult::Success
    }

    /// Port of NVDRV::QueryEvent
    pub fn query_event(
        &self,
        fd: DeviceFD,
        event_id: u32,
    ) -> (
        NvResult,
        Option<Arc<Mutex<crate::hle::kernel::k_readable_event::KReadableEvent>>>,
    ) {
        if Self::should_trace_lifecycle() {
            log::info!(
                "NVDRV::QueryEvent state self_ptr={:p} service_session_id={} initialized={} fd={} event_id={}",
                self,
                self.session_id.id,
                self.is_initialized,
                fd,
                event_id
            );
        }
        if !self.is_initialized {
            log::error!("NvServices is not initialized!");
            return (NvResult::NotInitialized, None);
        }

        let result = self.nvdrv.query_event(fd, event_id);
        if Self::should_trace_lifecycle() {
            if let (nv_result, Some(event)) = &result {
                let object_id = event.lock().unwrap().object_id;
                log::info!(
                    "NVDRV::QueryEvent result fd={} event_id={} nv_result={:?} readable_event_object_id={}",
                    fd,
                    event_id,
                    nv_result,
                    object_id
                );
            } else {
                log::info!(
                    "NVDRV::QueryEvent result fd={} event_id={} nv_result={:?} readable_event_object_id=<none>",
                    fd,
                    event_id,
                    result.0
                );
            }
        }

        result
    }

    /// Port of NVDRV::SetAruid
    pub fn set_aruid(&mut self, pid: u64) {
        self.pid = pid;
        log::warn!("(STUBBED) SetAruid called, pid=0x{:X}", pid);
    }

    /// Port of NVDRV::SetGraphicsFirmwareMemoryMarginEnabled
    pub fn set_graphics_firmware_memory_margin_enabled(&self) {
        log::warn!("(STUBBED) SetGraphicsFirmwareMemoryMarginEnabled called");
    }

    /// Port of NVDRV::GetStatus
    pub fn get_status(&self) -> NvResult {
        log::warn!("(STUBBED) GetStatus called");
        NvResult::Success
    }

    /// Port of NVDRV::DumpGraphicsMemoryInfo
    pub fn dump_graphics_memory_info(&self) {
        log::debug!("DumpGraphicsMemoryInfo called");
    }
}

impl Drop for NvdrvInterface {
    fn drop(&mut self) {
        if self.is_initialized {
            let container = self.nvdrv.get_container();
            container.close_session(self.session_id);
        }
    }
}

pub struct NvdrvService {
    name: String,
    interface: Mutex<NvdrvInterface>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl NvdrvService {
    fn should_trace_lifecycle() -> bool {
        std::env::var_os("RUZU_NVDRV_TRACE").is_some_and(|value| value != std::ffi::OsStr::new("0"))
    }

    pub fn new(nvdrv: Arc<Module>, name: &str) -> Self {
        Self {
            name: name.to_string(),
            interface: Mutex::new(NvdrvInterface::new(nvdrv)),
            handlers: build_handler_map(&[
                (0, Some(Self::open_handler), "Open"),
                (1, Some(Self::ioctl1_handler), "Ioctl"),
                (2, Some(Self::close_handler), "Close"),
                (3, Some(Self::initialize_handler), "Initialize"),
                (4, Some(Self::query_event_handler), "QueryEvent"),
                (6, Some(Self::get_status_handler), "GetStatus"),
                (8, Some(Self::set_aruid_handler), "SetAruid"),
                (
                    9,
                    Some(Self::dump_graphics_memory_info_handler),
                    "DumpGraphicsMemoryInfo",
                ),
                (11, Some(Self::ioctl2_handler), "Ioctl2"),
                (12, Some(Self::ioctl3_handler), "Ioctl3"),
                (
                    13,
                    Some(Self::set_graphics_firmware_memory_margin_enabled_handler),
                    "SetGraphicsFirmwareMemoryMarginEnabled",
                ),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    pub fn get_module(&self) -> Arc<Module> {
        self.interface.lock().unwrap().get_module()
    }

    // read_buffer / write_buffer removed — use ctx.read_buffer() / ctx.write_buffer()
    // from HLERequestContext (matches upstream where these are methods on HLERequestContext,
    // not on individual service classes).

    fn push_nv_result(ctx: &mut HLERequestContext, nv_result: NvResult) {
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(nv_result as u32);
    }

    fn service_error(ctx: &mut HLERequestContext, nv_result: NvResult) {
        Self::push_nv_result(ctx, nv_result);
    }

    fn open_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const NvdrvService) };
        if Self::should_trace_lifecycle() {
            log::info!(
                "NVDRV::open_handler service_ptr={:p} name={}",
                service,
                service.name
            );
        }
        if !service.interface.lock().unwrap().is_initialized() {
            Self::service_error(ctx, NvResult::NotInitialized);
            log::error!("NvServices is not initialized!");
            return;
        }

        let device_name_bytes = ctx.read_buffer(0);
        let end = device_name_bytes
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(device_name_bytes.len());
        let device_name = String::from_utf8_lossy(&device_name_bytes[..end]).to_string();

        let (fd, nv_result) = service.interface.lock().unwrap().open(&device_name);

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(fd);
        rb.push_u32(nv_result as u32);
    }

    fn ioctl1_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const NvdrvService) };
        if Self::should_trace_lifecycle() {
            log::info!(
                "NVDRV::ioctl1_handler service_ptr={:p} name={}",
                service,
                service.name
            );
        }
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();
        let command = rp.pop_raw::<Ioctl>();
        super::super::hle_ipc::IPC_TRACE_CURRENT.with(|c| {
            let mut slot = c.borrow_mut();
            slot.2 = command.raw;
        });

        if !service.interface.lock().unwrap().is_initialized() {
            Self::service_error(ctx, NvResult::NotInitialized);
            log::error!("NvServices is not initialized!");
            return;
        }

        let input = ctx.read_buffer(0);
        let write_size = ctx.get_write_buffer_size(0);
        record_nvdrv_ioctl_history(ctx, 1, fd, command, &input, write_size, 0);
        let nvmap_alloc_address = if command.raw == 0xC020_0104 {
            NvdrvInterface::read_le_u64_prefix(&input, 24)
        } else {
            None
        };
        let trace_iocalloc_context = std::env::var_os("RUZU_TRACE_IOCALLOC_CTX").is_some();
        if trace_iocalloc_context && command.raw == 0xC020_0104 {
            let (tid, pc, lr, sp) = ctx
                .get_thread()
                .map(|thread| {
                    let guard = thread.lock().unwrap();
                    (
                        guard.get_thread_id(),
                        guard.thread_context.pc,
                        guard.thread_context.lr,
                        guard.thread_context.sp,
                    )
                })
                .unwrap_or((0, 0, 0, 0));
            let a_desc = ctx
                .buffer_descriptor_a()
                .get(0)
                .map(|desc| (desc.address(), desc.size()));
            let x_desc = ctx
                .buffer_descriptor_x()
                .get(0)
                .map(|desc| (desc.address(), desc.size()));
            let b_desc = ctx
                .buffer_descriptor_b()
                .get(0)
                .map(|desc| (desc.address(), desc.size()));
            let c_desc = ctx
                .buffer_descriptor_c()
                .get(0)
                .map(|desc| (desc.address(), desc.size()));
            eprintln!(
                "[IOCALLOC_CTX] tid={} fd={} pc=0x{:X} lr=0x{:X} sp=0x{:X} tls=0x{:X} ioctl=0x{:08X} in_addr=0x{:X} in_len=0x{:X} write_size=0x{:X} is_out={} A0={:?} X0={:?} B0={:?} C0={:?} input={}",
                tid,
                fd,
                pc,
                lr,
                sp,
                ctx.tls_address(),
                command.raw,
                nvmap_alloc_address.unwrap_or(0),
                input.len(),
                write_size,
                command.is_out(),
                a_desc,
                x_desc,
                b_desc,
                c_desc,
                NvdrvInterface::format_ioctl_fp_hex(&input),
            );
            if nvmap_alloc_address.is_some_and(|address| address < 0x4000_0000) {
                dump_iocalloc_guest_words(ctx, sp.saturating_sub(0x80), 0x140, "stack");
                if let Some((addr, _)) = x_desc.or(c_desc) {
                    dump_iocalloc_guest_words(ctx, addr.saturating_sub(0x40), 0xC0, "ioctl-buffer");
                }
            }
        }
        log::trace!(
            "NVDRV::ioctl1_handler dispatch fd={} ioctl=0x{:08X} in_len=0x{:X} out_len=0x{:X}",
            fd,
            command.raw,
            input.len(),
            write_size
        );
        // `RUZU_TRACE_IOCTL_TID=N` — log each Ioctl1 invocation from tid=N with
        // the resolved ioctl_nr. Used in Phase-2 zuyu/ruzu diff to identify
        // in zuyu. One-liner per call; env-gated so default has zero cost.
        if let Some(filter) = std::env::var_os("RUZU_TRACE_IOCTL_TID") {
            if let Ok(target_tid) = filter.to_string_lossy().parse::<u64>() {
                let tid = ctx
                    .get_thread()
                    .map(|t| t.lock().unwrap().get_thread_id())
                    .unwrap_or(0);
                if tid == target_tid {
                    log::warn!(
                        "[IOCTL_TID] tid={} fd={} ioctl=0x{:08X} in_len=0x{:X} out_len=0x{:X}",
                        tid,
                        fd,
                        command.raw,
                        input.len(),
                        write_size
                    );
                }
            }
        }
        // Stash the ioctl number into IPC_TRACE_CURRENT so the IPC_REPLY hex-dump
        // can attribute this reply to the specific nvdrv ioctl (not just "cmd=1").
        let _ioctl_t0 = if nvdrv_ioctl_profile_enabled() {
            Some(std::time::Instant::now())
        } else {
            None
        };
        let mut interface = service.interface.lock().unwrap();
        let nv_result = interface.ioctl1(fd, command, &input, write_size);
        let output = &interface.output_buffer;
        record_nvdrv_ioctl_history_result(ctx, 1, fd, command, nv_result, &output, &[]);
        if let Some(t0) = _ioctl_t0 {
            record_nvdrv_ioctl(command.raw, fd, 1, t0.elapsed());
        }
        log::trace!(
            "NVDRV::ioctl1_handler return fd={} ioctl=0x{:08X} nv_result={:?}",
            fd,
            command.raw,
            nv_result
        );
        if command.raw == 0x4028_4109 {
            let in_preview_len = input.len().min(40);
            let out_preview_len = output.len().min(40);
            log::debug!(
                "NVDRV::Ioctl1 0x40284109 fd={} input_prefix={:02X?} output_prefix={:02X?} nv_result={:?}",
                fd,
                &input[..in_preview_len],
                &output[..out_preview_len],
                nv_result,
            );
        }
        if command.raw == 0xC018_4102 {
            let preview_len = output.len().min(24);
            log::debug!(
                "NVDRV::Ioctl1 0xC0184102 fd={} write_size=0x{:X} is_out={} nv_result={:?} output_prefix={:02X?}",
                fd,
                write_size,
                command.is_out(),
                nv_result,
                &output[..preview_len]
            );
        }
        if command.raw == 0xC040_4108 {
            let preview_len = output.len().min(48);
            let b_desc = ctx
                .buffer_descriptor_b()
                .get(0)
                .map(|d| (d.address(), d.size()));
            let c_desc = ctx
                .buffer_descriptor_c()
                .get(0)
                .map(|d| (d.address(), d.size()));
            log::debug!(
                "NVDRV::Ioctl1 0xC0404108 fd={} write_size=0x{:X} is_out={} can_write={} b_desc={:?} c_desc={:?} nv_result={:?} output_prefix={:02X?}",
                fd,
                write_size,
                command.is_out(),
                ctx.can_write_buffer(0),
                b_desc,
                c_desc,
                nv_result,
                &output[..preview_len]
            );
        }
        if command.raw == 0xC020_0104 {
            let preview_len = output.len().min(32);
            let b_desc = ctx
                .buffer_descriptor_b()
                .get(0)
                .map(|d| (d.address(), d.size()));
            let c_desc = ctx
                .buffer_descriptor_c()
                .get(0)
                .map(|d| (d.address(), d.size()));
            log::debug!(
                "NVDRV::Ioctl1 0xC0200104 fd={} write_size=0x{:X} is_out={} can_write={} b_desc={:?} c_desc={:?} nv_result={:?} output_prefix={:02X?}",
                fd,
                write_size,
                command.is_out(),
                ctx.can_write_buffer(0),
                b_desc,
                c_desc,
                nv_result,
                &output[..preview_len]
            );
        }
        if command.raw == 0xC028_4106 {
            let in_preview_len = input.len().min(40);
            let out_preview_len = output.len().min(40);
            log::debug!(
                "NVDRV::Ioctl1 0xC0284106 fd={} write_size=0x{:X} is_out={} nv_result={:?} input_prefix={:02X?} output_prefix={:02X?}",
                fd,
                write_size,
                command.is_out(),
                nv_result,
                &input[..in_preview_len],
                &output[..out_preview_len]
            );
        }
        if command.is_out() {
            let written = ctx.write_buffer(&output, 0);
            if command.raw == 0xC020_0104 {
                log::debug!(
                    "NVDRV::Ioctl1 0xC0200104 wrote {} bytes to guest buffer",
                    written
                );
            }
        }
        Self::push_nv_result(ctx, nv_result);
    }

    fn ioctl2_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const NvdrvService) };
        if Self::should_trace_lifecycle() {
            log::info!(
                "NVDRV::ioctl2_handler service_ptr={:p} name={}",
                service,
                service.name
            );
        }
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();
        let command = rp.pop_raw::<Ioctl>();
        super::super::hle_ipc::IPC_TRACE_CURRENT.with(|c| {
            let mut slot = c.borrow_mut();
            slot.2 = command.raw;
        });

        if !service.interface.lock().unwrap().is_initialized() {
            Self::service_error(ctx, NvResult::NotInitialized);
            log::error!("NvServices is not initialized!");
            return;
        }

        let input = ctx.read_buffer(0);
        let inline_input = ctx.read_buffer(1);
        let write_size = ctx.get_write_buffer_size(0);
        record_nvdrv_ioctl_history(ctx, 2, fd, command, &input, write_size, inline_input.len());
        log::trace!(
            "NVDRV::ioctl2_handler dispatch fd={} ioctl=0x{:08X} in_len=0x{:X} inline_in_len=0x{:X} out_len=0x{:X}",
            fd,
            command.raw,
            input.len(),
            inline_input.len(),
            write_size
        );
        let _ioctl_t0 = if nvdrv_ioctl_profile_enabled() {
            Some(std::time::Instant::now())
        } else {
            None
        };
        let mut interface = service.interface.lock().unwrap();
        let nv_result = interface.ioctl2(fd, command, &input, &inline_input, write_size);
        let output = &interface.output_buffer;
        record_nvdrv_ioctl_history_result(ctx, 2, fd, command, nv_result, &output, &[]);
        if let Some(t0) = _ioctl_t0 {
            record_nvdrv_ioctl(command.raw, fd, 2, t0.elapsed());
        }
        log::trace!(
            "NVDRV::ioctl2_handler return fd={} ioctl=0x{:08X} nv_result={:?}",
            fd,
            command.raw,
            nv_result
        );
        if command.is_out() {
            ctx.write_buffer(&output, 0);
        }
        Self::push_nv_result(ctx, nv_result);
    }

    fn ioctl3_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const NvdrvService) };
        if Self::should_trace_lifecycle() {
            log::info!(
                "NVDRV::ioctl3_handler service_ptr={:p} name={}",
                service,
                service.name
            );
        }
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();
        let command = rp.pop_raw::<Ioctl>();
        super::super::hle_ipc::IPC_TRACE_CURRENT.with(|c| {
            let mut slot = c.borrow_mut();
            slot.2 = command.raw;
        });

        if !service.interface.lock().unwrap().is_initialized() {
            Self::service_error(ctx, NvResult::NotInitialized);
            log::error!("NvServices is not initialized!");
            return;
        }

        let input = ctx.read_buffer(0);
        let write_size = ctx.get_write_buffer_size(0);
        let inline_write_size = ctx.get_write_buffer_size(1);
        record_nvdrv_ioctl_history(ctx, 3, fd, command, &input, write_size, inline_write_size);
        log::trace!(
            "NVDRV::ioctl3_handler dispatch fd={} ioctl=0x{:08X} in_len=0x{:X} out_len=0x{:X} inline_out_len=0x{:X}",
            fd,
            command.raw,
            input.len(),
            write_size,
            inline_write_size
        );
        let _ioctl_t0 = if nvdrv_ioctl_profile_enabled() {
            Some(std::time::Instant::now())
        } else {
            None
        };
        let mut interface = service.interface.lock().unwrap();
        let nv_result = interface.ioctl3(fd, command, &input, write_size, inline_write_size);
        let output = &interface.output_buffer;
        let inline_output = &interface.inline_output_buffer;
        record_nvdrv_ioctl_history_result(ctx, 3, fd, command, nv_result, &output, &inline_output);
        if let Some(t0) = _ioctl_t0 {
            record_nvdrv_ioctl(command.raw, fd, 3, t0.elapsed());
        }
        log::trace!(
            "NVDRV::ioctl3_handler return fd={} ioctl=0x{:08X} nv_result={:?}",
            fd,
            command.raw,
            nv_result
        );
        if command.is_out() {
            ctx.write_buffer(&output, 0);
            ctx.write_buffer(&inline_output, 1);
        }
        Self::push_nv_result(ctx, nv_result);
    }

    fn close_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const NvdrvService) };
        if Self::should_trace_lifecycle() {
            log::info!(
                "NVDRV::close_handler service_ptr={:p} name={}",
                service,
                service.name
            );
        }
        if !service.interface.lock().unwrap().is_initialized() {
            Self::service_error(ctx, NvResult::NotInitialized);
            log::error!("NvServices is not initialized!");
            return;
        }

        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();
        let nv_result = service.interface.lock().unwrap().close(fd);
        Self::push_nv_result(ctx, nv_result);
    }

    fn initialize_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const NvdrvService) };
        if Self::should_trace_lifecycle() {
            log::info!(
                "NVDRV::initialize_handler service_ptr={:p} name={}",
                service,
                service.name
            );
        }
        let mut rp = RequestParser::new(ctx);
        let process_handle = ctx.get_copy_handle(0);
        let _transfer_memory_handle = ctx.get_copy_handle(1);
        let _transfer_memory_size = rp.pop_u32();
        let nv_result = match Self::resolve_process_from_handle(ctx, process_handle) {
            Some(process) => service.interface.lock().unwrap().initialize(&process),
            None => {
                log::error!(
                    "NVDRV::Initialize could not resolve process handle {:#x}",
                    process_handle
                );
                NvResult::BadParameter
            }
        };
        Self::push_nv_result(ctx, nv_result);
    }

    fn resolve_process_from_handle(
        ctx: &HLERequestContext,
        process_handle: u32,
    ) -> Option<Arc<ProcessLock>> {
        let thread = ctx.get_thread()?;
        let parent = {
            let thread_guard = thread.lock().unwrap();
            thread_guard.parent.as_ref()?.upgrade()?
        };

        if process_handle == PseudoHandle::CurrentProcess as u32 || process_handle == 0 {
            return Some(parent);
        }

        let is_valid = {
            let parent_guard = parent.lock().unwrap();
            parent_guard
                .handle_table
                .get_object(process_handle)
                .is_some()
        };

        if !is_valid {
            log::error!(
                "NVDRV::Initialize handle {:#x} is not valid in the current process handle table",
                process_handle
            );
            return None;
        }

        Some(parent)
    }

    fn query_event_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const NvdrvService) };
        if Self::should_trace_lifecycle() {
            log::info!(
                "NVDRV::query_event_handler service_ptr={:p} name={}",
                service,
                service.name
            );
        }
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();
        let event_id = rp.pop_u32();
        let interface = service.interface.lock().unwrap();
        let (nv_result, maybe_event) = interface.query_event(fd, event_id);
        drop(interface);
        match (nv_result, maybe_event) {
            (NvResult::Success, Some(event)) => {
                if let Some(object_id) = ctx.register_readable_event_object(event) {
                    let mut rb = ResponseBuilder::new(ctx, 3, 1, 0);
                    rb.push_result(RESULT_SUCCESS);
                    rb.push_copy_object_id(object_id);
                    rb.push_u32(NvResult::Success as u32);
                } else {
                    log::error!("NVDRV::QueryEvent could not register readable event object");
                    Self::push_nv_result(ctx, NvResult::BadParameter);
                }
            }
            (result, _) => {
                Self::push_nv_result(ctx, result);
            }
        }
    }

    fn set_aruid_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const NvdrvService) };
        let mut rp = RequestParser::new(ctx);
        let pid = rp.pop_u64();
        service.interface.lock().unwrap().set_aruid(pid);
        Self::push_nv_result(ctx, NvResult::Success);
    }

    fn get_status_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const NvdrvService) };
        let result = service.interface.lock().unwrap().get_status();
        Self::push_nv_result(ctx, result);
    }

    fn dump_graphics_memory_info_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const NvdrvService) };
        service
            .interface
            .lock()
            .unwrap()
            .dump_graphics_memory_info();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn set_graphics_firmware_memory_margin_enabled_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const NvdrvService) };
        service
            .interface
            .lock()
            .unwrap()
            .set_graphics_firmware_memory_margin_enabled();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }
}

impl SessionRequestHandler for NvdrvService {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }

    fn service_name(&self) -> &str {
        &self.name
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl ServiceFramework for NvdrvService {
    fn get_service_name(&self) -> &str {
        &self.name
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

fn dump_iocalloc_guest_words(ctx: &HLERequestContext, start: u64, bytes: usize, label: &str) {
    let Some(memory) = ctx.get_memory() else {
        eprintln!("[IOCALLOC_DUMP] {} start=0x{:X}: <no-memory>", label, start);
        return;
    };
    let Ok(memory) = memory.try_lock() else {
        eprintln!(
            "[IOCALLOC_DUMP] {} start=0x{:X}: <guest-memory-lock-busy>",
            label, start
        );
        return;
    };

    let words = bytes / 4;
    for chunk_start in (0..words).step_by(8) {
        eprint!(
            "[IOCALLOC_DUMP] {} 0x{:08X}:",
            label,
            start + (chunk_start as u64) * 4
        );
        for index in chunk_start..(chunk_start + 8).min(words) {
            eprint!(" {:08X}", memory.read_32(start + (index as u64) * 4));
        }
        eprintln!();
    }
}

#[derive(Clone, Copy)]
struct NvdrvIoctlHistoryEvent {
    sequence: u64,
    kind: u8,
    fd: i32,
    ioctl: u32,
    guest_tid: u64,
    pc: u64,
    lr: u64,
    input_len: usize,
    output_len: usize,
    inline_len: usize,
    words: [u32; 8],
}

static NVDRV_IOCTL_HISTORY: std::sync::OnceLock<Mutex<VecDeque<NvdrvIoctlHistoryEvent>>> =
    std::sync::OnceLock::new();
static NVDRV_IOCTL_HISTORY_SEQUENCE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
const NVDRV_IOCTL_HISTORY_CAPACITY: usize = 4096;

fn record_nvdrv_ioctl_history(
    ctx: &HLERequestContext,
    kind: u8,
    fd: i32,
    command: Ioctl,
    input: &[u8],
    output_len: usize,
    inline_len: usize,
) {
    let (guest_tid, pc, lr) = ctx
        .get_thread()
        .map(|thread| {
            let guard = thread.lock().unwrap();
            (
                guard.get_thread_id(),
                guard.thread_context.pc,
                guard.thread_context.lr,
            )
        })
        .unwrap_or((0, 0, 0));
    let mut words = [0u32; 8];
    for (i, word) in words.iter_mut().enumerate() {
        *word = NvdrvInterface::read_le_u32_prefix(input, i * 4);
    }
    push_nvdrv_ioctl_history(NvdrvIoctlHistoryEvent {
        sequence: NVDRV_IOCTL_HISTORY_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1,
        kind,
        fd,
        ioctl: command.raw,
        guest_tid,
        pc,
        lr,
        input_len: input.len(),
        output_len,
        inline_len,
        words,
    });
}

fn record_nvdrv_ioctl_history_result(
    ctx: &HLERequestContext,
    kind: u8,
    fd: i32,
    command: Ioctl,
    result: NvResult,
    output: &[u8],
    inline_output: &[u8],
) {
    let (guest_tid, pc, lr) = ctx
        .get_thread()
        .map(|thread| {
            let guard = thread.lock().unwrap();
            (
                guard.get_thread_id(),
                guard.thread_context.pc,
                guard.thread_context.lr,
            )
        })
        .unwrap_or((0, 0, 0));
    let mut words = [0u32; 8];
    words[0] = result as u32;
    let payload = if !output.is_empty() {
        output
    } else {
        inline_output
    };
    if command.raw == 0xC028_4106 {
        // IoctlMapBufferEx's returned GPU offset is at byte 0x20, outside the
        // generic seven-word preview. Pack the fields needed for ruzu/zuyu
        // trace comparison into the fixed history payload.
        words[1] = NvdrvInterface::read_le_u32_prefix(payload, 8); // handle
        words[2] = NvdrvInterface::read_le_u32_prefix(payload, 0); // flags
        words[3] = NvdrvInterface::read_le_u32_prefix(payload, 4); // kind
        words[4] = NvdrvInterface::read_le_u32_prefix(payload, 12); // page_size
        words[5] = NvdrvInterface::read_le_u32_prefix(payload, 24); // mapping_size low
        words[6] = NvdrvInterface::read_le_u32_prefix(payload, 32); // offset low
        words[7] = NvdrvInterface::read_le_u32_prefix(payload, 36); // offset high
    } else {
        for (i, word) in words.iter_mut().enumerate().skip(1) {
            *word = NvdrvInterface::read_le_u32_prefix(payload, (i - 1) * 4);
        }
    }
    push_nvdrv_ioctl_history(NvdrvIoctlHistoryEvent {
        sequence: NVDRV_IOCTL_HISTORY_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1,
        kind: kind | 0x80,
        fd,
        ioctl: command.raw,
        guest_tid,
        pc,
        lr,
        input_len: 0,
        output_len: output.len(),
        inline_len: inline_output.len(),
        words,
    });
}

fn push_nvdrv_ioctl_history(event: NvdrvIoctlHistoryEvent) {
    let mut history = NVDRV_IOCTL_HISTORY
        .get_or_init(|| Mutex::new(VecDeque::with_capacity(NVDRV_IOCTL_HISTORY_CAPACITY)))
        .lock()
        .unwrap();
    if history.len() == NVDRV_IOCTL_HISTORY_CAPACITY {
        history.pop_front();
    }
    history.push_back(event);
}

pub fn dump_nvdrv_ioctl_history(reason: &str) {
    let Some(history) = NVDRV_IOCTL_HISTORY.get() else {
        return;
    };
    let history = history.lock().unwrap();
    eprintln!(
        "[NVDRV_IOCTL_HISTORY] reason={} events={}",
        reason,
        history.len()
    );
    for event in history.iter() {
        eprintln!(
            "[NVDRV_IOCTL_HISTORY] #{:05} kind={} fd={} ioctl=0x{:08X} tid={} pc=0x{:X} lr=0x{:X} in=0x{:X} out=0x{:X} inline=0x{:X} words={:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X}",
            event.sequence,
            event.kind,
            event.fd,
            event.ioctl,
            event.guest_tid,
            event.pc,
            event.lr,
            event.input_len,
            event.output_len,
            event.inline_len,
            event.words[0],
            event.words[1],
            event.words[2],
            event.words[3],
            event.words[4],
            event.words[5],
            event.words[6],
            event.words[7],
        );
    }
}

// =============================================================================
// RUZU_PROFILE_NVDRV_IOCTL: per-(ioctl_nr, fd, kind) wall-clock profile
// =============================================================================
//
// Identifies which nvdrv Ioctl handler is the per-call latency hot path. Set
// `RUZU_PROFILE_NVDRV_IOCTL=1` and send `SIGUSR2` (or rely on atexit) to dump.
//
// Output (top entries by total time):
//   [NVDRV_IOCTL_PROFILE] ioctl=0xC0304808 kind=1 count=691 total=4836.7ms avg=7000us max=20100us
//
// "nvdrv handle 0xB01EC consumes 66% of game runtime at avg 7ms/call". This
// profiler answers "which specific ioctl_nr blows the budget".

#[derive(Default, Clone)]
struct NvdrvIoctlAgg {
    count: u64,
    total_ns: u64,
    max_ns: u64,
}

static NVDRV_IOCTL_PROFILE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<(u32, i32, u8), NvdrvIoctlAgg>>,
> = std::sync::OnceLock::new();

fn nvdrv_ioctl_profile_enabled() -> bool {
    std::env::var_os("RUZU_PROFILE_NVDRV_IOCTL").is_some()
}

fn record_nvdrv_ioctl(ioctl_nr: u32, fd: i32, kind: u8, elapsed: std::time::Duration) {
    let agg =
        NVDRV_IOCTL_PROFILE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let ns = elapsed.as_nanos() as u64;
    let mut g = agg.lock().unwrap();
    let entry = g.entry((ioctl_nr, fd, kind)).or_default();
    entry.count += 1;
    entry.total_ns = entry.total_ns.saturating_add(ns);
    if ns > entry.max_ns {
        entry.max_ns = ns;
    }
}

pub fn dump_nvdrv_ioctl_profile() {
    let Some(agg) = NVDRV_IOCTL_PROFILE.get() else {
        return;
    };
    let entries: Vec<((u32, i32, u8), NvdrvIoctlAgg)> = {
        let g = agg.lock().unwrap();
        g.iter().map(|(k, v)| (*k, v.clone())).collect()
    };
    if entries.is_empty() {
        eprintln!("[NVDRV_IOCTL_PROFILE] no samples (RUZU_PROFILE_NVDRV_IOCTL=1?)");
        return;
    }
    let mut by_ioctl: std::collections::HashMap<u32, NvdrvIoctlAgg> =
        std::collections::HashMap::new();
    for ((nr, _fd, _kind), e) in entries.iter() {
        let merged = by_ioctl.entry(*nr).or_default();
        merged.count += e.count;
        merged.total_ns = merged.total_ns.saturating_add(e.total_ns);
        if e.max_ns > merged.max_ns {
            merged.max_ns = e.max_ns;
        }
    }
    let mut by_ioctl: Vec<(u32, NvdrvIoctlAgg)> = by_ioctl.into_iter().collect();
    by_ioctl.sort_by_key(|(_, e)| std::cmp::Reverse(e.total_ns));
    eprintln!("[NVDRV_IOCTL_PROFILE] top ioctl_nr by total time:");
    for (nr, e) in by_ioctl.iter().take(20) {
        eprintln!(
            "[NVDRV_IOCTL_PROFILE]   ioctl=0x{:08X} count={:<5} total={:>8.2}ms avg={:>8.1}us max={:>8.1}us",
            nr,
            e.count,
            e.total_ns as f64 / 1e6,
            e.total_ns as f64 / e.count as f64 / 1e3,
            e.max_ns as f64 / 1e3,
        );
    }

    let mut entries = entries;
    entries.sort_by_key(|(_, e)| std::cmp::Reverse(e.total_ns));
    eprintln!("[NVDRV_IOCTL_PROFILE] top 20 (ioctl_nr, fd, kind) by total time:");
    for ((nr, fd, kind), e) in entries.iter().take(20) {
        eprintln!(
            "[NVDRV_IOCTL_PROFILE]   ioctl=0x{:08X} fd={:<3} kind={} count={:<5} total={:>8.2}ms avg={:>8.1}us max={:>8.1}us",
            nr,
            fd,
            kind,
            e.count,
            e.total_ns as f64 / 1e6,
            e.total_ns as f64 / e.count as f64 / 1e3,
            e.max_ns as f64 / 1e3,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{Ioctl, Module, NvResult, NvdrvInterface};
    use crate::core::{System, SystemRef};

    #[test]
    fn ioctl_output_buffers_are_service_owned_and_zeroed() {
        let system = System::new_for_test();
        let module = Module::new(SystemRef::from_ref(&system));
        let mut interface = NvdrvInterface::new(module);
        interface.is_initialized = true;
        interface.output_buffer = vec![0xA5; 16];
        interface.inline_output_buffer = vec![0x5A; 16];

        assert_eq!(
            interface.ioctl3(-1, Ioctl { raw: 0 }, &[], 8, 4),
            NvResult::InvalidState
        );
        assert_eq!(interface.output_buffer, vec![0; 8]);
        assert_eq!(interface.inline_output_buffer, vec![0; 4]);

        std::mem::forget(system);
    }
}

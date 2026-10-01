// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/nvdrv/devices/nvhost_gpu.h
//! Port of zuyu/src/core/hle/service/nvdrv/devices/nvhost_gpu.cpp

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::core::SystemRef;
use crate::gpu_core::{
    GpuChannelHandle, GpuCommandHeader, GpuCommandList, GpuCommandListHeader,
    GpuMemoryManagerHandle,
};
use crate::hle::kernel::k_readable_event::KReadableEvent;
use crate::hle::service::nvdrv::core::container::Container;
use crate::hle::service::nvdrv::core::container::SessionId;
use crate::hle::service::nvdrv::core::syncpoint_manager::SyncpointManager;
use crate::hle::service::nvdrv::devices::nvdevice::NvDevice;
use crate::hle::service::nvdrv::devices::nvmap::{read_struct, write_struct};
use crate::hle::service::nvdrv::nvdata::*;
use crate::hle::service::nvdrv::nvdrv::EventInterface;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtxObjects {
    Ctx2D = 0x902D,
    Ctx3D = 0xB197,
    CtxCompute = 0xB1C0,
    CtxKepler = 0xA140,
    CtxDMA = 0xB0B5,
    CtxChannelGPFIFO = 0xB06F,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlSetNvmapFD {
    pub nvmap_fd: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlChannelSetTimeout {
    pub timeout: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlAllocGPFIFO {
    pub num_entries: u32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlClientData {
    pub data: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlZCullBind {
    pub gpu_va: u64,
    pub mode: u32,
    pub _pad: u32,
}
const _: () = assert!(std::mem::size_of::<IoctlZCullBind>() == 16);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlSetErrorNotifier {
    pub offset: u64,
    pub size: u64,
    pub mem: u32,
    pub _pad: u32,
}
const _: () = assert!(std::mem::size_of::<IoctlSetErrorNotifier>() == 24);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlChannelSetPriority {
    pub priority: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlSetTimeslice {
    pub timeslice: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlAllocGpfifoEx2 {
    pub num_entries: u32,
    pub flags: u32,
    pub unk0: u32,
    pub fence_out: NvFence,
    pub unk1: u32,
    pub unk2: u32,
    pub unk3: u32,
}
const _: () = assert!(std::mem::size_of::<IoctlAllocGpfifoEx2>() == 32);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlAllocObjCtx {
    pub class_num: u32,
    pub flags: u32,
    pub obj_id: u64,
}
const _: () = assert!(std::mem::size_of::<IoctlAllocObjCtx>() == 16);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlSubmitGpfifo {
    pub address: u64,
    pub num_entries: u32,
    pub flags: u32,
    pub fence: NvFence,
}
const _: () = assert!(std::mem::size_of::<IoctlSubmitGpfifo>() == 24);

impl IoctlSubmitGpfifo {
    pub fn fence_wait(&self) -> bool {
        self.flags & 1 != 0
    }
    pub fn fence_increment(&self) -> bool {
        (self.flags >> 1) & 1 != 0
    }
    pub fn suppress_wfi(&self) -> bool {
        (self.flags >> 4) & 1 != 0
    }
    pub fn increment_value(&self) -> bool {
        (self.flags >> 8) & 1 != 0
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlGetWaitbase {
    pub unknown: u32,
    pub value: u32,
}

const METHOD_SYNCPOINT_PAYLOAD: u32 = 0x1C;
const METHOD_SYNCPOINT_OPERATION: u32 = 0x1D;
const METHOD_WAIT_FOR_IDLE: u32 = 0x1E;
const SUBMISSION_MODE_INCREASING: u32 = 1;

fn build_command_header(method: u32, arg_count: u32, mode: u32) -> GpuCommandHeader {
    GpuCommandHeader {
        raw: (method & 0x1FFF) | ((arg_count & 0x1FFF) << 16) | ((mode & 0x7) << 29),
    }
}

fn build_fence_action(op: u32, syncpoint_id: u32) -> GpuCommandHeader {
    GpuCommandHeader {
        raw: (op & 1) | ((syncpoint_id & 0x00FF_FFFF) << 8),
    }
}

fn build_wait_command_list(fence: NvFence) -> GpuCommandList {
    GpuCommandList {
        command_lists: Vec::new(),
        prefetch_command_list: vec![
            build_command_header(METHOD_SYNCPOINT_PAYLOAD, 1, SUBMISSION_MODE_INCREASING),
            GpuCommandHeader { raw: fence.value },
            build_command_header(METHOD_SYNCPOINT_OPERATION, 1, SUBMISSION_MODE_INCREASING),
            build_fence_action(0, fence.id as u32),
        ],
    }
}

fn build_increment_command_list(fence: NvFence) -> GpuCommandList {
    GpuCommandList {
        command_lists: Vec::new(),
        prefetch_command_list: vec![
            build_command_header(METHOD_SYNCPOINT_PAYLOAD, 1, SUBMISSION_MODE_INCREASING),
            GpuCommandHeader { raw: 0 },
            build_command_header(METHOD_SYNCPOINT_OPERATION, 1, SUBMISSION_MODE_INCREASING),
            build_fence_action(1, fence.id as u32),
            build_command_header(METHOD_SYNCPOINT_OPERATION, 1, SUBMISSION_MODE_INCREASING),
            build_fence_action(1, fence.id as u32),
        ],
    }
}

// ---------------------------------------------------------------------------
// Fine-grained submit_gpfifo profile (RUZU_PROFILE_SUBMIT_GPFIFO=1)
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct SubmitGpfifoPhaseAgg {
    count: u64,
    total_ns: u64,
    max_ns: u64,
}

static SUBMIT_GPFIFO_PROFILE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<&'static str, SubmitGpfifoPhaseAgg>>,
> = std::sync::OnceLock::new();
static SUBMIT_GPFIFO_TRACE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn record_submit_gpfifo_phase(label: &'static str, elapsed: std::time::Duration) {
    let agg = SUBMIT_GPFIFO_PROFILE
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let ns = elapsed.as_nanos() as u64;
    let mut g = agg.lock().unwrap();
    let entry = g.entry(label).or_default();
    entry.count += 1;
    entry.total_ns = entry.total_ns.saturating_add(ns);
    if ns > entry.max_ns {
        entry.max_ns = ns;
    }
}

fn trace_submit_gpfifo_ring(
    stage: u64,
    seq: u64,
    bind_id: u64,
    syncpoint_id: u32,
    flags: u32,
    increment: u32,
    fence_in_id: i32,
    fence_in_value: u32,
    fence_out_value: u32,
    min_value: u32,
    max_value: u32,
) {
    if !common::trace::is_enabled(common::trace::cat::SUBMIT_GPFIFO) {
        return;
    }
    let tid = crate::hle::kernel::kernel::get_current_thread_id_fast().unwrap_or(0);
    common::trace::emit_raw(
        common::trace::cat::SUBMIT_GPFIFO,
        &[
            stage,
            seq,
            tid,
            bind_id,
            syncpoint_id as u64,
            flags as u64,
            increment as u64,
            fence_in_id as i64 as u64,
            fence_in_value as u64,
            fence_out_value as u64,
            min_value as u64,
            max_value as u64,
        ],
    );
}

pub fn dump_submit_gpfifo_profile() {
    let Some(agg) = SUBMIT_GPFIFO_PROFILE.get() else {
        return;
    };
    let entries: Vec<(&'static str, SubmitGpfifoPhaseAgg)> = {
        let g = agg.lock().unwrap();
        g.iter().map(|(k, v)| (*k, v.clone())).collect()
    };
    if entries.is_empty() {
        return;
    }
    let mut sorted = entries;
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    eprintln!("[SUBMIT_GPFIFO_PROFILE] per-phase timing (RUZU_PROFILE_SUBMIT_GPFIFO=1):");
    for (label, agg) in &sorted {
        let avg = if agg.count != 0 {
            agg.total_ns / agg.count
        } else {
            0
        };
        eprintln!(
            "[SUBMIT_GPFIFO_PROFILE]   {:24} count={:<5} total={:>8.2}ms avg={:>8.1}us max={:>8.1}us",
            label,
            agg.count,
            agg.total_ns as f64 / 1_000_000.0,
            avg as f64 / 1_000.0,
            agg.max_ns as f64 / 1_000.0,
        );
    }
}

fn build_increment_with_wfi_command_list(fence: NvFence) -> GpuCommandList {
    let mut result = GpuCommandList {
        command_lists: Vec::new(),
        prefetch_command_list: vec![
            build_command_header(METHOD_WAIT_FOR_IDLE, 1, SUBMISSION_MODE_INCREASING),
            GpuCommandHeader { raw: 0 },
        ],
    };
    result
        .prefetch_command_list
        .extend(build_increment_command_list(fence).prefetch_command_list);
    result
}

fn command_list_headers_as_bytes_mut(headers: &mut [GpuCommandListHeader]) -> &mut [u8] {
    let byte_len = std::mem::size_of_val(headers);
    unsafe { std::slice::from_raw_parts_mut(headers.as_mut_ptr().cast::<u8>(), byte_len) }
}

fn copy_command_list_headers_from_bytes(dest: &mut [GpuCommandListHeader], src: &[u8]) {
    let byte_len = std::mem::size_of_val(dest);
    command_list_headers_as_bytes_mut(dest).copy_from_slice(&src[..byte_len]);
}

/// nvhost_gpu device.
pub struct NvHostGpu {
    system: SystemRef,
    sm_exception_breakpoint_int_report_event: Arc<Mutex<KReadableEvent>>,
    sm_exception_breakpoint_pause_report_event: Arc<Mutex<KReadableEvent>>,
    error_notifier_event: Arc<Mutex<KReadableEvent>>,
    syncpoint_manager: *const SyncpointManager,
    container: *const Container,
    channel_syncpoint: AtomicU32,
    channel_initialized: AtomicBool,
    nvmap_fd: Mutex<i32>,
    user_data: Mutex<u64>,
    zcull_params: Mutex<IoctlZCullBind>,
    channel_priority: Mutex<u32>,
    channel_timeslice: Mutex<u32>,
    channel_state: Arc<dyn GpuChannelHandle>,
    bound_address_space: AtomicBool,
    channel_mutex: Mutex<()>,
    sessions: Mutex<HashMap<DeviceFD, SessionId>>,
}

unsafe impl Send for NvHostGpu {}
unsafe impl Sync for NvHostGpu {}

impl NvHostGpu {
    fn should_trace_init_path() -> bool {
        std::env::var_os("RUZU_TRACE_NVHOST_GPU_INIT")
            .is_some_and(|value| value != std::ffi::OsStr::new("0"))
    }

    fn trace_command_list_headers(label: &str, headers: &[GpuCommandListHeader]) {
        let explicit_limit = std::env::var("RUZU_TRACE_GPFIFO_HEADERS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok());
        if !Self::should_trace_init_path() && explicit_limit.is_none() {
            return;
        }
        let limit = explicit_limit.unwrap_or(8);
        for (index, header) in headers.iter().take(limit).enumerate() {
            let addr = header.raw & ((1_u64 << 40) - 1);
            let is_non_main = (header.raw >> 41) & 1;
            let size = (header.raw >> 42) & ((1_u64 << 21) - 1);
            log::info!(
                "nvhost_gpu::{} header[{}] raw=0x{:016X} addr=0x{:X} size={} non_main={}",
                label,
                index,
                header.raw,
                addr,
                size,
                is_non_main
            );
        }
    }

    pub fn new(
        system: SystemRef,
        events_interface: Arc<EventInterface>,
        container: &Container,
    ) -> Self {
        let channel_state = system
            .get()
            .gpu_core()
            .expect("GPU core must be initialized before nvhost_gpu open")
            .allocate_channel_handle();
        let channel_syncpoint = container.get_syncpoint_manager().allocate_syncpoint(false);
        Self {
            system,
            sm_exception_breakpoint_int_report_event: events_interface
                .create_event("GpuChannelSMExceptionBreakpointInt"),
            sm_exception_breakpoint_pause_report_event: events_interface
                .create_event("GpuChannelSMExceptionBreakpointPause"),
            error_notifier_event: events_interface.create_event("GpuChannelErrorNotifier"),
            syncpoint_manager: container.get_syncpoint_manager() as *const _,
            container: container as *const _,
            channel_syncpoint: AtomicU32::new(channel_syncpoint),
            channel_initialized: AtomicBool::new(false),
            nvmap_fd: Mutex::new(0),
            user_data: Mutex::new(0),
            zcull_params: Mutex::new(IoctlZCullBind::default()),
            channel_priority: Mutex::new(0),
            channel_timeslice: Mutex::new(0),
            channel_state,
            bound_address_space: AtomicBool::new(false),
            channel_mutex: Mutex::new(()),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn syncpoint_manager(&self) -> &SyncpointManager {
        unsafe { &*self.syncpoint_manager }
    }

    fn container(&self) -> &Container {
        unsafe { &*self.container }
    }

    pub fn set_nvmap_fd(&self, params: &mut IoctlSetNvmapFD) -> NvResult {
        log::debug!("nvhost_gpu::SetNVMAPfd called, fd={}", params.nvmap_fd);
        if Self::should_trace_init_path() {
            log::info!("nvhost_gpu::SetNVMAPfd fd={}", params.nvmap_fd);
        }
        *self.nvmap_fd.lock().unwrap() = params.nvmap_fd;
        NvResult::Success
    }

    pub fn set_client_data(&self, params: &mut IoctlClientData) -> NvResult {
        log::debug!("nvhost_gpu::SetClientData called");
        *self.user_data.lock().unwrap() = params.data;
        if Self::should_trace_init_path() {
            log::info!("nvhost_gpu::SetClientData data=0x{:X}", params.data);
        }
        NvResult::Success
    }

    pub fn get_client_data(&self, params: &mut IoctlClientData) -> NvResult {
        log::debug!("nvhost_gpu::GetClientData called");
        params.data = *self.user_data.lock().unwrap();
        NvResult::Success
    }

    pub fn zcull_bind(&self, params: &mut IoctlZCullBind) -> NvResult {
        *self.zcull_params.lock().unwrap() = *params;
        log::debug!(
            "nvhost_gpu::ZCullBind called, gpu_va={:X}, mode={:X}",
            params.gpu_va,
            params.mode
        );
        NvResult::Success
    }

    pub fn set_error_notifier(&self, params: &mut IoctlSetErrorNotifier) -> NvResult {
        log::warn!(
            "nvhost_gpu::SetErrorNotifier (STUBBED) called, offset={:X}, size={:X}, mem={:X}",
            params.offset,
            params.size,
            params.mem
        );
        if Self::should_trace_init_path() {
            log::info!(
                "nvhost_gpu::SetErrorNotifier offset=0x{:X} size=0x{:X} mem=0x{:X}",
                params.offset,
                params.size,
                params.mem
            );
        }
        NvResult::Success
    }

    pub fn set_channel_priority(&self, params: &mut IoctlChannelSetPriority) -> NvResult {
        *self.channel_priority.lock().unwrap() = params.priority;
        log::debug!(
            "nvhost_gpu::SetChannelPriority (STUBBED) called, priority={:X}",
            params.priority
        );
        if Self::should_trace_init_path() {
            log::info!(
                "nvhost_gpu::SetChannelPriority priority=0x{:X}",
                params.priority
            );
        }
        NvResult::Success
    }

    pub fn alloc_gpfifo_ex2(&self, params: &mut IoctlAllocGpfifoEx2, fd: DeviceFD) -> NvResult {
        log::warn!(
            "nvhost_gpu::AllocGPFIFOEx2 (STUBBED) called, num_entries={:X}, flags={:X}",
            params.num_entries,
            params.flags
        );

        if self.channel_initialized.swap(true, Ordering::AcqRel) {
            log::error!("nvhost_gpu::AllocGPFIFOEx2 called on already initialized channel");
            return NvResult::AlreadyAllocated;
        }

        let program_id = self
            .sessions
            .lock()
            .unwrap()
            .get(&fd)
            .copied()
            .and_then(|session_id| self.container().get_session_process(session_id))
            .map(|process| process.lock().unwrap().get_program_id())
            .unwrap_or(0);
        self.channel_state.init_channel(program_id);
        let channel_syncpoint = self.channel_syncpoint.load(Ordering::Acquire);
        params.fence_out = self
            .syncpoint_manager()
            .get_syncpoint_fence(channel_syncpoint);
        if Self::should_trace_init_path() {
            log::info!(
                "nvhost_gpu::AllocGPFIFOEx2 program_id=0x{:X} channel_syncpoint={} fence_out={{id={}, value={}}}",
                program_id,
                channel_syncpoint,
                params.fence_out.id,
                params.fence_out.value
            );
        }
        NvResult::Success
    }

    pub fn allocate_object_context(&self, params: &mut IoctlAllocObjCtx) -> NvResult {
        log::warn!(
            "nvhost_gpu::AllocateObjectContext (STUBBED) called, class_num={:X}, flags={:X}",
            params.class_num,
            params.flags
        );
        params.obj_id = 0x0;
        if Self::should_trace_init_path() {
            log::info!(
                "nvhost_gpu::AllocateObjectContext class_num=0x{:X} flags=0x{:X} obj_id=0x{:X}",
                params.class_num,
                params.flags,
                params.obj_id
            );
        }
        NvResult::Success
    }

    fn submit_gpfifo_impl(
        &self,
        params: &mut IoctlSubmitGpfifo,
        entries: GpuCommandList,
    ) -> NvResult {
        let submit_trace_seq =
            SUBMIT_GPFIFO_TRACE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let fence_in_id = params.fence.id;
        let fence_in_value = params.fence.value;
        let flags_raw = params.flags;
        log::trace!(
            "nvhost_gpu::SubmitGPFIFOImpl called, gpfifo={:X}, num_entries={:X}, flags={:X}",
            params.address,
            params.num_entries,
            params.flags
        );
        if Self::should_trace_init_path() {
            log::info!(
                "nvhost_gpu::SubmitGPFIFOImpl begin address=0x{:X} num_entries={} flags=0x{:X} fence_in={{id={}, value={}}}",
                params.address,
                params.num_entries,
                params.flags,
                params.fence.id,
                params.fence.value
            );
        }

        // RUZU_PROFILE_SUBMIT_GPFIFO=1: env-gated fine-grained timing of each
        // sub-step. Dumped via the same SIGUSR2 handler as the other profiles.
        let profile = std::env::var_os("RUZU_PROFILE_SUBMIT_GPFIFO").is_some();
        let mark = |label: &'static str, start: &mut Option<std::time::Instant>| {
            if let Some(t0) = start.take() {
                record_submit_gpfifo_phase(label, t0.elapsed());
            }
            if profile {
                *start = Some(std::time::Instant::now());
            }
        };

        let mut phase_start = if profile {
            Some(std::time::Instant::now())
        } else {
            None
        };

        let gpu = self
            .system
            .get()
            .gpu_core()
            .expect("GPU core must remain available while nvhost_gpu is alive");
        mark("01_gpu_lookup", &mut phase_start);

        let _channel_guard = self.channel_mutex.lock().unwrap();
        mark("02_channel_mutex_lock", &mut phase_start);

        let channel_syncpoint = self.channel_syncpoint.load(Ordering::Acquire);
        let bind_id = self.channel_state.bind_id();

        let entered_fence_wait_branch = if params.fence_wait() {
            if params.increment_value() {
                return NvResult::BadParameter;
            }
            mark("03a_fence_wait_check", &mut phase_start);

            let signalled = self.syncpoint_manager().is_fence_signalled(&params.fence);
            mark("03b_is_fence_signalled", &mut phase_start);

            if !signalled {
                gpu.push_gpu_entries(bind_id, build_wait_command_list(params.fence));
                mark("03c_push_wait_command", &mut phase_start);
            } else {
                mark("03d_fence_already_signalled", &mut phase_start);
            }
            true
        } else {
            mark("03e_no_fence_wait", &mut phase_start);
            false
        };
        let _ = entered_fence_wait_branch;

        params.fence.id = channel_syncpoint as i32;
        let increment = (if params.fence_increment() { 2 } else { 0 })
            + if params.increment_value() {
                params.fence.value
            } else {
                0
            };
        params.fence.value = self
            .syncpoint_manager()
            .increment_syncpoint_max_ext(channel_syncpoint, increment);
        if std::env::var_os("RUZU_TRACE_GPU_SUBMIT").is_some() {
            log::info!(
                "nvhost_gpu::SubmitGPFIFOImpl syncpoint_update seq={} bind_id={} flags=0x{:X} fence_wait={} fence_increment={} suppress_wfi={} increment_value={} increment={} fence_in={{id={}, value={}}} fence_out={{id={}, value={}}} main_lists={} main_prefetch_words={}",
                submit_trace_seq,
                bind_id,
                flags_raw,
                (flags_raw & 1) != 0,
                ((flags_raw >> 1) & 1) != 0,
                ((flags_raw >> 4) & 1) != 0,
                ((flags_raw >> 8) & 1) != 0,
                increment,
                fence_in_id,
                fence_in_value,
                params.fence.id,
                params.fence.value,
                entries.command_lists.len(),
                entries.prefetch_command_list.len(),
            );
        }
        trace_submit_gpfifo_ring(
            1,
            submit_trace_seq,
            bind_id as u64,
            channel_syncpoint,
            flags_raw,
            increment,
            fence_in_id,
            fence_in_value,
            params.fence.value,
            self.syncpoint_manager()
                .read_syncpoint_min_value(channel_syncpoint),
            self.syncpoint_manager()
                .read_syncpoint_max_value(channel_syncpoint),
        );
        mark("04_syncpoint_increment", &mut phase_start);

        gpu.push_gpu_entries(bind_id, entries);
        trace_submit_gpfifo_ring(
            2,
            submit_trace_seq,
            bind_id as u64,
            channel_syncpoint,
            flags_raw,
            increment,
            fence_in_id,
            fence_in_value,
            params.fence.value,
            self.syncpoint_manager()
                .read_syncpoint_min_value(channel_syncpoint),
            self.syncpoint_manager()
                .read_syncpoint_max_value(channel_syncpoint),
        );
        mark("05_push_main_entries", &mut phase_start);

        if params.fence_increment() {
            if params.suppress_wfi() {
                gpu.push_gpu_entries(bind_id, build_increment_command_list(params.fence));
            } else {
                gpu.push_gpu_entries(bind_id, build_increment_with_wfi_command_list(params.fence));
            }
        }
        trace_submit_gpfifo_ring(
            3,
            submit_trace_seq,
            bind_id as u64,
            channel_syncpoint,
            flags_raw,
            increment,
            fence_in_id,
            fence_in_value,
            params.fence.value,
            self.syncpoint_manager()
                .read_syncpoint_min_value(channel_syncpoint),
            self.syncpoint_manager()
                .read_syncpoint_max_value(channel_syncpoint),
        );
        mark("06_push_fence_increment", &mut phase_start);
        // [SP_TRACE] log MAX vs MIN of the channel syncpoint after each
        // submit. If MAX advances but MIN doesn't catch up, the GPU is
        // dropping syncpoint-increment commands and game waits stall.
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNT: AtomicU64 = AtomicU64::new(0);
            let n = COUNT.fetch_add(1, Ordering::Relaxed);
            let trace_after = std::env::var("RUZU_TRACE_SUBMIT_AFTER")
                .ok()
                .and_then(|value| value.parse::<u32>().ok());
            if n < 16 || n.is_power_of_two() || trace_after.is_some_and(|v| params.fence.value >= v)
            {
                let min_v = self
                    .syncpoint_manager()
                    .read_syncpoint_min_value(channel_syncpoint);
                let max_v = self
                    .syncpoint_manager()
                    .read_syncpoint_max_value(channel_syncpoint);
                log::info!(
                    "[SP_TRACE] submit#{} sp_id={} min={} max={} pending={} (out fence value={})",
                    n,
                    channel_syncpoint,
                    min_v,
                    max_v,
                    max_v.wrapping_sub(min_v),
                    params.fence.value,
                );
            }
        }

        params.flags = 0;
        if Self::should_trace_init_path() {
            log::info!(
                "nvhost_gpu::SubmitGPFIFOImpl end bind_id={} channel_syncpoint={} fence_out={{id={}, value={}}}",
                bind_id,
                channel_syncpoint,
                params.fence.id,
                params.fence.value
            );
        }
        NvResult::Success
    }

    pub fn submit_gpfifo_base1(
        &self,
        params: &mut IoctlSubmitGpfifo,
        commands: &[u8],
        kickoff: bool,
    ) -> NvResult {
        let command_count = params.num_entries as usize;
        let entry_size = std::mem::size_of::<GpuCommandListHeader>();
        let available_entries = commands.len() / entry_size;
        log::trace!(
            "nvhost_gpu::SubmitGPFIFOBase1 kickoff={} num_entries={} address=0x{:X} input_len=0x{:X}",
            kickoff,
            params.num_entries,
            params.address,
            commands.len()
        );
        if Self::should_trace_init_path() {
            log::info!(
                "nvhost_gpu::SubmitGPFIFOBase1 kickoff={} num_entries={} address=0x{:X} input_len=0x{:X}",
                kickoff,
                params.num_entries,
                params.address,
                commands.len()
            );
        }
        if command_count > available_entries {
            log::error!(
                "nvhost_gpu::SubmitGPFIFOBase1 invalid size num_entries={} available_entries={}",
                params.num_entries,
                available_entries
            );
            return NvResult::InvalidSize;
        }

        let mut command_lists = vec![GpuCommandListHeader::default(); command_count];
        if kickoff {
            let Some(memory) = self.system.get().get_svc_memory() else {
                log::error!(
                    "nvhost_gpu::SubmitGPFIFOBase1 kickoff path without application memory"
                );
                return NvResult::InvalidState;
            };
            memory.access().unwrap().read_block(
                params.address,
                command_list_headers_as_bytes_mut(&mut command_lists),
            );
        } else {
            copy_command_list_headers_from_bytes(&mut command_lists, commands);
        }
        Self::trace_command_list_headers("SubmitGPFIFOBase1", &command_lists);

        self.submit_gpfifo_impl(
            params,
            GpuCommandList {
                command_lists,
                prefetch_command_list: Vec::new(),
            },
        )
    }

    pub fn submit_gpfifo_base2(&self, params: &mut IoctlSubmitGpfifo, commands: &[u8]) -> NvResult {
        let command_count = params.num_entries as usize;
        let entry_size = std::mem::size_of::<GpuCommandListHeader>();
        let available_entries = commands.len() / entry_size;
        log::trace!(
            "nvhost_gpu::SubmitGPFIFOBase2 num_entries={} address=0x{:X} inline_len=0x{:X}",
            params.num_entries,
            params.address,
            commands.len()
        );
        if Self::should_trace_init_path() {
            log::info!(
                "nvhost_gpu::SubmitGPFIFOBase2 num_entries={} address=0x{:X} inline_len=0x{:X}",
                params.num_entries,
                params.address,
                commands.len()
            );
        }
        if command_count > available_entries {
            log::error!(
                "nvhost_gpu::SubmitGPFIFOBase2 invalid size num_entries={} available_entries={}",
                params.num_entries,
                available_entries
            );
            return NvResult::InvalidSize;
        }

        let mut command_lists = vec![GpuCommandListHeader::default(); command_count];
        copy_command_list_headers_from_bytes(&mut command_lists, commands);
        Self::trace_command_list_headers("SubmitGPFIFOBase2", &command_lists);
        self.submit_gpfifo_impl(
            params,
            GpuCommandList {
                command_lists,
                prefetch_command_list: Vec::new(),
            },
        )
    }

    pub fn get_waitbase(&self, params: &mut IoctlGetWaitbase) -> NvResult {
        log::trace!(
            "nvhost_gpu::GetWaitbase called, unknown=0x{:X}",
            params.unknown
        );
        params.value = 0;
        if Self::should_trace_init_path() {
            log::info!(
                "nvhost_gpu::GetWaitbase unknown=0x{:X} value=0x{:X}",
                params.unknown,
                params.value
            );
        }
        NvResult::Success
    }

    pub fn channel_set_timeout(&self, params: &mut IoctlChannelSetTimeout) -> NvResult {
        log::trace!(
            "nvhost_gpu::ChannelSetTimeout called, timeout=0x{:X}",
            params.timeout
        );
        NvResult::Success
    }

    pub fn channel_set_timeslice(&self, params: &mut IoctlSetTimeslice) -> NvResult {
        log::trace!(
            "nvhost_gpu::ChannelSetTimeslice called, timeslice=0x{:X}",
            params.timeslice
        );
        *self.channel_timeslice.lock().unwrap() = params.timeslice;
        NvResult::Success
    }

    pub fn bind_address_space(&self, memory_manager: Arc<dyn GpuMemoryManagerHandle>) {
        self.channel_state.bind_memory_manager(memory_manager);
        self.bound_address_space.store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn has_bound_address_space(&self) -> bool {
        self.bound_address_space.load(Ordering::Acquire)
    }
}

impl Drop for NvHostGpu {
    fn drop(&mut self) {
        let channel_syncpoint = self.channel_syncpoint.load(Ordering::Acquire);
        if channel_syncpoint != 0 {
            self.syncpoint_manager().free_syncpoint(channel_syncpoint);
        }
    }
}

impl NvDevice for NvHostGpu {
    fn ioctl1(&self, fd: DeviceFD, command: Ioctl, input: &[u8], output: &mut [u8]) -> NvResult {
        match command.group() {
            0x0 => match command.cmd() {
                0x3 => {
                    let mut params: IoctlGetWaitbase = read_struct(input);
                    let r = self.get_waitbase(&mut params);
                    write_struct(output, &params);
                    r
                }
                _ => {
                    log::error!("Unimplemented ioctl={:08X}", command.raw);
                    NvResult::NotImplemented
                }
            },
            b'H' => match command.cmd() {
                0x1 => {
                    let mut params: IoctlSetNvmapFD = read_struct(input);
                    let r = self.set_nvmap_fd(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x3 => {
                    let mut params: IoctlChannelSetTimeout = read_struct(input);
                    let r = self.channel_set_timeout(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x8 => {
                    let fixed_size = std::mem::size_of::<IoctlSubmitGpfifo>();
                    let mut params: IoctlSubmitGpfifo = read_struct(input);
                    let var_data = if input.len() > fixed_size {
                        &input[fixed_size..]
                    } else {
                        &[]
                    };
                    let r = self.submit_gpfifo_base1(&mut params, var_data, false);
                    // Match upstream `WrapFixedVariable`: copy both the fixed struct
                    // AND the trailing variable-length gpfifo entries to the output
                    // buffer. The server doesn't modify the variable data, so it
                    // must be echoed back from input for the game's runtime to read
                    // the submitted entries from the output buffer.
                    write_struct(output, &params);
                    if output.len() > fixed_size && !var_data.is_empty() {
                        let max_var = output.len() - fixed_size;
                        let copy_len = var_data.len().min(max_var);
                        output[fixed_size..fixed_size + copy_len]
                            .copy_from_slice(&var_data[..copy_len]);
                    }
                    r
                }
                0x1b => {
                    let fixed_size = std::mem::size_of::<IoctlSubmitGpfifo>();
                    let mut params: IoctlSubmitGpfifo = read_struct(input);
                    let var_data = if input.len() > fixed_size {
                        &input[fixed_size..]
                    } else {
                        &[]
                    };
                    let r = self.submit_gpfifo_base1(&mut params, var_data, true);
                    write_struct(output, &params);
                    if output.len() > fixed_size && !var_data.is_empty() {
                        let max_var = output.len() - fixed_size;
                        let copy_len = var_data.len().min(max_var);
                        output[fixed_size..fixed_size + copy_len]
                            .copy_from_slice(&var_data[..copy_len]);
                    }
                    r
                }
                0x9 => {
                    let mut params: IoctlAllocObjCtx = read_struct(input);
                    let r = self.allocate_object_context(&mut params);
                    write_struct(output, &params);
                    r
                }
                0xb => {
                    let mut params: IoctlZCullBind = read_struct(input);
                    let r = self.zcull_bind(&mut params);
                    write_struct(output, &params);
                    r
                }
                0xc => {
                    let mut params: IoctlSetErrorNotifier = read_struct(input);
                    let r = self.set_error_notifier(&mut params);
                    write_struct(output, &params);
                    r
                }
                0xd => {
                    let mut params: IoctlChannelSetPriority = read_struct(input);
                    let r = self.set_channel_priority(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x1a => {
                    let mut params: IoctlAllocGpfifoEx2 = read_struct(input);
                    let r = self.alloc_gpfifo_ex2(&mut params, fd);
                    write_struct(output, &params);
                    r
                }
                0x1d => {
                    let mut params: IoctlSetTimeslice = read_struct(input);
                    let r = self.channel_set_timeslice(&mut params);
                    write_struct(output, &params);
                    r
                }
                _ => {
                    log::error!("Unimplemented ioctl={:08X}", command.raw);
                    NvResult::NotImplemented
                }
            },
            b'G' => match command.cmd() {
                0x14 => {
                    let mut params: IoctlClientData = read_struct(input);
                    let r = self.set_client_data(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x15 => {
                    let mut params: IoctlClientData = read_struct(input);
                    let r = self.get_client_data(&mut params);
                    write_struct(output, &params);
                    r
                }
                _ => {
                    log::error!("Unimplemented ioctl={:08X}", command.raw);
                    NvResult::NotImplemented
                }
            },
            _ => {
                log::error!("Unimplemented ioctl={:08X}", command.raw);
                NvResult::NotImplemented
            }
        }
    }

    fn ioctl2(
        &self,
        _fd: DeviceFD,
        command: Ioctl,
        input: &[u8],
        inline_input: &[u8],
        output: &mut [u8],
    ) -> NvResult {
        match command.group() {
            b'H' => match command.cmd() {
                0x1b => {
                    let mut params: IoctlSubmitGpfifo = read_struct(input);
                    let r = self.submit_gpfifo_base2(&mut params, inline_input);
                    write_struct(output, &params);
                    r
                }
                _ => {
                    log::error!("Unimplemented ioctl={:08X}", command.raw);
                    NvResult::NotImplemented
                }
            },
            _ => {
                log::error!("Unimplemented ioctl={:08X}", command.raw);
                NvResult::NotImplemented
            }
        }
    }

    fn ioctl3(
        &self,
        _fd: DeviceFD,
        command: Ioctl,
        _input: &[u8],
        _output: &mut [u8],
        _inline_output: &mut [u8],
    ) -> NvResult {
        log::error!("Unimplemented ioctl={:08X}", command.raw);
        NvResult::NotImplemented
    }

    fn on_open(&self, session_id: SessionId, fd: DeviceFD) {
        let mut sessions = self.sessions.lock().unwrap();
        sessions.insert(fd, session_id);
    }

    fn on_close(&self, fd: DeviceFD) {
        let mut sessions = self.sessions.lock().unwrap();
        sessions.remove(&fd);
    }

    fn query_event(&self, event_id: u32) -> Option<Arc<Mutex<KReadableEvent>>> {
        match event_id {
            1 => Some(Arc::clone(&self.sm_exception_breakpoint_int_report_event)),
            2 => Some(Arc::clone(&self.sm_exception_breakpoint_pause_report_event)),
            3 => Some(Arc::clone(&self.error_notifier_event)),
            _ => {
                log::error!("Unknown Ctrl GPU Event {}", event_id);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{IoctlAllocGpfifoEx2, IoctlSubmitGpfifo, NvHostGpu};
    use crate::gpu_core::{
        GpuChannelHandle, GpuCommandList, GpuCoreInterface, GpuMemoryManagerHandle,
    };
    use crate::hle::service::nvdrv::core::container::Container;
    use crate::hle::service::nvdrv::devices::nvdevice::NvDevice;
    use crate::hle::service::nvdrv::nvdata::{NvFence, NvResult};
    use crate::hle::service::nvdrv::nvdrv::EventInterface;

    #[derive(Default)]
    struct FakeGpuCore {
        pushed: Mutex<Vec<(i32, GpuCommandList)>>,
    }

    struct FakeGpuMemoryManagerHandle;
    struct FakeGpuChannelHandle {
        bind_id: i32,
    }

    impl GpuChannelHandle for FakeGpuChannelHandle {
        fn bind_memory_manager(&self, _memory_manager: Arc<dyn GpuMemoryManagerHandle>) {}

        fn init_channel(&self, _program_id: u64) {}

        fn bind_id(&self) -> i32 {
            self.bind_id
        }
    }

    impl GpuMemoryManagerHandle for FakeGpuMemoryManagerHandle {
        fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
            self
        }

        fn map(
            &self,
            _gpu_addr: u64,
            _device_addr: u64,
            _size: u64,
            _kind: u32,
            _is_big_pages: bool,
        ) {
        }

        fn map_sparse(&self, _gpu_addr: u64, _size: u64, _is_big_pages: bool) {}

        fn unmap(&self, _gpu_addr: u64, _size: u64) {}
    }

    impl GpuCoreInterface for FakeGpuCore {
        fn as_any(&self) -> &(dyn std::any::Any + Send) {
            self
        }

        fn allocate_channel_handle(&self) -> Arc<dyn GpuChannelHandle> {
            Arc::new(FakeGpuChannelHandle { bind_id: 1 })
        }

        fn allocate_memory_manager_handle(
            &self,
            _address_space_bits: u64,
            _split_address: u64,
            _big_page_bits: u64,
            _page_bits: u64,
        ) -> Arc<dyn GpuMemoryManagerHandle> {
            Arc::new(FakeGpuMemoryManagerHandle)
        }

        fn init_address_space(&self, _memory_manager: Arc<dyn GpuMemoryManagerHandle>) {}

        fn push_gpu_entries(&self, channel_id: i32, entries: GpuCommandList) {
            self.pushed.lock().unwrap().push((channel_id, entries));
        }

        fn request_composite(
            &self,
            _layers: Vec<crate::gpu_core::FramebufferConfig>,
            _fences: Vec<crate::hle::service::nvdrv::nvdata::NvFence>,
        ) {
        }

        fn wait_for_composite(&self) {}

        fn on_cpu_write(&self, _addr: u64, _size: u64) -> bool {
            false
        }

        fn on_cpu_read(&self, addr: u64, _size: u64) -> crate::gpu_core::RasterizerDownloadArea {
            crate::gpu_core::RasterizerDownloadArea {
                start_address: addr,
                end_address: addr,
                preemptive: true,
            }
        }

        fn flush_region(&self, _addr: u64, _size: u64) {}
    }

    #[test]
    fn alloc_gpfifo_ex2_allocates_syncpoint_fence() {
        let mut system = crate::core::System::new_for_test();
        system.set_gpu_core(Box::new(FakeGpuCore::default()));
        let container = Container::new();
        let events = Arc::new(EventInterface::new(crate::core::SystemRef::from_ref(
            &system,
        )));
        let gpu = NvHostGpu::new(
            crate::core::SystemRef::from_ref(&system),
            events,
            &container,
        );
        let mut params = IoctlAllocGpfifoEx2::default();

        let result = gpu.alloc_gpfifo_ex2(&mut params, 1);

        assert_eq!(result, NvResult::Success);
        assert_ne!(params.fence_out.id, 0);
    }

    #[test]
    fn submit_gpfifo_base1_returns_signalled_fence_and_clears_flags() {
        let mut system = crate::core::System::new_for_test();
        system.set_gpu_core(Box::new(FakeGpuCore::default()));
        let container = Container::new();
        let events = Arc::new(EventInterface::new(crate::core::SystemRef::from_ref(
            &system,
        )));
        let gpu = NvHostGpu::new(
            crate::core::SystemRef::from_ref(&system),
            events,
            &container,
        );
        let mut alloc = IoctlAllocGpfifoEx2::default();
        assert_eq!(gpu.alloc_gpfifo_ex2(&mut alloc, 1), NvResult::Success);

        let mut params = IoctlSubmitGpfifo {
            num_entries: 1,
            flags: (1 << 1) | (1 << 8),
            fence: NvFence { id: 0, value: 3 },
            ..Default::default()
        };
        let commands = [0u8; 8];

        let result = gpu.submit_gpfifo_base1(&mut params, &commands, false);

        assert_eq!(result, NvResult::Success);
        assert_eq!(params.flags, 0);
        assert_eq!(params.fence.id, alloc.fence_out.id);
        assert_eq!(params.fence.value, 5);
        let gpu = system
            .gpu_core()
            .unwrap()
            .as_any()
            .downcast_ref::<FakeGpuCore>()
            .unwrap();
        let pushed = gpu.pushed.lock().unwrap();
        assert_eq!(pushed.len(), 2);
        assert_eq!(pushed[0].0, 1);
        assert_eq!(pushed[0].1.command_lists.len(), 1);
        assert_eq!(pushed[1].1.prefetch_command_list.len(), 6);
    }

    #[test]
    fn submit_gpfifo_base1_kickoff_validates_size_before_memory_read() {
        let mut system = crate::core::System::new_for_test();
        system.set_gpu_core(Box::new(FakeGpuCore::default()));
        let container = Container::new();
        let events = Arc::new(EventInterface::new(crate::core::SystemRef::from_ref(
            &system,
        )));
        let gpu = NvHostGpu::new(
            crate::core::SystemRef::from_ref(&system),
            events,
            &container,
        );
        let mut alloc = IoctlAllocGpfifoEx2::default();
        assert_eq!(gpu.alloc_gpfifo_ex2(&mut alloc, 1), NvResult::Success);

        let mut params = IoctlSubmitGpfifo {
            num_entries: 2,
            ..Default::default()
        };
        let commands = [0u8; 8];

        let result = gpu.submit_gpfifo_base1(&mut params, &commands, true);

        assert_eq!(result, NvResult::InvalidSize);
    }

    #[test]
    fn query_event_returns_three_known_events() {
        let mut system = crate::core::System::new_for_test();
        system.set_gpu_core(Box::new(FakeGpuCore::default()));
        let container = Container::new();
        let events = Arc::new(EventInterface::new(crate::core::SystemRef::from_ref(
            &system,
        )));
        let gpu = NvHostGpu::new(
            crate::core::SystemRef::from_ref(&system),
            events,
            &container,
        );

        assert!(gpu.query_event(1).is_some());
        assert!(gpu.query_event(2).is_some());
        assert!(gpu.query_event(3).is_some());
        assert!(gpu.query_event(4).is_none());
    }
}

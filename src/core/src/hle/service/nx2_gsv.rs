// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of eden-nx2/core/hle/service/nx2_gsv.h.
//! Raw-session helpers, nvsrv, syncpt, GPU, VI and video counterparts. GSV factory is pending.

use super::kernel_helpers::ServiceContext;
use super::service::build_handler_map;
use crate::core::SystemRef;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::hle_ipc::{HLERequestContext, SessionRequestHandler, SessionRequestManager};
use super::ipc_helpers::ResponseBuilder;
use super::service::{FunctionInfo, ServiceFramework};
use crate::hle::kernel::k_resource_limit::LimitableResource;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};

/// Returns the client endpoint's registered object identity. The existing IPC
/// machinery resolves it into a guest handle when serializing the response.
/// The request owner supplies upstream's current process across the Rust bridge.
pub fn create_raw_session(
    ctx: &mut HLERequestContext,
    iface: Arc<dyn SessionRequestHandler>,
) -> u64 {
    let process = ctx
        .owner_process_arc()
        .expect("raw session requires a request owner");
    let limit = process
        .lock()
        .unwrap()
        .resource_limit
        .clone()
        .expect("raw session requires a process resource limit");
    assert!(limit.reserve(LimitableResource::SessionCountMax, 1));

    let (server, queue, wakeup) = {
        let manager = ctx
            .get_manager()
            .expect("raw session requires a manager")
            .lock()
            .unwrap();
        (
            manager.get_server_manager(),
            manager.pending_registrations().cloned(),
            manager.server_wakeup().cloned(),
        )
    };
    let manager = Arc::new(Mutex::new(
        SessionRequestManager::from_parent_server_manager_endpoints(server, queue, wakeup),
    ));
    manager.lock().unwrap().set_session_handler(iface);
    let object_id = ctx
        .create_session_with_manager_object_id(manager.clone())
        .expect("raw session creation requires a live owner");
    // KSession's Rust lifecycle requires explicit commitment to release the
    // reservation at finalization, matching upstream's registered KSession.
    process
        .lock()
        .unwrap()
        .get_session_by_object_id(object_id)
        .unwrap()
        .lock()
        .unwrap()
        .mark_session_resource_committed();
    assert!(ctx.register_last_created_session_with_manager(manager));
    object_id
}

pub fn tipc_reply_words(ctx: &mut HLERequestContext, words: &[u32]) {
    let mut rb = ResponseBuilder::new(ctx, 2 + words.len() as u32, 0, 0);
    // TIPC has one result word, unlike ResponseBuilder::push_result's CMIF pair.
    rb.push_u32(RESULT_SUCCESS.get_inner_value());
    for &word in words {
        rb.push_u32(word);
    }
}

pub fn return_raw_session(ctx: &mut HLERequestContext, iface: Arc<dyn SessionRequestHandler>) {
    let client = create_raw_session(ctx, iface);
    let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
    rb.push_result(RESULT_SUCCESS);
    rb.push_copy_object_id(client);
}

// Upstream intentionally leaves this particular experimental server empty.
#[derive(Default)]
pub struct INx2NvsrvSession {
    handlers: BTreeMap<u32, FunctionInfo>,
}

impl INx2NvsrvSession {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SessionRequestHandler for INx2NvsrvSession {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }
}

impl ServiceFramework for INx2NvsrvSession {
    fn get_service_name(&self) -> &str {
        "nx2:nvsrv"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
}

// SDK camera context, matching INx2CameraSession in Eglantine ae9d3eb8
// nx2_gsv.h. Only the factory/lifetime ABI is established. Camera operations
// without a verified contract report unsupported instead of empty success.
struct INx2CameraSession {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    _context: Arc<Mutex<u32>>,
    events: Mutex<CameraEventState>,
}
struct CameraEventState {
    context: ServiceContext,
    handles: BTreeMap<u64, u32>,
}
impl INx2CameraSession {
    // Inferred unavailable-device result: MKW main+0xcded80..0xcded88
    // explicitly tolerates 0xdc4 from named-camera CMIF3. This backend has
    // no camera producer. Return that recoverable condition, never success
    // with a fabricated camera identity. Original symbolic error name unknown.
    const RESULT_CAMERA_UNAVAILABLE: ResultCode = ResultCode::new(0xdc4);
    // Main +0x1c6de30 CMIF21 takes u64 and copies a readable event.
    // Parser +0x1c6d270 stores COPY-array offset at metadata byte+1;
    // +0xcdc5cc attaches it via nn::os::AttachReadableHandleToSystemEvent.
    // No camera producer: initial unsignaled state is an explicit model policy.
    fn get_event(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let id = super::ipc_helpers::RequestParser::new(ctx).pop_u64();
        let event = {
            let mut state = this.events.lock().unwrap();
            let handle = if let Some(&handle) = state.handles.get(&id) { handle } else {
                let handle = state.context.create_event(format!("CameraEvent{id}"));
                state.handles.insert(id, handle);
                handle
            };
            state.context.get_event(handle).expect("camera event allocation")
        };
        let object = event.copy_object_id(ctx).expect("camera event requires request owner");
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object);
        log::info!("nx2:camera CMIF21 copied readable event for {id:#x}");
    }
    fn open_camera(_: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::info!("nx2:camera CMIF3: no camera producer available");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(Self::RESULT_CAMERA_UNAVAILABLE);
    }
    // Main +0x1c6c860 / +0x1c6e4f0: CMIF29 takes 40 opaque bytes
    // followed by a u64, and returns only a result. Its original method name
    // is unknown. Caller +0x25a680 tolerates 0xdc4 at +0x25a6d8..e4,
    // just as OpenCamera does. No producer exists to apply this request to.
    fn camera_command_29(_: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = super::ipc_helpers::RequestParser::new(ctx);
        let parameters: [u32; 10] = rp.pop_raw();
        let camera = rp.pop_u64();
        log::info!("nx2:camera CMIF29: no camera producer available, camera={camera:#x}, parameters={parameters:08x?}");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(Self::RESULT_CAMERA_UNAVAILABLE);
    }
}
impl SessionRequestHandler for INx2CameraSession {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }
}
impl ServiceFramework for INx2CameraSession {
    fn get_service_name(&self) -> &str { "nx2:camera" }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> { &self.handlers }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> { &self.handlers_tipc }
    fn report_unimplemented_function(&self, ctx: &mut HLERequestContext, _: Option<&FunctionInfo>) {
        log::error!("Unsupported nx2:camera command {} words={:08x?}", ctx.get_command(), &ctx.command_buffer()[..12]);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(super::ipc_helpers::RESULT_NOT_SUPPORTED);
    }
}

struct SlotRequest {
    syncpoint: u32,
    // Arc provides stable lifetime for the field captured by a GPU callback.
    pending: Arc<AtomicBool>,
    handle: Option<u64>,
}

impl Default for SlotRequest {
    fn default() -> Self {
        Self {
            syncpoint: 0,
            pending: Arc::new(AtomicBool::new(false)),
            handle: None,
        }
    }
}

struct SyncptState {
    context: ServiceContext,
    events: BTreeMap<u32, u32>,
    requests: BTreeMap<u32, SlotRequest>,
}

pub struct INx2SyncptSession {
    system: SystemRef,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    state: Mutex<SyncptState>,
    // Upstream SharedState { mutex, alive }; callbacks outlive the session.
    shared_alive: Arc<Mutex<bool>>,
}

impl INx2SyncptSession {
    const HOST_SYNCPOINTS: u32 = 192;

    pub fn new(system: SystemRef) -> Self {
        Self {
            system,
            handlers: BTreeMap::new(),
            handlers_tipc: build_handler_map(&[
                (0, Some(Self::handshake), "Handshake"),
                (1, Some(Self::get_count), "GetCount (guessed)"),
                (22, Some(Self::get_slot_event), "GetSlotEvent (guessed)"),
                (103, Some(Self::query_counter), "QueryCounter"),
                (
                    201,
                    Some(Self::request_slot_event),
                    "RequestSlotEvent (guessed)",
                ),
                (
                    202,
                    Some(Self::cancel_slot_event),
                    "CancelSlotEvent (guessed)",
                ),
            ]),
            state: Mutex::new(SyncptState {
                context: ServiceContext::new("nx2:syncpt".into()),
                events: BTreeMap::new(),
                requests: BTreeMap::new(),
            }),
            shared_alive: Arc::new(Mutex::new(true)),
        }
    }

    fn handshake(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let words = [ctx.command_buffer()[2], ctx.command_buffer()[3]];
        log::info!(
            "nx2:syncpt TIPC 0 handshake {:#x}",
            words[0] as u64 | ((words[1] as u64) << 32)
        );
        tipc_reply_words(ctx, &words);
    }

    fn get_count(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) nx2:syncpt TIPC 1, returning 32");
        tipc_reply_words(ctx, &[32]);
    }

    fn query_counter(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        // Handler table is owned exclusively by INx2SyncptSession.
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let syncpoint = !ctx.command_buffer()[2];
        let value = if syncpoint < Self::HOST_SYNCPOINTS {
            this.system
                .get()
                .host1x_core()
                .expect("syncpt requires Host1x")
                .get_guest_syncpoint_value(syncpoint) as u64
        } else {
            0
        };
        log::debug!("nx2:syncpt 103 syncpoint={} value={:#x}", syncpoint, value);
        tipc_reply_words(ctx, &[value as u32, (value >> 32) as u32, 0]);
    }

    fn get_slot_event(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let slot = ctx.command_buffer()[2];
        log::warn!(
            "(STUBBED) nx2:syncpt TIPC 22 slot={:#x}, returning an event",
            slot
        );
        let mut state = this.state.lock().unwrap();
        let handle = if let Some(&handle) = state.events.get(&slot) {
            handle
        } else {
            let handle = state
                .context
                .create_event(format!("nx2:syncpt:Slot{slot:x}"));
            state.events.insert(slot, handle);
            handle
        };
        let event = state.context.get_event(handle).unwrap();
        let object = event
            .copy_object_id(ctx)
            .expect("slot event requires request owner");
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object);
    }

    fn request_slot_event(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let words = ctx.command_buffer();
        let slot = words[2];
        let syncpoint = !words[3];
        let value = words[4] as u64 | ((words[5] as u64) << 32);
        let mode = words[6];
        log::info!(
            "nx2:syncpt TIPC 201 slot={:#x} syncpoint={} value={:#x} mode={}",
            slot,
            syncpoint,
            value,
            mode
        );
        let mut state = this.state.lock().unwrap();
        let handle = if let Some(&handle) = state.events.get(&slot) {
            handle
        } else {
            let handle = state
                .context
                .create_event(format!("nx2:syncpt:Slot{slot:x}"));
            state.events.insert(slot, handle);
            handle
        };
        let event = state.context.get_event(handle).unwrap();
        if syncpoint >= Self::HOST_SYNCPOINTS {
            log::warn!(
                "nx2:syncpt TIPC 201 syncpoint {} out of range, signalling",
                syncpoint
            );
            event.signal();
            tipc_reply_words(ctx, &[]);
            return;
        }
        let request = state.requests.entry(slot).or_default();
        this.cancel(request);
        event.clear();
        request.syncpoint = syncpoint;
        request.pending.store(true, Ordering::SeqCst);
        let shared = this.shared_alive.clone();
        let pending = request.pending.clone();
        request.handle = this
            .system
            .get()
            .host1x_core()
            .expect("syncpt requires Host1x")
            .register_guest_action(
                syncpoint,
                value as u32,
                Box::new(move || {
                    let alive = shared.lock().unwrap();
                    if !*alive {
                        return;
                    }
                    pending.store(false, Ordering::SeqCst);
                    event.signal();
                }),
            );
        tipc_reply_words(ctx, &[]);
    }

    fn cancel_slot_event(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let slot = ctx.command_buffer()[2];
        log::info!("nx2:syncpt TIPC 202 slot={:#x}", slot);
        let mut state = this.state.lock().unwrap();
        if let Some(request) = state.requests.get_mut(&slot) {
            this.cancel(request);
        }
        let handle = if let Some(&handle) = state.events.get(&slot) {
            handle
        } else {
            let handle = state
                .context
                .create_event(format!("nx2:syncpt:Slot{slot:x}"));
            state.events.insert(slot, handle);
            handle
        };
        state.context.get_event(handle).unwrap().signal();
        tipc_reply_words(ctx, &[]);
    }

    fn cancel(&self, request: &mut SlotRequest) -> bool {
        if request.pending.swap(false, Ordering::SeqCst) {
            if let Some(handle) = request.handle {
                self.system
                    .get()
                    .host1x_core()
                    .expect("syncpt requires Host1x")
                    .deregister_guest_action(request.syncpoint, handle);
            }
            return true;
        }
        false
    }
}

impl Drop for INx2SyncptSession {
    fn drop(&mut self) {
        *self.shared_alive.lock().unwrap() = false;
        let mut state = self.state.lock().unwrap();
        let SyncptState {
            context,
            events,
            requests,
        } = &mut *state;
        for request in requests.values_mut() {
            self.cancel(request);
        }
        for &handle in events.values() {
            context.close_event(handle);
        }
    }
}

impl SessionRequestHandler for INx2SyncptSession {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }
}

impl ServiceFramework for INx2SyncptSession {
    fn get_service_name(&self) -> &str {
        "nx2:syncpt"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

// INx2GpuSession and its private state map to the same header-only upstream class.
use super::nvdrv::devices::nx2_stub_device::Nx2GpuMemoryRegistry;
use super::os::event::Event;
use crate::hle::kernel::k_memory_block::{KMemoryPermission, KMemoryState, PAGE_SIZE};
use crate::hle::kernel::k_memory_manager::{Direction, KMemoryManager, Pool};
use crate::hle::kernel::k_page_group::KPageGroup;

struct GpuOnlyRegion {
    cpu_address: u64,
    size: u64,
    pages: Box<KPageGroup>,
}
struct ReleasedRegion {
    region: GpuOnlyRegion,
    done: Arc<AtomicBool>,
}
#[derive(Clone, Copy)]
struct GpuMemory {
    address: u64,
    size: u64,
}
struct GpuSessionState {
    context: ServiceContext,
    events: [Option<u32>; INx2GpuSession::NUM_IDS],
    memory: BTreeMap<u32, GpuMemory>,
    next_memory_handle: u32,
    gpu_only_regions: BTreeMap<u64, GpuOnlyRegion>,
    released_regions: Vec<ReleasedRegion>,
}
pub struct INx2GpuSession {
    system: SystemRef,
    self_reference: std::sync::Weak<Self>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    state: Mutex<GpuSessionState>,
}
impl INx2GpuSession {
    const NUM_IDS: usize = 32;

    pub fn new(system: SystemRef) -> Arc<Self> {
        Arc::new_cyclic(|self_reference| Self {
            system,
            self_reference: self_reference.clone(),
            handlers: BTreeMap::new(),
            handlers_tipc: build_handler_map(&[
                (11, Some(Self::get_id_range), "GetIdRange (guessed)"),
                (12, Some(Self::get_id_event), "GetIdEvent (guessed)"),
                (21, Some(Self::start), "Start (guessed)"),
                (22, Some(Self::open_sub_session), "OpenSubSession (guessed)"),
                (33, Some(Self::get_count33), "Unknown33 (u32 count)"),
                (
                    201,
                    Some(Self::get_user_partition),
                    "GetUserPartition (newer SDKs)",
                ),
                (
                    202,
                    Some(Self::get_gsv_partition),
                    "GetGsvPartition (newer SDKs)",
                ),
                (203, Some(Self::get_va_range), "GetVaRange (guessed)"),
                (
                    101,
                    Some(Self::sub_register_memory),
                    "CreateGpuAccessibleMemory",
                ),
                (
                    102,
                    Some(Self::sub_destroy_memory),
                    "DestroyGpuAccessibleMemory (guessed)",
                ),
                (211, Some(Self::sub_submit_job), "NormalMapGpuMemory4K"),
                (212, Some(Self::sub_modify_kind), "NormalModifyMemoryKind4K"),
                (213, Some(Self::sub_unmap_job), "NormalUnmapGpuMemory4K"),
                (
                    214,
                    Some(Self::sub_create_gpu_only_region),
                    "CreateSparseRegion",
                ),
                (
                    215,
                    Some(Self::sub_destroy_sparse_region),
                    "DestroySparseRegion",
                ),
            ]),
            state: Mutex::new(GpuSessionState {
                context: ServiceContext::new("nx2:gpu".into()),
                events: [None; Self::NUM_IDS],
                memory: BTreeMap::new(),
                next_memory_handle: 1,
                gpu_only_regions: BTreeMap::new(),
                released_regions: Vec::new(),
            }),
        })
    }

    fn event_for(&self, id: u32) -> Arc<Event> {
        let mut state = self.state.lock().unwrap();
        let id = id as usize % Self::NUM_IDS;
        let handle = match state.events[id] {
            Some(handle) => handle,
            None => {
                let handle = state.context.create_event(format!("nx2:gpu:Event{id}"));
                state.events[id] = Some(handle);
                handle
            }
        };
        state.context.get_event(handle).unwrap()
    }

    fn get_id_range(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        tipc_reply_words(ctx, &[0, Self::NUM_IDS as u32 - 1]);
    }
    fn get_id_event(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let event = this.event_for(ctx.command_buffer()[2]);
        let object = event
            .copy_object_id(ctx)
            .expect("GPU event requires request owner");
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object);
    }
    fn start(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        this.event_for(ctx.command_buffer()[2]).signal();
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }
    fn get_count33(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        tipc_reply_words(ctx, &[1]);
    }
    fn get_va_range(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        const KIND: u64 = 1;
        const START: u64 = 0x8000000;
        const SIZE: u64 = 0xC000000000 - START;
        tipc_reply_words(
            ctx,
            &[
                KIND as u32,
                (KIND >> 32) as u32,
                START as u32,
                (START >> 32) as u32,
                SIZE as u32,
                (SIZE >> 32) as u32,
            ],
        );
    }
    fn get_user_partition(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        const FIRST: u64 = 0x8000000;
        const SECOND: u64 = 0xC000000000 - FIRST;
        tipc_reply_words(
            ctx,
            &[
                FIRST as u32,
                (FIRST >> 32) as u32,
                SECOND as u32,
                (SECOND >> 32) as u32,
            ],
        );
    }
    fn get_gsv_partition(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        tipc_reply_words(ctx, &[0, 0, 0, 0]);
    }
    fn open_sub_session(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        return_raw_session(
            ctx,
            this.self_reference.upgrade().expect("live GPU session"),
        );
    }
    fn sub_register_memory(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let b = ctx.command_buffer();
        let start = b[2] as u64 | ((b[3] as u64) << 32);
        let last = b[4] as u64 | ((b[5] as u64) << 32);
        let size = last.wrapping_sub(start).wrapping_add(1);
        log::info!(
            "nx2:gpu 101 start={start:#x} last={last:#x} flags={:#x} {:#x}",
            b[6],
            b[7]
        );
        if last > start {
            Nx2GpuMemoryRegistry::get().register(start, size);
            if size == 0x2000 {
                this.system
                    .get()
                    .gpu_core()
                    .expect("GSV requires GPU")
                    .nx2_add_semaphore_region(start);
            }
        }
        let mut state = this.state.lock().unwrap();
        let handle = state.next_memory_handle;
        state.next_memory_handle = handle.wrapping_add(1);
        state.memory.insert(
            handle,
            GpuMemory {
                address: start,
                size,
            },
        );
        tipc_reply_words(ctx, &[handle]);
    }
    // Upstream uninstantiated template retained in its class owner.
    #[allow(dead_code)]
    fn sub_slot_job<const SLOT: usize>(&self, ctx: &mut HLERequestContext) {
        self.event_for(ctx.command_buffer()[SLOT]).signal();
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }
    fn sub_submit_job(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let b = ctx.command_buffer();
        let first = b[3] as u64 | ((b[4] as u64) << 32);
        let last = b[5] as u64 | ((b[6] as u64) << 32);
        let offset = b[8] as u64 | ((b[9] as u64) << 32);
        log::info!("nx2:gpu 211 map [{first:#x}, {last:#x}] handle={} offset={offset:#x} slot={}", b[7], b[17]);
        let memory = this.state.lock().unwrap().memory.get(&b[7]).copied();
        if let Some(memory) = memory.filter(|_| last > first) {
            Nx2GpuMemoryRegistry::get().map_at(
                first,
                memory.address.wrapping_add(offset),
                last.wrapping_sub(first).wrapping_add(1),
            );
        } else {
            log::warn!("nx2:gpu 211 map of unknown handle {}", b[7]);
        }
        this.event_for(b[17]).signal();
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }
    fn sub_modify_kind(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let kind = ctx.command_buffer()[7] & 0xff;
        this.event_for(ctx.command_buffer()[10]).signal();
        tipc_reply_words(ctx, &[kind]);
    }
    fn sub_unmap_job(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let w = ctx.command_buffer();
        let first = w[3] as u64 | ((w[4] as u64) << 32);
        let last = w[5] as u64 | ((w[6] as u64) << 32);
        log::debug!("nx2:gpu 213 unmap [{first:#x}, {last:#x}] slot={}", w[8]);
        if last > first {
            Nx2GpuMemoryRegistry::get().unmap(first, last.wrapping_sub(first).wrapping_add(1));
        }
        this.signal_after_gpu(w[8]);
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }
    fn signal_after_gpu(&self, slot: u32) {
        let event = self.event_for(slot);
        // The Arc keeps the callback's event alive without a dangling C++ this/event capture.
        self.system
            .get()
            .gpu_core()
            .expect("GSV requires GPU")
            .run_in_order(Box::new(move || event.signal()));
    }
    fn sub_destroy_memory(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let w = ctx.command_buffer();
        let mut state = this.state.lock().unwrap();
        if let Some(memory) = state.memory.get(&w[2]).copied() {
            if memory.size != 0 {
                Nx2GpuMemoryRegistry::get().unmap(memory.address, memory.size);
            }
            if memory.size == 0x2000 {
                this.system
                    .get()
                    .gpu_core()
                    .expect("GSV requires GPU")
                    .nx2_remove_semaphore_region(memory.address);
            }
            state.memory.remove(&w[2]);
        } else {
            log::warn!("nx2:gpu 102 destroy of unknown handle {}", w[2]);
        }
        drop(state);
        this.signal_after_gpu(w[3]);
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }
    /// Whether `nx2:gpu` command 214 leaves its GPU-only regions unbacked.
    ///
    /// Eager backing commits the whole region out of the SystemNonSecure pool;
    /// Mario Kart World asks for about 15 GiB that way against a 9.5 GiB pool.
    /// This is opt-in while the title-side consequences are being measured.
    fn lazy_sparse_regions_enabled() -> bool {
        std::env::var_os("RUZU_NX2_LAZY_SPARSE")
            .is_some_and(|value| value.to_string_lossy().starts_with('1'))
    }

    fn sub_create_gpu_only_region(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let w = ctx.command_buffer();
        let start = w[3] as u64 | ((w[4] as u64) << 32);
        let last = w[5] as u64 | ((w[6] as u64) << 32);
        // Preserve the upstream diagnostic: words 7..10 carry page/creation policy.
        log::info!(
            "nx2:gpu 214 words {:#x} {:#x} {:#x} {:#x} {:#x} {:#x} {:#x} {:#x} {:#x} {:#x} {:#x} {:#x}",
            w[1], w[2], w[3], w[4], w[5], w[6], w[7], w[8], w[9], w[10], w[11], w[12]
        );
        let size = last.wrapping_add(1).wrapping_sub(start);
        if last > start && size % PAGE_SIZE as u64 == 0 {
            this.free_released_regions();
            if Self::lazy_sparse_regions_enabled() {
                // A sparse region is address space, not memory: backing the
                // whole range up front commits guest pages the title may never
                // map, which is what exhausts the SystemNonSecure pool. Leave
                // it unbacked and let reads of never-mapped pages return zeros,
                // which is what sparse semantics call for anyway.
                log::info!(
                    "nx2:gpu 214 GPU-only region [{start:#x}, {last:#x}] left unbacked \
                     ({size:#x} bytes, lazy sparse regions)"
                );
            } else if let Some(address) = this.back_gpu_only_region(start, size) {
                Nx2GpuMemoryRegistry::get().map_at(start, address, size);
                log::info!("nx2:gpu 214 GPU-only region [{start:#x}, {last:#x}] backed at {address:#x}");
            } else {
                // Nothing is mapped here, so every read of this range returns
                // zeros: black textures and popping scenery. Reporting the
                // failure instead was measured and is worse - Mario Kart World
                // aborts within 120 ms of receiving the error (guest backtrace
                // right after two of these lines), so the success answer is
                // load-bearing. Giving the range a device address with no
                // guest memory was also tried (2026-11-10) and is worse too:
                // the title writes into these regions (about 510 MiB of page
                // writes in the first 31 s were dropped) and the GPU thread
                // then died creating an image there (ERROR_OUT_OF_DEVICE_MEMORY
                // on an 8 GiB card). Leave it, and name the consequence here.
                log::error!(
                    "nx2:gpu 214 GPU-only region [{start:#x}, {last:#x}] is UNBACKED \
                     ({size:#x} bytes); reads of it will return zeros"
                );
            }
        } else {
            log::warn!("nx2:gpu 214 unexpected region [{start:#x}, {last:#x}]");
        }
        this.event_for(w[11]).signal();
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }
    fn sub_destroy_sparse_region(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let w = ctx.command_buffer();
        let first = w[3] as u64 | ((w[4] as u64) << 32);
        let last = w[5] as u64 | ((w[6] as u64) << 32);
        log::info!("nx2:gpu 215 destroy sparse region [{first:#x}, {last:#x}] words {:#x} {:#x} {:#x} {:#x}", w[1], w[2], w[7], w[8]);
        this.free_released_regions();
        if last > first {
            let size = last.wrapping_sub(first).wrapping_add(1);
            Nx2GpuMemoryRegistry::get().unmap(first, size);
            let mut state = this.state.lock().unwrap();
            if let Some(region) = state
                .gpu_only_regions
                .get(&first)
                .filter(|r| r.size == size)
            {
                Nx2GpuMemoryRegistry::get().release_backing(region.cpu_address, region.size);
                let done = Arc::new(AtomicBool::new(false));
                let completion = done.clone();
                this.system
                    .get()
                    .gpu_core()
                    .expect("GSV requires GPU")
                    .run_in_order(Box::new(move || completion.store(true, Ordering::Release)));
                let region = state.gpu_only_regions.remove(&first).unwrap();
                state.released_regions.push(ReleasedRegion { region, done });
            }
        }
        this.signal_after_gpu(w[8]);
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }
    fn free_released_regions(&self) {
        let process = self.system.get().application_process();
        self.state
            .lock()
            .unwrap()
            .released_regions
            .retain(|released| {
                if !released.done.load(Ordering::Acquire) {
                    return true;
                }
                let region = &released.region;
                if let Some(process) = &process {
                    if process
                        .lock()
                        .unwrap()
                        .page_table
                        .get_base_mut()
                        .unmap_page_group(
                            region.cpu_address as usize,
                            &region.pages,
                            KMemoryState::IO_MEMORY,
                        )
                        != 0
                    {
                        log::error!(
                            "nx2:gpu could not unmap GPU-only backing {:#x}+{:#x}",
                            region.cpu_address,
                            region.size
                        );
                        return false; // Drop metadata, retain allocated page references like upstream.
                    }
                }
                region.pages.close();
                false
            });
    }
    fn back_gpu_only_region(&self, gpu_address: u64, size: u64) -> Option<u64> {
        let process = self.system.get().application_process()?;
        let kernel =
            crate::hle::kernel::kernel::get_kernel_mut().expect("GSV requires live kernel");
        let manager = kernel
            .get_system_system_resource()
            .expect("system resource initialized")
            .block_info_manager_arc();
        let mut pg = Box::new(KPageGroup::with_block_info_manager(Some(manager)));
        // These regions belong in SystemNonSecure and must stay there. Falling
        // back to Applet and Application when it is exhausted was tried and
        // breaks the title: borrowing 1.5 GiB from those pools at t=285.7 was
        // followed at t=319.7 by a guest `svcBreak` / Userspace PANIC with
        // debug_buffer_err_code=E401. Application is the title's own heap, so
        // satisfying GPU-only regions out of it starves the allocation the
        // title makes next. An unbacked region renders black; a drained
        // application pool aborts the game, which is worse.
        if kernel.memory_manager_mut().allocate_and_open(
            &mut pg,
            (size / PAGE_SIZE as u64) as usize,
            KMemoryManager::encode_option(Pool::SystemNonSecure, Direction::FromBack),
        ) != 0
        {
            log::error!("nx2:gpu 214 could not allocate {size:#x} bytes");
            return None;
        }
        let mut process = process.lock().unwrap();
        let table = process.page_table.get_base_mut();
        let region_start = table.get_alias_code_region_start() as u64;
        let region_size = table.get_alias_code_region_size() as u64;
        const ALIGNMENT: u64 = 0x200000;
        let mut offset = 0u64;
        while offset.wrapping_add(size) <= region_size {
            let address = region_start.wrapping_add(offset);
            if table.map_page_group(
                address as usize,
                &pg,
                KMemoryState::IO_MEMORY,
                KMemoryPermission::USER_READ_WRITE,
            ) == 0
            {
                self.state.lock().unwrap().gpu_only_regions.insert(
                    gpu_address,
                    GpuOnlyRegion {
                        cpu_address: address,
                        size,
                        pages: pg,
                    },
                );
                return Some(address);
            }
            offset = offset.wrapping_add(ALIGNMENT);
        }
        log::error!("nx2:gpu 214 no room for {size:#x} bytes in alias code [{region_start:#x}, +{region_size:#x})");
        pg.close();
        None
    }
}
impl Drop for INx2GpuSession {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap();
        for handle in state.events.iter().flatten() {
            state.context.close_event(*handle);
        }
        // Upstream destroys page-group metadata here without eagerly closing live backing pages.
    }
}
impl SessionRequestHandler for INx2GpuSession {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }
}
impl ServiceFramework for INx2GpuSession {
    fn get_service_name(&self) -> &str {
        "nx2:gpu"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

// Header-only INx2Vi2Session; state remains owned by this service.
#[derive(Default)]
struct PresentStats {
    frame_us: i64,
    fence_us: i64,
    composite_us: i64,
    pace_us: i64,
    max_fence_us: i64,
    max_frame_us: i64,
    hitches: i64,
    presents: u32,
    last_present: Option<std::time::Instant>,
}
struct Vi2State {
    context: ServiceContext,
    events: Vec<u32>,
    vsync_timer: Option<Arc<parking_lot::Mutex<crate::core_timing::EventType>>>,
    presents: u64,
    wait_timeouts: u32,
    stats: PresentStats,
    previous_fence_id: u32,
    previous_fence_value: u32,
    next_vblank: Option<std::time::Instant>,
}
pub struct INx2Vi2Session {
    system: SystemRef,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    objects: Arc<Mutex<Nx2NativeObjects>>,
    state: Mutex<Vi2State>,
    vsync: Arc<Mutex<Vec<Arc<Event>>>>,
    capture_pending: Arc<AtomicBool>,
}
impl INx2Vi2Session {
    pub fn new(system: SystemRef, objects: Arc<Mutex<Nx2NativeObjects>>) -> Self {
        Self {
            system,
            objects,
            handlers: BTreeMap::new(),
            handlers_tipc: build_handler_map(&[
                (1, Some(Self::return_one), "Unknown1"),
                (3, Some(Self::return_one), "GetDisplayCount (guessed)"),
                (
                    5,
                    Some(Self::get_timestamp_recorder_count),
                    "GetTimestampRecorderCount (guessed)",
                ),
                (11, Some(Self::return_event), "Unknown11"),
                (31, Some(Self::get_vsync_event), "GetVsyncEvent (guessed)"),
                (
                    32,
                    Some(Self::get_vsync_event),
                    "GetDisplayVsyncEvent (guessed)",
                ),
                (
                    104,
                    Some(Self::succeed_quietly),
                    "Unknown104 (display default)",
                ),
                (
                    105,
                    Some(Self::succeed_quietly),
                    "Unknown105 (display default)",
                ),
                (228, Some(Self::cancel_client_buffer), "CancelClientBuffer"),
                (
                    229,
                    Some(Self::present_client_buffer),
                    "PresentClientBuffer",
                ),
            ]),
            state: Mutex::new(Vi2State {
                context: ServiceContext::new("nx2:vi2".into()),
                events: Vec::new(),
                vsync_timer: None,
                presents: 0,
                wait_timeouts: 0,
                stats: PresentStats::default(),
                previous_fence_id: u32::MAX,
                previous_fence_value: 0,
                next_vblank: None,
            }),
            vsync: Arc::new(Mutex::new(Vec::new())),
            capture_pending: Arc::new(AtomicBool::new(false)),
        }
    }

    fn present_client_buffer(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let w = ctx.command_buffer();
        let mut state = this.state.lock().unwrap();
        state.presents = state.presents.wrapping_add(1);
        crate::core::G_NX2_PRESENT_COUNT.fetch_add(1, Ordering::Relaxed);
        if state.presents <= 4 || state.presents % 600 == 0 {
            log::info!(
                "nx2:vi2 229 present #{} token={:#x} slot={} image={}",
                state.presents,
                w[2] as u64 | ((w[3] as u64) << 32),
                w[4],
                w[5]
            );
        }
        static FRAMES_AHEAD: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
            std::env::var("EDEN_NX2_FRAMES_AHEAD").map_or(true, |s| !s.starts_with('0'))
        });
        let entered = std::time::Instant::now();
        if *FRAMES_AHEAD {
            log::debug!(
                "nx2:vi2 present #{} fence ({}, {:#x}), waiting for ({}, {:#x}); now {:#x}",
                state.presents,
                !w[22],
                w[24],
                state.previous_fence_id,
                state.previous_fence_value,
                if !w[22] < 192 {
                    this.system
                        .get()
                        .host1x_core()
                        .expect("VI requires Host1x")
                        .get_guest_syncpoint_value(!w[22])
                } else {
                    0
                }
            );
            let (id, value) = (state.previous_fence_id, state.previous_fence_value);
            this.wait_for_frame(&mut state, id, value);
            state.previous_fence_id = !w[22];
            state.previous_fence_value = w[24];
        } else {
            this.wait_for_frame(&mut state, !w[22], w[24]);
        }
        let waited = std::time::Instant::now();
        // VI2's crop-present flag is bit 1 of the presentation descriptor.
        let crop = (w[8] & (1 << 1) != 0).then_some([w[6], w[7]]);
        this.composite(state.presents, w[5], crop, !w[22], w[24]);
        let composited = std::time::Instant::now();
        Self::pace_to_vblank(&mut state);
        let paced = std::time::Instant::now();
        let stats = &mut state.stats;
        let fence_us = (waited - entered).as_micros() as i64;
        stats.fence_us += fence_us;
        stats.composite_us += (composited - waited).as_micros() as i64;
        stats.pace_us += (paced - composited).as_micros() as i64;
        stats.max_fence_us = stats.max_fence_us.max(fence_us);
        if let Some(last) = stats.last_present {
            let frame = (entered - last).as_micros() as i64;
            stats.frame_us += frame;
            stats.max_frame_us = stats.max_frame_us.max(frame);
            stats.hitches += i64::from(frame > 100_000);
        }
        stats.last_present = Some(entered);
        stats.presents += 1;
        if stats.presents == 600 {
            log::info!("nx2:vi2 last 600 presents: frame {} us (max {}, {} over 100 ms), blocked on the GPU fence {} us (max {}), composite {} us, vblank pacing {} us",
                stats.frame_us / 599, stats.max_frame_us, stats.hitches,
                stats.fence_us / 600, stats.max_fence_us, stats.composite_us / 600, stats.pace_us / 600);
            *stats = PresentStats {
                last_present: Some(entered),
                ..Default::default()
            };
        }
        const AVAILABLE_SLOTS: u64 = 0x7fff;
        tipc_reply_words(
            ctx,
            &[
                AVAILABLE_SLOTS as u32,
                (AVAILABLE_SLOTS >> 32) as u32,
                0,
                0,
                0,
                0,
            ],
        );
    }

    fn pace_to_vblank(state: &mut Vi2State) {
        let speed = common::settings::speed_limit();
        if !*common::settings::values().use_speed_limit.get_value() || speed == 0 {
            return;
        }
        let interval = std::time::Duration::from_nanos(100_000_000_000 / 60 / speed as u64);
        let now = std::time::Instant::now();
        let mut next = state.next_vblank.unwrap_or(now);
        if now > next + interval {
            next = now;
        } else if now < next {
            std::thread::sleep(next - now);
        }
        state.next_vblank = Some(next + interval);
    }

    fn wait_for_frame(&self, state: &mut Vi2State, fence_id: u32, fence_value: u32) {
        if fence_id >= 192 {
            return;
        }
        let syncpoints = self.system.get().host1x_core().expect("VI requires Host1x");
        let start = std::time::Instant::now();
        // Upstream gives up on the frame's fence after 4 s. That is shorter
        // than stalls this emulator really has - a burst of synchronous
        // pipeline builds on a cold shader cache holds the GPU thread ~10 s -
        // and giving up is not harmless: the present returns, the title moves
        // on to the next frame while the GPU thread is still consuming the
        // previous one, and the two diverge. Measured on Mario Kart World,
        // three runs out of three: semaphore acquires that are never
        // released, then command streams with nonsense state (a render target
        // with 879 layers, a macro slot of 4096) and a dead GPU thread. With
        // the wait left to finish, the same stalls pass without incident.
        // Keep a bound so a truly lost fence still surfaces, but well beyond
        // any stall observed. RUZU_VI2_FENCE_TIMEOUT_MS overrides it.
        static FENCE_TIMEOUT_MS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        let timeout_ms = *FENCE_TIMEOUT_MS.get_or_init(|| {
            std::env::var("RUZU_VI2_FENCE_TIMEOUT_MS")
                .ok()
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(120_000)
        });
        let deadline = start + std::time::Duration::from_millis(timeout_ms);
        if !syncpoints.is_ready_guest(fence_id, fence_value) {
            let wake = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
            let signal = wake.clone();
            syncpoints.register_guest_action(
                fence_id,
                fence_value,
                Box::new(move || {
                    *signal.0.lock().unwrap() = true;
                    signal.1.notify_all();
                }),
            );
            let mut done = wake.0.lock().unwrap();
            while !*done
                && self.system.get().is_powered_on()
                && std::time::Instant::now() < deadline
            {
                done = wake
                    .1
                    .wait_timeout(done, std::time::Duration::from_millis(50))
                    .unwrap()
                    .0;
            }
            // Source deliberately leaves the action registered after a timeout.
        }
        if !syncpoints.is_ready_guest(fence_id, fence_value) {
            let previous = state.wait_timeouts;
            state.wait_timeouts = previous.wrapping_add(1);
            if previous < 64 {
                log::warn!(
                    "nx2:vi2 present #{} fence ({}, {}) not reached in 4 s (now {})",
                    state.presents,
                    fence_id,
                    fence_value,
                    syncpoints.get_guest_syncpoint_value(fence_id)
                );
            }
        }
        if state.presents <= 8 || state.presents % 600 == 0 {
            log::info!(
                "nx2:vi2 present #{} fence ({}, {}) now {}, waited {} us",
                state.presents,
                fence_id,
                fence_value,
                syncpoints.get_guest_syncpoint_value(fence_id),
                start.elapsed().as_micros()
            );
        }
    }

    fn composite(
        &self,
        presents: u64,
        image_id: u32,
        crop: Option<[u32; 2]>,
        fence_id: u32,
        fence_value: u32,
    ) {
        let (address, layout) = {
            let objects = self.objects.lock().unwrap();
            let Some(image) = objects.images.get(&image_id) else {
                return;
            };
            let Some(memory) = objects.memories.get(&image.memory) else {
                return;
            };
            if memory.address == 0 {
                return;
            }
            (
                memory.address,
                nx2_calculate_image_layout(&image.preference),
            )
        };
        let Some(device) = Nx2GpuMemoryRegistry::get().device_address_of(address) else {
            log::warn!(
                "nx2:vi2 present: image {} at {:#x} has no device mapping",
                image_id,
                address
            );
            return;
        };
        let layers = vec![crate::gpu_core::FramebufferConfig {
            address: device,
            offset: 0,
            width: layout.width,
            height: layout.height,
            stride: (layout.pitch / 4) as u32,
            pixel_format: super::nvnflinger::pixel_format::PixelFormat::Rgba8888,
            transform_flags: super::nvnflinger::buffer_transform_flags::BufferTransformFlags::empty(
            ),
            // Correction to Eden-NX2's full-allocation crop: VI2 command 229
            // carries packed u16 source bounds in words 6 and 7. Its maximum
            // coordinates are inclusive; NormalizeCrop expects exclusive ends.
            // Keep the allocation dimensions/stride for texture lookup/upload.
            crop_rect: if let Some(crop) = crop {
                common::math_util::Rectangle {
                    left: (crop[0] & 0xffff) as i32,
                    top: (crop[0] >> 16) as i32,
                    right: (crop[1] & 0xffff) as i32 + 1,
                    bottom: (crop[1] >> 16) as i32 + 1,
                }
            } else {
                common::math_util::Rectangle::new(0, 0, layout.width as i32, layout.height as i32)
            },
            blending: crate::gpu_core::BlendMode::Opaque,
            native_tiling: true,
            ..Default::default()
        }];
        let fences = if fence_id < 192 {
            vec![super::nvdrv::nvdata::NvFence {
                id: fence_id as i32,
                value: fence_value,
            }]
        } else {
            Vec::new()
        };
        self.system
            .get()
            .gpu_core()
            .expect("VI requires GPU")
            .request_composite(layers, fences);
        self.capture_frame(presents);
        self.dump_ranges(presents);
        self.system.do_speed_limiting_now();
        let stats = self
            .system
            .get()
            .get_perf_stats()
            .expect("VI requires perf stats");
        stats.end_system_frame();
        stats.begin_system_frame();
        // NX2 session extension: pace the guest counter by the present rate.
        self.system.get().core_timing().notify_frame_presented();
    }

    fn capture_frame(&self, presents: u64) {
        static DIRECTORY: std::sync::LazyLock<Option<String>> =
            std::sync::LazyLock::new(|| std::env::var("EDEN_NX2_FRAME_CAPTURE").ok());
        let Some(directory) = DIRECTORY.as_ref() else {
            return;
        };
        if presents % 120 != 1 || self.capture_pending.swap(true, Ordering::SeqCst) {
            return;
        }
        let layout = crate::frontend::framebuffer_layout::default_frame_layout(1280, 720);
        let pixels = vec![0; layout.width as usize * layout.height as usize * 4];
        let path = format!("{directory}/nx2-frame-{presents:05}.tga");
        let pending = self.capture_pending.clone();
        let (width, height) = (layout.width, layout.height);
        self.system
            .get()
            .gpu_core()
            .expect("VI requires GPU")
            .request_screenshot(
                pixels,
                Box::new(move |pixels, invert_y| {
                    use std::io::Write;
                    let mut header = [0u8; 18];
                    header[2] = 2;
                    header[12..14].copy_from_slice(&(width as u16).to_le_bytes());
                    header[14..16].copy_from_slice(&(height as u16).to_le_bytes());
                    header[16] = 32;
                    header[17] = if invert_y { 0x08 } else { 0x28 };
                    if let Ok(mut file) = std::fs::File::create(&path) {
                        let _ = file.write_all(&header);
                        let _ = file.write_all(&pixels);
                    }
                    pending.store(false, Ordering::SeqCst);
                }),
                layout,
                super::nvnflinger::hwc_layer::LayerStackId::Default,
            );
    }

    fn dump_ranges(&self, presents: u64) {
        static SPEC: std::sync::LazyLock<String> =
            std::sync::LazyLock::new(|| std::env::var("EDEN_NX2_DUMP").unwrap_or_default());
        if SPEC.is_empty() {
            return;
        }
        let text = std::ffi::CString::new(SPEC.as_bytes()).unwrap();
        // Match strtoull's prefix, sign, overflow and end-pointer behavior.
        if presents != unsafe { libc::strtoull(text.as_ptr(), std::ptr::null_mut(), 10) } {
            return;
        }
        let directory = std::env::var("EDEN_NX2_FRAME_CAPTURE").unwrap_or_else(|_| ".".into());
        let mut position = SPEC.find(':');
        while let Some(pos) = position {
            let mut end = std::ptr::null_mut();
            let address = unsafe { libc::strtoull(text.as_ptr().add(pos + 1), &mut end, 16) };
            let size = if unsafe { *end } == b':' as libc::c_char {
                unsafe { libc::strtoull(end.add(1), &mut end, 16) }
            } else {
                0
            };
            position = SPEC[pos + 1..].find(',').map(|n| pos + 1 + n);
            let path = format!("{directory}/nx2-dump-{address:x}.bin");
            let system = self.system;
            self.system
                .get()
                .gpu_core()
                .expect("VI requires GPU")
                .run_on_gpu_thread(Box::new(move || {
                    let mut bytes = vec![0; size as usize];
                    let mut current = address;
                    let limit = address.wrapping_add(size);
                    while current < limit {
                        let chunk = (current | 0xfff)
                            .wrapping_add(1)
                            .min(limit)
                            .wrapping_sub(current);
                        if let Some(device) = Nx2GpuMemoryRegistry::get().device_address_of(current)
                        {
                            system
                                .get()
                                .gpu_core()
                                .expect("VI requires GPU")
                                .renderer_flush_region(device, chunk);
                            let offset = current.wrapping_sub(address) as usize;
                            system
                                .get()
                                .host1x_core()
                                .expect("VI requires Host1x")
                                .smmu_read_block(
                                    device,
                                    &mut bytes[offset..offset + chunk as usize],
                                );
                        }
                        current = current.wrapping_add(chunk);
                    }
                    let _ = std::fs::write(&path, &bytes);
                    log::info!(
                        "nx2: dumped {:#x} bytes at {:#x} to {}",
                        size,
                        address,
                        path
                    );
                }));
        }
    }

    fn cancel_client_buffer(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        const AVAILABLE_SLOTS: u64 = 0x7fff;
        tipc_reply_words(
            ctx,
            &[
                AVAILABLE_SLOTS as u32,
                (AVAILABLE_SLOTS >> 32) as u32,
                0,
                0,
                0,
                0,
            ],
        );
    }
    fn return_one(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) nx2:vi2 TIPC {}, returning 1", ctx.get_command());
        tipc_reply_words(ctx, &[1]);
    }
    fn get_timestamp_recorder_count(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) nx2:vi2 TIPC 5, returning 7");
        tipc_reply_words(ctx, &[7]);
    }
    fn succeed_quietly(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }
    fn return_event(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        log::warn!(
            "(STUBBED) nx2:vi2 TIPC {} [2]={:#x} [3]={:#x}, returning an event",
            ctx.get_command(),
            ctx.command_buffer()[2],
            ctx.command_buffer()[3]
        );
        let mut state = this.state.lock().unwrap();
        let name = format!("nx2:vi2:Event{}", state.events.len());
        let handle = state.context.create_event(name);
        state.events.push(handle);
        let event = state.context.get_event(handle).unwrap();
        let object = event.copy_object_id(ctx).expect("VI event requires owner");
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object);
    }
    fn get_vsync_event(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        log::info!(
            "nx2:vi2 {} vsync event, [2]={:#x}",
            ctx.get_command(),
            ctx.command_buffer()[2]
        );
        let mut state = this.state.lock().unwrap();
        let name = format!("nx2:vi2:Vsync{}", state.events.len());
        let handle = state.context.create_event(name);
        state.events.push(handle);
        let event = state.context.get_event(handle).unwrap();
        this.vsync.lock().unwrap().push(event.clone());
        if state.vsync_timer.is_none() {
            let vsync = this.vsync.clone();
            let timer = crate::core_timing::create_event(
                "nx2:vi2 vsync".into(),
                Box::new(move |_, _| {
                    for event in vsync.lock().unwrap().iter() {
                        event.signal();
                    }
                    None
                }),
            );
            static INTERVAL: std::sync::LazyLock<std::time::Duration> =
                std::sync::LazyLock::new(|| {
                    let hz = std::env::var("EDEN_NX2_VSYNC_HZ")
                        .ok()
                        .map(|text| {
                            let text = std::ffi::CString::new(text).unwrap();
                            unsafe { libc::strtoll(text.as_ptr(), std::ptr::null_mut(), 10) }.max(1)
                        })
                        .unwrap_or(60);
                    std::time::Duration::from_nanos((1_000_000_000 / hz) as u64)
                });
            this.system
                .get()
                .core_timing()
                .schedule_looping_event(*INTERVAL, *INTERVAL, &timer, false);
            state.vsync_timer = Some(timer);
        }
        let object = event.copy_object_id(ctx).expect("VI event requires owner");
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object);
    }
}
impl Drop for INx2Vi2Session {
    fn drop(&mut self) {
        self.vsync.lock().unwrap().clear();
        let state = self.state.get_mut().unwrap();
        if let Some(timer) = &state.vsync_timer {
            self.system
                .get()
                .core_timing()
                .unschedule_event(timer, crate::core_timing::UnscheduleEventType::Wait);
        }
        for &handle in &state.events {
            state.context.close_event(handle);
        }
    }
}
impl SessionRequestHandler for INx2Vi2Session {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }
}
impl ServiceFramework for INx2Vi2Session {
    fn get_service_name(&self) -> &str {
        "nx2:vi2"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Header-only upstream helper: first command logging is process-wide.
fn nx2_first_video_command(service: &str, command: u32) -> bool {
    static SEEN: std::sync::LazyLock<Mutex<std::collections::BTreeSet<(String, u32)>>> =
        std::sync::LazyLock::new(|| Mutex::new(std::collections::BTreeSet::new()));
    SEEN.lock().unwrap().insert((service.to_owned(), command))
}
struct VideoInput {
    data: Vec<u8>,
    pts: u64,
    end: bool,
}
struct VideoDecoded {
    picture: Option<crate::host1x_core::Nx2Picture>,
    pts: u64,
    end: bool,
}
struct VideoOutput {
    image: u32,
    width: u32,
    height: u32,
    pts: u64,
    end: bool,
}
#[derive(Clone, Copy)]
struct VideoFence {
    syncpoint: u32,
    value: u32,
}
struct VideoReadyState {
    event: Mutex<Option<Arc<Event>>>,
    room: AtomicBool,
}
struct VideoState {
    codec: String,
    width: u32,
    height: u32,
    handed: u32,
    frames_out: u64,
    syncpoint: u32,
    sync_base: u32,
    inputs: std::collections::VecDeque<VideoInput>,
    decoded: std::collections::VecDeque<VideoDecoded>,
    free: std::collections::VecDeque<u32>,
    release_fences: BTreeMap<u32, VideoFence>,
    outputs: std::collections::VecDeque<VideoOutput>,
    flush: bool,
    quit: bool,
}
struct VideoShared {
    state: Mutex<VideoState>,
    cv: std::sync::Condvar,
}
struct VideoControl {
    context: ServiceContext,
    event: Option<u32>,
    ready_timer: Option<Arc<parking_lot::Mutex<crate::core_timing::EventType>>>,
    worker: Option<std::thread::JoinHandle<()>>,
    submitted: u64,
}
pub struct INx2VideoSession {
    system: SystemRef,
    name: String,
    objects: Arc<Mutex<Nx2NativeObjects>>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    control: Mutex<VideoControl>,
    shared: Arc<VideoShared>,
    ready: Arc<VideoReadyState>,
    // Retain the allocator of our syncpoint through deferred service teardown.
    // Rust can destroy this session after System's ServiceManager is gone.
    syncpoints: std::sync::OnceLock<Arc<super::nvdrv::core::syncpoint_manager::SyncpointManager>>,
}
impl INx2VideoSession {
    const NO_SYNCPOINT: u32 = u32::MAX;
    const MAX_QUEUED_INPUTS: usize = 8;
    const MAX_DECODED_AHEAD: usize = 2;
    const HOST_SYNCPOINTS: u32 = 192;

    pub fn new(system: SystemRef, objects: Arc<Mutex<Nx2NativeObjects>>, name: &str) -> Self {
        Self {
            system,
            objects,
            name: name.into(),
            handlers: BTreeMap::new(),
            handlers_tipc: build_handler_map(&[
                (
                    1,
                    Some(Self::create_decoder_service),
                    "CreateVideoDecoderService",
                ),
                (3, Some(Self::get_event), "GetEvent"),
                (6, Some(Self::succeed), "Start"),
                (8, Some(Self::flush), "Flush"),
                (101, Some(Self::configure), "Configure"),
                (109, Some(Self::submit_input), "SubmitInputBuffer"),
                (115, Some(Self::succeed), "Finalize"),
                (200, Some(Self::succeed), "Initialize"),
                (207, Some(Self::flush), "Stop"),
                (212, Some(Self::unregister_buffer), "UnregisterBuffer"),
                (213, Some(Self::get_output_info), "GetOutputInfo"),
                (311, Some(Self::register_buffer), "RegisterBuffer"),
                (314, Some(Self::release_buffer), "ReleaseBuffer"),
            ]),
            control: Mutex::new(VideoControl {
                context: ServiceContext::new(name.into()),
                event: None,
                ready_timer: None,
                worker: None,
                submitted: 0,
            }),
            shared: Arc::new(VideoShared {
                state: Mutex::new(VideoState {
                    codec: String::new(),
                    width: 0,
                    height: 0,
                    handed: 0,
                    frames_out: 0,
                    syncpoint: Self::NO_SYNCPOINT,
                    sync_base: 0,
                    inputs: Default::default(),
                    decoded: Default::default(),
                    free: Default::default(),
                    release_fences: Default::default(),
                    outputs: Default::default(),
                    flush: false,
                    quit: false,
                }),
                cv: std::sync::Condvar::new(),
            }),
            ready: Arc::new(VideoReadyState {
                event: Mutex::new(None),
                room: AtomicBool::new(true),
            }),
            syncpoints: std::sync::OnceLock::new(),
        }
    }
    fn log(&self, ctx: &HLERequestContext, what: &str) {
        let w = ctx.command_buffer();
        if nx2_first_video_command(&self.name, ctx.get_command()) {
            log::info!(
                "{} TIPC {} {} (first use): words {:08x?}",
                self.name,
                ctx.get_command(),
                what,
                &w[..12]
            );
        } else {
            log::debug!(
                "{} TIPC {} {}: [2]={:#x} [3]={:#x} [4]={:#x} [5]={:#x}",
                self.name,
                ctx.get_command(),
                what,
                w[2],
                w[3],
                w[4],
                w[5]
            );
        }
    }
    fn nvdrv_syncpoints(&self) -> Arc<super::nvdrv::core::syncpoint_manager::SyncpointManager> {
        Arc::clone(self.syncpoints.get_or_init(|| {
            let manager = self
                .system
                .get()
                .service_manager()
                .expect("video requires ServiceManager");
            let service =
                super::sm::sm::ServiceManager::get_service_blocking(&manager, self.system, "nvdrv");
            service
                .as_any()
                .downcast_ref::<super::nvdrv::nvdrv_interface::NvdrvService>()
                .expect("nvdrv must own NVDRV")
                .get_module()
                .get_container()
                .get_syncpoint_manager_handle()
        }))
    }
    fn create_decoder_service(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        this.log(ctx, "create decoder service");
        return_raw_session(
            ctx,
            Arc::new(Self::new(this.system, this.objects.clone(), "nx2:vdecsvc")),
        );
    }
    fn succeed(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        this.log(ctx, "");
        tipc_reply_words(ctx, &[]);
    }
    fn configure(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let codec = if ctx.buffer_descriptor_a().is_empty() {
            String::new()
        } else {
            let name = ctx.read_buffer_a(0);
            let len = name.iter().position(|&b| b == 0).unwrap_or(name.len());
            String::from_utf8_lossy(&name[..len]).into_owned()
        };
        this.log(ctx, "configure");
        let w = ctx.command_buffer();
        log::info!(
            "{} configure codec {:?} {}x{} rate {}",
            this.name,
            codec,
            w[5],
            w[6],
            w[7]
        );
        {
            let mut state = this.shared.state.lock().unwrap();
            state.codec = codec;
            state.width = w[5];
            state.height = w[6];
        }
        let mut control = this.control.lock().unwrap();
        if control.worker.is_none() {
            let (system, objects, shared, ready, name) = (
                this.system,
                this.objects.clone(),
                this.shared.clone(),
                this.ready.clone(),
                this.name.clone(),
            );
            control.worker = Some(
                std::thread::Builder::new()
                    .name("nx2:vdec".into())
                    .spawn(move || Self::decode_loop(system, objects, shared, ready, name))
                    .expect("start video decoder thread"),
            );
        }
        tipc_reply_words(ctx, &[]);
    }
    fn get_event(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        this.log(ctx, "event");
        let mut control = this.control.lock().unwrap();
        if control.event.is_none() {
            let handle = control.context.create_event(format!("{}:Ready", this.name));
            *this.ready.event.lock().unwrap() = control.context.get_event(handle);
            control.event = Some(handle);
            let ready = this.ready.clone();
            let timer = crate::core_timing::create_event(
                "nx2:vdec ready".into(),
                Box::new(move |_, _| {
                    let event = ready.event.lock().unwrap();
                    if ready.room.load(Ordering::Relaxed) {
                        if let Some(event) = event.as_ref() {
                            event.signal();
                        }
                    }
                    None
                }),
            );
            let interval = std::time::Duration::from_millis(4);
            this.system
                .get()
                .core_timing()
                .schedule_looping_event(interval, interval, &timer, false);
            control.ready_timer = Some(timer);
        }
        let event = control.context.get_event(control.event.unwrap()).unwrap();
        event.signal();
        let object = event
            .copy_object_id(ctx)
            .expect("video ready event requires owner");
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object);
    }
    fn hand_out(&self, image: u32, release_fence: Option<VideoFence>) -> [u32; 4] {
        let mut state = self.shared.state.lock().unwrap();
        if state.syncpoint == Self::NO_SYNCPOINT {
            state.syncpoint = self.nvdrv_syncpoints().allocate_syncpoint(false);
            state.sync_base = self
                .system
                .get()
                .host1x_core()
                .expect("video requires Host1x")
                .get_guest_syncpoint_value(state.syncpoint);
            log::info!(
                "{} counts decoded frames on syncpoint {} from {}",
                self.name,
                state.syncpoint,
                state.sync_base
            );
        }
        state.free.push_back(image);
        if let Some(fence) = release_fence {
            state.release_fences.insert(image, fence);
        }
        state.handed = state.handed.wrapping_add(1);
        let value = state.sync_base.wrapping_add(state.handed);
        let handle = [!state.syncpoint, 0, value, 0];
        drop(state);
        self.shared.cv.notify_one();
        handle
    }
    fn register_buffer(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        this.log(ctx, "register output");
        let handle = this.hand_out(ctx.command_buffer()[2], None);
        tipc_reply_words(ctx, &handle);
    }
    fn release_buffer(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        this.log(ctx, "release output");
        let w = ctx.command_buffer();
        let handle = this.hand_out(
            w[2],
            Some(VideoFence {
                syncpoint: !w[4],
                value: w[6],
            }),
        );
        tipc_reply_words(ctx, &handle);
    }
    fn unregister_buffer(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        this.log(ctx, "unregister output");
        let image = ctx.command_buffer()[2];
        {
            let mut state = this.shared.state.lock().unwrap();
            state.free.retain(|&id| id != image);
            state.release_fences.remove(&image);
        }
        tipc_reply_words(ctx, &[]);
    }
    fn submit_input(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let w = ctx.command_buffer();
        let pts = w[5] as u64 | ((w[6] as u64) << 32);
        let end = w[7] & 0xff != 0;
        this.log(ctx, "submit");
        let data = if ctx.buffer_descriptor_a().is_empty() {
            Vec::new()
        } else {
            ctx.read_buffer_a(0)
        };
        {
            let mut state = this.shared.state.lock().unwrap();
            state.inputs.push_back(VideoInput { data, pts, end });
            this.ready.room.store(
                state.inputs.len() < Self::MAX_QUEUED_INPUTS,
                Ordering::Relaxed,
            );
        }
        this.shared.cv.notify_one();
        let mut control = this.control.lock().unwrap();
        control.submitted = control.submitted.wrapping_add(1);
        tipc_reply_words(
            ctx,
            &[control.submitted as u32, (control.submitted >> 32) as u32],
        );
    }
    fn flush(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        this.log(
            ctx,
            if ctx.get_command() == 8 {
                "flush"
            } else {
                "stop"
            },
        );
        {
            let mut state = this.shared.state.lock().unwrap();
            state.inputs.clear();
            state.flush = true;
            this.ready.room.store(true, Ordering::Relaxed);
        }
        this.shared.cv.notify_one();
        tipc_reply_words(ctx, &[]);
    }
    fn get_output_info(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        this.log(ctx, "output info");
        let output = this.shared.state.lock().unwrap().outputs.pop_front();
        let Some(output) = output else {
            ResponseBuilder::new(ctx, 2, 0, 0).push_u32(0x1c286f);
            return;
        };
        let pts_ns = output.pts.wrapping_mul(1000);
        tipc_reply_words(
            ctx,
            &[
                output.image,
                output.width,
                output.height,
                output.width,
                output.height,
                0,
                pts_ns as u32,
                (pts_ns >> 32) as u32,
                output.end as u32,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
        );
    }
    fn decode_loop(
        system: SystemRef,
        objects: Arc<Mutex<Nx2NativeObjects>>,
        shared: Arc<VideoShared>,
        ready: Arc<VideoReadyState>,
        name: String,
    ) {
        let mut decoder: Option<Box<dyn crate::host1x_core::Nx2VideoDecoderHandle>> = None;
        let mut pictures = Vec::new();
        let mut bytes = Vec::new();
        let mut state = shared.state.lock().unwrap();
        loop {
            while !(state.quit
                || state.flush
                || (!state.decoded.is_empty() && !state.free.is_empty())
                || (!state.inputs.is_empty() && state.decoded.len() < Self::MAX_DECODED_AHEAD))
            {
                state = shared.cv.wait(state).unwrap();
            }
            if state.quit {
                return;
            }
            if state.flush {
                state.flush = false;
                state.decoded.clear();
                if let Some(decoder) = decoder.as_mut() {
                    drop(state);
                    decoder.reset();
                    state = shared.state.lock().unwrap();
                }
                continue;
            }
            if !state.decoded.is_empty() && !state.free.is_empty() {
                let frame = state.decoded.pop_front().unwrap();
                let image = state.free.pop_front().unwrap();
                let fence = state.release_fences.remove(&image);
                drop(state);
                if let Some(fence) = fence {
                    Self::wait_for_release(system, fence);
                }
                let (width, height) = Self::write_image(
                    system,
                    &objects,
                    &shared,
                    decoder.as_deref_mut(),
                    image,
                    &frame,
                    &mut bytes,
                );
                state = shared.state.lock().unwrap();
                state.outputs.push_back(VideoOutput {
                    image,
                    width,
                    height,
                    pts: frame.pts,
                    end: frame.end,
                });
                system
                    .get()
                    .host1x_core()
                    .expect("video requires Host1x")
                    .increment_guest(state.syncpoint);
                state.frames_out = state.frames_out.wrapping_add(1);
                if state.frames_out == 1 || state.frames_out % 600 == 0 || frame.end {
                    log::info!(
                        "{} frame {} ({}x{}, pts {} us{}) in image {}",
                        name,
                        state.frames_out,
                        width,
                        height,
                        frame.pts,
                        if frame.end { ", end of stream" } else { "" },
                        image
                    );
                }
                continue;
            }
            let input = state.inputs.pop_front().unwrap();
            ready.room.store(
                state.inputs.len() < Self::MAX_QUEUED_INPUTS,
                Ordering::Relaxed,
            );
            let codec = state.codec.clone();
            drop(state);
            if decoder.as_ref().is_none_or(|d| d.name() != codec) {
                decoder = Some(
                    system
                        .get()
                        .host1x_core()
                        .expect("video requires Host1x")
                        .create_nx2_video_decoder(&codec),
                );
            }
            pictures.clear();
            let current = decoder.as_mut().unwrap();
            current.decode(&input.data, input.pts, &mut pictures);
            if input.end {
                current.drain(&mut pictures);
            }
            state = shared.state.lock().unwrap();
            if state.flush {
                continue;
            }
            for picture in pictures.drain(..) {
                let pts = picture.pts;
                state.decoded.push_back(VideoDecoded {
                    picture: Some(picture),
                    pts,
                    end: false,
                });
            }
            if input.end {
                if let Some(last) = state.decoded.back_mut() {
                    last.end = true;
                } else {
                    state.decoded.push_back(VideoDecoded {
                        picture: None,
                        pts: input.pts,
                        end: true,
                    });
                }
            }
        }
    }
    fn wait_for_release(system: SystemRef, fence: VideoFence) {
        if fence.syncpoint >= Self::HOST_SYNCPOINTS {
            return;
        }
        let syncpoints = system.get().host1x_core().expect("video requires Host1x");
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
        while !syncpoints.is_ready_guest(fence.syncpoint, fence.value)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    fn write_image(
        system: SystemRef,
        objects: &Mutex<Nx2NativeObjects>,
        shared: &VideoShared,
        decoder: Option<&mut (dyn crate::host1x_core::Nx2VideoDecoderHandle + 'static)>,
        image_id: u32,
        frame: &VideoDecoded,
        bytes: &mut Vec<u8>,
    ) -> (u32, u32) {
        let (configured_width, configured_height, frames_out) = {
            let state = shared.state.lock().unwrap();
            (state.width, state.height, state.frames_out)
        };
        let (address, layout) = {
            let objects = objects.lock().unwrap();
            let Some(image) = objects.images.get(&image_id) else {
                return (configured_width, configured_height);
            };
            let layout = nx2_calculate_decoder_image_layout(&image.preference);
            let address = objects
                .memories
                .get(&image.memory)
                .filter(|m| m.address != 0)
                .map_or(0, |memory| memory.address.wrapping_add(image.offset));
            (address, layout)
        };
        let fallback = (
            configured_width.min(layout.width),
            configured_height.min(layout.height),
        );
        let (Some(picture), Some(decoder)) = (frame.picture.as_ref(), decoder) else {
            return fallback;
        };
        if address == 0 || system.get().application_process().is_none() {
            return fallback;
        }
        let width = picture.width.min(layout.width);
        let height = picture.height.min(layout.height);
        bytes.resize(layout.size as usize, 0);
        decoder.render(
            picture,
            layout.format,
            width,
            height,
            layout.pitch,
            layout.chroma_offset,
            bytes,
        );
        Self::dump_frame(bytes, &layout, width, height, frames_out);
        let memory = system
            .get()
            .memory_shared()
            .expect("video requires ApplicationMemory");
        let memory = memory.access().unwrap();
        memory.write_block(address, &bytes[..(layout.pitch * height as u64) as usize]);
        if layout.format == crate::host1x_core::Nx2PictureFormat::Nv12 {
            let offset = layout.chroma_offset as usize;
            memory.write_block(
                address.wrapping_add(layout.chroma_offset),
                &bytes[offset..offset + (layout.pitch * ((height as u64 + 1) / 2)) as usize],
            );
        }
        (width, height)
    }
    fn dump_frame(
        bytes: &[u8],
        layout: &Nx2DecoderImageLayout,
        width: u32,
        height: u32,
        frames_out: u64,
    ) {
        static DIRECTORY: std::sync::LazyLock<Option<String>> =
            std::sync::LazyLock::new(|| std::env::var("EDEN_NX2_VDEC_DUMP").ok());
        let Some(directory) = DIRECTORY.as_ref() else {
            return;
        };
        if frames_out % 120 != 0 {
            return;
        }
        let bpp = if layout.format == crate::host1x_core::Nx2PictureFormat::Nv12 {
            1
        } else {
            4
        };
        let (w, h) = (width / 4, height / 4);
        let path = format!("{directory}/nx2-vdec-{:05}.pgm", frames_out.wrapping_add(1));
        if let Ok(mut file) = std::fs::File::create(path) {
            use std::io::Write;
            let _ = writeln!(file, "P5 {w} {h} 255");
            for y in 0..h {
                for x in 0..w {
                    let _ = file.write_all(&[
                        bytes[(y as u64 * 4 * layout.pitch + x as u64 * 4 * bpp) as usize]
                    ]);
                }
            }
        }
    }
}
impl Drop for INx2VideoSession {
    fn drop(&mut self) {
        let control = self.control.get_mut().unwrap();
        if let Some(worker) = control.worker.take() {
            self.shared.state.lock().unwrap().quit = true;
            self.shared.cv.notify_all();
            worker.join().expect("join video decoder thread");
        }
        *self.ready.event.lock().unwrap() = None;
        if let Some(timer) = &control.ready_timer {
            self.system
                .get()
                .core_timing()
                .unschedule_event(timer, crate::core_timing::UnscheduleEventType::Wait);
        }
        if let Some(event) = control.event {
            control.context.close_event(event);
        }
        let syncpoint = self.shared.state.lock().unwrap().syncpoint;
        if syncpoint != Self::NO_SYNCPOINT {
            self.nvdrv_syncpoints().free_syncpoint(syncpoint);
        }
    }
}
impl SessionRequestHandler for INx2VideoSession {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }
}
impl ServiceFramework for INx2VideoSession {
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

struct GraphicsSystemState {
    context: ServiceContext,
    syncpt_events: [Option<u32>; 2],
    syncpt_memory: Option<Arc<crate::memory::memory::SharedMemory>>,
    syncpt_rw: u64,
    syncpt_ro: u64,
    syncpt_count: u32,
    syncpt_poll: Option<Arc<parking_lot::Mutex<crate::core_timing::EventType>>>,
}
pub struct INx2GraphicsSystemSession {
    system: SystemRef,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    objects: Arc<Mutex<Nx2NativeObjects>>,
    state: Arc<Mutex<GraphicsSystemState>>,
    chat: super::chat::gsv::ChatGraphicsService,
    camera_contexts: Arc<Mutex<u32>>,
}
impl INx2GraphicsSystemSession {
    const SYNCPT_VALUES_OFFSET: u64 = 0x40;
    const SYNCPT_RO_SUMMARY_OFFSET: u64 = 0x1040;
    const SYNCPT_RO_BITS_OFFSET: u64 = 0x1100;
    const SYNCPT_RW_SUMMARY_OFFSET: u64 = 0;
    const SYNCPT_RW_BITS_OFFSET: u64 = 0x100;
    const HOST_SYNCPOINTS: u32 = 192;
    const SYNCPT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_micros(500);
    pub fn new(system: SystemRef) -> Self {
        Self {
            system,
            chat: super::chat::gsv::ChatGraphicsService::default(),
            camera_contexts: Arc::new(Mutex::new(0)),
            handlers: BTreeMap::new(),
            objects: Arc::new(Mutex::new(Nx2NativeObjects::default())),
            state: Arc::new(Mutex::new(GraphicsSystemState {
                context: ServiceContext::new("nx2:gsv".into()),
                syncpt_events: [None, None],
                syncpt_memory: None,
                syncpt_rw: 0,
                syncpt_ro: 0,
                syncpt_count: 0,
                syncpt_poll: None,
            })),
            handlers_tipc: build_handler_map(&[
                (1201, Some(Self::initialize_chat), "InitializeChat"),
                (1202, Some(Self::open_chat_session), "OpenChatSession"),
                (1203, Some(Self::finalize_chat), "FinalizeChat"),
                (1291, Some(Self::calculate_chat_image_pool), "CalculateChatImagePool"),
                (1211, Some(Self::create_chat_image_pool), "CreateChatImagePool"),
                (1212, Some(Self::destroy_chat_image_pool), "DestroyChatImagePool"),
                (4, Some(Self::stub), "Boot (Switch 2, guessed)"),
                (5, Some(Self::stub), "Unknown5"),
                (101, Some(Self::stub), "Unknown101"),
                (
                    202,
                    Some(Self::open_syncpt_session),
                    "OpenSyncptSession (guessed)",
                ),
                (
                    204,
                    Some(Self::attach_syncpt_memory),
                    "AttachSyncptMemory (guessed)",
                ),
                (305, Some(Self::open_vi_session), "OpenViSession (guessed)"),
                (
                    497,
                    Some(Self::open_gpu_session),
                    "OpenGpuSession (guessed)",
                ),
                (111, Some(Self::create_native_memory), "CreateNativeMemory"),
                (
                    112,
                    Some(Self::destroy_native_memory),
                    "DestroyNativeMemory",
                ),
                (114, Some(Self::bind_native_memory), "BindNativeMemory"),
                (
                    314,
                    Some(Self::configure_window_image_pool),
                    "DestroyNativeWindowAliasImagePool",
                ),
                (
                    315,
                    Some(Self::configure_window_image_pool),
                    "CreateNativeWindowAliasImagePool",
                ),
                (
                    423,
                    Some(Self::bind_native_gpu_memory),
                    "BindNativeGpuMemory",
                ),
                (601, Some(Self::create_camera_context), "CreateCameraContext"),
                (602, Some(Self::open_camera_session), "OpenCameraSession"),
                (603, Some(Self::destroy_camera_context), "DestroyCameraContext"),
                (426, Some(Self::query_native_image), "MapNativeImage"),
                (427, Some(Self::unmap_native_image), "UnmapNativeImage"),
                (428, Some(Self::query_native_image), "QueryNativeImage"),
                (429, Some(Self::create_native_image), "CreateNativeImage"),
                (430, Some(Self::destroy_native_image), "DestroyNativeImage"),
                (
                    435,
                    Some(Self::calculate_native_image_size),
                    "CalculateNativeImageSize",
                ),
                (
                    1402,
                    Some(Self::open_nvsrv_session),
                    "OpenNvsrvSession (guessed)",
                ),
                (
                    1510,
                    Some(Self::calculate_requirements),
                    "CalculateRequirements",
                ),
                (
                    991,
                    Some(Self::calculate_decoder_image_pool_buffer),
                    "CalculateDecoderImagePoolBuffer",
                ),
                (901, Some(Self::stub), "VideoInitialize (guessed)"),
                (
                    902,
                    Some(Self::open_video_session),
                    "OpenVideoDecoderSession (guessed)",
                ),
                (
                    906,
                    Some(Self::open_video_session),
                    "OpenVideoDecoderSessionWithVersion",
                ),
                (1001, Some(Self::stub), "VideoInitialize2 (guessed)"),
                (
                    1002,
                    Some(Self::open_video_session),
                    "OpenVideoDecoderSession2 (guessed)",
                ),
                (911, Some(Self::create_decoder_image), "CreateDecoderImage"),
                (
                    961,
                    Some(Self::get_cpu_image_descriptor),
                    "GetCpuUserImageDescriptor",
                ),
            ]),
        }
    }
    fn tipc_data_offset(cmd: &[u32]) -> usize {
        if cmd[1] & 0x80000000 == 0 {
            return 2;
        }
        let desc = cmd[2];
        (3 + if desc & 1 != 0 { 2 } else { 0 } + ((desc >> 1) & 0xf) + ((desc >> 5) & 0xf)) as usize
    }
    fn pair(w: &[u32], i: usize) -> u64 {
        w[i] as u64 | ((w[i + 1] as u64) << 32)
    }
    fn stub(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let w = ctx.command_buffer();
        log::warn!(
            "(STUBBED) nx2:gsv TIPC {} [2]={:#x} [3]={:#x} [4]={:#x} [5]={:#x}",
            ctx.get_command(),
            w[2],
            w[3],
            w[4],
            w[5]
        );
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }
    // SDK-derived chat dependency, authorized separately from upstream port.
    fn calculate_chat_image_pool(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        this.chat.calculate_image_pool_buffer(ctx);
    }
    fn create_chat_image_pool(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        this.chat.create_image_pool(ctx, &this.objects);
    }
    fn destroy_chat_image_pool(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        this.chat.destroy_image_pool(ctx, &this.objects);
    }
    fn initialize_chat(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        this.chat.initialize(ctx);
    }
    fn open_chat_session(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        this.chat.open_session(ctx);
    }
    fn finalize_chat(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        this.chat.finalize(ctx);
    }
    fn open_video_session(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let name = if ctx.get_command() == 1002 {
            "nx2:vdec2"
        } else {
            "nx2:vdec"
        };
        log::info!("nx2:gsv TIPC {} returning {}", ctx.get_command(), name);
        return_raw_session(
            ctx,
            Arc::new(INx2VideoSession::new(
                this.system,
                this.objects.clone(),
                name,
            )),
        );
    }
    fn open_syncpt_session(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        return_raw_session(ctx, Arc::new(INx2SyncptSession::new(this.system)));
    }
    fn open_vi_session(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let w = ctx.command_buffer();
        let data = Self::tipc_data_offset(w);
        log::warn!(
            "(STUBBED) nx2:gsv TIPC 305 arg={:#x}, returning nx2:vi2",
            Self::pair(w, data)
        );
        return_raw_session(
            ctx,
            Arc::new(INx2Vi2Session::new(this.system, this.objects.clone())),
        );
    }
    fn open_gpu_session(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let w = ctx.command_buffer();
        let data = Self::tipc_data_offset(w);
        log::warn!(
            "(STUBBED) nx2:gsv TIPC 497 arg={:#x}, returning nx2:gpu",
            Self::pair(w, data)
        );
        return_raw_session(ctx, INx2GpuSession::new(this.system));
    }
    fn open_nvsrv_session(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let manager = this
            .system
            .get()
            .service_manager()
            .expect("GSV requires ServiceManager");
        let service =
            super::sm::sm::ServiceManager::get_service_blocking(&manager, this.system, "nvdrv");
        let module = service
            .as_any()
            .downcast_ref::<super::nvdrv::nvdrv_interface::NvdrvService>()
            .expect("nvdrv service type")
            .get_module();
        let nvsrv = Arc::new(super::nvdrv::nvdrv_interface::NvdrvService::new(
            module,
            "nx2:nvsrv",
        ));
        nvsrv.initialize_for_process(&ctx.owner_process_arc().expect("GSV requires owner process"));
        return_raw_session(ctx, nvsrv);
    }
    fn attach_syncpt_memory(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let w = ctx.command_buffer();
        let data = Self::tipc_data_offset(w);
        let max_syncpoints = w[data + 1].min((0x1000 - 0x40) / 8);
        log::warn!(
            "(STUBBED) nx2:gsv TIPC 204, version={} max_syncpoints={:#x}",
            w[data],
            max_syncpoints
        );
        let get_transfer = |index| {
            ctx.owner_process_arc().and_then(|owner| {
                let owner = owner.lock().unwrap();
                let id = owner.handle_table.get_object(ctx.get_copy_handle(index))?;
                owner.get_transfer_memory_by_object_id(id)
            })
        };
        let (rw, ro) = (get_transfer(0), get_transfer(1));
        let mut state = this.state.lock().unwrap();
        let valid = ro
            .as_ref()
            .is_some_and(|ro| ro.lock().unwrap().get_size() >= 0x2000)
            && rw.is_some();
        if valid {
            let rw = rw.unwrap();
            let ro = ro.unwrap();
            state.syncpt_rw = rw.lock().unwrap().get_source_address();
            let ro = ro.lock().unwrap();
            state.syncpt_ro = ro.get_source_address();
            state.syncpt_memory = ro
                .get_owner()
                .expect("transfer memory owner")
                .lock()
                .unwrap()
                .get_memory();
            drop(ro);
            state.syncpt_count = max_syncpoints;
            {
                let memory = state
                    .syncpt_memory
                    .as_ref()
                    .expect("syncpt memory owner")
                    .access()
                    .unwrap();
                for (offset, value) in [
                    1,
                    0,
                    max_syncpoints,
                    Self::SYNCPT_VALUES_OFFSET as u32,
                    Self::SYNCPT_RW_SUMMARY_OFFSET as u32,
                    Self::SYNCPT_RO_SUMMARY_OFFSET as u32,
                    Self::SYNCPT_RW_BITS_OFFSET as u32,
                    Self::SYNCPT_RO_BITS_OFFSET as u32,
                ]
                .into_iter()
                .enumerate()
                {
                    memory.write_32(state.syncpt_ro.wrapping_add(offset as u64 * 4), value);
                }
            }
            Self::update_syncpt_values(this.system, &state);
            if state.syncpt_poll.is_none() {
                let shared = this.state.clone();
                let system = this.system;
                let timer = crate::core_timing::create_event(
                    "nx2:gsv:SyncptPoll".into(),
                    Box::new(move |_, _| {
                        Self::poll_syncpts(system, &shared.lock().unwrap());
                        None
                    }),
                );
                this.system.get().core_timing().schedule_looping_event(
                    Self::SYNCPT_POLL_INTERVAL,
                    Self::SYNCPT_POLL_INTERVAL,
                    &timer,
                    false,
                );
                state.syncpt_poll = Some(timer);
            }
        } else {
            log::error!("nx2:gsv TIPC 204: missing syncpt transfer memory");
        }
        if state.syncpt_events[0].is_none() {
            state.syncpt_events[0] =
                Some(state.context.create_event("nx2:gsv:SyncptRequest".into()));
            state.syncpt_events[1] =
                Some(state.context.create_event("nx2:gsv:SyncptNotify".into()));
        }
        let request = state
            .context
            .get_event(state.syncpt_events[0].unwrap())
            .unwrap()
            .copy_writable_object_id(ctx)
            .expect("GSV writable event owner");
        let notify = state
            .context
            .get_event(state.syncpt_events[1].unwrap())
            .unwrap()
            .copy_object_id(ctx)
            .expect("GSV readable event owner");
        let mut rb = ResponseBuilder::new(ctx, 2, 2, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(request);
        rb.push_copy_object_id(notify);
    }
    fn update_syncpt_values(system: SystemRef, state: &GraphicsSystemState) -> bool {
        let manager = system.get().host1x_core().expect("GSV requires Host1x");
        let memory = state
            .syncpt_memory
            .as_ref()
            .expect("syncpt memory")
            .access()
            .unwrap();
        let mut changed = false;
        for id in 0..state.syncpt_count.min(Self::HOST_SYNCPOINTS) {
            let value = manager.get_guest_syncpoint_value(id) as u64;
            let address = state
                .syncpt_ro
                .wrapping_add(Self::SYNCPT_VALUES_OFFSET + id as u64 * 8);
            if memory.read_64(address) != value {
                memory.write_64(address, value);
                changed = true;
            }
        }
        changed
    }
    fn poll_syncpts(system: SystemRef, state: &GraphicsSystemState) {
        let Some(shared_memory) = state.syncpt_memory.as_ref() else {
            return;
        };
        Self::update_syncpt_values(system, state);
        let memory = shared_memory.access().unwrap();
        let mut acked = false;
        for word in 0..(state.syncpt_count + 63) / 64 {
            let bits = memory.read_64(
                state
                    .syncpt_rw
                    .wrapping_add(Self::SYNCPT_RW_BITS_OFFSET + word as u64 * 8),
            );
            let address = state
                .syncpt_ro
                .wrapping_add(Self::SYNCPT_RO_BITS_OFFSET + word as u64 * 8);
            if memory.read_64(address) != bits {
                memory.write_64(address, bits);
                acked = true;
            }
        }
        memory.write_64(
            state.syncpt_ro.wrapping_add(Self::SYNCPT_RO_SUMMARY_OFFSET),
            memory.read_64(state.syncpt_rw.wrapping_add(Self::SYNCPT_RW_SUMMARY_OFFSET)),
        );
        if acked {
            state
                .context
                .get_event(state.syncpt_events[1].expect("syncpt notify event"))
                .unwrap()
                .signal();
        }
    }
    fn calculate_requirements(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let preference = ctx.command_buffer()[2..18].try_into().unwrap();
        let layout = nx2_calculate_image_layout(preference);
        tipc_reply_words(
            ctx,
            &[
                layout.size as u32,
                (layout.size >> 32) as u32,
                layout.alignment as u32,
                (layout.alignment >> 32) as u32,
            ],
        );
    }
    fn calculate_decoder_image_pool_buffer(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let w = ctx.command_buffer();
        let layout = nx2_calculate_decoder_image_layout(w[2..18].try_into().unwrap());
        let size = layout.size.wrapping_mul(w[18].max(1) as u64);
        let alignment = 64 * 128u64;
        tipc_reply_words(
            ctx,
            &[
                size as u32,
                (size >> 32) as u32,
                alignment as u32,
                (alignment >> 32) as u32,
            ],
        );
    }
    // SDK +0x2be980: 601 then 602(u64 1); +0x2bebb0 destroys via 603.
    // Reference-counted local contexts are an explicit offline policy, not a
    // claim about original hardware allocation limits or server error codes.
    fn create_camera_context(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let mut count = this.camera_contexts.lock().unwrap();
        let Some(next) = count.checked_add(1) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(super::ipc_helpers::RESULT_NOT_SUPPORTED);
            return;
        };
        *count = next;
        tipc_reply_words(ctx, &[]);
    }
    fn open_camera_session(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        if *this.camera_contexts.lock().unwrap() == 0 || Self::pair(ctx.command_buffer(), 2) != 1 {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(super::ipc_helpers::RESULT_NOT_SUPPORTED);
            return;
        }
        let object = create_raw_session(ctx, Arc::new(INx2CameraSession {
            handlers: build_handler_map(&[
                (3, Some(INx2CameraSession::open_camera), "OpenCamera"),
                (21, Some(INx2CameraSession::get_event), "GetEvent"),
                (29, Some(INx2CameraSession::camera_command_29), "CameraCommand29"),
            ]),
            handlers_tipc: BTreeMap::new(),
            _context: this.camera_contexts.clone(),
            events: Mutex::new(CameraEventState {
                context: ServiceContext::new("nx2:camera".into()),
                handles: BTreeMap::new(),
            }),
        }));
        // SDK +0x2beb20 reads handle word3, then result word4; MOVE header0x20.
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_move_object_id(object);
        log::info!("nx2:gsv 602 created camera session");
    }
    fn destroy_camera_context(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let mut count = this.camera_contexts.lock().unwrap();
        let Some(next) = count.checked_sub(1) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(super::ipc_helpers::RESULT_NOT_SUPPORTED);
            return;
        };
        *count = next;
        tipc_reply_words(ctx, &[]);
    }

    fn calculate_native_image_size(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let w = ctx.command_buffer();
        let size = nx2_calculate_image_layout(w[2..18].try_into().unwrap())
            .size
            .wrapping_mul(w[18].max(1) as u64);
        tipc_reply_words(ctx, &[size as u32, (size >> 32) as u32]);
    }
    fn create_native_memory(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let size = Self::pair(ctx.command_buffer(), 3);
        let mut objects = this.objects.lock().unwrap();
        let id = objects.next_memory;
        objects.next_memory = id.wrapping_add(1);
        objects
            .memories
            .insert(id, Nx2NativeMemory { size, address: 0 });
        tipc_reply_words(ctx, &[id]);
    }
    fn destroy_native_memory(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        this.objects
            .lock()
            .unwrap()
            .memories
            .remove(&ctx.command_buffer()[2]);
        tipc_reply_words(ctx, &[]);
    }
    fn bind_native_memory(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let w = ctx.command_buffer();
        this.bind_memory(w[2], Self::pair(w, 3), Self::pair(w, 5), "114");
        tipc_reply_words(ctx, &[]);
    }
    fn bind_native_gpu_memory(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let w = ctx.command_buffer();
        this.bind_memory(w[2], Self::pair(w, 3), Self::pair(w, 5), "423");
        tipc_reply_words(ctx, &[]);
    }
    fn bind_memory(&self, id: u32, address: u64, size: u64, command: &str) {
        let mut objects = self.objects.lock().unwrap();
        let memory = objects.memories.entry(id).or_default();
        memory.address = address;
        if memory.size == 0 {
            memory.size = size;
        }
        log::info!(
            "nx2:gsv {} bind native memory {} at {:#x}+{:#x}",
            command,
            id,
            address,
            size
        );
    }
    // SDK-derived extension: Eglantine ae9d3eb8 nx2_gsv.h QueryNativeImage.
    // SDK +0x461cd0/+0x461ef0 consume 2*u64, descriptor[128], flags:u32.
    // The RGBA descriptor uses the fork's inferred block-height/pitch fields;
    // these are not claimed to be a verified hardware descriptor definition.
    fn query_native_image(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let map = ctx.get_command() == 426;
        let id = ctx.command_buffer()[2];
        let mut objects = this.objects.lock().unwrap();
        let info = objects.images.get(&id).and_then(|image| {
            let memory = objects.memories.get(&image.memory)?;
            let end = image.offset.checked_add(image.size)?;
            let address = memory.address.checked_add(image.offset)?;
            // Decoder descriptors have a separate ABI; never return an RGBA
            // descriptor for them or for absent/undersized backing memory.
            if image.decoder || image.preference[1] != 1 || image.size == 0
                || image.size > u32::MAX as u64 || memory.address == 0 || end > memory.size
                || address.checked_add(image.size).is_none()
            { return None; }
            Some((image.preference, image.offset, image.size, address))
        });
        let Some((preference, offset, size, address)) = info else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(super::ipc_helpers::RESULT_NOT_SUPPORTED);
            return;
        };
        let mut register_mapping = false;
        if map {
            let image = objects.images.get_mut(&id).unwrap();
            let Some(count) = image.gpu_maps.checked_add(1) else {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(super::ipc_helpers::RESULT_NOT_SUPPORTED);
                return;
            };
            register_mapping = image.gpu_maps == 0
                && Nx2GpuMemoryRegistry::get().device_address_of(address).is_none();
            image.gpu_maps = count;
            image.mapped_here |= register_mapping;
        }
        drop(objects);
        if register_mapping { Nx2GpuMemoryRegistry::get().register(address, size); }
        let layout = nx2_calculate_image_layout(&preference);
        let gpu_address = if map { address } else { 0 };
        let mut words = [0u32; 37];
        words[0] = gpu_address as u32;
        words[1] = (gpu_address >> 32) as u32;
        words[2] = offset as u32;
        words[3] = (offset >> 32) as u32;
        let desc = &mut words[4..36];
        desc[0] = 1;
        desc[1] = preference[1];
        desc[2] = preference[2];
        desc[4] = layout.block_height_log2;
        desc[5] = (layout.pitch / 64) as u32;
        desc[8] = 1;
        desc[9] = 1;
        desc[11] = size as u32;
        log::info!("nx2:gsv {} image={} size={:#x} offset={:#x} gpu={:#x}",
            ctx.get_command(), id, size, offset, gpu_address);
        tipc_reply_words(ctx, &words);
    }

    // Eglantine UnmapNativeImage/TakeDecoderMapping, applied to native RGBA
    // allocations too: only the final mapping reference releases our mapping.
    fn unmap_native_image(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let mut objects = this.objects.lock().unwrap();
        let id = ctx.command_buffer()[2];
        let Nx2NativeObjects { images, memories, .. } = &mut *objects;
        let mut mapping = None;
        if let Some(image) = images.get_mut(&id) {
            if image.gpu_maps != 0 {
                image.gpu_maps -= 1;
                if image.gpu_maps == 0 { mapping = take_native_image_mapping(image, memories); }
            }
        }
        drop(objects);
        drop_native_image_mapping(mapping);
        tipc_reply_words(ctx, &[]);
    }

    fn create_native_image(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let w = ctx.command_buffer();
        let mut objects = this.objects.lock().unwrap();
        let id = objects.next_image;
        objects.next_image = id.wrapping_add(1);
        let image = objects.images.entry(id).or_default();
        image.memory = w[19];
        image.preference.copy_from_slice(&w[2..18]);
        image.size = nx2_calculate_image_layout(&image.preference).size;
        tipc_reply_words(ctx, &[id]);
    }
    fn create_decoder_image(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let w = ctx.command_buffer();
        let count = w[18].max(1);
        let layout = nx2_calculate_decoder_image_layout(w[2..18].try_into().unwrap());
        let mut objects = this.objects.lock().unwrap();
        let first = objects.next_image;
        objects.next_image = first.wrapping_add(count);
        for i in 0..count {
            let image = objects.images.entry(first.wrapping_add(i)).or_default();
            image.memory = w[19];
            image.preference.copy_from_slice(&w[2..18]);
            image.size = layout.size;
            image.offset = layout.size.wrapping_mul(i as u64);
            image.decoder = true;
        }
        tipc_reply_words(ctx, &[first]);
    }
    fn get_cpu_image_descriptor(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(_this as *const dyn ServiceFramework as *const Self) };
        let id = ctx.command_buffer()[2];
        let mut words = [0u32; 32];
        {
            let objects = this.objects.lock().unwrap();
            if let Some(image) = objects.images.get(&id).filter(|image| image.decoder) {
                let layout = nx2_calculate_decoder_image_layout(&image.preference);
                let offset = image.offset;
                use crate::host1x_core::Nx2PictureFormat;
                words[0] = match layout.format {
                    Nx2PictureFormat::Nv12 => 10,
                    Nx2PictureFormat::Bgra8 => 9,
                    _ => 8,
                };
                words[2] = offset as u32;
                words[4] = layout.pitch as u32;
                if layout.format == Nx2PictureFormat::Nv12 {
                    words[6] = offset.wrapping_add(layout.chroma_offset) as u32;
                    words[8] = layout.pitch as u32;
                }
            }
        }
        if nx2_first_video_command("nx2:gsv", 961) || words[0] == 0 {
            log::info!(
                "nx2:gsv 961 image {} -> kind {:#x} offset {:#x} pitch {:#x}",
                id,
                words[0],
                words[2],
                words[4]
            );
        }
        tipc_reply_words(ctx, &words);
    }
    fn destroy_native_image(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let mut objects = this.objects.lock().unwrap();
        let mapping = objects.images.remove(&ctx.command_buffer()[2])
            .and_then(|mut image| take_native_image_mapping(&mut image, &objects.memories));
        drop(objects);
        drop_native_image_mapping(mapping);
        tipc_reply_words(ctx, &[]);
    }
    fn configure_window_image_pool(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        if let Some(image) = this
            .objects
            .lock()
            .unwrap()
            .images
            .get_mut(&ctx.command_buffer()[2])
        {
            image.window_pool = ctx.get_command() == 315;
        }
        tipc_reply_words(ctx, &[]);
    }
}
impl Drop for INx2GraphicsSystemSession {
    fn drop(&mut self) {
        // Do not hold the state lock while waiting for a running timer callback.
        let timer = self.state.lock().unwrap().syncpt_poll.take();
        if let Some(timer) = timer {
            self.system
                .get()
                .core_timing()
                .unschedule_event(&timer, crate::core_timing::UnscheduleEventType::Wait);
        }
        let mut state = self.state.lock().unwrap();
        let events = state.syncpt_events;
        for handle in events.into_iter().flatten() {
            state.context.close_event(handle);
        }
    }
}
impl SessionRequestHandler for INx2GraphicsSystemSession {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }
}
impl ServiceFramework for INx2GraphicsSystemSession {
    fn get_service_name(&self) -> &str {
        "nx2:gsv"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Upstream Nx2NativeObjects::Memory (host bookkeeping, not a wire payload).
#[derive(Default)]
pub struct Nx2NativeMemory {
    pub size: u64,
    pub address: u64,
}

/// Upstream Nx2NativeObjects::Image.
#[derive(Default)]
pub struct Nx2NativeImage {
    pub memory: u32,
    pub preference: [u32; 16],
    pub size: u64,
    pub offset: u64,
    pub window_pool: bool,
    pub decoder: bool,
    pub gpu_maps: u32,
    pub mapped_here: bool,
}

// Mechanical extraction of the fork's TakeDecoderMapping/DropDecoderMapping:
// shared by root image destruction and the separately owned chat pool teardown.
pub(super) fn take_native_image_mapping(
    image: &mut Nx2NativeImage,
    memories: &BTreeMap<u32, Nx2NativeMemory>,
) -> Option<(u64, u64)> {
    image.gpu_maps = 0;
    if !std::mem::take(&mut image.mapped_here) { return None; }
    let memory = memories.get(&image.memory)?;
    Some((memory.address + image.offset, image.size))
}

pub(super) fn drop_native_image_mapping(mapping: Option<(u64, u64)>) {
    if let Some((address, size)) = mapping {
        Nx2GpuMemoryRegistry::get().unmap(address, size);
        Nx2GpuMemoryRegistry::get().release_backing(address, size);
    }
}

/// Contents protected by upstream Nx2NativeObjects::mutex. Callers share
/// Arc<Mutex<Nx2NativeObjects>> so both maps and counters use that one lock.
pub struct Nx2NativeObjects {
    pub memories: BTreeMap<u32, Nx2NativeMemory>,
    pub images: BTreeMap<u32, Nx2NativeImage>,
    pub next_memory: u32,
    pub next_image: u32,
}
impl Default for Nx2NativeObjects {
    fn default() -> Self {
        Self {
            memories: BTreeMap::new(),
            images: BTreeMap::new(),
            next_memory: 1,
            next_image: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nx2ImageLayout {
    pub width: u32,
    pub height: u32,
    pub pitch: u64,
    pub block_height_log2: u32,
    pub size: u64,
    pub alignment: u64,
}

pub fn nx2_calculate_image_layout(preference: &[u32; 16]) -> Nx2ImageLayout {
    let width = (preference[2] & 0xffff) + 1;
    let height = (preference[2] >> 16) + 1;
    let pitch_gobs = preference[5] as u64;
    let block_height_log2 = preference[4];
    let (pitch, block_height_log2) = if block_height_log2 <= 8
        && pitch_gobs >= (width as u64 * 4 + 63) / 64
        && pitch_gobs <= 4096
    {
        (pitch_gobs * 64, block_height_log2)
    } else {
        (common::alignment::align_up(width as u64 * 4, 64), 7)
    };
    let block_rows = 1u64 << block_height_log2;
    Nx2ImageLayout {
        width,
        height,
        pitch,
        block_height_log2,
        size: pitch * common::alignment::align_up(height as u64, block_rows),
        alignment: (64 * block_rows).max(0x1000),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nx2DecoderImageLayout {
    pub code: u32,
    pub width: u32,
    pub height: u32,
    pub format: crate::host1x_core::Nx2PictureFormat,
    pub pitch: u64,
    pub chroma_offset: u64,
    pub size: u64,
}

pub fn nx2_calculate_decoder_image_layout(preference: &[u32; 16]) -> Nx2DecoderImageLayout {
    use crate::host1x_core::Nx2PictureFormat;
    let code = preference[1];
    let width = (preference[2] & 0xffff) + 1;
    let height = (preference[2] >> 16) + 1;
    let format = match code {
        1 | 4 => Nx2PictureFormat::Nv12,
        2 | 5 => Nx2PictureFormat::Bgra8,
        _ => Nx2PictureFormat::Rgba8,
    };
    let nv12 = format == Nx2PictureFormat::Nv12;
    const BLOCK_ROWS: u64 = 128;
    let pitch = common::alignment::align_up(width as u64 * if nv12 { 1 } else { 4 }, 64);
    let chroma_offset = pitch * common::alignment::align_up(height as u64, BLOCK_ROWS);
    let size = chroma_offset
        + if nv12 {
            pitch * common::alignment::align_up((height as u64 + 1) / 2, BLOCK_ROWS)
        } else {
            0
        };
    Nx2DecoderImageLayout {
        code,
        width,
        height,
        format,
        pitch,
        chroma_offset,
        size,
    }
}

#[cfg(test)]
mod tests {
    use super::super::hle_ipc::KAutoObjectRef;
    use super::super::server_manager::ServerManager;
    use super::*;
    use crate::core::SystemRef;
    use crate::hle::ipc;
    use crate::hle::kernel::k_process::{KProcess, ProcessLock};
    use crate::hle::kernel::k_resource_limit::KResourceLimit;
    use crate::hle::kernel::k_thread::{KThread, KThreadLock};
    use crate::hle::kernel::kernel::ScopedKernelForTest;

    #[test]
    fn decoder_image_layout_preserves_format_pairs_and_odd_chroma_rows() {
        use crate::host1x_core::Nx2PictureFormat as Format;
        let mut preference = [0; 16];
        preference[2] = 64 | (256 << 16); // 65 x 257
        for code in [1, 4, 2, 5, 0, 3, 6, 10, 11, 12, 13, u32::MAX] {
            preference[1] = code;
            let layout = nx2_calculate_decoder_image_layout(&preference);
            assert_eq!((layout.code, layout.width, layout.height), (code, 65, 257));
            match code {
                1 | 4 => {
                    assert_eq!(layout.format, Format::Nv12);
                    assert_eq!(
                        (layout.pitch, layout.chroma_offset, layout.size),
                        (128, 49152, 81920)
                    );
                }
                _ => {
                    assert_eq!(
                        layout.format,
                        if code == 2 || code == 5 {
                            Format::Bgra8
                        } else {
                            Format::Rgba8
                        }
                    );
                    assert_eq!(
                        (layout.pitch, layout.chroma_offset, layout.size),
                        (320, 122880, 122880)
                    );
                }
            }
        }
        preference[1] = 1;
        preference[2] = u32::MAX;
        let layout = nx2_calculate_decoder_image_layout(&preference);
        assert_eq!(
            (layout.pitch, layout.chroma_offset, layout.size),
            (65536, 4294967296, 6442450944)
        );
    }

    #[test]
    fn image_layout_matches_upstream_boundaries_and_fallback() {
        let mut preference = [0; 16];
        // 17 x 129: the second GOB is needed, even though only one pixel uses it.
        preference[2] = 16 | (128 << 16);
        preference[4] = 8;
        preference[5] = 2;
        assert_eq!(
            nx2_calculate_image_layout(&preference),
            Nx2ImageLayout {
                width: 17,
                height: 129,
                pitch: 128,
                block_height_log2: 8,
                size: 32768,
                alignment: 16384,
            }
        );
        for (height_log, pitch_gobs) in [(9, 2), (8, 1), (8, 4097)] {
            preference[4] = height_log;
            preference[5] = pitch_gobs;
            let layout = nx2_calculate_image_layout(&preference);
            assert_eq!(
                (
                    layout.pitch,
                    layout.block_height_log2,
                    layout.size,
                    layout.alignment
                ),
                (128, 7, 32768, 8192)
            );
        }
        preference[2] = u32::MAX;
        preference[4] = 0;
        preference[5] = 4096;
        let layout = nx2_calculate_image_layout(&preference);
        assert_eq!(
            (
                layout.width,
                layout.height,
                layout.pitch,
                layout.size,
                layout.alignment
            ),
            (65536, 65536, 262144, 17179869184, 4096)
        );
        let layout = nx2_calculate_image_layout(&[0; 16]);
        assert_eq!(
            (layout.width, layout.height, layout.pitch, layout.size),
            (1, 1, 64, 8192)
        );
        let objects = Nx2NativeObjects::default();
        assert!(objects.images.is_empty() && objects.memories.is_empty());
        assert_eq!((objects.next_memory, objects.next_image), (1, 1));
    }

    #[derive(Clone, Default)]
    struct QueuedGpu {
        callbacks: Arc<Mutex<std::collections::VecDeque<Box<dyn FnOnce() + Send>>>>,
        semaphores: Arc<Mutex<Vec<(bool, u64)>>>,
        composites: Arc<
            Mutex<
                Vec<(
                    Vec<crate::gpu_core::FramebufferConfig>,
                    Vec<super::super::nvdrv::nvdata::NvFence>,
                )>,
            >,
        >,
    }
    impl QueuedGpu {
        fn drain(&self) {
            loop {
                let callback = self.callbacks.lock().unwrap().pop_front();
                match callback {
                    Some(callback) => callback(),
                    None => break,
                }
            }
        }
    }
    impl crate::gpu_core::GpuCoreInterface for QueuedGpu {
        fn as_any(&self) -> &(dyn std::any::Any + Send) {
            self
        }
        fn run_in_order(&self, callback: Box<dyn FnOnce() + Send>) {
            self.callbacks.lock().unwrap().push_back(callback);
        }
        fn set_nx2_ampere(&self, _enabled: bool) {}

        fn nx2_add_semaphore_region(&self, start: u64) {
            self.semaphores.lock().unwrap().push((true, start));
        }
        fn nx2_remove_semaphore_region(&self, start: u64) {
            self.semaphores.lock().unwrap().push((false, start));
        }
        fn allocate_channel_handle(&self) -> Arc<dyn crate::gpu_core::GpuChannelHandle> {
            panic!("unused test operation")
        }
        fn allocate_memory_manager_handle(
            &self,
            _: u64,
            _: u64,
            _: u64,
            _: u64,
        ) -> Arc<dyn crate::gpu_core::GpuMemoryManagerHandle> {
            panic!("unused test operation")
        }
        fn init_address_space(&self, _: Arc<dyn crate::gpu_core::GpuMemoryManagerHandle>) {
            panic!("unused test operation")
        }
        fn push_gpu_entries(&self, _: i32, _: crate::gpu_core::GpuCommandList) {
            panic!("unused test operation")
        }
        fn request_composite(
            &self,
            layers: Vec<crate::gpu_core::FramebufferConfig>,
            fences: Vec<crate::hle::service::nvdrv::nvdata::NvFence>,
        ) {
            self.composites.lock().unwrap().push((layers, fences));
        }
        fn run_on_gpu_thread(&self, _action: Box<dyn FnOnce() + Send>) -> u64 {
            panic!("GPU sync operation is outside this test fixture");
        }

        fn renderer_flush_region(&self, _addr: u64, _size: u64) {
            panic!("renderer flush is outside this test fixture");
        }

        fn request_screenshot(
            &self,
            _pixels: Vec<u8>,
            _callback: Box<dyn FnOnce(Vec<u8>, bool) + Send>,
            _layout: crate::frontend::framebuffer_layout::FramebufferLayout,
            _layer_stack: crate::hle::service::nvnflinger::hwc_layer::LayerStackId,
        ) {
            panic!("screenshot is outside this test fixture");
        }

        fn wait_for_composite(&self) {
            panic!("unused test operation")
        }
        fn on_cpu_write(&self, _: u64, _: u64) -> bool {
            panic!("unused test operation")
        }
        fn on_cpu_read(&self, _: u64, _: u64) -> crate::gpu_core::RasterizerDownloadArea {
            panic!("unused test operation")
        }
        fn flush_region(&self, _: u64, _: u64) {
            panic!("unused test operation")
        }
    }
    fn gpu_request(service: &INx2GpuSession, command: u32, words: &[u32]) -> HLERequestContext {
        let mut request = vec![command + 16, words.len() as u32];
        request.extend_from_slice(words);
        let mut ctx = HLERequestContext::new();
        ctx.populate_from_incoming_command_buffer(&request);
        assert_eq!(service.handle_sync_request(&mut ctx), RESULT_SUCCESS);
        ctx
    }

    #[test]
    fn gpu_memory_jobs_preserve_handles_mapping_and_deferred_events() {
        let _scope = ScopedKernelForTest::without_kernel();
        let mut system = crate::core::System::new_boxed_for_test();
        let gpu = QueuedGpu::default();
        system.set_gpu_core(Box::new(gpu.clone()));
        let service = INx2GpuSession::new(SystemRef::from_ref(&system));
        assert_eq!(
            &gpu_request(&service, 11, &[]).command_buffer()[2..5],
            &[0, 0, 31]
        );
        assert_eq!(
            &gpu_request(&service, 203, &[0]).command_buffer()[2..9],
            &[0, 1, 0, 0x8000000, 0, 0xf8000000, 0xbf]
        );
        let maps = Arc::new(Mutex::new(Vec::new()));
        let maps_out = maps.clone();
        let owner = Arc::as_ptr(&maps) as usize;
        Nx2GpuMemoryRegistry::get().add_listener(
            owner,
            Box::new(move |a, b, c| maps_out.lock().unwrap().push((a, b, c))),
            Box::new(|_, _| {}),
            None,
        );
        maps.lock().unwrap().clear();
        let reply = gpu_request(&service, 101, &[0x900000, 0, 0x901fff, 0, 0, 0]);
        assert_eq!(reply.command_buffer()[3], 1);
        assert_eq!(*gpu.semaphores.lock().unwrap(), vec![(true, 0x900000)]);
        let slot = service.event_for(3);
        assert!(Arc::ptr_eq(&slot, &service.event_for(35)));
        assert!(!slot.is_signaled());
        let mut words = [0; 16];
        words[1] = 0xa00000;
        words[3] = 0xa00fff;
        words[5] = 1;
        words[6] = 0x1000;
        words[15] = 35;
        gpu_request(&service, 211, &words);
        assert!(slot.is_signaled());
        assert_eq!(
            *maps.lock().unwrap(),
            vec![(0x900000, 0x900000, 0x2000), (0xa00000, 0x901000, 0x1000)]
        );
        slot.clear();
        gpu_request(&service, 102, &[1, 3]);
        assert!(!slot.is_signaled());
        assert!(service.state.lock().unwrap().memory.is_empty());
        assert_eq!(
            *gpu.semaphores.lock().unwrap(),
            vec![(true, 0x900000), (false, 0x900000)]
        );
        gpu.drain();
        assert!(slot.is_signaled());
        // Inverted ranges still allocate a handle and retain unsigned arithmetic.
        let reply = gpu_request(&service, 101, &[5, 0, 3, 0, 0, 0]);
        let handle = reply.command_buffer()[3];
        assert_eq!(service.state.lock().unwrap().memory[&handle].size, u64::MAX);
        let mut words = [0; 9];
        words[5] = 0x1234;
        words[8] = 35;
        assert_eq!(gpu_request(&service, 212, &words).command_buffer()[3], 0x34);
        Nx2GpuMemoryRegistry::get().unmap(0xa00000, 0x1000);
        Nx2GpuMemoryRegistry::get().remove_listener(owner);
    }

    #[test]
    fn gpu_sparse_backing_is_freed_only_after_gpu_completion() {
        use crate::device_memory::{dram_memory_map, DeviceMemory};
        use crate::memory::memory::{Memory, SharedMemory};
        let mut system = crate::core::System::new_boxed_for_test();
        system.kernel_mut().unwrap().initialize();
        system.kernel_mut().unwrap().initialize_resource_managers(
            0xffff_e000_0000_0000,
            crate::hle::kernel::k_memory_layout::KERNEL_PAGE_TABLE_HEAP_SIZE,
        );
        system
            .kernel_mut()
            .unwrap()
            .memory_manager_mut()
            .initialize_pool(
                Pool::SystemNonSecure,
                dram_memory_map::BASE + 0x100000,
                16 * PAGE_SIZE,
            );
        let device = Box::new(DeviceMemory::with_size(0x400000));
        let memory = Arc::new(SharedMemory::new(unsafe {
            Memory::new(
                SystemRef::null(),
                device.as_ref() as *const _,
                &device.buffer as *const _,
            )
        }));
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        {
            let mut process = process.lock().unwrap();
            let table = process.page_table.get_base_mut();
            table.m_address_space_start = 0x1000_0000;
            table.m_address_space_end = 0x1040_0000;
            table.m_alias_code_region_start = 0x1000_0000;
            table.m_alias_code_region_end = 0x1040_0000;
            table.m_address_space_width = 32;
            let resource = system
                .kernel()
                .unwrap()
                .get_system_system_resource()
                .unwrap();
            table.m_memory_block_slab_manager = Some(resource.memory_block_slab_manager_arc());
            table.m_block_info_manager = Some(resource.block_info_manager_arc());
            table.m_memory = Some(memory.clone());
            table
                .m_memory_block_manager
                .initialize(0x1000_0000, 0x1040_0000, None)
                .unwrap();
            table.initialize_impl();
        }
        system
            .kernel()
            .unwrap()
            .set_application_process(Some(process.clone()));
        let gpu = QueuedGpu::default();
        system.set_gpu_core(Box::new(gpu.clone()));
        let service = INx2GpuSession::new(SystemRef::from_ref(&system));
        let available = system
            .kernel()
            .unwrap()
            .memory_manager()
            .get_free_size(Pool::SystemNonSecure);
        let mut create = [0; 10];
        create[1] = 0;
        create[2] = 0x81;
        create[3] = 0x1fff;
        create[4] = 0x81;
        create[9] = 4;
        gpu_request(&service, 214, &create);
        assert!(service.event_for(4).is_signaled());
        assert_eq!(
            service.state.lock().unwrap().gpu_only_regions[&0x8100000000].cpu_address,
            0x10000000
        );
        assert_eq!(
            system
                .kernel()
                .unwrap()
                .memory_manager()
                .get_free_size(Pool::SystemNonSecure),
            available - 2 * PAGE_SIZE
        );
        let mut destroy = [0; 7];
        destroy[1] = 0;
        destroy[2] = 0x81;
        destroy[3] = 0x1fff;
        destroy[4] = 0x81;
        destroy[6] = 5;
        gpu_request(&service, 215, &destroy);
        assert!(!service.event_for(5).is_signaled());
        service.free_released_regions();
        assert_eq!(service.state.lock().unwrap().released_regions.len(), 1);
        gpu.drain();
        assert!(service.event_for(5).is_signaled());
        service.free_released_regions();
        assert!(service.state.lock().unwrap().released_regions.is_empty());
        assert_eq!(
            system
                .kernel()
                .unwrap()
                .memory_manager()
                .get_free_size(Pool::SystemNonSecure),
            available
        );
        drop(service);
        system.kernel_mut().unwrap().shutdown();
    }

    fn gsv_request(
        service: &INx2GraphicsSystemSession,
        command: u32,
        words: &[u32],
    ) -> HLERequestContext {
        let mut request = vec![16 + command, words.len() as u32];
        request.extend_from_slice(words);
        let mut ctx = HLERequestContext::new();
        ctx.populate_from_incoming_command_buffer(&request);
        assert_eq!(service.handle_sync_request(&mut ctx), RESULT_SUCCESS);
        ctx
    }
    #[test]
    fn gsv_tipc_data_offset_accounts_for_pid_copy_and_move_words() {
        assert_eq!(
            INx2GraphicsSystemSession::tipc_data_offset(&[0, 0, u32::MAX]),
            2
        );
        for pid in 0..=1 {
            for copies in 0..16 {
                for moves in 0..16 {
                    let desc = pid | (copies << 1) | (moves << 5);
                    assert_eq!(
                        INx2GraphicsSystemSession::tipc_data_offset(&[0, 0x80000000, desc]),
                        (3 + pid * 2 + copies + moves) as usize
                    );
                }
            }
        }
    }
    #[test]
    fn gsv_camera_context_moves_real_session_and_balances_lifetime() {
        use crate::core::SystemRef;
        use crate::hle::kernel::k_process::{KProcess, ProcessLock};
        use crate::hle::kernel::k_resource_limit::{KResourceLimit, LimitableResource};
        use crate::hle::kernel::k_thread::{KThread, KThreadLock};
        use crate::hle::service::hle_ipc::{KAutoObjectRef, SessionRequestManager};
        use crate::hle::service::server_manager::ServerManager;
        let _scope = ScopedKernelForTest::without_kernel();
        let limit = Arc::new(KResourceLimit::new());
        limit
            .set_limit_value(LimitableResource::SessionCountMax, 2)
            .unwrap();
        let mut process = KProcess::new();
        process.resource_limit = Some(limit.clone());
        process.initialize_handle_table();
        let process = Arc::new(ProcessLock::from_value(process));
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
        let server = ServerManager::new_shared(SystemRef::null());
        let (queue, wakeup) = {
            let guard = server.lock().unwrap();
            (guard.pending_registrations_arc(), guard.wakeup_event_arc())
        };
        let manager = Arc::new(Mutex::new(
            SessionRequestManager::new_with_server_manager_full(
                server.clone(),
                queue.clone(),
                wakeup,
            ),
        ));
        let mut ctx = HLERequestContext::new_with_thread(thread.clone(), 0);
        ctx.set_session_request_manager(manager);
        let service = INx2GraphicsSystemSession::new(SystemRef::null());
        ctx.populate_from_incoming_command_buffer(&[0x26a, 2, 1, 0]);
        service.handle_sync_request(&mut ctx);
        assert_eq!(ctx.command_buffer()[2], super::super::ipc_helpers::RESULT_NOT_SUPPORTED.get_inner_value());
        ctx.populate_from_incoming_command_buffer(&[0x269, 0]);
        service.handle_sync_request(&mut ctx);
        assert_eq!(ctx.command_buffer()[2], 0);
        ctx.populate_from_incoming_command_buffer(&[0x26a, 2, 1, 1]);
        service.handle_sync_request(&mut ctx);
        assert_eq!(ctx.command_buffer()[2], super::super::ipc_helpers::RESULT_NOT_SUPPORTED.get_inner_value());
        ctx.populate_from_incoming_command_buffer(&[0x26a, 2, 1, 0]);
        service.handle_sync_request(&mut ctx);
        assert_eq!(&ctx.command_buffer()[..3], &[0x26a, 0x80000001, 0x20]);
        assert!(ctx.outgoing_copy_objects.is_empty());
        let KAutoObjectRef::ObjectId(session_id) = ctx.outgoing_move_objects[0] else { panic!("session identity") };
        assert_eq!(limit.get_current_value(LimitableResource::SessionCountMax), 1);
        ctx.populate_from_incoming_command_buffer(&[0x26b, 0]);
        service.handle_sync_request(&mut ctx);
        assert_eq!(ctx.command_buffer()[2], 0);
        assert_eq!(*service.camera_contexts.lock().unwrap(), 0);
        ctx.populate_from_incoming_command_buffer(&[0x26b, 0]);
        service.handle_sync_request(&mut ctx);
        assert_eq!(ctx.command_buffer()[2], super::super::ipc_helpers::RESULT_NOT_SUPPORTED.get_inner_value());
        drop(service);
        let child_manager = queue.lock().unwrap()[0].1.clone();
        let child = child_manager.lock().unwrap().session_handler().unwrap().clone();
        let mut request = HLERequestContext::new();
        request.populate_from_incoming_command_buffer(&[0x13, 2, 0x61666544, 0x00746c75]);
        child.handle_sync_request(&mut request);
        assert_eq!(request.command_buffer()[2], super::super::ipc_helpers::RESULT_NOT_SUPPORTED.get_inner_value());
        // Exact captured CMIF3 request: named camera Default, sixteen bytes.
        let mut request = HLERequestContext::new();
        request.populate_from_incoming_command_buffer(&[6, 12, 0, 0, 0x49434653, 1, 3, 0,
            0x61666544, 0x00746c75, 0, 0, 0, 0]);
        child.handle_sync_request(&mut request);
        assert!(!request.is_tipc());
        assert_eq!(request.command_buffer()[4], 0x4f434653);
        assert_eq!(request.command_buffer()[6], 0xdc4);
        assert!(request.outgoing_copy_objects.is_empty());
        assert!(request.outgoing_move_objects.is_empty());
        let _kernel = ScopedKernelForTest::new();
        // CMIF29 serializes exactly 40 parameter bytes followed by a u64.
        // The client explicitly permits the same unavailable result as CMIF3.
        for camera in [0u64, 1u64 << 32, u64::MAX] {
            let mut request = HLERequestContext::new();
            request.populate_from_incoming_command_buffer(&[
                6, 20, 0, 0, 0x49434653, 1, 29, 0,
                384, 1280, 216, 720, 0, 0, 0, 0, 0, 0,
                camera as u32, (camera >> 32) as u32, 0, 0,
            ]);
            child.handle_sync_request(&mut request);
            assert!(!request.is_tipc());
            assert_eq!(request.command_buffer()[4], 0x4f434653);
            assert_eq!(request.command_buffer()[6], 0xdc4);
            assert!(request.outgoing_copy_objects.is_empty());
            assert!(request.outgoing_move_objects.is_empty());
        }
        let mut event_ids = Vec::new();
        for camera in [0u64, 0, 1u64 << 32] {
            let mut request = HLERequestContext::new_with_thread(thread.clone(), 0);
            request.populate_from_incoming_command_buffer(&[6, 10, 0, 0, 0x49434653, 1, 21, 0,
                camera as u32, (camera >> 32) as u32, 0, 0]);
            child.handle_sync_request(&mut request);
            assert_eq!(request.command_buffer()[2], 2); // one COPY, zero MOVE
            assert_eq!(request.command_buffer()[4], 0x4f434653);
            assert_eq!(request.command_buffer()[6], 0);
            assert!(request.outgoing_move_objects.is_empty());
            let KAutoObjectRef::ObjectId(event_id) = request.outgoing_copy_objects[0] else { panic!("event identity") };
            let event = process.lock().unwrap().get_readable_event_by_object_id(event_id).unwrap();
            assert!(!event.lock().unwrap().is_signaled());
            event_ids.push(event_id);
        }
        assert_eq!(event_ids[0], event_ids[1]);
        assert_ne!(event_ids[0], event_ids[2]); // retain full u64 identity
        process.lock().unwrap().unregister_session_object_by_object_id(session_id);
        assert_eq!(limit.get_current_value(LimitableResource::SessionCountMax), 0);
    }

    #[test]
    fn gsv_native_image_query_map_unmap_preserve_pool_offsets_and_lifetime() {
        let _scope = ScopedKernelForTest::without_kernel();
        let service = INx2GraphicsSystemSession::new(SystemRef::null());
        let base = 0x7e80000000u64;
        let size = 393216u64;
        let memory = gsv_request(&service, 111, &[0, (size * 3) as u32, 0]).command_buffer()[3];
        gsv_request(&service, 114, &[memory, base as u32, (base >> 32) as u32, (size * 3) as u32, 0]);
        let mut request = [0u32; 18];
        request[1] = 1;
        request[2] = 383 | (215 << 16);
        request[16] = 3;
        request[17] = memory;
        let first = gsv_request(&service, 1211, &request).command_buffer()[3];
        let id = first + 1;
        let query = gsv_request(&service, 428, &[id]);
        let mut expected = [0u32; 38];
        expected[3] = size as u32;
        expected[5] = 1;
        expected[6] = 1;
        expected[7] = request[2];
        expected[9] = 7;
        expected[10] = 24;
        expected[13] = 1;
        expected[14] = 1;
        expected[16] = size as u32;
        assert_eq!(&query.command_buffer()[2..40], &expected);
        assert_eq!(service.objects.lock().unwrap().images[&id].gpu_maps, 0);
        let events = Arc::new(Mutex::new(Vec::new()));
        let mapped = events.clone();
        let unmapped = events.clone();
        let released = events.clone();
        let owner = Arc::as_ptr(&events) as usize;
        let address = base + size;
        Nx2GpuMemoryRegistry::get().add_listener(owner,
            Box::new(move |gpu, cpu, len| { if gpu == address { mapped.lock().unwrap().push((0, gpu, cpu, len)); } }),
            Box::new(move |gpu, len| { if gpu == address { unmapped.lock().unwrap().push((1, gpu, 0, len)); } }),
            Some(Box::new(move |cpu, len| { if cpu == address { released.lock().unwrap().push((2, cpu, 0, len)); } })));
        for _ in 0..2 {
            let map = gsv_request(&service, 426, &[id, 1, 1, 3]);
            expected[1] = address as u32;
            expected[2] = (address >> 32) as u32;
            assert_eq!(&map.command_buffer()[2..40], &expected);
        }
        assert_eq!(*events.lock().unwrap(), [(0, address, address, size)]);
        gsv_request(&service, 427, &[id, 1, address as u32, (address >> 32) as u32, size as u32, 0]);
        assert_eq!(events.lock().unwrap().len(), 1);
        // Pool destruction owns the outstanding map and releases it exactly once.
        gsv_request(&service, 1212, &[first]);
        assert_eq!(*events.lock().unwrap(), [(0, address, address, size), (1, address, 0, size), (2, address, 0, size)]);
        Nx2GpuMemoryRegistry::get().remove_listener(owner);
        assert!(service.objects.lock().unwrap().images.is_empty());
        assert_eq!(gsv_request(&service, 428, &[id]).command_buffer()[2],
            super::super::ipc_helpers::RESULT_NOT_SUPPORTED.get_inner_value());
    }

    #[test]
    fn gsv_native_memory_and_decoder_pool_match_source_ownership_and_descriptors() {
        let _scope = ScopedKernelForTest::without_kernel();
        let service = INx2GraphicsSystemSession::new(SystemRef::null());
        let memory = gsv_request(&service, 111, &[3, 0x80000, 0]).command_buffer()[3];
        gsv_request(&service, 114, &[memory, 0x1000, 1, 0x90000, 0, 0, 0, 0, 0]);
        assert_eq!(
            service.objects.lock().unwrap().memories[&memory].size,
            0x80000
        );
        assert_eq!(
            service.objects.lock().unwrap().memories[&memory].address,
            0x100001000
        );
        gsv_request(&service, 423, &[77, 0x2000, 0, 0x3000, 0, 99, 0x777, 0]);
        assert_eq!(service.objects.lock().unwrap().memories[&77].size, 0x3000);
        let mut words = [0; 18];
        words[1] = 1;
        words[2] = (128 << 16) | 16;
        words[16] = 3;
        words[17] = memory;
        let layout = nx2_calculate_decoder_image_layout((&words[..16]).try_into().unwrap());
        let pool = gsv_request(&service, 991, &words);
        assert_eq!(pool.command_buffer()[3], (layout.size * 3) as u32);
        assert_eq!(pool.command_buffer()[5], 8192);
        let first = gsv_request(&service, 911, &words).command_buffer()[3];
        for i in 0..3 {
            let descriptor = gsv_request(&service, 961, &[first + i]);
            let mut expected = [0; 32];
            expected[0] = 10;
            expected[2] = (layout.size * i as u64) as u32;
            expected[4] = layout.pitch as u32;
            expected[6] = (layout.size * i as u64 + layout.chroma_offset) as u32;
            expected[8] = layout.pitch as u32;
            assert_eq!(&descriptor.command_buffer()[3..35], &expected);
            let objects = service.objects.lock().unwrap();
            let image = &objects.images[&(first + i)];
            assert_eq!(image.memory, memory);
            assert_eq!(image.offset, layout.size * i as u64);
            assert!(image.decoder);
        }
        gsv_request(&service, 315, &[first, 3]);
        assert!(service.objects.lock().unwrap().images[&first].window_pool);
        gsv_request(&service, 314, &[first]);
        assert!(!service.objects.lock().unwrap().images[&first].window_pool);
        gsv_request(&service, 430, &[first]);
        assert!(gsv_request(&service, 961, &[first]).command_buffer()[3..35]
            .iter()
            .all(|&x| x == 0));
        gsv_request(&service, 112, &[memory]);
        assert!(service
            .objects
            .lock()
            .unwrap()
            .images
            .contains_key(&(first + 1))); // Source does not erase dependent images.
        words[16] = 0;
        let zero_count = gsv_request(&service, 991, &words);
        assert_eq!(zero_count.command_buffer()[3], layout.size as u32);
    }
    #[test]
    fn gsv_missing_transfer_memory_still_returns_writable_then_readable_events() {
        let _without = ScopedKernelForTest::without_kernel();
        let service = INx2GraphicsSystemSession::new(SystemRef::null());
        let _kernel = ScopedKernelForTest::new();
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
        let mut ctx = HLERequestContext::new_with_thread(thread, 0);
        ctx.populate_from_incoming_command_buffer(&[16 + 204, 2, 1, 192]);
        assert_eq!(service.handle_sync_request(&mut ctx), RESULT_SUCCESS);
        assert_eq!(ctx.outgoing_copy_objects.len(), 2);
        let ids = ctx
            .outgoing_copy_objects
            .iter()
            .map(|object| match object {
                KAutoObjectRef::ObjectId(id) => *id,
                _ => panic!("event identity"),
            })
            .collect::<Vec<_>>();
        let process = process.lock().unwrap();
        assert!(process.get_event_by_object_id(ids[0]).is_some());
        assert!(process.get_readable_event_by_object_id(ids[0]).is_none());
        assert!(process.get_event_by_object_id(ids[1]).is_none());
        assert!(process.get_readable_event_by_object_id(ids[1]).is_some());
        assert!(service.state.lock().unwrap().syncpt_poll.is_none());
    }

    fn video_request(service: &INx2VideoSession, command: u32, words: &[u32]) -> HLERequestContext {
        let mut request = vec![16 + command, words.len() as u32];
        request.extend_from_slice(words);
        let mut ctx = HLERequestContext::new();
        ctx.populate_from_incoming_command_buffer(&request);
        assert_eq!(service.handle_sync_request(&mut ctx), RESULT_SUCCESS);
        ctx
    }
    #[test]
    fn video_empty_frame_reports_configured_size_clamped_to_image() {
        let _scope = ScopedKernelForTest::without_kernel();
        let objects = Arc::new(Mutex::new(Nx2NativeObjects::default()));
        let service = INx2VideoSession::new(SystemRef::null(), objects.clone(), "nx2:vdec-test");
        {
            let mut state = service.shared.state.lock().unwrap();
            state.width = 1920;
            state.height = 1080;
        }
        let frame = VideoDecoded {
            picture: None,
            pts: 4,
            end: true,
        };
        let mut bytes = vec![0x5a; 8];
        assert_eq!(
            INx2VideoSession::write_image(
                SystemRef::null(),
                &objects,
                &service.shared,
                None,
                4,
                &frame,
                &mut bytes
            ),
            (1920, 1080)
        );
        let mut preference = [0; 16];
        preference[2] = (719 << 16) | 1279;
        objects.lock().unwrap().images.insert(
            4,
            Nx2NativeImage {
                preference,
                ..Default::default()
            },
        );
        assert_eq!(
            INx2VideoSession::write_image(
                SystemRef::null(),
                &objects,
                &service.shared,
                None,
                4,
                &frame,
                &mut bytes
            ),
            (1280, 720)
        );
        assert_eq!(bytes, [0x5a; 8]);
    }

    #[test]
    fn video_queue_pressure_flush_preserves_image_ring_and_completed_outputs() {
        let _scope = ScopedKernelForTest::without_kernel();
        let service = INx2VideoSession::new(
            SystemRef::null(),
            Arc::new(Mutex::new(Nx2NativeObjects::default())),
            "nx2:vdec-test",
        );
        for n in 1..=9 {
            let response = video_request(&service, 109, &[0, 0, 0, 0x12345678, 0xabcdef01, 0x100]);
            assert_eq!(&response.command_buffer()[2..5], &[0, n, 0]);
            assert_eq!(service.ready.room.load(Ordering::Relaxed), n < 8);
        }
        {
            let mut state = service.shared.state.lock().unwrap();
            assert_eq!(state.inputs[0].pts, 0xabcdef0112345678);
            assert!(!state.inputs[0].end); // Only the low byte counts.
            state.free.extend([4, 5, 4]);
            state.release_fences.insert(
                4,
                VideoFence {
                    syncpoint: 7,
                    value: 8,
                },
            );
            state.decoded.push_back(VideoDecoded {
                picture: None,
                pts: 9,
                end: true,
            });
            state.outputs.push_back(VideoOutput {
                image: 5,
                width: 3,
                height: 2,
                pts: 9,
                end: false,
            });
        }
        video_request(&service, 8, &[]);
        {
            let state = service.shared.state.lock().unwrap();
            assert!(state.inputs.is_empty() && state.flush);
            assert_eq!(state.decoded.len(), 1); // Worker, not IPC flush, clears these.
            assert_eq!(state.free.iter().copied().collect::<Vec<_>>(), [4, 5, 4]);
            assert_eq!(state.outputs.len(), 1);
            assert!(service.ready.room.load(Ordering::Relaxed));
        }
        video_request(&service, 212, &[4]);
        let state = service.shared.state.lock().unwrap();
        assert_eq!(state.free.iter().copied().collect::<Vec<_>>(), [5]);
        assert!(state.release_fences.is_empty());
    }
    #[test]
    fn video_output_info_empty_ring_and_wrapping_nanoseconds_match_wire() {
        let _scope = ScopedKernelForTest::without_kernel();
        let service = INx2VideoSession::new(
            SystemRef::null(),
            Arc::new(Mutex::new(Nx2NativeObjects::default())),
            "nx2:vdec-test",
        );
        let empty = video_request(&service, 213, &[]);
        assert_eq!(empty.command_buffer()[2], 0x1c286f);
        service
            .shared
            .state
            .lock()
            .unwrap()
            .outputs
            .push_back(VideoOutput {
                image: 19,
                width: 1920,
                height: 1080,
                pts: u64::MAX,
                end: true,
            });
        let output = video_request(&service, 213, &[]);
        let pts = u64::MAX.wrapping_mul(1000);
        assert_eq!(
            &output.command_buffer()[2..19],
            &[
                0,
                19,
                1920,
                1080,
                1920,
                1080,
                0,
                pts as u32,
                (pts >> 32) as u32,
                1,
                0,
                0,
                0,
                0,
                0,
                0,
                0
            ]
        );
        assert!(service.shared.state.lock().unwrap().outputs.is_empty());
    }
    #[test]
    fn video_idle_worker_starts_once_and_joins_before_session_drop() {
        let _scope = ScopedKernelForTest::without_kernel();
        let service = INx2VideoSession::new(
            SystemRef::null(),
            Arc::new(Mutex::new(Nx2NativeObjects::default())),
            "nx2:vdec-test",
        );
        video_request(&service, 101, &[0, 0, 0, 640, 360, 30]);
        let first = service
            .control
            .lock()
            .unwrap()
            .worker
            .as_ref()
            .unwrap()
            .thread()
            .id();
        video_request(&service, 101, &[0, 0, 0, 1280, 720, 60]);
        assert_eq!(
            service
                .control
                .lock()
                .unwrap()
                .worker
                .as_ref()
                .unwrap()
                .thread()
                .id(),
            first
        );
        let shared = service.shared.clone();
        {
            let state = shared.state.lock().unwrap();
            assert_eq!((state.width, state.height), (1280, 720));
        }
        drop(service);
        assert!(shared.state.lock().unwrap().quit);
        assert_eq!(Arc::strong_count(&shared), 1);
    }
    #[test]
    fn video_drop_releases_syncpoint_after_service_manager_shutdown() {
        let _scope = ScopedKernelForTest::without_kernel();
        let mut system = crate::core::System::new_boxed_for_test();
        let module = super::super::nvdrv::nvdrv::Module::new(SystemRef::from_ref(&system));
        let allocator = module.get_container().get_syncpoint_manager_handle();
        system.service_manager().unwrap().lock().unwrap().register_service(
            "nvdrv".into(), 64,
            Box::new(move || Arc::new(super::super::nvdrv::nvdrv_interface::NvdrvService::new(
                Arc::clone(&module), "nvdrv",
            ))),
        );
        let service = INx2VideoSession::new(
            SystemRef::from_ref(&system),
            Arc::new(Mutex::new(Nx2NativeObjects::default())),
            "nx2:vdec-test",
        );
        let syncpoints = service.nvdrv_syncpoints();
        assert!(Arc::ptr_eq(&syncpoints, &allocator));
        let id = syncpoints.allocate_syncpoint(false);
        service.shared.state.lock().unwrap().syncpoint = id;
        system.shutdown_main_process();
        assert!(system.service_manager().is_none());
        assert!(allocator.is_syncpoint_allocated(id));
        drop(service);
        assert!(!allocator.is_syncpoint_allocated(id));
    }
    #[test]
    fn video_ready_timer_observes_room_and_is_disarmed_before_event_close() {
        let _without = ScopedKernelForTest::without_kernel();
        let system = crate::core::System::new_boxed_for_test();
        let service = INx2VideoSession::new(
            SystemRef::from_ref(&system),
            Arc::new(Mutex::new(Nx2NativeObjects::default())),
            "nx2:vdec-test",
        );
        let _kernel = ScopedKernelForTest::new();
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
        let mut ctx = HLERequestContext::new_with_thread(thread, 0);
        ctx.populate_from_incoming_command_buffer(&[19, 0]);
        service.handle_sync_request(&mut ctx);
        assert_eq!(ctx.outgoing_copy_objects.len(), 1);
        let event = service
            .ready
            .event
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .clone();
        assert!(event.is_signaled());
        event.clear();
        let timer = service
            .control
            .lock()
            .unwrap()
            .ready_timer
            .as_ref()
            .unwrap()
            .clone();
        let callback = timer.lock().callback.clone();
        service.ready.room.store(false, Ordering::Relaxed);
        callback(0, std::time::Duration::ZERO);
        assert!(!event.is_signaled());
        service.ready.room.store(true, Ordering::Relaxed);
        callback(0, std::time::Duration::ZERO);
        assert!(event.is_signaled());
        event.clear();
        let ready = service.ready.clone();
        drop(service);
        assert!(ready.event.lock().unwrap().is_none());
        callback(0, std::time::Duration::ZERO);
        assert!(!event.is_signaled());
    }

    #[test]
    fn vi2_composite_uses_memory_base_native_layout_and_only_valid_fences() {
        let _scope = ScopedKernelForTest::without_kernel();
        let mut system = crate::core::System::new_boxed_for_test();
        system.init_perf_stats(1);
        let gpu = QueuedGpu::default();
        system.set_gpu_core(Box::new(gpu.clone()));
        let objects = Arc::new(Mutex::new(Nx2NativeObjects::default()));
        let mut preference = [0u32; 16];
        preference[2] = (128 << 16) | 16;
        preference[4] = 8;
        preference[5] = 2;
        objects.lock().unwrap().memories.insert(
            1,
            Nx2NativeMemory {
                size: 0x10000,
                address: 0x7d000000,
            },
        );
        objects.lock().unwrap().images.insert(
            2,
            Nx2NativeImage {
                memory: 1,
                preference,
                offset: 0x8000,
                ..Default::default()
            },
        );
        let service = INx2Vi2Session::new(SystemRef::from_ref(&system), objects);
        service.composite(2, 99, Some([0, (128 << 16) | 16]), 1, 7);
        service.composite(2, 2, Some([0, (128 << 16) | 16]), 1, 7); // No device mapping yet.
        assert!(gpu.composites.lock().unwrap().is_empty());
        Nx2GpuMemoryRegistry::get().record_device_range(0x7d000000, 0x10000, 0x8d000000);
        service.composite(2, 2, Some([0, (128 << 16) | 16]), 191, 0xffff_fffe);
        service.composite(2, 2, Some([0, (128 << 16) | 16]), 192, 8);
        // Exercise the real TIPC path, including nonzero origins, a single
        // pixel, and the largest inclusive u16 end (must not wrap to zero).
        for (min, max, enabled) in [
            (0, 0x059f09ff, true), // Captured Mario Kart World presentation rectangle.
            (2 | (3 << 16), 12 | (100 << 16), true),
            (7 | (9 << 16), 7 | (9 << 16), true),
            (0, u32::MAX, true),
            (0, 0, true), // Enabled zero bounds mean one pixel, not no crop.
            (0, 0, false),
            (0, 0x059f09ff, false), // Ignore stale bounds when crop is disabled.
        ] {
            let mut request = [0u32; 26];
            request[0] = 16 + 229;
            request[1] = 24;
            request[5] = 2;
            request[6] = min;
            request[7] = max;
            request[8] = 1 | if enabled { 2 } else { 0 };
            request[22] = !192;
            let mut ctx = HLERequestContext::new();
            ctx.populate_from_incoming_command_buffer(&request);
            assert_eq!(service.handle_sync_request(&mut ctx), RESULT_SUCCESS);
            assert_eq!(&ctx.command_buffer()[2..9], &[0, 0x7fff, 0, 0, 0, 0, 0]);
        }
        Nx2GpuMemoryRegistry::get().release_backing(0x7d000000, 0x10000);
        let calls = gpu.composites.lock().unwrap();
        assert_eq!(calls.len(), 9);
        let layer = calls[0].0[0];
        assert_eq!((layer.address, layer.offset), (0x8d000000, 0));
        assert_eq!((layer.width, layer.height, layer.stride), (17, 129, 32));
        assert!(layer.native_tiling);
        assert_eq!(layer.crop_rect.right, 17);
        assert_eq!(layer.crop_rect.bottom, 129);
        assert_eq!(calls[0].1.len(), 1);
        assert_eq!((calls[0].1[0].id, calls[0].1[0].value), (191, 0xffff_fffe));
        assert!(calls[1].1.is_empty());
        for (call, expected) in calls[2..].iter().zip([
            common::math_util::Rectangle::new(0, 0, 2560, 1440),
            common::math_util::Rectangle::new(2, 3, 13, 101),
            common::math_util::Rectangle::new(7, 9, 8, 10),
            common::math_util::Rectangle::new(0, 0, 65536, 65536),
            common::math_util::Rectangle::new(0, 0, 1, 1),
            common::math_util::Rectangle::new(0, 0, 17, 129),
            common::math_util::Rectangle::new(0, 0, 17, 129),
        ]) {
            assert_eq!(call.0[0].crop_rect, expected);
            assert_eq!((call.0[0].width, call.0[0].height, call.0[0].stride), (17, 129, 32));
        }
    }

    #[test]
    fn vi2_replies_and_missing_image_present_preserve_wire_words() {
        let _scope = ScopedKernelForTest::without_kernel();
        let system = crate::core::System::new_boxed_for_test();
        let service = INx2Vi2Session::new(
            SystemRef::from_ref(&system),
            Arc::new(Mutex::new(Nx2NativeObjects::default())),
        );
        for (command, expected) in [(1, 1), (3, 1), (5, 7)] {
            let mut ctx = HLERequestContext::new();
            ctx.populate_from_incoming_command_buffer(&[16 + command, 0]);
            assert_eq!(service.handle_sync_request(&mut ctx), RESULT_SUCCESS);
            assert_eq!(&ctx.command_buffer()[2..4], &[0, expected]);
            assert_eq!(ctx.write_size, 4);
        }
        let mut ctx = HLERequestContext::new();
        ctx.populate_from_incoming_command_buffer(&[16 + 228, 0]);
        service.handle_sync_request(&mut ctx);
        assert_eq!(&ctx.command_buffer()[2..9], &[0, 0x7fff, 0, 0, 0, 0, 0]);
        let before = crate::core::G_NX2_PRESENT_COUNT.load(Ordering::Relaxed);
        let mut request = [0u32; 25];
        request[0] = 16 + 229;
        request[1] = 23;
        request[5] = 999; // Missing image: source performs no GPU work.
        request[22] = !192; // Invalid sentinel: no Host1x dependency.
        request[24] = 123;
        ctx.populate_from_incoming_command_buffer(&request);
        service.handle_sync_request(&mut ctx);
        assert_eq!(&ctx.command_buffer()[2..9], &[0, 0x7fff, 0, 0, 0, 0, 0]);
        assert!(crate::core::G_NX2_PRESENT_COUNT.load(Ordering::Relaxed) > before);
        let state = service.state.lock().unwrap();
        assert_eq!(state.presents, 1);
        assert_eq!(state.wait_timeouts, 0);
        assert_eq!(state.stats.presents, 1);
    }

    #[test]
    fn vi2_vsync_signals_all_events_and_callback_survives_session_drop() {
        let _without_kernel = ScopedKernelForTest::without_kernel();
        let system = crate::core::System::new_boxed_for_test();
        let service = INx2Vi2Session::new(
            SystemRef::from_ref(&system),
            Arc::new(Mutex::new(Nx2NativeObjects::default())),
        );
        let _kernel = ScopedKernelForTest::new();
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
        for command in [11, 31, 32] {
            let mut ctx = HLERequestContext::new_with_thread(thread.clone(), 0);
            ctx.populate_from_incoming_command_buffer(&[16 + command, 1, 0]);
            assert_eq!(service.handle_sync_request(&mut ctx), RESULT_SUCCESS);
            assert_eq!(ctx.outgoing_copy_objects.len(), 1);
            assert!(ctx.outgoing_move_objects.is_empty());
        }
        let (events, timer) = {
            let state = service.state.lock().unwrap();
            (
                state
                    .events
                    .iter()
                    .map(|h| state.context.get_event(*h).unwrap())
                    .collect::<Vec<_>>(),
                state.vsync_timer.as_ref().unwrap().clone(),
            )
        };
        assert!(events.iter().all(|event| !event.is_signaled()));
        let callback = timer.lock().callback.clone();
        assert_eq!(callback(0, std::time::Duration::ZERO), None);
        assert!(!events[0].is_signaled());
        assert!(events[1].is_signaled() && events[2].is_signaled());
        for event in &events {
            event.clear();
        }
        let shared = service.vsync.clone();
        let sequence = timer.lock().sequence_number;
        drop(service);
        assert!(shared.lock().unwrap().is_empty());
        assert_eq!(timer.lock().sequence_number, sequence + 1);
        assert_eq!(callback(0, std::time::Duration::ZERO), None);
        assert!(events.iter().all(|event| !event.is_signaled()));
        drop(callback);
        drop(events);
        drop(thread);
        drop(process);
    }

    #[test]
    fn syncpt_handshake_invalid_counter_and_cancel_without_request() {
        let _scope = ScopedKernelForTest::without_kernel();
        let service = INx2SyncptSession::new(SystemRef::null());
        let mut ctx = HLERequestContext::new();
        ctx.populate_from_incoming_command_buffer(&[16, 2, 0x12345678, 0xabcdef01]);
        assert_eq!(service.handle_sync_request(&mut ctx), RESULT_SUCCESS);
        assert_eq!(&ctx.command_buffer()[2..5], &[0, 0x12345678, 0xabcdef01]);
        ctx.populate_from_incoming_command_buffer(&[16 + 103, 1, !192]);
        service.handle_sync_request(&mut ctx);
        assert_eq!(&ctx.command_buffer()[2..6], &[0, 0, 0, 0]);
        ctx.populate_from_incoming_command_buffer(&[16 + 202, 1, 7]);
        service.handle_sync_request(&mut ctx);
        let event = {
            let state = service.state.lock().unwrap();
            state.context.get_event(state.events[&7]).unwrap()
        };
        assert!(event.is_signaled());
        event.clear();
        ctx.populate_from_incoming_command_buffer(&[16 + 201, 5, 7, !192, 0, 0, 1]);
        service.handle_sync_request(&mut ctx);
        assert!(event.is_signaled());
        assert!(service.state.lock().unwrap().requests.is_empty());
        let alive = service.shared_alive.clone();
        drop(service);
        assert!(!*alive.lock().unwrap());
    }

    #[test]
    fn tipc_words_follow_single_result_without_cmif_padding() {
        let mut ctx = HLERequestContext::new();
        ctx.populate_from_incoming_command_buffer(&[16 + 202, 0]);
        tipc_reply_words(&mut ctx, &[0x11223344, 0x55667788]);
        assert_eq!(
            &ctx.command_buffer()[..5],
            &[218, 3, 0, 0x11223344, 0x55667788]
        );
        assert_eq!(ctx.write_size, 5); // two header words plus result/data
        assert!(ctx.command_buffer()[5..].iter().all(|&w| w == 0));
    }

    #[test]
    fn raw_session_from_domain_queues_copy_endpoint_and_releases_resource() {
        let _scope = ScopedKernelForTest::without_kernel();
        let limit = Arc::new(KResourceLimit::new());
        limit
            .set_limit_value(LimitableResource::SessionCountMax, 2)
            .unwrap();
        let mut process = KProcess::new();
        process.process_id = 42;
        process.resource_limit = Some(limit.clone());
        process.initialize_handle_table();
        let process = Arc::new(ProcessLock::from_value(process));
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
        let server = ServerManager::new_shared(SystemRef::null());
        let (queue, wakeup) = {
            let server = server.lock().unwrap();
            (
                server.pending_registrations_arc(),
                server.wakeup_event_arc(),
            )
        };
        let manager = Arc::new(Mutex::new(
            SessionRequestManager::new_with_server_manager_full(
                server.clone(),
                queue.clone(),
                wakeup,
            ),
        ));
        let gpu = INx2GpuSession::new(SystemRef::null());
        manager.lock().unwrap().set_session_handler(gpu.clone());
        manager.lock().unwrap().convert_to_domain();
        let mut ctx = HLERequestContext::new_with_thread(thread, 0);
        ctx.set_session_request_manager(manager.clone());
        ctx.populate_from_incoming_command_buffer(&[ipc::CommandType::Request as u32, 0]);
        INx2GpuSession::open_sub_session(gpu.as_ref(), &mut ctx);
        assert_eq!(manager.lock().unwrap().domain_handler_count(), 1);
        assert_eq!(ctx.outgoing_copy_objects.len(), 1);
        assert!(ctx.outgoing_move_objects.is_empty());
        let object = match ctx.outgoing_copy_objects[0] {
            KAutoObjectRef::ObjectId(id) => id,
            _ => panic!("expected unresolved client object"),
        };
        assert_eq!(
            limit.get_current_value(LimitableResource::SessionCountMax),
            1
        );
        assert_eq!(queue.lock().unwrap().len(), 1);
        let child = queue.lock().unwrap()[0].1.clone();
        assert!(!child.lock().unwrap().is_domain());
        let expected: Arc<dyn SessionRequestHandler> = gpu.clone();
        assert!(Arc::ptr_eq(
            child.lock().unwrap().session_handler().unwrap(),
            &expected
        ));
        let event = gpu.event_for(7);
        let mut request = HLERequestContext::new();
        request.populate_from_incoming_command_buffer(&[16 + 21, 1, 7]);
        child
            .lock()
            .unwrap()
            .session_handler()
            .unwrap()
            .handle_sync_request(&mut request);
        assert!(event.is_signaled());
        assert!(child.lock().unwrap().pending_registrations().is_some());
        assert!(process
            .lock()
            .unwrap()
            .client_session_objects
            .contains_key(&object));
        process
            .lock()
            .unwrap()
            .unregister_session_object_by_object_id(object);
        assert_eq!(
            limit.get_current_value(LimitableResource::SessionCountMax),
            0
        );
    }
}

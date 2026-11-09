// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/dma_pusher.h and video_core/dma_pusher.cpp
//!
//! DMA command submission to FIFOs, assembling pushbuffers into a command stream.
//! See https://envytools.readthedocs.io/en/latest/hw/fifo/dma-pusher.html

use std::collections::VecDeque;
use std::sync::Arc;

use crate::engines::engine_interface::EngineTypes;
use crate::engines::engine_interface::{EngineHandle, EngineInterface};
use crate::engines::puller::{MethodCall, Puller};
use crate::engines::{kepler_compute, maxwell_3d};
use crate::guest_memory::{GpuGuestMemory, GpuMemoryManagerHandle, GuestMemoryFlags};
use common::scratch_buffer::ScratchBuffer;
use common::settings;
use parking_lot::{Condvar, Mutex};
use ruzu_core::core::SystemRef;
use smallvec::SmallVec;

/// GPU virtual address type.
pub type GPUVAddr = u64;

/// DMA submission modes for command headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SubmissionMode {
    IncreasingOld = 0,
    Increasing = 1,
    NonIncreasingOld = 2,
    NonIncreasing = 3,
    Inline = 4,
    IncreaseOnce = 5,
}

impl SubmissionMode {
    pub fn from_u32(val: u32) -> Option<Self> {
        match val {
            0 => Some(Self::IncreasingOld),
            1 => Some(Self::Increasing),
            2 => Some(Self::NonIncreasingOld),
            3 => Some(Self::NonIncreasing),
            4 => Some(Self::Inline),
            5 => Some(Self::IncreaseOnce),
            _ => None,
        }
    }
}

/// Buffer methods used by the DMA pusher (register addresses).
///
/// Note: methods are treated as 4-byte addressable locations, values here are NOT
/// multiplied by 4. Docs may show values multiplied by 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum BufferMethods {
    BindObject = 0x0,
    Illegal = 0x1,
    Nop = 0x2,
    SemaphoreAddressHigh = 0x4,
    SemaphoreAddressLow = 0x5,
    SemaphoreSequencePayload = 0x6,
    SemaphoreOperation = 0x7,
    NonStallInterrupt = 0x8,
    WrcacheFlush = 0x9,
    MemOpA = 0xA,
    MemOpB = 0xB,
    MemOpC = 0xC,
    MemOpD = 0xD,
    RefCnt = 0x14,
    SemaphoreAcquire = 0x1A,
    SemaphoreRelease = 0x1B,
    SyncpointPayload = 0x1C,
    SyncpointOperation = 0x1D,
    WaitForIdle = 0x1E,
    CrcCheck = 0x1F,
    Yield = 0x20,
    NonPullerMethods = 0x40,
}

/// Command list header (64-bit), packed as a bitfield.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct CommandListHeader {
    pub raw: u64,
}

impl CommandListHeader {
    /// GPU address (bits 0..40).
    pub fn addr(&self) -> GPUVAddr {
        self.raw & ((1u64 << 40) - 1)
    }

    /// Whether command-buffer cache flushing is allowed (bit 40).
    pub fn allow_flush(&self) -> bool {
        (self.raw >> 40) & 1 != 0
    }

    /// Whether this is a push-buffer entry (bit 41).
    pub fn is_push_buffer(&self) -> bool {
        (self.raw >> 41) & 1 != 0
    }

    /// Size in words (bits 42..63, excluding bit 63).
    pub fn size(&self) -> u32 {
        ((self.raw >> 42) & ((1u64 << 21) - 1)) as u32
    }

    /// Whether this entry waits for the preceding GPU fence (bit 63).
    pub fn sync(&self) -> bool {
        self.raw >> 63 != 0
    }
}

/// Command header (32-bit), packed as a bitfield.
#[derive(Debug, Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct CommandHeader {
    pub raw: u32,
}

impl CommandHeader {
    pub fn argument(&self) -> u32 {
        self.raw
    }

    /// Method index (bits 0..13).
    pub fn method(&self) -> u32 {
        self.raw & 0x1FFF
    }

    /// Legacy 24-bit method-count view (bits 0..24).
    pub fn method_count_(&self) -> u32 {
        self.raw & 0x00FF_FFFF
    }

    /// Subchannel (bits 13..16).
    pub fn subchannel(&self) -> u32 {
        (self.raw >> 13) & 0x7
    }

    /// Argument/method count (bits 16..29).
    pub fn arg_count(&self) -> u32 {
        (self.raw >> 16) & 0x1FFF
    }

    /// Method count (same field as arg_count).
    pub fn method_count(&self) -> u32 {
        self.arg_count()
    }

    /// Submission mode (bits 29..32).
    pub fn mode(&self) -> Option<SubmissionMode> {
        SubmissionMode::from_u32((self.raw >> 29) & 0x7)
    }
}

/// Build a command header from parts.
pub fn build_command_header(
    method: BufferMethods,
    arg_count: u32,
    mode: SubmissionMode,
) -> CommandHeader {
    let raw = (method as u32 & 0x1FFF) | ((arg_count & 0x1FFF) << 16) | ((mode as u32 & 0x7) << 29);
    CommandHeader { raw }
}

/// A list of commands to be submitted to the DMA pusher.
#[derive(Debug, Default)]
pub struct CommandList {
    /// Indirect buffer entries (list of GPU addresses + sizes).
    pub command_lists: SmallVec<[CommandListHeader; 512]>,
    /// Prefetched command list (used for synchronization).
    pub prefetch_command_list: SmallVec<[CommandHeader; 512]>,
}

impl CommandList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_size(size: usize) -> Self {
        let mut command_lists = SmallVec::new();
        command_lists.resize(size, CommandListHeader::default());
        Self {
            command_lists,
            prefetch_command_list: SmallVec::new(),
        }
    }

    pub fn from_prefetch(prefetch: impl IntoIterator<Item = CommandHeader>) -> Self {
        let mut prefetch_command_list = SmallVec::new();
        prefetch_command_list.extend(prefetch);
        Self {
            command_lists: SmallVec::new(),
            prefetch_command_list,
        }
    }
}

/// Constants matching upstream.
const NON_PULLER_METHODS: u32 = 0x40;
const MAX_SUBCHANNELS: usize = 8;
const MACRO_REGISTERS_START: u32 = 0xE00;
const COMPUTE_INLINE: u32 = 0x6D;

/// Internal DMA state tracking.
#[derive(Debug, Default)]
struct DmaState {
    method: u32,
    subchannel: u32,
    method_count: u32,
    #[allow(dead_code)]
    length_pending: u32,
    dma_get: GPUVAddr,
    dma_word_offset: u64,
    non_incrementing: bool,
    is_last_call: bool,
}

#[derive(Default)]
struct DmaSyncState {
    synced: Mutex<bool>,
    cv: Condvar,
}

// Rust lifetime adaptation for upstream engine->Front(): cache the separate
// frontend allocation, not a reference into the concurrently mutated engine.
enum FrontEngine {
    Backend,
    Maxwell(Arc<Mutex<maxwell_3d::FrontState>>),
    Kepler(Arc<Mutex<kepler_compute::FrontState>>),
}

#[derive(Clone, Copy)]
struct DeferredListEnd(*const crate::gpu::Gpu);
// The GPU owner outlives the joined backend, as for DmaPusher::gpu.
unsafe impl Send for DeferredListEnd {}
impl DeferredListEnd {
    unsafe fn run(self) {
        let gpu = unsafe { &*self.0 };
        gpu.flush_commands();
        gpu.on_command_list_end();
    }
}

/// Implements DMA submission to FIFOs, assembling pushbuffers into a stream
/// of 32-bit commands. Owns the puller and channel engine bindings, matching
/// upstream DmaPusher; split sessions retain each engine's frontend allocation.
pub struct DmaPusher {
    dma_pushbuffer: VecDeque<CommandList>,
    dma_pushbuffer_subindex: usize,
    command_resume_offset: usize,
    dma_state: DmaState,
    dma_increment_once: bool,
    ib_enable: bool,
    command_headers: ScratchBuffer<CommandHeader>,
    subchannels: [Option<EngineHandle>; MAX_SUBCHANNELS],
    subchannel_type: [EngineTypes; MAX_SUBCHANNELS],
    front_engines: [Option<FrontEngine>; MAX_SUBCHANNELS],
    gpu: *const crate::gpu::Gpu,
    system: SystemRef,
    memory_manager: Arc<Mutex<crate::memory_manager::MemoryManager>>,
    channel_state: *mut crate::control::channel_state::ChannelState,
    puller: Puller,
    rasterizer: Option<crate::rasterizer_interface::RasterizerHandle>,
    signal_sync: bool,
    sync_state: Arc<DmaSyncState>,
}

// Safety: `gpu` points back to the owning `Gpu`, which outlives the `DmaPusher`
// through `ChannelState`. `memory_manager` is already synchronized by `Arc<Mutex<_>>`.
unsafe impl Send for DmaPusher {}

impl DmaPusher {
    /// Creates a new DmaPusher.
    pub fn new(
        gpu: *const crate::gpu::Gpu,
        system: SystemRef,
        memory_manager: Arc<Mutex<crate::memory_manager::MemoryManager>>,
        channel_state: *mut crate::control::channel_state::ChannelState,
    ) -> Self {
        let puller_memory_manager = Arc::clone(&memory_manager);
        Self {
            dma_pushbuffer: VecDeque::new(),
            dma_pushbuffer_subindex: 0,
            command_resume_offset: 0,
            dma_state: DmaState::default(),
            dma_increment_once: false,
            ib_enable: true,
            command_headers: ScratchBuffer::new(),
            subchannels: [None; MAX_SUBCHANNELS],
            subchannel_type: [EngineTypes::Maxwell3D; MAX_SUBCHANNELS],
            front_engines: std::array::from_fn(|_| None),
            gpu,
            system,
            memory_manager,
            channel_state,
            puller: Puller::new(puller_memory_manager, std::ptr::null_mut(), channel_state),
            rasterizer: None,
            signal_sync: false,
            sync_state: Arc::new(DmaSyncState::default()),
        }
    }

    /// Install the stable boxed self pointer into the embedded puller.
    ///
    /// This must be called only after the `DmaPusher` has reached its final
    /// owning address. Doing it inside `new()` would capture a pre-move
    /// address and make `Puller::ProcessBindMethod()` write subchannel
    /// bindings into stale storage.
    pub fn install_self_reference(&mut self) {
        debug_assert_eq!(self.puller.channel_state_ptr(), self.channel_state);
        let self_ptr: *mut DmaPusher = self;
        self.puller.set_dma_pusher(self_ptr);
    }

    pub fn bind_rasterizer(
        &mut self,
        rasterizer: &dyn crate::rasterizer_interface::RasterizerInterface,
    ) {
        self.rasterizer = Some(crate::rasterizer_interface::RasterizerHandle::from_ref(
            rasterizer,
        ));
        self.puller.bind_rasterizer(rasterizer);
    }

    pub fn bind_subchannel(
        &mut self,
        engine: &mut dyn EngineInterface,
        subchannel_id: u32,
        engine_type: EngineTypes,
    ) {
        self.bind_subchannel_handle(EngineHandle::from_ref(engine), subchannel_id, engine_type);
    }

    /// Pointer-only form of upstream BindSubchannel. No engine state is read
    /// or borrowed: the channel owns the initialized engine for dispatch's lifetime.
    pub(crate) fn bind_subchannel_handle(
        &mut self,
        engine: EngineHandle,
        subchannel_id: u32,
        engine_type: EngineTypes,
    ) {
        let index = subchannel_id as usize;
        if self.subchannels[index].is_none_or(|bound| !bound.ptr_eq(engine))
            || self.subchannel_type[index] != engine_type
        { self.front_engines[index] = None; }
        self.subchannels[subchannel_id as usize] = Some(engine);
        self.subchannel_type[subchannel_id as usize] = engine_type;
    }

    #[cfg(test)]
    pub(crate) fn subchannel_binding_for_test(&self, subchannel_id: usize) -> (bool, EngineTypes) {
        (
            self.subchannels[subchannel_id].is_some(),
            self.subchannel_type[subchannel_id],
        )
    }

    #[cfg(test)]
    pub(crate) fn subchannel_handle_for_test(&self, subchannel: usize) -> Option<EngineHandle> {
        self.subchannels[subchannel]
    }

    /// Push a command list into the DMA pushbuffer queue.
    pub fn push(&mut self, entries: CommandList) {
        self.dma_pushbuffer.push_back(entries);
    }

    /// Dispatch all pending command lists. Matches upstream `DmaPusher::DispatchCalls`.
    pub fn dispatch_calls(&mut self) {
        self.dma_state.is_last_call = true;

        while self.system.is_null() || self.system.get().is_powered_on() {
            if !self.step() {
                break;
            }
        }

        if let Some(split) = crate::gpu_split::current() {
            let end = DeferredListEnd(self.gpu);
            split.defer(move || unsafe { end.run() });
            split.publish();
            return;
        }
        let gpu = unsafe { &*self.gpu };
        // Optional Rust diagnostic zones; upstream flush/end ordering is unchanged.
        {
            let _scope = crate::nx2_profile::Scope::new(crate::nx2_profile::Zone::DmaFlush);
            gpu.flush_commands();
        }
        {
            let _scope = crate::nx2_profile::Scope::new(crate::nx2_profile::Zone::DmaListEnd);
            gpu.on_command_list_end();
        }
    }

    fn update_current_dirty_for_fetch(&mut self, command_gpu_addr: GPUVAddr, word_count: u32) {
        if self.dma_state.method_count == 0 || word_count == 0 {
            return;
        }
        let Some(engine) = self.subchannels[self.dma_state.subchannel as usize] else {
            return;
        };

        let engine_type = self.subchannel_type[self.dma_state.subchannel as usize];
        let kepler_payload = engine_type == EngineTypes::KeplerCompute
            && self.dma_state.method == COMPUTE_INLINE && self.dma_state.non_incrementing;
        let macro_payload = engine_type == EngineTypes::Maxwell3D
            && self.dma_state.method >= MACRO_REGISTERS_START;
        if !kepler_payload && !macro_payload {
            return;
        }
        let words = self.dma_state.method_count.min(word_count);

        if let Some(split) = crate::gpu_split::current() {
            let memory = Arc::clone(&self.memory_manager);
            split.defer_function(Box::new(move || {
                let dirty = memory.lock().is_memory_dirty(command_gpu_addr, words as u64 * 4);
                unsafe { engine.as_mut() }.set_current_dirty(dirty);
            }));
            return;
        }
        let dirty = self.memory_manager.lock().is_memory_dirty(
            command_gpu_addr,
            words as u64 * std::mem::size_of::<u32>() as u64,
        );
        unsafe { engine.as_mut() }.set_current_dirty(dirty);
    }

    /// Process the next step of command submission. Matches upstream `DmaPusher::Step`.
    fn step(&mut self) -> bool {
        if !self.ib_enable || self.dma_pushbuffer.is_empty() {
            return false;
        }
        if !self.puller.resume_acquire() {
            return false;
        }

        let command_list = match self.dma_pushbuffer.front() {
            Some(cl) => cl,
            None => return false,
        };

        if command_list.command_lists.is_empty() && command_list.prefetch_command_list.is_empty() {
            self.dma_pushbuffer.pop_front();
            self.dma_pushbuffer_subindex = 0;
            return true;
        }

        if !command_list.prefetch_command_list.is_empty() {
            let commands = unsafe {
                // `process_commands` mutates DMA state and bound engines, but
                // never the pushbuffer queue. The front command list therefore
                // remains alive and immovable until the upstream-ordered pop
                // immediately after processing.
                std::slice::from_raw_parts(
                    command_list.prefetch_command_list.as_ptr(),
                    command_list.prefetch_command_list.len(),
                )
            };
            self.command_resume_offset += self.process_commands(
                &commands[self.command_resume_offset..], self.command_resume_offset);
            if self.has_pending_acquire() { return false; }
            self.command_resume_offset = 0;
            self.dma_pushbuffer.pop_front();
        } else {
            let command_list_total = command_list.command_lists.len();
            let command_list_header = command_list.command_lists[self.dma_pushbuffer_subindex];
            self.dma_state.dma_get = command_list_header.addr();

            let must_wait_for_sync = self.signal_sync && !*self.sync_state.synced.lock();
            if must_wait_for_sync {
                let _scope = crate::nx2_profile::Scope::new(crate::nx2_profile::Zone::DmaWait);
                let mut synced = self.sync_state.synced.lock();
                while !*synced {
                    self.sync_state.cv.wait(&mut synced);
                }
                self.signal_sync = false;
                *synced = false;
            }

            if command_list_header.size() > 0 {
                let fetch_scope = crate::nx2_profile::Scope::new(crate::nx2_profile::Zone::DmaFetch);
                self.update_current_dirty_for_fetch(
                    command_list_header.addr(),
                    command_list_header.size(),
                );

                let memory_manager = GpuMemoryManagerHandle::new(Arc::clone(&self.memory_manager));
                let mut command_headers = std::mem::take(&mut self.command_headers);
                let flags = if self.should_use_unsafe_read() {
                    GuestMemoryFlags::UNSAFE_READ
                } else {
                    GuestMemoryFlags::SAFE_READ
                };
                let headers = GpuGuestMemory::new_with_backup(
                    &memory_manager,
                    command_list_header.addr(),
                    command_list_header.size() as usize,
                    flags,
                    &mut command_headers,
                );
                let commands = unsafe { headers.as_slice() };
                drop(fetch_scope);
                // Re-read a resumed entry: the CPU may have written its suffix
                // before releasing the semaphore that suspended this channel.
                self.command_resume_offset += self.process_commands(
                    &commands[self.command_resume_offset..], self.command_resume_offset);
                drop(headers);
                self.command_headers = command_headers;
                if self.has_pending_acquire() { return false; }
                self.command_resume_offset = 0;
            }

            self.dma_pushbuffer_subindex += 1;
            if self.dma_pushbuffer_subindex >= command_list_total {
                self.dma_pushbuffer.pop_front();
                self.dma_pushbuffer_subindex = 0;
            } else {
                let next_header = self
                    .dma_pushbuffer
                    .front()
                    .expect("active DMA command list")
                    .command_lists[self.dma_pushbuffer_subindex];
                self.signal_sync =
                    next_header.sync() && *settings::values().sync_memory_operations.get_value();
            }

            if self.signal_sync {
                if let Some(split) = crate::gpu_split::current() { split.drain(); }
                let sync_state = Arc::clone(&self.sync_state);
                let rasterizer = self.rasterizer.expect("DMA rasterizer must be bound");
                unsafe {
                    rasterizer.with_mut(|rasterizer| {
                        rasterizer.signal_fence(Box::new(move || {
                            let mut synced = sync_state.synced.lock();
                            *synced = true;
                            sync_state.cv.notify_all();
                        }));
                    });
                }
            }
        }
        true
    }

    fn should_use_unsafe_read(&self) -> bool {
        let use_safe = {
            let values = settings::values();
            if settings::is_dma_level_default(&values) {
                settings::is_gpu_level_high(&values)
            } else {
                settings::is_dma_level_safe(&values)
            }
        };
        !use_safe
    }

    pub fn has_pending_acquire(&self) -> bool {
        self.puller.has_pending_acquire()
    }

    pub fn can_resume_acquire(&mut self) -> bool {
        self.puller.can_resume_acquire()
    }

    pub fn is_powered_on(&self) -> bool {
        self.system.is_null() || self.system.get().is_powered_on()
    }

    fn process_commands(&mut self, commands: &[CommandHeader], word_offset: usize) -> usize {
        let _scope = crate::nx2_profile::Scope::new(crate::nx2_profile::Zone::DmaDispatch);
        let mut index = 0;
        while index < commands.len() {
            let command_header = commands[index];

            if self.dma_state.method_count > 0 {
                self.dma_state.dma_word_offset = (word_offset + index).wrapping_mul(4) as u64;
                if self.dma_state.non_incrementing {
                    let mut max_write =
                        std::cmp::min(index + self.dma_state.method_count as usize, commands.len())
                            - index;
                    if crate::nx2::G_NX2_AMPERE.load(std::sync::atomic::Ordering::SeqCst)
                        && self.dma_state.method < NON_PULLER_METHODS
                    {
                        max_write = 1;
                    }
                    self.call_multi_method(&commands[index..index + max_write]);
                    self.dma_state.method_count -= max_write as u32;
                    self.dma_state.is_last_call = true;
                    index += max_write;
                    if self.has_pending_acquire() { return index; }
                    continue;
                } else {
                    self.dma_state.is_last_call = self.dma_state.method_count <= 1;
                    self.call_method(command_header.argument());
                }

                if !self.dma_state.non_incrementing {
                    self.dma_state.method += 1;
                }

                if self.dma_increment_once {
                    self.dma_state.non_incrementing = true;
                }

                self.dma_state.method_count -= 1;
            } else {
                match command_header.mode() {
                    Some(SubmissionMode::Increasing) => {
                        self.set_state(&command_header);
                        self.dma_state.non_incrementing = false;
                        self.dma_increment_once = false;
                    }
                    Some(SubmissionMode::NonIncreasing) => {
                        self.set_state(&command_header);
                        self.dma_state.non_incrementing = true;
                        self.dma_increment_once = false;
                    }
                    Some(SubmissionMode::Inline) => {
                        self.dma_state.method = command_header.method();
                        self.dma_state.subchannel = command_header.subchannel();
                        self.dma_state.dma_word_offset =
                            (self.dma_state.dma_get as i64).wrapping_neg() as u64;
                        self.call_method(command_header.arg_count());
                        self.dma_state.non_incrementing = true;
                        self.dma_increment_once = false;
                    }
                    Some(SubmissionMode::IncreaseOnce) => {
                        self.set_state(&command_header);
                        self.dma_state.non_incrementing = false;
                        self.dma_increment_once = true;
                    }
                    _ => {}
                }
            }
            index += 1;
            if self.has_pending_acquire() { return index; }
        }
        commands.len()
    }

    fn set_state(&mut self, command_header: &CommandHeader) {
        self.dma_state.method = command_header.method();
        self.dma_state.subchannel = command_header.subchannel();
        self.dma_state.method_count = command_header.method_count();
    }

    fn wants_segment(&self) -> bool {
        let engine = self.subchannel_type[self.dma_state.subchannel as usize];
        engine == EngineTypes::KeplerCompute
            || (engine == EngineTypes::Maxwell3D && self.dma_state.method >= MACRO_REGISTERS_START)
    }

    fn report_unbound_subchannel(&self) {
        use std::sync::atomic::{AtomicU32, Ordering};
        static REPORTED: AtomicU32 = AtomicU32::new(0);
        if REPORTED.fetch_add(1, Ordering::Relaxed) < 8 {
            log::warn!("Dropping method {:#x} on unbound subchannel {}",
                self.dma_state.method, self.dma_state.subchannel);
        }
    }

    // Mechanical equivalent of upstream's typed engine selection and Front().
    // Native-mode flags are initialized with the channel, before dispatch. Read
    // them once per binding, after draining; subsequent calls use only FrontState.
    fn prepare_front_engine(&mut self, engine: EngineHandle,
        split: &crate::gpu_split::GpuSplitFrontend)
    {
        let index = self.dma_state.subchannel as usize;
        if !split.front_macros() || self.front_engines[index].is_some() { return; }
        let kind = self.subchannel_type[index];
        self.front_engines[index] = Some(match kind {
            EngineTypes::Maxwell3D => {
                split.drain();
                // Binding pairs the concrete channel engine with this type.
                let ptr = unsafe { engine.as_mut() } as *mut dyn EngineInterface
                    as *mut maxwell_3d::Maxwell3D;
                let native = unsafe { &*ptr }.get_native_macro_engine_read(|m| m.native_turing);
                if native {
                    FrontEngine::Maxwell(unsafe { maxwell_3d::Maxwell3D::front(ptr, split) })
                } else { FrontEngine::Backend }
            }
            EngineTypes::KeplerCompute => {
                split.drain();
                let ptr = unsafe { engine.as_mut() } as *mut dyn EngineInterface
                    as *mut kepler_compute::KeplerCompute;
                let native = unsafe { &*ptr }.front_runs_macros();
                if native {
                    FrontEngine::Kepler(unsafe { kepler_compute::KeplerCompute::front(ptr, split) })
                } else { FrontEngine::Backend }
            }
            _ => FrontEngine::Backend,
        });
    }

    /// Dispatch a single method call to an engine. Matches upstream
    /// `DmaPusher::CallMethod`.
    fn call_method(&mut self, argument: u32) {
        if self.dma_state.method < NON_PULLER_METHODS {
            self.puller.call_method(&MethodCall::new(
                self.dma_state.method,
                argument,
                self.dma_state.subchannel,
                self.dma_state.method_count,
            ));
            return;
        }

        let Some(subchannel) = self.subchannels[self.dma_state.subchannel as usize] else {
            self.report_unbound_subchannel();
            return;
        };
        if let Some(split) = crate::gpu_split::current() {
            self.prepare_front_engine(subchannel, &split);
            let state = &self.dma_state;
            let segment = state.dma_get.wrapping_add(state.dma_word_offset);
            match if split.front_macros() { self.front_engines[state.subchannel as usize].as_ref() } else { None } {
                Some(FrontEngine::Maxwell(front)) => {
                    maxwell_3d::Maxwell3D::front_call_method(&mut front.lock(), subchannel,
                        &split, state.method, argument, state.is_last_call);
                }
                Some(FrontEngine::Kepler(front)) => {
                    kepler_compute::KeplerCompute::front_call_method(&mut front.lock(), subchannel,
                        &split, state.method, argument, state.is_last_call, Some(segment));
                }
                _ => split.method(subchannel, state.method, argument, state.is_last_call,
                    self.wants_segment().then_some(segment)),
            }
            return;
        }
        let subchannel = unsafe { subchannel.as_mut() };
        // Upstream reads the public EngineInterface members directly; one
        // interface_state access stands in for that, keeping sink deferral,
        // ConsumeSink-before-segment ordering and dispatch unchanged.
        let sink_pending = {
            let state = subchannel.interface_state_mut();
            if !state.execution_mask[self.dma_state.method as usize] {
                state.method_sink.push((self.dma_state.method, argument));
                return;
            }
            !state.method_sink.is_empty()
        };
        if sink_pending {
            subchannel.consume_sink_impl();
        }
        subchannel.interface_state_mut().current_dma_segment = self
            .dma_state
            .dma_get
            .wrapping_add(self.dma_state.dma_word_offset);
        subchannel.call_method(self.dma_state.method, argument, self.dma_state.is_last_call);
    }

    /// Dispatch a multi-method call to an engine. Matches upstream
    /// `DmaPusher::CallMultiMethod`.
    fn call_multi_method(&mut self, commands: &[CommandHeader]) {
        let args: &[u32] = bytemuck::cast_slice(commands);
        if self.dma_state.method < NON_PULLER_METHODS {
            self.puller.call_multi_method(
                self.dma_state.method,
                self.dma_state.subchannel,
                args,
                args.len() as u32,
                self.dma_state.method_count,
            );
            return;
        }
        let Some(subchannel) = self.subchannels[self.dma_state.subchannel as usize] else {
            self.report_unbound_subchannel();
            return;
        };
        if let Some(split) = crate::gpu_split::current() {
            self.prepare_front_engine(subchannel, &split);
            let state = &self.dma_state;
            let segment = state.dma_get.wrapping_add(state.dma_word_offset);
            match if split.front_macros() { self.front_engines[state.subchannel as usize].as_ref() } else { None } {
                Some(FrontEngine::Maxwell(front)) => {
                    maxwell_3d::Maxwell3D::front_call_multi_method(&mut front.lock(), subchannel,
                        &split, state.method, args, args.len() as u32, state.method_count);
                }
                Some(FrontEngine::Kepler(front)) => {
                    kepler_compute::KeplerCompute::front_call_multi_method(&mut front.lock(), subchannel,
                        &split, state.method, args, args.len() as u32, state.method_count, Some(segment));
                }
                _ => split.multi_method(subchannel, state.method, args, args.len() as u32,
                    state.method_count, self.wants_segment().then_some(segment)),
            }
            return;
        }
        let subchannel = unsafe { subchannel.as_mut() };
        if !subchannel.interface_state().method_sink.is_empty() {
            subchannel.consume_sink_impl();
        }
        subchannel.interface_state_mut().current_dma_segment = self
            .dma_state
            .dma_get
            .wrapping_add(self.dma_state.dma_word_offset);
        subchannel.call_multi_method(
            self.dma_state.method,
            args,
            args.len() as u32,
            self.dma_state.method_count,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::channel_state::ChannelState;
    use crate::host1x::syncpoint_manager::SyncpointManager;
    use crate::renderer_null::null_rasterizer::RasterizerNull;
    use common::settings;
    use common::settings_enums::GpuAccuracy;

    struct RecordingEngine {
        channel_access: Option<std::sync::Weak<Mutex<ChannelState>>>,
        state: crate::engines::engine_interface::EngineInterfaceState,
        calls: Vec<(u32, u32, u64)>,
    }
    impl Default for RecordingEngine {
        fn default() -> Self {
            let mut state = crate::engines::engine_interface::EngineInterfaceState::new();
            // Recorded methods execute immediately, as the old all-true test mask did.
            state.execution_mask.iter_mut().for_each(|flag| *flag = true);
            Self { channel_access: None, state, calls: Vec::new() }
        }
    }
    impl EngineInterface for RecordingEngine {
        fn call_method(&mut self, method: u32, argument: u32, _last: bool) {
            if let Some(channel) = &self.channel_access {
                let channel = channel.upgrade().expect("channel must outlive dispatch");
                assert!(channel.try_lock().is_some(), "DMA must not retain the channel lock while executing commands");
            }
            self.calls.push((method, argument, self.state.current_dma_segment));
        }
        fn call_multi_method(&mut self, method: u32, args: &[u32], amount: u32, _pending: u32) {
            for &argument in &args[..amount as usize] { self.call_method(method, argument, true); }
        }
        fn consume_sink_impl(&mut self) {}
        fn push_method_sink(&mut self, _: u32, _: u32) { panic!("recorded methods execute immediately"); }
        fn interface_state(&self) -> &crate::engines::engine_interface::EngineInterfaceState { &self.state }
        fn interface_state_mut(&mut self) -> &mut crate::engines::engine_interface::EngineInterfaceState { &mut self.state }
    }

    #[test]
    fn split_dma_copies_multi_payloads_and_preserves_compute_segments_across_ring_wrap() {
        let gpu = Box::new(crate::gpu::Gpu::new(false, false));
        let memory = Arc::new(Mutex::new(crate::memory_manager::MemoryManager::new(1)));
        let mut engine = Box::new(RecordingEngine::default());
        let mut dma = DmaPusher::new(&*gpu, SystemRef::null(), memory, std::ptr::null_mut());
        dma.bind_subchannel(&mut *engine, 0, EngineTypes::KeplerCompute);
        let split = crate::gpu_split::GpuSplitFrontend::new(false);
        let scope = split.install();
        split.start_back(|| {});
        dma.dma_state.method = 0x80;
        dma.dma_state.method_count = 20000;
        dma.dma_state.is_last_call = true;
        let mut words = vec![CommandHeader { raw: 0 }; 20000];
        for batch in 0..120u32 {
            dma.dma_state.dma_get = 0x10000 + batch as u64 * 0x100;
            dma.dma_state.dma_word_offset = 12;
            for (i, word) in words.iter_mut().enumerate() { word.raw = batch * 20000 + i as u32; }
            dma.call_multi_method(&words);
            // Producer's reusable pushbuffer can be overwritten immediately.
            words.fill(CommandHeader { raw: u32::MAX });
        }
        dma.dispatch_calls(); // Also publishes the ordered flush/list-end packet.
        split.drain();
        split.stop().unwrap();
        drop(scope);
        assert_eq!(engine.calls.len(), 2400000);
        for (i, &(method, argument, segment)) in engine.calls.iter().enumerate() {
            assert_eq!((method, argument, segment),
                (0x80, i as u32, 0x1000c + (i / 20000) as u64 * 0x100));
        }
    }

    #[test]
    fn dma_front_macro_uploads_switch_concrete_engines_without_repeating_on_backend() {
        let gpu = Box::new(crate::gpu::Gpu::new(false, false));
        let memory = Arc::new(Mutex::new(crate::memory_manager::MemoryManager::new(1)));
        let context = Arc::new(parking_lot::ReentrantMutex::new(std::cell::RefCell::new(
            crate::r#macro::NativeMacroContext::default())));
        context.lock().borrow_mut().compute_engine.native_turing = true;
        let mut graphics = Box::new(maxwell_3d::Maxwell3D::default());
        graphics.set_native_macro_context(Some(context.clone()));
        graphics.set_native_turing_macro_mode(true);
        let mut compute = Box::new(kepler_compute::KeplerCompute::new(memory.clone()));
        compute.set_native_macro_mode(Some(context.clone()), Some(context.clone()));
        let graphics_handle = EngineHandle::from_ref(&*graphics);
        let compute_handle = EngineHandle::from_ref(&*compute);
        let mut dma = DmaPusher::new(&*gpu, SystemRef::null(), memory, std::ptr::null_mut());
        let split = crate::gpu_split::GpuSplitFrontend::new(true);
        let scope = split.install();
        split.start_back(|| {});
        for (handle, kind) in [(graphics_handle, EngineTypes::Maxwell3D),
                                (compute_handle, EngineTypes::KeplerCompute)] {
            dma.bind_subchannel_handle(handle, 0, kind);
            assert!(dma.front_engines[0].is_none());
            dma.dma_state.method = 0x45;
            dma.dma_state.is_last_call = true;
            dma.call_method(2); // Native instruction address: 3 words per instruction.
            assert!(matches!((kind, &dma.front_engines[0]),
                (EngineTypes::Maxwell3D, Some(FrontEngine::Maxwell(_))) |
                (EngineTypes::KeplerCompute, Some(FrontEngine::Kepler(_)))));
            dma.dma_state.method = 0x46;
            dma.dma_state.method_count = 3;
            dma.call_multi_method(&[CommandHeader { raw: 21 },
                CommandHeader { raw: 22 }, CommandHeader { raw: 23 }]);
            dma.bind_subchannel_handle(handle, 0, kind);
            assert!(dma.front_engines[0].is_some(), "same binding retains frontend allocation");
        }
        split.drain();
        split.stop().unwrap();
        drop(scope);
        let guard = context.lock();
        let context = guard.borrow();
        for macro_engine in [&context.engine, &context.compute_engine] {
            assert_eq!(&macro_engine.native_code[6..9], &[21, 22, 23]);
            assert_eq!(macro_engine.native_write_offset, 9);
            assert_eq!(macro_engine.native_code_generation, 3, "backend must not repeat uploads");
        }
    }

    #[test]
    fn unbound_dma_methods_are_dropped_and_segment_selection_matches_upstream() {
        let gpu = crate::gpu::Gpu::new(false, false);
        let memory = Arc::new(Mutex::new(crate::memory_manager::MemoryManager::new(1)));
        let mut dma = DmaPusher::new(&gpu, SystemRef::null(), memory, std::ptr::null_mut());
        dma.dma_state.method = 0x80;
        dma.call_method(42);
        dma.call_multi_method(&[CommandHeader { raw: 43 }]);
        for kind in [EngineTypes::Fermi2D, EngineTypes::Maxwell3D, EngineTypes::KeplerCompute] {
            dma.subchannel_type[0] = kind;
            for method in [0x40, 0xdff, 0xe00, 0xfff] {
                dma.dma_state.method = method;
                assert_eq!(dma.wants_segment(), kind == EngineTypes::KeplerCompute
                    || (kind == EngineTypes::Maxwell3D && method >= 0xe00));
            }
        }
    }

    #[test]
    fn scheduler_dispatch_releases_channel_lock_before_engine_work() {
        let gpu = crate::gpu::Gpu::new(false, false);
        let channel = gpu.create_channel(3);
        let memory = Arc::new(Mutex::new(crate::memory_manager::MemoryManager::new(1)));
        let mut engine = RecordingEngine {
            channel_access: Some(Arc::downgrade(&channel)),
            ..Default::default()
        };
        let rasterizer = RasterizerNull::new(Arc::new(SyncpointManager::new()));
        let mut dma = Box::new(DmaPusher::new(&gpu, SystemRef::null(), memory, std::ptr::null_mut()));
        dma.bind_subchannel(&mut engine, 0, EngineTypes::Fermi2D);
        dma.bind_rasterizer(&rasterizer);
        channel.lock().dma_pusher = Some(Arc::new(Mutex::new(dma)));
        let scheduler = crate::control::scheduler::Scheduler::new();
        scheduler.declare_channel(channel.clone());
        scheduler.push(&gpu, 3, CommandList::from_prefetch([
            CommandHeader { raw: (1 << 29) | (1 << 16) | 0x80 },
            CommandHeader { raw: 42 },
        ]));
        assert_eq!(engine.calls, [(0x80, 42, 4)]); // argument follows the 4-byte header
        // Retire borrowed engine/rasterizer handles before their local owners.
        channel.lock().dma_pusher = None;
    }

    #[test]
    fn nx2_acquire_resumes_prefetch_and_memory_entries_without_replay_or_skip() {
        const CHILD: &str = "RUZU_DMA_ACQUIRE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", std::thread::current().name().unwrap(), "--test-threads=1"])
                .env(CHILD, "1").status().unwrap();
            assert!(status.success());
            return;
        }
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use crate::memory_manager::MemoryManager;
        for nx2 in [false, true] {
            crate::nx2::G_NX2_AMPERE.store(nx2, std::sync::atomic::Ordering::SeqCst);
            for prefetch in [false, true] {
                let gpu = crate::gpu::Gpu::new(false, false);
                let mut backing = vec![0u8; 0x1000];
                let device = Arc::new(MaxwellDeviceMemoryManager::default());
                device.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
                device.smmu_map_with_cpu_backing(0x9000_0000, backing.as_mut_ptr(), 0x5000_0000, backing.len(), 3, true);
                let mut memory = MemoryManager::new_with_geometry_and_device_memory(13, device, 32, 0x1_0000_0000, 16, 12);
                memory.map(0x20000, 0x9000_0000, 0x1000, 0, false);
                let memory = Arc::new(Mutex::new(memory));
                let mut dma = Box::new(DmaPusher::new(&gpu, SystemRef::null(), memory.clone(), std::ptr::null_mut()));
                let mut engine = RecordingEngine::default();
                dma.bind_subchannel(&mut engine, 0, EngineTypes::Fermi2D);
                let rasterizer = RasterizerNull::new(Arc::new(SyncpointManager::new()));
                dma.bind_rasterizer(&rasterizer);
                dma.puller.regs.reg_array[5] = 0x20800;
                dma.puller.regs.reg_array[6] = 7;
                // Two non-incrementing acquire words: stop after the first,
                // keeping the second as data, not a fresh command header.
                let words: Vec<CommandHeader> = [
                    (1 << 29) | (1 << 16) | 0x80, 11,
                    (3 << 29) | (2 << 16) | 7, 1, 1,
                    (1 << 29) | (1 << 16) | 0x81, 22,
                ].into_iter().map(|raw| CommandHeader { raw }).collect();
                if prefetch {
                    dma.push(CommandList::from_prefetch(words));
                } else {
                    memory.lock().write_block_unsafe(0x20000, bytemuck::cast_slice(&words));
                    dma.push(CommandList {
                        command_lists: vec![CommandListHeader { raw: 0 },
                            CommandListHeader { raw: 0x20000 | ((words.len() as u64) << 42) }].into(),
                        prefetch_command_list: SmallVec::new(),
                    });
                }
                dma.dispatch_calls();
                let base = if prefetch { 0 } else { 0x20000 };
                let mut suffix = 22;
                if nx2 {
                    assert!(dma.has_pending_acquire());
                    assert_eq!(dma.command_resume_offset, 4);
                    assert_eq!(dma.dma_state.method_count, 1);
                    assert_eq!(engine.calls, vec![(0x80, 11, base + 4)]);
                    if !prefetch {
                        assert_eq!(dma.dma_pushbuffer_subindex, 1);
                        suffix = 33;
                        memory.lock().write::<u32>(0x20018, suffix);
                    }
                    dma.dispatch_calls(); // Still unmet: no progress or replay.
                    assert_eq!(engine.calls.len(), 1);
                    memory.lock().write::<u32>(0x20800, 7);
                    dma.dispatch_calls();
                }
                assert!(!dma.has_pending_acquire());
                assert!(dma.dma_pushbuffer.is_empty());
                assert_eq!(dma.command_resume_offset, 0);
                assert_eq!(engine.calls, vec![(0x80, 11, base + 4), (0x81, suffix, base + 24)]);
                if nx2 && prefetch {
                    // Shutdown after suspension must not count an already met
                    // acquire as progress when DispatchCalls cannot execute.
                    memory.lock().write::<u32>(0x20800, 0);
                    dma.push(CommandList::from_prefetch([CommandHeader {
                        raw: (4 << 29) | (1 << 16) | 7,
                    }]));
                    dma.dispatch_calls();
                    assert!(dma.has_pending_acquire());
                    memory.lock().write::<u32>(0x20800, 7);
                    let system = ruzu_core::core::System::new();
                    dma.system = SystemRef::from_ref(&system);
                    let channel = gpu.create_channel(3);
                    channel.lock().dma_pusher = Some(Arc::new(Mutex::new(dma)));
                    let scheduler = crate::control::scheduler::Scheduler::new();
                    scheduler.declare_channel(channel.clone());
                    scheduler.push(&gpu, 3, CommandList::default());
                    assert!(scheduler.has_pending_channels());
                    assert!(channel.lock().dma_pusher.as_ref().unwrap().lock().has_pending_acquire());
                }
            }
        }
    }

    #[test]
    fn continuation_dirty_sampling_only_touches_matching_payloads() {
        // Each case overwrites all relevant DMA fields and the dirty flag;
        // reuse the unchanged unmapped address space instead of rebuilding it
        // for every matrix element.
        let memory = Arc::new(Mutex::new(crate::memory_manager::MemoryManager::new(1)));
        let mut channel = Box::new(ChannelState::new(7));
        let mut engine = crate::engines::kepler_compute::KeplerCompute::new(Arc::clone(&memory));
        let mut dma = DmaPusher::new(std::ptr::null(), SystemRef::null(), memory, &mut *channel);
        for engine_type in [EngineTypes::KeplerCompute, EngineTypes::Maxwell3D, EngineTypes::Fermi2D] {
            for method in [COMPUTE_INLINE, MACRO_REGISTERS_START - 1, MACRO_REGISTERS_START] {
                for non_incrementing in [false, true] {
                    for remaining in [0, 1, 20] {
                        for words in [0, 1, 40] {
                            dma.bind_subchannel(&mut engine, 0, engine_type);
                            dma.dma_state.method = method;
                            dma.dma_state.method_count = remaining;
                            dma.dma_state.non_incrementing = non_incrementing;
                            engine.set_current_dirty(true);
                            dma.update_current_dirty_for_fetch(0x10000, words);
                            let sampled = remaining != 0 && words != 0 &&
                                ((engine_type == EngineTypes::KeplerCompute && method == COMPUTE_INLINE && non_incrementing)
                                || (engine_type == EngineTypes::Maxwell3D && method >= MACRO_REGISTERS_START));
                            // Unbound memory is clean: only an actual sample resets the flag.
                            assert_eq!(engine.current_dirty(), !sampled);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn increasing_fermi_command_preserves_pending_arguments_across_every_split() {
        // Upstream ProcessCommands retains method_count/method/subchannel across
        // spans. Parameter words must not become headers at a GPFIFO boundary.
        let mut words = vec![CommandHeader { raw: (1 << 29) | (13 << 16) | (3 << 13) | 0x80 }];
        words.extend((0..13).map(|value| CommandHeader { raw: 0x100 + value }));
        for split in 0..=words.len() {
            let memory = Arc::new(Mutex::new(crate::memory_manager::MemoryManager::new(1)));
            let mut channel = Box::new(ChannelState::new(7));
            let mut engine = crate::engines::fermi_2d::Fermi2D::new(Arc::clone(&memory));
            let mut dma = DmaPusher::new(
                std::ptr::null(), SystemRef::null(), memory, &mut *channel,
            );
            dma.bind_subchannel(&mut engine, 3, EngineTypes::Fermi2D);
            dma.process_commands(&words[..split], 0);
            if split > 0 {
                assert_eq!(dma.dma_state.method_count, (words.len() - split) as u32);
            }
            dma.process_commands(&words[split..], 0);
            assert_eq!(dma.dma_state.method_count, 0);
            assert_eq!(dma.dma_state.method, 0x8d);
            assert_eq!(dma.dma_state.subchannel, 3);
            engine.consume_sink();
            assert_eq!(engine.dst_format(), 0x100);
        }
    }

    #[test]
    fn command_header_scratch_has_upstream_word_layout() {
        assert_eq!(
            std::mem::size_of::<CommandHeader>(),
            std::mem::size_of::<u32>()
        );
        assert_eq!(
            std::mem::align_of::<CommandHeader>(),
            std::mem::align_of::<u32>()
        );

        let mut headers = [CommandHeader::default(); 2];
        bytemuck::cast_slice_mut::<CommandHeader, u8>(&mut headers)
            .copy_from_slice(&[0x78, 0x56, 0x34, 0x12, 0xEF, 0xCD, 0xAB, 0x90]);

        assert_eq!(headers[0].raw, 0x1234_5678);
        assert_eq!(headers[1].raw, 0x90AB_CDEF);
        assert_eq!(headers[0].method_count_(), 0x34_5678);
        assert_eq!(headers[0].method_count(), 0x1234 & 0x1FFF);
    }

    #[test]
    fn command_list_header_has_exact_upstream_bit_layout() {
        let addr = 0xAB_CDEF_0123;
        let size = 0x12_345;
        let header = CommandListHeader {
            raw: addr | (1 << 40) | (1 << 41) | ((size as u64) << 42) | (1 << 63),
        };

        assert_eq!(std::mem::size_of::<CommandListHeader>(), 8);
        assert_eq!(header.addr(), addr);
        assert!(header.allow_flush());
        assert!(header.is_push_buffer());
        assert_eq!(header.size(), size);
        assert!(header.sync());

        let list = CommandList::with_size(3);
        assert_eq!(list.command_lists.len(), 3);
        assert!(list.command_lists.iter().all(|entry| entry.raw == 0));
    }

    #[test]
    fn command_lists_keep_upstream_512_entries_inline() {
        let list = CommandList::with_size(512);
        assert!(!list.command_lists.spilled());
        assert!(!list.prefetch_command_list.spilled());

        let prefetch =
            CommandList::from_prefetch(std::iter::repeat_n(CommandHeader::default(), 512));
        assert!(!prefetch.command_lists.spilled());
        assert!(!prefetch.prefetch_command_list.spilled());
    }

    #[test]
    fn sync_marked_entry_waits_on_fence_signal_before_processing() {
        let mut channel_state = Box::new(ChannelState::new(8));
        let memory_manager = Arc::new(Mutex::new(crate::memory_manager::MemoryManager::new(1)));
        let channel_ptr: *mut ChannelState = &mut *channel_state;
        let mut dma = DmaPusher::new(
            std::ptr::null(),
            SystemRef::null(),
            memory_manager,
            channel_ptr,
        );
        let mut rasterizer = RasterizerNull::new(Arc::new(SyncpointManager::new()));
        dma.bind_rasterizer(&mut rasterizer);

        let (previous_global, previous_use_global) = {
            let values = settings::values();
            (
                *values.sync_memory_operations.get_value_global(),
                values.sync_memory_operations.using_global(),
            )
        };
        {
            let mut values = settings::values_mut();
            values.sync_memory_operations.set_global(true);
            values.sync_memory_operations.set_value(true);
        }

        dma.push(CommandList {
            command_lists: vec![
                CommandListHeader { raw: 0 },
                CommandListHeader { raw: 1 << 63 },
            ]
            .into(),
            prefetch_command_list: SmallVec::new(),
        });

        assert!(dma.step());
        assert!(dma.signal_sync);
        assert!(*dma.sync_state.synced.lock());
        assert_eq!(dma.dma_pushbuffer_subindex, 1);

        *dma.sync_state.synced.lock() = false;
        let sync_state = Arc::clone(&dma.sync_state);
        let delayed_signal = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(25));
            let mut synced = sync_state.synced.lock();
            *synced = true;
            sync_state.cv.notify_all();
        });
        assert!(dma.step());
        delayed_signal.join().unwrap();
        assert!(!dma.signal_sync);
        assert!(!*dma.sync_state.synced.lock());
        assert!(dma.dma_pushbuffer.is_empty());

        let mut values = settings::values_mut();
        values
            .sync_memory_operations
            .setting
            .set_value(previous_global);
        values
            .sync_memory_operations
            .set_global(previous_use_global);
    }

    #[test]
    fn install_self_reference_uses_stable_boxed_address() {
        let mut channel_state = Box::new(ChannelState::new(7));
        let memory_manager = Arc::new(Mutex::new(crate::memory_manager::MemoryManager::new(1)));
        let channel_ptr: *mut ChannelState = &mut *channel_state;
        let mut dma = Box::new(DmaPusher::new(
            std::ptr::null(),
            SystemRef::null(),
            memory_manager,
            channel_ptr,
        ));
        let dma_ptr: *mut DmaPusher = &mut *dma;

        dma.install_self_reference();

        assert_eq!(dma.puller.dma_pusher_ptr_for_test(), dma_ptr);
        assert_eq!(dma.channel_state, channel_ptr);
    }

    #[test]
    fn inline_dma_offset_preserves_signed_minimum_bit_pattern() {
        let mut channel_state = Box::new(ChannelState::new(12));
        let memory_manager = Arc::new(Mutex::new(crate::memory_manager::MemoryManager::new(1)));
        let channel_ptr: *mut ChannelState = &mut *channel_state;
        let mut dma = DmaPusher::new(
            std::ptr::null(),
            SystemRef::null(),
            memory_manager,
            channel_ptr,
        );
        dma.dma_state.dma_get = 0x8000_0000_0000_0000;

        dma.process_commands(&[build_command_header(
            BufferMethods::Nop,
            0,
            SubmissionMode::Inline,
        )], 0);

        assert_eq!(dma.dma_state.dma_word_offset, 0x8000_0000_0000_0000);
    }

    #[test]
    fn should_use_unsafe_read_matches_upstream_gpu_accuracy_branching() {
        let _gpu_accuracy = crate::test_support::GpuAccuracyGuard::set(GpuAccuracy::High);
        let mut channel_state = Box::new(ChannelState::new(9));
        let memory_manager = Arc::new(Mutex::new(crate::memory_manager::MemoryManager::new(1)));
        let channel_ptr: *mut ChannelState = &mut *channel_state;
        let dma = DmaPusher::new(
            std::ptr::null(),
            SystemRef::null(),
            memory_manager,
            channel_ptr,
        );

        assert!(!dma.should_use_unsafe_read());

        {
            let mut values = settings::values_mut();
            values.current_gpu_accuracy = GpuAccuracy::Low;
        }
        assert!(dma.should_use_unsafe_read());
    }

    #[test]
    fn dispatch_calls_stops_when_system_is_not_powered_on() {
        let system = ruzu_core::core::System::new();
        let gpu = crate::gpu::Gpu::new(false, false);
        gpu.set_system_ref(SystemRef::from_ref(&system));
        let mut channel_state = Box::new(ChannelState::new(11));
        let memory_manager = Arc::new(Mutex::new(crate::memory_manager::MemoryManager::new(1)));
        let channel_ptr: *mut ChannelState = &mut *channel_state;
        let mut dma = DmaPusher::new(
            &gpu as *const crate::gpu::Gpu,
            SystemRef::from_ref(&system),
            memory_manager,
            channel_ptr,
        );

        dma.push(CommandList::from_prefetch(vec![CommandHeader {
            raw: build_command_header(BufferMethods::Nop, 0, SubmissionMode::Increasing).raw,
        }]));
        dma.dma_pushbuffer_subindex = 7;
        dma.dispatch_calls();

        assert_eq!(dma.dma_pushbuffer.len(), 1);
        assert_eq!(dma.dma_pushbuffer_subindex, 7);
    }
}

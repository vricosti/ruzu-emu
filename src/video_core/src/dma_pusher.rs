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
#[allow(dead_code)]
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

/// The DmaPusher implements DMA submission to FIFOs.
///
/// The pushbuffers are assembled into a "command stream" of 32-bit words.
/// In the full GPU integration, this holds references to GPU, MemoryManager,
/// and Puller. For now, engine dispatch is performed via callback closures
/// passed through the dispatch chain.
pub struct DmaPusher {
    dma_pushbuffer: VecDeque<CommandList>,
    dma_pushbuffer_subindex: usize,
    dma_state: DmaState,
    dma_increment_once: bool,
    ib_enable: bool,
    command_headers: ScratchBuffer<CommandHeader>,
    subchannels: [Option<EngineHandle>; MAX_SUBCHANNELS],
    subchannel_type: [EngineTypes; MAX_SUBCHANNELS],
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
            dma_state: DmaState::default(),
            dma_increment_once: false,
            ib_enable: true,
            command_headers: ScratchBuffer::new(),
            subchannels: [None; MAX_SUBCHANNELS],
            subchannel_type: [EngineTypes::Maxwell3D; MAX_SUBCHANNELS],
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
        self.subchannels[subchannel_id as usize] = Some(EngineHandle::from_ref(engine));
        self.subchannel_type[subchannel_id as usize] = engine_type;
    }

    #[cfg(test)]
    pub(crate) fn subchannel_binding_for_test(&self, subchannel_id: usize) -> (bool, EngineTypes) {
        (
            self.subchannels[subchannel_id].is_some(),
            self.subchannel_type[subchannel_id],
        )
    }

    /// Push a command list into the DMA pushbuffer queue.
    pub fn push(&mut self, entries: CommandList) {
        self.dma_pushbuffer.push_back(entries);
    }

    /// Dispatch all pending command lists. Matches upstream `DmaPusher::DispatchCalls`.
    pub fn dispatch_calls(&mut self) {
        self.dma_pushbuffer_subindex = 0;
        self.dma_state.is_last_call = true;

        while self.system.is_null() || self.system.get().is_powered_on() {
            if !self.step() {
                break;
            }
        }

        let gpu = unsafe { &*self.gpu };
        gpu.flush_commands();
        gpu.on_command_list_end();
    }

    fn update_current_dirty_for_fetch(&mut self, command_gpu_addr: GPUVAddr, word_count: u32) {
        if self.dma_state.method < MACRO_REGISTERS_START {
            return;
        }
        let Some(engine) = self.subchannels[self.dma_state.subchannel as usize] else {
            return;
        };

        let dirty = self.memory_manager.lock().is_memory_dirty(
            command_gpu_addr,
            word_count as u64 * std::mem::size_of::<u32>() as u64,
        );
        unsafe { engine.as_mut() }.set_current_dirty(dirty);
    }

    /// Process the next step of command submission. Matches upstream `DmaPusher::Step`.
    ///
    /// Without MemoryManager integration, only prefetched command lists can be
    /// processed. GPU-memory-resident command lists are deferred until the memory
    /// manager is wired in.
    fn step(&mut self) -> bool {
        if !self.ib_enable || self.dma_pushbuffer.is_empty() {
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
            self.process_commands(&commands);
            self.dma_pushbuffer.pop_front();
        } else {
            let command_list_total = command_list.command_lists.len();
            let command_list_header = command_list.command_lists[self.dma_pushbuffer_subindex];
            self.dma_state.dma_get = command_list_header.addr();

            let must_wait_for_sync = self.signal_sync && !*self.sync_state.synced.lock();
            if must_wait_for_sync {
                let mut synced = self.sync_state.synced.lock();
                while !*synced {
                    self.sync_state.cv.wait(&mut synced);
                }
                self.signal_sync = false;
                *synced = false;
            }

            if command_list_header.size() > 0 {
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
                self.process_commands(commands);
                drop(headers);
                self.command_headers = command_headers;
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

    fn process_commands(&mut self, commands: &[CommandHeader]) {
        let mut index = 0;
        while index < commands.len() {
            let command_header = commands[index];

            if self.dma_state.method_count > 0 {
                self.dma_state.dma_word_offset = (index as u32).wrapping_mul(4) as u64;
                if self.dma_state.non_incrementing {
                    let max_write =
                        std::cmp::min(index + self.dma_state.method_count as usize, commands.len())
                            - index;
                    self.dispatch_multi_method(&commands[index..index + max_write]);
                    self.dma_state.method_count -= max_write as u32;
                    self.dma_state.is_last_call = true;
                    index += max_write;
                    continue;
                } else {
                    self.dma_state.is_last_call = self.dma_state.method_count <= 1;
                    self.dispatch_method(command_header.argument());
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
                        self.dispatch_method(command_header.arg_count());
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
        }
    }

    fn set_state(&mut self, command_header: &CommandHeader) {
        self.dma_state.method = command_header.method();
        self.dma_state.subchannel = command_header.subchannel();
        self.dma_state.method_count = command_header.method_count();
    }

    /// Dispatch a single method call to an engine. Matches upstream
    /// `DmaPusher::CallMethod`.
    fn dispatch_method(&mut self, argument: u32) {
        if self.dma_state.method < NON_PULLER_METHODS {
            self.puller.call_method(&MethodCall::new(
                self.dma_state.method,
                argument,
                self.dma_state.subchannel,
                self.dma_state.method_count,
            ));
            return;
        }

        let subchannel = self.subchannels[self.dma_state.subchannel as usize]
            .expect("DMA method requires a bound subchannel");
        let subchannel = unsafe { subchannel.as_mut() };
        if !subchannel.execution_mask()[self.dma_state.method as usize] {
            subchannel.push_method_sink(self.dma_state.method, argument);
            return;
        }
        subchannel.consume_sink();
        subchannel.set_current_dma_segment(
            self.dma_state
                .dma_get
                .wrapping_add(self.dma_state.dma_word_offset),
        );
        subchannel.call_method(self.dma_state.method, argument, self.dma_state.is_last_call);
    }

    /// Dispatch a multi-method call to an engine. Matches upstream
    /// `DmaPusher::CallMultiMethod`.
    fn dispatch_multi_method(&mut self, commands: &[CommandHeader]) {
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
        let subchannel = self.subchannels[self.dma_state.subchannel as usize]
            .expect("DMA multi-method requires a bound subchannel");
        let subchannel = unsafe { subchannel.as_mut() };
        subchannel.consume_sink();
        subchannel.set_current_dma_segment(
            self.dma_state
                .dma_get
                .wrapping_add(self.dma_state.dma_word_offset),
        );
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
        )]);

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
        assert_eq!(dma.dma_pushbuffer_subindex, 0);
    }
}

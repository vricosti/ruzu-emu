// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of video_core/engines/puller.h and puller.cpp
//!
//! The Puller is the front-end GPU command processor. It reads methods from
//! the GPFIFO command stream and dispatches them either to built-in puller
//! methods (semaphores, fences, sync points) or to bound engine subchannels.

use crate::control::channel_state::ChannelState;
use crate::engines::engine_interface::{EngineHandle, EngineInterface};
use crate::engines::engine_interface::EngineTypes;
use crate::query_cache::types::{QueryPropertiesFlags, QueryType};
use crate::rasterizer_interface::{RasterizerHandle, RasterizerInterface};
use std::sync::Arc;

// Port of puller.cpp's anonymous OnBack/DrainBack helpers. Only capture
// immutable command arguments and the non-owning rasterizer handle, never the
// frontend-owned Puller. The renderer must outlive the joined backend worker.
fn on_back<F>(handle: Option<RasterizerHandle>, func: F)
where F: Fn(&mut dyn RasterizerInterface) + Copy + Send + 'static {
    let Some(handle) = handle else { return; };
    let call = move || unsafe { handle.with_mut(func) };
    if let Some(split) = crate::gpu_split::current() {
        split.defer(call);
    } else {
        call();
    }
}

fn drain_back() {
    if let Some(split) = crate::gpu_split::current() { split.drain(); }
}

const GF100_BIND_CLASS_MASK: u32 = 0xFFFF;
const GF100_BIND_VALID_MASK: u32 = 0x1F0000 | GF100_BIND_CLASS_MASK;

/// GPU virtual address type.
pub type GPUVAddr = u64;

// ── Engine ID ───────────────────────────────────────────────────────────────

/// GPU engine class identifiers used during subchannel binding.
///
/// Corresponds to the C++ `Tegra::EngineID` enum.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct EngineID(u32);

#[allow(non_upper_case_globals)]
impl EngineID {
    pub const Nv01Timer: Self = Self(0x0004);
    pub const FermiTwodA: Self = Self(0x902D);
    pub const MaxwellB: Self = Self(0xB197);
    pub const KeplerComputeB: Self = Self(0xB1C0);
    pub const KeplerInlineToMemoryB: Self = Self(0xA140);
    pub const MaxwellDmaCopyA: Self = Self(0xB0B5);

    /// Convert a raw u32 value to an EngineID.
    ///
    /// Upstream stores `static_cast<EngineID>(method_call.argument)` in the
    /// fixed subchannel array even when the value is not one of the known
    /// engine classes. Preserve that raw value instead of using `Option`.
    pub const fn from_raw(value: u32) -> Self {
        Self(value)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl Default for EngineID {
    fn default() -> Self {
        Self(0)
    }
}

impl std::fmt::Debug for EngineID {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Nv01Timer => f.write_str("Nv01Timer"),
            Self::FermiTwodA => f.write_str("FermiTwodA"),
            Self::MaxwellB => f.write_str("MaxwellB"),
            Self::KeplerComputeB => f.write_str("KeplerComputeB"),
            Self::KeplerInlineToMemoryB => f.write_str("KeplerInlineToMemoryB"),
            Self::MaxwellDmaCopyA => f.write_str("MaxwellDmaCopyA"),
            _ => write!(f, "EngineID(0x{:04X})", self.0),
        }
    }
}

// ── Buffer methods (puller command IDs) ─────────────────────────────────────

/// Well-known puller method indices.
///
/// Corresponds to the C++ `BufferMethods` enum from `dma_pusher.h`, used by
/// the puller to decide whether a method should be handled locally or
/// dispatched to a bound engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

impl BufferMethods {
    pub fn from_raw(v: u32) -> Option<Self> {
        match v {
            0x0 => Some(Self::BindObject),
            0x1 => Some(Self::Illegal),
            0x2 => Some(Self::Nop),
            0x4 => Some(Self::SemaphoreAddressHigh),
            0x5 => Some(Self::SemaphoreAddressLow),
            0x6 => Some(Self::SemaphoreSequencePayload),
            0x7 => Some(Self::SemaphoreOperation),
            0x8 => Some(Self::NonStallInterrupt),
            0x9 => Some(Self::WrcacheFlush),
            0xA => Some(Self::MemOpA),
            0xB => Some(Self::MemOpB),
            0xC => Some(Self::MemOpC),
            0xD => Some(Self::MemOpD),
            0x14 => Some(Self::RefCnt),
            0x1A => Some(Self::SemaphoreAcquire),
            0x1B => Some(Self::SemaphoreRelease),
            0x1C => Some(Self::SyncpointPayload),
            0x1D => Some(Self::SyncpointOperation),
            0x1E => Some(Self::WaitForIdle),
            0x1F => Some(Self::CrcCheck),
            0x20 => Some(Self::Yield),
            v if v >= 0x40 => Some(Self::NonPullerMethods),
            _ => None,
        }
    }
}

// ── Puller types ────────────────────────────────────────────────────────────

/// A single GPU method call, as decoded from the GPFIFO stream.
///
/// Corresponds to `Puller::MethodCall`.
#[derive(Debug, Clone, Copy)]
pub struct MethodCall {
    pub method: u32,
    pub argument: u32,
    pub subchannel: u32,
    pub method_count: u32,
}

impl MethodCall {
    pub fn new(method: u32, argument: u32, subchannel: u32, method_count: u32) -> Self {
        Self {
            method,
            argument,
            subchannel,
            method_count,
        }
    }

    /// Whether this is the last call in a multi-method sequence.
    pub fn is_last_call(&self) -> bool {
        self.method_count <= 1
    }
}

/// Fence operation type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FenceOperation {
    Acquire = 0,
    Increment = 1,
}

/// Packed fence action register value.
#[derive(Debug, Clone, Copy, Default)]
pub struct FenceAction {
    pub raw: u32,
}

impl FenceAction {
    /// Fence operation (bit 0).
    pub fn op(&self) -> FenceOperation {
        if (self.raw & 1) == 0 {
            FenceOperation::Acquire
        } else {
            FenceOperation::Increment
        }
    }

    /// Syncpoint ID (bits [8:31]).
    pub fn syncpoint_id(&self) -> u32 {
        (self.raw >> 8) & 0x00FF_FFFF
    }
}

/// GPU semaphore operation types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum GpuSemaphoreOperation {
    AcquireEqual = 0x1,
    WriteLong = 0x2,
    AcquireGequal = 0x4,
    AcquireMask = 0x8,
}

// ── Puller register file ────────────────────────────────────────────────────

/// Number of registers in the puller register file.
///
/// Upstream: `Puller::Regs::NUM_REGS = 0x800`.
const PULLER_NUM_REGS: usize = 0x800;

/// Puller register file.
///
/// Corresponds to the C++ `Puller::Regs` inner struct.
/// Register positions verified by upstream `ASSERT_REG_POSITION` macros:
///   semaphore_address  = 0x04
///   semaphore_sequence = 0x06
///   semaphore_trigger  = 0x07
///   reference_count    = 0x14
///   semaphore_acquire  = 0x1A
///   semaphore_release  = 0x1B
///   fence_value        = 0x1C
///   fence_action       = 0x1D
///   acquire_mode       = 0x100 (in the outer 0x800 space)
///   acquire_source     = 0x101
///   acquire_active     = 0x102
///   acquire_timeout    = 0x103
///   acquire_value      = 0x104
#[derive(Clone)]
pub struct PullerRegs {
    pub reg_array: [u32; PULLER_NUM_REGS],
}

impl Default for PullerRegs {
    fn default() -> Self {
        Self {
            reg_array: [0u32; PULLER_NUM_REGS],
        }
    }
}

impl PullerRegs {
    // ── Typed accessors ─────────────────────────────────────────────────

    pub fn semaphore_address_high(&self) -> u32 {
        self.reg_array[0x04]
    }

    pub fn semaphore_address_low(&self) -> u32 {
        self.reg_array[0x05]
    }

    pub fn semaphore_address(&self) -> GPUVAddr {
        ((self.semaphore_address_high() as u64) << 32) | (self.semaphore_address_low() as u64)
    }

    pub fn semaphore_sequence(&self) -> u32 {
        self.reg_array[0x06]
    }

    pub fn semaphore_trigger(&self) -> u32 {
        self.reg_array[0x07]
    }

    pub fn reference_count(&self) -> u32 {
        self.reg_array[0x14]
    }

    pub fn semaphore_acquire(&self) -> u32 {
        self.reg_array[0x1A]
    }

    pub fn semaphore_release(&self) -> u32 {
        self.reg_array[0x1B]
    }

    pub fn fence_value(&self) -> u32 {
        self.reg_array[0x1C]
    }

    pub fn fence_action(&self) -> FenceAction {
        FenceAction {
            raw: self.reg_array[0x1D],
        }
    }

    pub fn acquire_mode(&self) -> u32 {
        self.reg_array[0x100]
    }

    pub fn set_acquire_mode(&mut self, value: u32) {
        self.reg_array[0x100] = value;
    }

    pub fn acquire_source(&self) -> u32 {
        self.reg_array[0x101]
    }

    pub fn set_acquire_source(&mut self, value: u32) {
        self.reg_array[0x101] = value;
    }

    pub fn acquire_active(&self) -> u32 {
        self.reg_array[0x102]
    }

    pub fn set_acquire_active(&mut self, value: u32) {
        self.reg_array[0x102] = value;
    }

    pub fn acquire_timeout(&self) -> u32 {
        self.reg_array[0x103]
    }

    pub fn set_acquire_timeout(&mut self, value: u32) {
        self.reg_array[0x103] = value;
    }

    pub fn acquire_value(&self) -> u32 {
        self.reg_array[0x104]
    }

    pub fn set_acquire_value(&mut self, value: u32) {
        self.reg_array[0x104] = value;
    }
}

// ── Puller engine ───────────────────────────────────────────────────────────

/// Number of subchannels.
const NUM_SUBCHANNELS: usize = 8;

/// Corresponds to Puller::PendingAcquire in puller.h.
#[derive(Clone, Copy)]
struct PendingAcquire {
    address: u64,
    value: u32,
    operation: u32,
    since: std::time::Instant,
    reported: bool,
}

impl PendingAcquire {
    fn matches(&self, current: u32) -> bool {
        match self.operation {
            0x1 => current == self.value,
            0x4 => current >= self.value,
            0x8 => current & self.value != 0,
            _ => true,
        }
    }
}

/// The GPU command puller, corresponding to the C++ Puller class.
pub struct Puller {
    pub regs: PullerRegs,
    /// Mapping of subchannels to bound engine IDs.
    pub bound_engines: [EngineID; NUM_SUBCHANNELS],
    memory_manager: Arc<parking_lot::Mutex<crate::memory_manager::MemoryManager>>,
    dma_pusher: *mut crate::dma_pusher::DmaPusher,
    rasterizer: Option<RasterizerHandle>,
    channel_state: *mut ChannelState,
    // Upstream takes stable payload engine addresses during SetObject. Cache
    // those non-owning handles before dispatch so binding never borrows an
    // engine that a split backend may currently be mutating.
    engine_handles: [Option<EngineHandle>; 6],
    pending_acquire: Option<PendingAcquire>,
    // Keep the registered identity for Drop even if a standalone Rust puller
    // moves. Production pullers live inside the channel's boxed DmaPusher.
    waiter_owner: usize,
}

impl Drop for Puller {
    fn drop(&mut self) {
        crate::nx2::Nx2SemaphoreWaiters::get().remove(self.waiter_owner);
    }
}

impl Puller {
    pub fn new(
        memory_manager: Arc<parking_lot::Mutex<crate::memory_manager::MemoryManager>>,
        dma_pusher: *mut crate::dma_pusher::DmaPusher,
        channel_state: *mut ChannelState,
    ) -> Self {
        // Production constructs Puller after all six boxed engines and before
        // any channel commands can run. Channel lifetime covers these handles;
        // replacing payload engines requires constructing a new DMA/Puller.
        let engine_handles = unsafe { channel_state.as_ref() }.map(|channel| [
            channel.nv01_timer.as_ref().map(|engine| EngineHandle::from_ref(engine.as_ref())),
            channel.kepler_compute.as_ref().map(|engine| EngineHandle::from_ref(engine.as_ref())),
            channel.maxwell_3d.as_ref().map(|engine| EngineHandle::from_ref(engine.as_ref())),
            channel.fermi_2d.as_ref().map(|engine| EngineHandle::from_ref(engine.as_ref())),
            channel.maxwell_dma.as_ref().map(|engine| EngineHandle::from_ref(engine.as_ref())),
            channel.kepler_memory.as_ref().map(|engine| EngineHandle::from_ref(engine.as_ref())),
        ]).unwrap_or([None; 6]);
        Self {
            regs: PullerRegs::default(),
            bound_engines: [EngineID::default(); NUM_SUBCHANNELS],
            memory_manager,
            dma_pusher,
            rasterizer: None,
            channel_state,
            engine_handles,
            pending_acquire: None,
            waiter_owner: 0,
        }
    }

    pub fn has_pending_acquire(&self) -> bool {
        self.pending_acquire.is_some()
    }

    pub fn can_resume_acquire(&mut self) -> bool {
        let Some(acquire) = self.pending_acquire else { return true; };
        drain_back();
        let current = self.read_semaphore_word(acquire.address);
        if crate::nx2::Nx2SemaphoreWaiters::get().latched(self.waiter_owner)
            || acquire.matches(current)
        {
            return true;
        }
        if !acquire.reported && acquire.since.elapsed() > std::time::Duration::from_secs(2) {
            self.pending_acquire.as_mut().unwrap().reported = true;
            log::warn!("Semaphore {:#x} waits for 2 s: operation {}, expected {:#x}, current {:#x}",
                acquire.address, acquire.operation, acquire.value, current);
        }
        false
    }

    pub fn resume_acquire(&mut self) -> bool {
        if !self.can_resume_acquire() { return false; }
        self.pending_acquire = None;
        crate::nx2::Nx2SemaphoreWaiters::get().remove(self.waiter_owner);
        self.waiter_owner = 0;
        true
    }

    /// Bind a rasterizer interface to this puller.
    ///
    /// Corresponds to `Puller::BindRasterizer`.
    pub fn bind_rasterizer(&mut self, rasterizer: &dyn RasterizerInterface) {
        self.rasterizer = Some(RasterizerHandle::from_ref(rasterizer));
    }

    fn log_unimplemented_engine(
        &self,
        caller: &str,
        engine: EngineID,
        subchannel: u32,
        method: u32,
        argument: Option<u32>,
        amount: Option<u32>,
        methods_pending: Option<u32>,
    ) {
        log::error!(
            "{} unsupported engine 0x{:04X} on subchannel {} method=0x{:X} argument={:?} amount={:?} methods_pending={:?}",
            caller,
            engine.raw(),
            subchannel,
            method,
            argument,
            amount,
            methods_pending,
        );
    }

    pub fn channel_state_ptr(&self) -> *mut ChannelState {
        self.channel_state
    }

    pub fn set_dma_pusher(&mut self, dma_pusher: *mut crate::dma_pusher::DmaPusher) {
        self.dma_pusher = dma_pusher;
    }

    #[cfg(test)]
    pub(crate) fn dma_pusher_ptr_for_test(&self) -> *mut crate::dma_pusher::DmaPusher {
        self.dma_pusher
    }

    fn with_rasterizer_mut(&mut self, func: impl FnOnce(&mut dyn RasterizerInterface)) {
        let Some(handle) = self.rasterizer else {
            return;
        };
        unsafe { handle.with_mut(func) };
    }

    fn read_gpu_u32(&self, gpu_va: u64) -> u32 {
        self.memory_manager.lock().read::<u32>(gpu_va)
    }

    /// Port of puller.cpp's ReadSemaphoreWord, owned here with the acquire path.
    fn read_semaphore_word(&mut self, gpu_va: u64) -> u32 {
        // Release the memory-manager guard before entering the rasterizer:
        // downloading the buffer may access the same manager again.
        let device = self.memory_manager.lock().gpu_to_cpu_address(gpu_va);
        if let Some(device) = device {
            self.with_rasterizer_mut(|rasterizer| {
                let which = crate::cache_types::CacheType::BUFFER_CACHE;
                if rasterizer.must_flush_region(device, 4, which) {
                    rasterizer.flush_region(device, 4, which);
                }
            });
        }
        self.memory_manager.lock().read::<u32>(gpu_va)
    }

    fn write_gpu_u32(&self, gpu_va: u64, value: u32) {
        let bytes = value.to_le_bytes();
        self.write_gpu_bytes(gpu_va, &bytes);
    }

    fn write_gpu_bytes(&self, gpu_va: u64, data: &[u8]) {
        self.memory_manager.lock().write_block_unsafe(gpu_va, data);
    }

    /// Determine whether a method should be dispatched to an engine (true)
    /// or handled by the puller itself (false).
    ///
    /// Corresponds to `Puller::ExecuteMethodOnEngine`.
    pub fn execute_method_on_engine(method: u32) -> bool {
        method >= BufferMethods::NonPullerMethods as u32
    }

    /// Process a single GPU method call.
    ///
    /// Corresponds to `Puller::CallMethod`.
    pub fn call_method(&mut self, method_call: &MethodCall) {
        assert!((method_call.subchannel as usize) < NUM_SUBCHANNELS);

        if Self::execute_method_on_engine(method_call.method) {
            self.call_engine_method(method_call);
        } else {
            self.call_puller_method(method_call);
        }
    }

    /// Process multiple GPU method calls.
    ///
    /// Corresponds to `Puller::CallMultiMethod`.
    pub fn call_multi_method(
        &mut self,
        method: u32,
        subchannel: u32,
        base_start: &[u32],
        amount: u32,
        methods_pending: u32,
    ) {
        log::trace!(
            "Processing method {:08X} on subchannel {}",
            method,
            subchannel
        );

        assert!((subchannel as usize) < NUM_SUBCHANNELS);

        if Self::execute_method_on_engine(method) {
            self.call_engine_multi_method(method, subchannel, base_start, amount, methods_pending);
        } else {
            for i in 0..amount {
                self.call_puller_method(&MethodCall::new(
                    method,
                    base_start[i as usize],
                    subchannel,
                    methods_pending - i,
                ));
            }
        }
    }

    /// Handle a puller-level method.
    ///
    /// Corresponds to `Puller::CallPullerMethod`.
    pub fn call_puller_method(&mut self, method_call: &MethodCall) {
        let method_idx = method_call.method as usize;
        if method_idx < PULLER_NUM_REGS {
            self.regs.reg_array[method_idx] = method_call.argument;
        }

        match BufferMethods::from_raw(method_call.method) {
            Some(BufferMethods::BindObject) => {
                self.process_bind_method(method_call);
            }
            Some(BufferMethods::Nop)
            | Some(BufferMethods::SemaphoreAddressHigh)
            | Some(BufferMethods::SemaphoreAddressLow)
            | Some(BufferMethods::SemaphoreSequencePayload)
            | Some(BufferMethods::SyncpointPayload)
            | Some(BufferMethods::WrcacheFlush) => {
                // No-op for these methods
            }
            Some(BufferMethods::RefCnt) => {
                on_back(self.rasterizer, move |rasterizer| rasterizer.signal_reference());
            }
            Some(BufferMethods::SyncpointOperation) => {
                self.process_fence_action_method();
            }
            Some(BufferMethods::WaitForIdle) => {
                on_back(self.rasterizer, move |rasterizer| rasterizer.wait_for_idle());
            }
            Some(BufferMethods::SemaphoreOperation) => {
                self.process_semaphore_trigger_method();
            }
            Some(BufferMethods::NonStallInterrupt) => {
                log::error!("Special puller engine method NonStallInterrupt not implemented");
            }
            Some(BufferMethods::MemOpA) => {
                log::trace!("Memory Operation A");
            }
            Some(BufferMethods::MemOpB) => {
                on_back(self.rasterizer, move |rasterizer| rasterizer.invalidate_gpu_cache());
            }
            Some(BufferMethods::MemOpC) | Some(BufferMethods::MemOpD) => {
                log::trace!("Memory Operation C,D");
            }
            Some(BufferMethods::SemaphoreAcquire) => {
                // Ampere uses 0x17..0x1A as address/payload words, not an acquire command.
                if !crate::nx2::G_NX2_AMPERE.load(std::sync::atomic::Ordering::SeqCst) {
                    self.process_semaphore_acquire();
                }
            }
            Some(BufferMethods::SemaphoreRelease) => {
                if crate::nx2::G_NX2_AMPERE.load(std::sync::atomic::Ordering::SeqCst) {
                    self.process_semaphore_execute(method_call.argument);
                } else {
                    self.process_semaphore_release();
                }
            }
            Some(BufferMethods::Yield) => {
                log::error!("Special puller engine method Yield not implemented");
            }
            _ => {
                log::error!(
                    "Special puller engine method {:X} not implemented",
                    method_call.method
                );
            }
        }
    }

    /// Dispatch a method call to the bound engine.
    ///
    /// Corresponds to `Puller::CallEngineMethod`.
    /// Stubbed — requires access to channel_state engines to look up the bound engine for
    /// the subchannel and call engine->CallMethod(method_call).
    /// Upstream: Puller::CallEngineMethod() in video_core/engines/puller.cpp
    pub fn call_engine_method(&mut self, method_call: &MethodCall) {
        let engine = self.bound_engines[method_call.subchannel as usize];

        match engine {
            EngineID::MaxwellB => {
                let channel_state = unsafe { &mut *self.channel_state };
                if let Some(engine) = channel_state.maxwell_3d.as_mut() {
                    engine.call_method(
                        method_call.method,
                        method_call.argument,
                        method_call.is_last_call(),
                    );
                } else {
                    use std::sync::atomic::{AtomicU32, Ordering};
                    static DROP_COUNT: AtomicU32 = AtomicU32::new(0);
                    let c = DROP_COUNT.fetch_add(1, Ordering::Relaxed);
                    if c < 5 {
                        log::warn!(
                            "Puller: MaxwellB method {:#x} DROPPED (maxwell_3d is None)",
                            method_call.method
                        );
                    }
                }
            }
            EngineID::KeplerInlineToMemoryB => {
                let channel_state = unsafe { &mut *self.channel_state };
                if let Some(engine) = channel_state.kepler_memory.as_mut() {
                    engine.call_method(
                        method_call.method,
                        method_call.argument,
                        method_call.is_last_call(),
                    );
                }
            }
            EngineID::FermiTwodA => {
                let channel_state = unsafe { &mut *self.channel_state };
                if let Some(engine) = channel_state.fermi_2d.as_mut() {
                    engine.call_method(
                        method_call.method,
                        method_call.argument,
                        method_call.is_last_call(),
                    );
                }
            }
            EngineID::KeplerComputeB => {
                let channel_state = unsafe { &mut *self.channel_state };
                if let Some(engine) = channel_state.kepler_compute.as_mut() {
                    engine.call_method(
                        method_call.method,
                        method_call.argument,
                        method_call.is_last_call(),
                    );
                }
            }
            EngineID::MaxwellDmaCopyA => {
                let channel_state = unsafe { &mut *self.channel_state };
                if let Some(engine) = channel_state.maxwell_dma.as_mut() {
                    engine.call_method(
                        method_call.method,
                        method_call.argument,
                        method_call.is_last_call(),
                    );
                }
            }
            EngineID::Nv01Timer => {
                let channel_state = unsafe { &mut *self.channel_state };
                if let Some(engine) = channel_state.nv01_timer.as_mut() {
                    engine.call_method(
                        method_call.method,
                        method_call.argument,
                        method_call.is_last_call(),
                    );
                }
            }
            _ => {
                self.log_unimplemented_engine(
                    "Puller::CallEngineMethod",
                    engine,
                    method_call.subchannel,
                    method_call.method,
                    Some(method_call.argument),
                    None,
                    Some(method_call.method_count),
                );
            }
        }
    }

    /// Dispatch a multi-method call to the bound engine.
    ///
    /// Corresponds to `Puller::CallEngineMultiMethod`.
    pub fn call_engine_multi_method(
        &mut self,
        method: u32,
        subchannel: u32,
        base_start: &[u32],
        amount: u32,
        methods_pending: u32,
    ) {
        let engine = self.bound_engines[subchannel as usize];

        match engine {
            EngineID::MaxwellB => {
                let channel_state = unsafe { &mut *self.channel_state };
                if let Some(engine) = channel_state.maxwell_3d.as_mut() {
                    engine.call_multi_method(method, base_start, amount, methods_pending);
                }
            }
            EngineID::KeplerInlineToMemoryB => {
                let channel_state = unsafe { &mut *self.channel_state };
                if let Some(engine) = channel_state.kepler_memory.as_mut() {
                    engine.call_multi_method(method, base_start, amount, methods_pending);
                }
            }
            EngineID::FermiTwodA => {
                let channel_state = unsafe { &mut *self.channel_state };
                if let Some(engine) = channel_state.fermi_2d.as_mut() {
                    engine.call_multi_method(method, base_start, amount, methods_pending);
                }
            }
            EngineID::KeplerComputeB => {
                let channel_state = unsafe { &mut *self.channel_state };
                if let Some(engine) = channel_state.kepler_compute.as_mut() {
                    engine.call_multi_method(method, base_start, amount, methods_pending);
                }
            }
            EngineID::MaxwellDmaCopyA => {
                let channel_state = unsafe { &mut *self.channel_state };
                if let Some(engine) = channel_state.maxwell_dma.as_mut() {
                    engine.call_multi_method(method, base_start, amount, methods_pending);
                }
            }
            EngineID::Nv01Timer => {
                let channel_state = unsafe { &mut *self.channel_state };
                if let Some(engine) = channel_state.nv01_timer.as_mut() {
                    engine.call_multi_method(method, base_start, amount, methods_pending);
                }
            }
            _ => {
                self.log_unimplemented_engine(
                    "Puller::CallEngineMultiMethod",
                    engine,
                    subchannel,
                    method,
                    None,
                    Some(amount),
                    Some(methods_pending),
                );
            }
        }
    }

    // ── Private helpers ─────────────────────────────────────────────────

    /// Bind a subchannel to an engine.
    ///
    /// Corresponds to `Puller::ProcessBindMethod`.
    fn process_bind_method(&mut self, method_call: &MethodCall) {
        log::debug!(
            "Binding subchannel {} to engine {}",
            method_call.subchannel,
            method_call.argument
        );
        let mut engine = method_call.argument;
        if (engine & !GF100_BIND_CLASS_MASK) != 0 && (engine & !GF100_BIND_VALID_MASK) == 0 {
            engine &= GF100_BIND_CLASS_MASK;
        }
        // NX2 binds Ampere classes to the corresponding existing engines.
        match engine & 0xffff {
            0xc797 => engine = EngineID::MaxwellB.raw(),
            0xc7c0 => engine = EngineID::KeplerComputeB.raw(),
            0xc7b5 => engine = EngineID::MaxwellDmaCopyA.raw(),
            0 if crate::nx2::G_NX2_AMPERE.load(std::sync::atomic::Ordering::SeqCst) => {
                log::debug!("Switch 2 unbinds subchannel {}", method_call.subchannel);
                return;
            }
            _ => {}
        }
        let eid = EngineID::from_raw(engine);
        self.bound_engines[method_call.subchannel as usize] = eid;
        match eid {
            EngineID::FermiTwodA
            | EngineID::MaxwellB
            | EngineID::KeplerComputeB
            | EngineID::KeplerInlineToMemoryB
            | EngineID::MaxwellDmaCopyA
            | EngineID::Nv01Timer => {
                let engine_type = match eid {
                    EngineID::FermiTwodA => EngineTypes::Fermi2D,
                    EngineID::MaxwellB => EngineTypes::Maxwell3D,
                    EngineID::KeplerComputeB => EngineTypes::KeplerCompute,
                    EngineID::KeplerInlineToMemoryB => EngineTypes::KeplerMemory,
                    EngineID::MaxwellDmaCopyA => EngineTypes::MaxwellDMA,
                    EngineID::Nv01Timer => EngineTypes::Nv01Timer,
                    _ => unreachable!("known engine ids are handled by the outer match"),
                };
                if let (Some(engine), Some(dma)) = (
                    self.engine_handles[engine_type as usize],
                    unsafe { self.dma_pusher.as_mut() },
                ) {
                    dma.bind_subchannel_handle(engine, method_call.subchannel, engine_type);
                }
            }
            _ => {
                self.log_unimplemented_engine(
                    "Puller::ProcessBindMethod",
                    eid,
                    method_call.subchannel,
                    method_call.method,
                    Some(method_call.argument),
                    None,
                    Some(method_call.method_count),
                );
            }
        }
    }

    /// Process a fence action (acquire or increment).
    ///
    /// Corresponds to `Puller::ProcessFenceActionMethod`.
    fn process_fence_action_method(&mut self) {
        let action = self.regs.fence_action();
        match action.op() {
            FenceOperation::Acquire => {
                on_back(self.rasterizer, |rasterizer| rasterizer.release_fences(true));
            }
            FenceOperation::Increment => {
                let syncpoint_id = action.syncpoint_id();
                on_back(self.rasterizer, move |rasterizer| rasterizer.signal_sync_point(syncpoint_id));
            }
        }
    }

    /// Process a semaphore trigger.
    ///
    /// Corresponds to `Puller::ProcessSemaphoreTriggerMethod`.
    fn process_semaphore_trigger_method(&mut self) {
        let semaphore_op_mask = 0xF;
        let op = self.regs.semaphore_trigger() & semaphore_op_mask;
        if op == GpuSemaphoreOperation::WriteLong as u32 {
            let sequence_address = self.regs.semaphore_address();
            let payload = self.regs.semaphore_sequence();
            let nx2 = crate::nx2::G_NX2_AMPERE.load(std::sync::atomic::Ordering::SeqCst);
            // RELEASE_SIZE selects a four-byte payload on NX2. A timestamp
            // release here would overwrite adjacent semaphore words.
            let properties = if nx2 && self.regs.semaphore_trigger() & (1 << 24) != 0 {
                QueryPropertiesFlags::IS_A_FENCE
            } else {
                QueryPropertiesFlags::HAS_TIMEOUT
            };
            let mirror = if nx2 {
                crate::nx2::nx2_semaphore_mirror(sequence_address)
            } else {
                0
            };
            on_back(self.rasterizer, move |rasterizer| {
                rasterizer.query(
                    sequence_address,
                    QueryType::Payload as u32,
                    properties,
                    payload,
                    0,
                );
                if mirror != 0 {
                    rasterizer.query(mirror, QueryType::Payload as u32, properties, payload, 0);
                }
            });
        } else if crate::nx2::G_NX2_AMPERE.load(std::sync::atomic::Ordering::SeqCst) {
            if !matches!(op, 0x1 | 0x4 | 0x8) {
                log::error!("Invalid semaphore operation {}", op);
                return;
            }
            let acquire = PendingAcquire {
                address: self.regs.semaphore_address(),
                value: self.regs.semaphore_sequence(),
                operation: op,
                since: std::time::Instant::now(),
                reported: false,
            };
            crate::nx2_profile::add(crate::nx2_profile::Counter::AcquireChecks, 1);
            drain_back();
            if acquire.matches(self.read_semaphore_word(acquire.address)) { return; }
            let waiters = crate::nx2::Nx2SemaphoreWaiters::get();
            let device = self.memory_manager.lock().gpu_to_cpu_address(acquire.address);
            if let Some(device) = device {
                self.waiter_owner = self as *const Self as usize;
                waiters.add(self.waiter_owner, device, acquire.value, acquire.operation);
            }
            // Register before draining: a matching release must remain latched
            // even if another fence overwrites the same word in this drain.
            self.with_rasterizer_mut(|rasterizer| rasterizer.release_fences(true));
            if !waiters.latched(self.waiter_owner)
                && !acquire.matches(self.read_semaphore_word(acquire.address))
            {
                self.pending_acquire = Some(acquire);
                crate::nx2_profile::add(crate::nx2_profile::Counter::AcquireWaits, 1);
            } else {
                waiters.remove(self.waiter_owner);
                self.waiter_owner = 0;
            }
        } else {
            drain_back();
            let word = self.read_gpu_u32(self.regs.semaphore_address());
            let acquire_value = self.regs.semaphore_sequence();
            self.regs.set_acquire_source(1);
            self.regs.set_acquire_value(acquire_value);
            match op {
                x if x == GpuSemaphoreOperation::AcquireEqual as u32 => {
                    self.regs.set_acquire_active(1);
                    self.regs.set_acquire_mode(0);
                    if word != acquire_value {
                        self.with_rasterizer_mut(|rasterizer| rasterizer.release_fences(true));
                    }
                }
                x if x == GpuSemaphoreOperation::AcquireGequal as u32 => {
                    self.regs.set_acquire_active(1);
                    self.regs.set_acquire_mode(1);
                    if word < acquire_value {
                        self.with_rasterizer_mut(|rasterizer| rasterizer.release_fences(true));
                    }
                }
                x if x == GpuSemaphoreOperation::AcquireMask as u32 => {
                    if word != 0 && acquire_value == 0 {
                        self.with_rasterizer_mut(|rasterizer| rasterizer.release_fences(true));
                    }
                }
                _ => {
                    log::error!("Invalid semaphore operation {}", op);
                }
            }
        }
    }

    /// Port of Puller::ProcessSemaphoreExecute. Releases are ordered on the
    /// backend; acquires drain it before checking memory and remain nonblocking.
    fn process_semaphore_execute(&mut self, argument: u32) {
        let address = ((self.regs.reg_array[0x18] as u64 & 0x1ffffff) << 32)
            | (self.regs.reg_array[0x17] & !3) as u64;
        let payload = self.regs.reg_array[0x19];
        let operation = argument & 7;
        if operation == 1 {
            on_back(self.rasterizer, move |rasterizer| {
                rasterizer.query(address, QueryType::Payload as u32,
                    QueryPropertiesFlags::IS_A_FENCE, payload, 0);
            });
            return;
        }
        drain_back();
        self.with_rasterizer_mut(|rasterizer| rasterizer.release_fences(true));
        let word = self.read_gpu_u32(address);
        let ready = match operation {
            0 => word == payload,
            2 => word >= payload,
            3 => word.wrapping_sub(payload) as i32 >= 0,
            4 => word & payload != 0,
            5 => word & payload == 0,
            _ => true,
        };
        if !ready {
            static REPORTED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if REPORTED.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 8 {
                log::warn!("SEM_EXECUTE op {} at {:#x}: {:#x} vs {:#x} not met, continuing",
                    operation, address, word, payload);
            }
        }
    }

    /// Process a semaphore acquire.
    ///
    /// Corresponds to `Puller::ProcessSemaphoreAcquire`.
    fn process_semaphore_acquire(&mut self) {
        drain_back();
        let addr = self.regs.semaphore_address();
        let value = self.regs.semaphore_acquire();
        let mut word = self.read_gpu_u32(addr);
        while word != value {
            self.regs.set_acquire_active(1);
            self.regs.set_acquire_value(value);
            self.with_rasterizer_mut(|rasterizer| rasterizer.release_fences(true));
            word = self.read_gpu_u32(addr);
            // TODO(kemathe73) figure out how to do the acquire_timeout
            self.regs.set_acquire_mode(0);
            self.regs.set_acquire_source(0);
        }
    }

    /// Process a semaphore release.
    ///
    /// Corresponds to `Puller::ProcessSemaphoreRelease`.
    /// Upstream calls `rasterizer->Query(address, Payload, IsAFence, payload, 0)`
    /// which writes the payload value to GPU memory at the semaphore address.
    fn process_semaphore_release(&mut self) {
        let sequence_address = self.regs.semaphore_address();
        let payload = self.regs.semaphore_release();
        log::trace!(
            "Puller::SemaphoreRelease addr=0x{:X} payload=0x{:X}",
            sequence_address,
            payload
        );
        if self.rasterizer.is_none() {
            self.write_gpu_u32(sequence_address, payload);
            return;
        }
        on_back(self.rasterizer, move |rasterizer| {
            rasterizer.query(
                sequence_address,
                QueryType::Payload as u32,
                QueryPropertiesFlags::IS_A_FENCE,
                payload,
                0,
            )
        });
    }
}

#[cfg(test)]
impl Default for Puller {
    fn default() -> Self {
        Self::new(
            Arc::new(parking_lot::Mutex::new(
                crate::memory_manager::MemoryManager::default(),
            )),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rasterizer_interface::{RasterizerDownloadArea, RasterizerInterface};
    use std::sync::{Arc as StdArc, Mutex as StdMutex};

    #[test]
    fn register_file_and_timer_engine_id_match_eden() {
        assert_eq!(std::mem::size_of::<PullerRegs>(), 0x800 * 4);
        assert_eq!(EngineID::Nv01Timer.raw(), 0x0004);
    }

    #[derive(Default)]
    struct FakeRasterizer {
        events: StdArc<StdMutex<Vec<(&'static str, std::thread::ThreadId)>>>,
        accelerate_dma: crate::rasterizer_interface::TestAccelerateDMA,
        syncpoints: StdArc<StdMutex<Vec<u32>>>,
        wait_for_idle_calls: StdArc<StdMutex<u32>>,
        reference_calls: StdArc<StdMutex<u32>>,
        release_fence_calls: StdArc<StdMutex<Vec<bool>>>,
        query_calls: StdArc<StdMutex<Vec<(u64, u32, QueryPropertiesFlags, u32)>>>,
        flush_needed: bool,
        flush_checks: StdArc<StdMutex<Vec<(u64, u64, crate::cache_types::CacheType)>>>,
        flush_calls: StdArc<StdMutex<Vec<(u64, u64, crate::cache_types::CacheType)>>>,
        flush_write: Option<(Arc<parking_lot::Mutex<crate::memory_manager::MemoryManager>>, u64, u32)>,
        release_writes: Option<(Arc<parking_lot::Mutex<crate::memory_manager::MemoryManager>>, u64, Vec<u32>)>,
    }

    impl RasterizerInterface for FakeRasterizer {
        fn access_accelerate_dma(
            &mut self,
        ) -> &mut dyn crate::engines::maxwell_dma::AccelerateDMAInterface {
            &mut self.accelerate_dma
        }

        fn draw(
            &mut self,
            _draw_view: crate::engines::draw_manager::Maxwell3DDrawView<'_>,
            _instance_count: u32,
        ) {
        }
        fn draw_texture(
            &mut self,
            _draw_texture_view: crate::engines::draw_manager::Maxwell3DDrawTextureView<'_>,
        ) {
        }
        fn clear(
            &mut self,
            _clear_view: crate::engines::draw_manager::Maxwell3DClearView<'_>,
            _layer_count: u32,
        ) {
        }
        fn dispatch_compute(&mut self, _dispatch: &crate::engines::kepler_compute::DispatchCall) {}
        fn reset_counter(&mut self, _query_type: u32) {}
        fn query(
            &mut self,
            gpu_addr: u64,
            query_type: u32,
            flags: QueryPropertiesFlags,
            payload: u32,
            _subreport: u32,
        ) {
            self.events.lock().unwrap().push(("query", std::thread::current().id()));
            self.query_calls
                .lock()
                .unwrap()
                .push((gpu_addr, query_type, flags, payload));
        }
        fn bind_graphics_uniform_buffer(
            &mut self,
            _stage: usize,
            _index: u32,
            _gpu_addr: u64,
            _size: u32,
        ) {
        }
        fn disable_graphics_uniform_buffer(&mut self, _stage: usize, _index: u32) {}
        fn signal_fence(&mut self, _func: Box<dyn FnOnce() + Send>) {}
        fn sync_operation(&mut self, _func: Box<dyn FnOnce() + Send>) {}
        fn signal_sync_point(&mut self, value: u32) {
            self.events.lock().unwrap().push(("syncpoint", std::thread::current().id()));
            self.syncpoints.lock().unwrap().push(value);
        }
        fn signal_reference(&mut self) {
            self.events.lock().unwrap().push(("reference", std::thread::current().id()));
            *self.reference_calls.lock().unwrap() += 1;
        }
        fn release_fences(&mut self, force: bool) {
            self.events.lock().unwrap().push(("release", std::thread::current().id()));
            self.release_fence_calls.lock().unwrap().push(force);
            if let Some((memory, address, values)) = &self.release_writes {
                for &value in values {
                    let mut memory = memory.lock();
                    memory.write::<u32>(*address, value);
                    let device = memory.gpu_to_cpu_address(*address).unwrap();
                    drop(memory);
                    crate::nx2::Nx2SemaphoreWaiters::get().on_write(device, value);
                }
            }
        }
        fn flush_all(&mut self) {}
        fn flush_region(&mut self, addr: u64, size: u64, which: crate::cache_types::CacheType) {
            self.flush_calls.lock().unwrap().push((addr, size, which));
            if let Some((memory, address, value)) = &self.flush_write {
                memory.try_lock().expect("puller must release memory lock before flush")
                    .write::<u32>(*address, *value);
            }
        }
        fn must_flush_region(
            &self,
            addr: u64,
            size: u64,
            which: crate::cache_types::CacheType,
        ) -> bool {
            self.flush_checks.lock().unwrap().push((addr, size, which));
            self.flush_needed
        }
        fn get_flush_area(&self, addr: u64, _size: u64) -> RasterizerDownloadArea {
            RasterizerDownloadArea {
                start_address: addr,
                end_address: addr,
                preemtive: true,
            }
        }
        fn invalidate_region(
            &mut self,
            _addr: u64,
            _size: u64,
            _which: crate::cache_types::CacheType,
        ) {
        }
        fn on_cache_invalidation(&mut self, _addr: u64, _size: u64) {}
        fn on_cpu_write(&mut self, _addr: u64, _size: u64) -> bool {
            false
        }
        fn invalidate_gpu_cache(&mut self) {
            self.events.lock().unwrap().push(("invalidate", std::thread::current().id()));
        }
        fn unmap_memory(&mut self, _addr: u64, _size: u64) {}
        fn modify_gpu_memory(&mut self, _as_id: usize, _addr: u64, _size: u64) {}
        fn flush_and_invalidate_region(
            &mut self,
            _addr: u64,
            _size: u64,
            _which: crate::cache_types::CacheType,
        ) {
        }
        fn wait_for_idle(&mut self) {
            self.events.lock().unwrap().push(("wait", std::thread::current().id()));
            *self.wait_for_idle_calls.lock().unwrap() += 1;
        }
        fn fragment_barrier(&mut self) {}
        fn tiled_cache_barrier(&mut self) {}
        fn flush_commands(&mut self) {}
        fn tick_frame(&mut self) {}
        fn accelerate_inline_to_memory(
            &mut self,
            _address: u64,
            _copy_size: usize,
            _memory: &[u8],
        ) {
        }
    }

    #[test]
    fn split_acquire_paths_drain_pending_backend_work_before_returning() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let mut puller = Puller::default();
        let fake = Box::new(FakeRasterizer::default());
        puller.bind_rasterizer(fake.as_ref());
        let split = crate::gpu_split::GpuSplitFrontend::new(false);
        let scope = split.install();
        split.start_back(|| {});
        for path in 0..4 {
            let completed = Arc::new(AtomicBool::new(false));
            let signal = completed.clone();
            split.defer_function(Box::new(move || { signal.store(true, Ordering::Release); }));
            match path {
                0 => {
                    puller.regs.reg_array[0x07] = GpuSemaphoreOperation::AcquireEqual as u32;
                    puller.process_semaphore_trigger_method();
                }
                1 => puller.process_semaphore_acquire(),
                2 => {
                    puller.pending_acquire = Some(PendingAcquire {
                        address: 0, value: 0, operation: GpuSemaphoreOperation::AcquireEqual as u32,
                        since: std::time::Instant::now(), reported: false,
                    });
                    assert!(puller.resume_acquire());
                }
                3 => puller.process_semaphore_execute(0),
                _ => unreachable!(),
            }
            assert!(completed.load(Ordering::Acquire), "acquire path {path} did not drain");
        }
        split.stop().unwrap();
        drop(scope);
    }

    #[test]
    fn split_puller_preserves_fifo_and_captures_register_values_before_execution() {
        let mut puller = Puller::default();
        let fake = Box::new(FakeRasterizer::default());
        puller.bind_rasterizer(fake.as_ref());
        let split = crate::gpu_split::GpuSplitFrontend::new(false);
        let scope = split.install();
        let (ready, wait) = std::sync::mpsc::channel();
        split.start_back(move || { wait.recv_timeout(std::time::Duration::from_secs(10)).unwrap(); });
        puller.regs.reg_array[0x1D] = 1 | (7 << 8);
        puller.process_fence_action_method();
        puller.regs.reg_array[0x1D] = 0;
        puller.process_fence_action_method();
        for method in [BufferMethods::RefCnt, BufferMethods::WaitForIdle, BufferMethods::MemOpB] {
            puller.call_puller_method(&MethodCall::new(method as u32, 0, 0, 1));
        }
        puller.regs.reg_array[0x05] = 0x1000;
        puller.regs.reg_array[0x1B] = 11;
        puller.process_semaphore_release();
        puller.regs.reg_array[0x05] = 0x2000;
        puller.regs.reg_array[0x06] = 22;
        puller.regs.reg_array[0x07] = 2;
        puller.process_semaphore_trigger_method();
        puller.regs.reg_array[0x17] = 0x3003;
        puller.regs.reg_array[0x19] = 33;
        puller.process_semaphore_execute(1);
        puller.regs.reg_array.fill(0);
        assert!(fake.events.lock().unwrap().is_empty());
        ready.send(()).unwrap();
        // An acquire must drain the preceding releases before using the
        // rasterizer directly on the frontend. Unmapped zero equals payload.
        puller.process_semaphore_execute(0);
        split.stop().unwrap();
        drop(scope);
        assert_eq!(*fake.syncpoints.lock().unwrap(), vec![7]);
        let events = fake.events.lock().unwrap();
        assert_eq!(events.iter().map(|e| e.0).collect::<Vec<_>>(),
            ["syncpoint", "release", "reference", "wait", "invalidate", "query", "query", "query", "release"]);
        let front = std::thread::current().id();
        assert!(events[..8].iter().all(|e| e.1 == events[0].1 && e.1 != front));
        assert_eq!(events[8].1, front);
        let queries = fake.query_calls.lock().unwrap();
        assert_eq!(queries.iter().map(|q| (q.0, q.3)).collect::<Vec<_>>(),
            [(0x1000, 11), (0x2000, 22), (0x3000, 33)]);
    }

    #[test]
    fn fence_increment_signals_syncpoint() {
        let mut puller = Puller::default();
        let fake = Box::new(FakeRasterizer::default());
        let syncpoints = fake.syncpoints.clone();
        let raw: *const dyn RasterizerInterface = Box::leak(fake);
        puller.bind_rasterizer(unsafe { &*raw });
        puller.regs.reg_array[0x1D] = 1 | (7 << 8);

        puller.process_fence_action_method();

        assert_eq!(*syncpoints.lock().unwrap(), vec![7]);
    }

    #[test]
    fn fence_acquire_forces_fence_release() {
        let mut puller = Puller::default();
        let fake = Box::new(FakeRasterizer::default());
        let releases = fake.release_fence_calls.clone();
        let raw: *const dyn RasterizerInterface = Box::leak(fake);
        puller.bind_rasterizer(unsafe { &*raw });
        puller.regs.reg_array[0x1D] = 9 << 8;

        puller.process_fence_action_method();

        assert_eq!(*releases.lock().unwrap(), vec![true]);
    }

    #[test]
    fn wait_for_idle_and_refcnt_call_rasterizer() {
        let mut puller = Puller::default();
        let fake = Box::new(FakeRasterizer::default());
        let waits = fake.wait_for_idle_calls.clone();
        let refs = fake.reference_calls.clone();
        let raw: *const dyn RasterizerInterface = Box::leak(fake);
        puller.bind_rasterizer(unsafe { &*raw });

        puller.call_puller_method(&MethodCall::new(BufferMethods::WaitForIdle as u32, 0, 0, 1));
        puller.call_puller_method(&MethodCall::new(BufferMethods::RefCnt as u32, 0, 0, 1));

        assert_eq!(*waits.lock().unwrap(), 1);
        assert_eq!(*refs.lock().unwrap(), 1);
    }

    #[test]
    fn semaphore_release_uses_payload_query_type() {
        let mut puller = Puller::default();
        let fake = Box::new(FakeRasterizer::default());
        let queries = fake.query_calls.clone();
        let raw: *const dyn RasterizerInterface = Box::leak(fake);
        puller.bind_rasterizer(unsafe { &*raw });
        puller.regs.reg_array[0x04] = 0x1;
        puller.regs.reg_array[0x05] = 0x2345_6780;
        puller.regs.reg_array[0x1B] = 0xDEAD_BEEF;

        puller.process_semaphore_release();

        let calls = queries.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, 0x1_2345_6780);
        assert_eq!(calls[0].1, QueryType::Payload as u32);
        assert_eq!(calls[0].2, QueryPropertiesFlags::IS_A_FENCE);
        assert_eq!(calls[0].3, 0xDEAD_BEEF);
    }

    #[test]
    fn semaphore_trigger_acquire_mismatch_releases_fences_once_and_returns() {
        let mut puller = Puller::default();
        let fake = Box::new(FakeRasterizer::default());
        let releases = fake.release_fence_calls.clone();
        let raw: *const dyn RasterizerInterface = Box::leak(fake);
        puller.bind_rasterizer(unsafe { &*raw });
        puller.regs.reg_array[0x06] = 1;
        puller.regs.reg_array[0x07] = GpuSemaphoreOperation::AcquireEqual as u32;

        puller.process_semaphore_trigger_method();

        assert_eq!(*releases.lock().unwrap(), vec![true]);
        assert_eq!(puller.regs.acquire_source(), 1);
        assert_eq!(puller.regs.acquire_active(), 1);
        assert_eq!(puller.regs.acquire_mode(), 0);
        assert_eq!(puller.regs.acquire_value(), 1);
    }

    #[test]
    fn bind_uses_initialized_engine_handles_without_accessing_channel_or_engine_state() {
        let gpu = crate::gpu::Gpu::new(false, false);
        let mut channel = Box::new(ChannelState::new(1));
        let memory = Arc::new(parking_lot::Mutex::new(crate::memory_manager::MemoryManager::default()));
        channel.memory_manager = Some(memory.clone());
        channel.init(&gpu, 1);
        let dma_handle = channel.dma_pusher.as_ref().unwrap().clone();
        let mut dma = dma_handle.lock();
        let mut puller = Puller::new(memory, dma.as_mut(), channel.as_mut());
        let cases = [
            (EngineID::Nv01Timer.raw(), EngineTypes::Nv01Timer, EngineHandle::from_ref(channel.nv01_timer.as_ref().unwrap().as_ref())),
            (EngineID::KeplerComputeB.raw(), EngineTypes::KeplerCompute, EngineHandle::from_ref(channel.kepler_compute.as_ref().unwrap().as_ref())),
            (EngineID::MaxwellB.raw(), EngineTypes::Maxwell3D, EngineHandle::from_ref(channel.maxwell_3d.as_ref().unwrap().as_ref())),
            (EngineID::FermiTwodA.raw(), EngineTypes::Fermi2D, EngineHandle::from_ref(channel.fermi_2d.as_ref().unwrap().as_ref())),
            (EngineID::MaxwellDmaCopyA.raw(), EngineTypes::MaxwellDMA, EngineHandle::from_ref(channel.maxwell_dma.as_ref().unwrap().as_ref())),
            (EngineID::KeplerInlineToMemoryB.raw(), EngineTypes::KeplerMemory, EngineHandle::from_ref(channel.kepler_memory.as_ref().unwrap().as_ref())),
        ];
        // A valid bind must need only copied handles, not the channel pointer.
        // The real channel and boxed engines still own every handle throughout.
        puller.channel_state = std::ptr::null_mut();
        for (class, kind, handle) in cases {
            for subchannel in 0..NUM_SUBCHANNELS {
                puller.process_bind_method(&MethodCall::new(0, class, subchannel as u32, 1));
                assert_eq!(dma.subchannel_binding_for_test(subchannel), (true, kind));
                assert!(dma.subchannel_handle_for_test(subchannel).unwrap().ptr_eq(handle));
            }
        }
        for (alias, index) in [(0xc797, 2), (0xc7c0, 1), (0xc7b5, 4)] {
            puller.process_bind_method(&MethodCall::new(0, alias, 7, 1));
            assert!(dma.subchannel_handle_for_test(7).unwrap().ptr_eq(cases[index].2));
        }
    }

    #[test]
    fn unknown_bind_engine_stores_raw_value_then_logs_like_upstream_without_debug_asserts() {
        let mut puller = Puller::default();

        assert_eq!(puller.bound_engines[0].raw(), 0);

        puller.process_bind_method(&MethodCall::new(
            BufferMethods::BindObject as u32,
            0x1234,
            2,
            1,
        ));

        assert_eq!(puller.bound_engines[2].raw(), 0x1234);
    }

    #[test]
    fn ampere_classes_bind_existing_engines_and_zero_preserves_native_binding() {
        const CHILD: &str = "RUZU_PULLER_AMPERE_BIND_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", std::thread::current().name().unwrap(), "--test-threads=1"])
                .env(CHILD, "1").status().unwrap();
            assert!(status.success());
            return;
        }
        crate::nx2::G_NX2_AMPERE.store(true, std::sync::atomic::Ordering::SeqCst);
        let mut channel = ChannelState::new(1);
        let mut puller = Puller::new(
            Arc::new(parking_lot::Mutex::new(crate::memory_manager::MemoryManager::default())),
            std::ptr::null_mut(), &mut channel);
        for (class, expected) in [
            (0xc797, EngineID::MaxwellB),
            (0xc7c0, EngineID::KeplerComputeB),
            (0xc7b5, EngineID::MaxwellDmaCopyA),
        ] {
            for flags in [0, 0x1f0000, 0x80000000] {
                puller.process_bind_method(&MethodCall::new(0, flags | class, 4, 1));
                assert_eq!(puller.bound_engines[4], expected);
                puller.process_bind_method(&MethodCall::new(0, 0, 4, 1));
                assert_eq!(puller.bound_engines[4], expected);
            }
        }
        let fake = Box::new(FakeRasterizer::default());
        let queries = fake.query_calls.clone();
        puller.bind_rasterizer(fake.as_ref());
        for (method, argument) in [(0x17, 0x23456783), (0x18, 0xfe000001),
            (0x19, 0xdeadbeef), (0x1a, 0xffffffff), (0x1b, 0x101)] {
            puller.call_puller_method(&MethodCall::new(method, argument, 0, 1));
        }
        assert_eq!(*queries.lock().unwrap(),
            vec![(0x123456780, QueryType::Payload as u32, QueryPropertiesFlags::IS_A_FENCE, 0xdeadbeef)]);
        crate::nx2::G_NX2_AMPERE.store(false, std::sync::atomic::Ordering::SeqCst);
        puller.process_bind_method(&MethodCall::new(0, 0, 4, 1));
        assert_eq!(puller.bound_engines[4].raw(), 0);
    }

    #[test]
    fn nx2_acquire_predicates_latching_timeout_and_drop_preserve_wait_contract() {
        const CHILD: &str = "RUZU_PULLER_ACQUIRE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", std::thread::current().name().unwrap(), "--test-threads=1"])
                .env(CHILD, "1").status().unwrap();
            assert!(status.success());
            return;
        }
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use crate::memory_manager::MemoryManager;
        use crate::nx2::Nx2SemaphoreWaiters;
        crate::nx2::G_NX2_AMPERE.store(true, std::sync::atomic::Ordering::SeqCst);
        let mut backing = vec![0u8; 0x1000];
        let device = Arc::new(MaxwellDeviceMemoryManager::default());
        device.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device.smmu_map_with_cpu_backing(0x9000_0000, backing.as_mut_ptr(), 0x5000_0000, backing.len(), 3, true);
        let mut memory = MemoryManager::new_with_geometry_and_device_memory(13, device, 32, 0x1_0000_0000, 16, 12);
        memory.map(0x20000, 0x9000_0000, 0x1000, 0, false);
        let memory = Arc::new(parking_lot::Mutex::new(memory));
        let mut fake = Box::new(FakeRasterizer::default());
        for (op, target, unmet, met) in [(1, 7, 8, 7), (4, 0x80000000, 0x7fffffff, u32::MAX), (8, 0x80, 1, 0x81)] {
            let mut puller = Box::new(Puller::new(memory.clone(), std::ptr::null_mut(), std::ptr::null_mut()));
            puller.bind_rasterizer(fake.as_ref());
            puller.regs.reg_array[5] = 0x20040;
            puller.regs.reg_array[6] = target;
            puller.regs.reg_array[7] = op;
            memory.lock().write::<u32>(0x20040, unmet);
            puller.process_semaphore_trigger_method();
            assert!(puller.has_pending_acquire());
            assert!(!puller.resume_acquire());
            puller.pending_acquire.as_mut().unwrap().since -= std::time::Duration::from_secs(3);
            assert!(!puller.resume_acquire(), "diagnostic timeout must never release a channel");
            assert!(puller.pending_acquire.unwrap().reported);
            // CPU writes have no registry callback: polling must still resume.
            memory.lock().write::<u32>(0x20040, met);
            assert!(puller.resume_acquire());
            assert!(!puller.has_pending_acquire());
            // A release followed by an overwrite is latched during fence drain.
            fake.release_writes = Some((memory.clone(), 0x20040, vec![met, unmet]));
            memory.lock().write::<u32>(0x20040, unmet);
            puller.process_semaphore_trigger_method();
            assert!(!puller.has_pending_acquire());
            fake.release_writes = None;
            puller.process_semaphore_trigger_method();
            let owner = puller.waiter_owner;
            Nx2SemaphoreWaiters::get().on_write(0x9000_0040, met);
            Nx2SemaphoreWaiters::get().on_write(0x9000_0040, unmet);
            assert!(puller.can_resume_acquire());
            assert!(puller.resume_acquire());
            assert!(!Nx2SemaphoreWaiters::get().latched(owner));
            puller.process_semaphore_trigger_method();
            Nx2SemaphoreWaiters::get().on_write(0x9000_0040, met);
            assert!(Nx2SemaphoreWaiters::get().latched(owner));
            drop(puller);
            assert!(!Nx2SemaphoreWaiters::get().latched(owner), "session teardown must remove waiters");
        }
    }

    #[test]
    fn semaphore_trigger_release_size_and_mirror_match_nx2_without_changing_switch1() {
        // Isolate the session globals from other parallel tests.
        const CHILD: &str = "RUZU_PULLER_RELEASE_SIZE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", std::thread::current().name().unwrap(), "--test-threads=1"])
                .env(CHILD, "1").status().unwrap();
            assert!(status.success());
            return;
        }
        let region = 0x1_2340_0000;
        crate::nx2::nx2_add_semaphore_region(region);
        let mut puller = Puller::default();
        let fake = Box::new(FakeRasterizer::default());
        puller.bind_rasterizer(fake.as_ref());
        for nx2 in [false, true] {
            crate::nx2::G_NX2_AMPERE.store(nx2, std::sync::atomic::Ordering::SeqCst);
            for short in [false, true] {
                for address in [region + 0x40, region + 0x1040] {
                    fake.query_calls.lock().unwrap().clear();
                    puller.regs.reg_array[0x4] = (address >> 32) as u32;
                    puller.regs.reg_array[0x5] = address as u32;
                    puller.regs.reg_array[0x6] = 0xdeadbeef;
                    puller.regs.reg_array[0x7] = 2 | if short { 1 << 24 } else { 0 };
                    puller.process_semaphore_trigger_method();
                    let flags = if nx2 && short {
                        QueryPropertiesFlags::IS_A_FENCE
                    } else {
                        QueryPropertiesFlags::HAS_TIMEOUT
                    };
                    let mut expected = vec![(address, QueryType::Payload as u32, flags, 0xdeadbeef)];
                    if nx2 && address == region + 0x1040 {
                        expected.push((region + 0x40, QueryType::Payload as u32, flags, 0xdeadbeef));
                    }
                    assert_eq!(*fake.query_calls.lock().unwrap(), expected);
                }
            }
        }
        crate::nx2::nx2_remove_semaphore_region(region);
        crate::nx2::G_NX2_AMPERE.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    #[test]
    fn semaphore_read_flushes_only_dirty_buffer_bytes_before_reading() {
        use crate::cache_types::CacheType;
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use crate::memory_manager::MemoryManager;

        let mut backing = vec![0u8; 0x1000];
        let device = Arc::new(MaxwellDeviceMemoryManager::default());
        device.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device.smmu_map_with_cpu_backing(
            0x9000_0000, backing.as_mut_ptr(), 0x5000_0000, backing.len(), 3, true,
        );
        let mut memory = MemoryManager::new_with_geometry_and_device_memory(
            13, device, 32, 0x1_0000_0000, 16, 12,
        );
        memory.map(0x20000, 0x9000_0000, 0x1000, 0, false);
        memory.write::<u32>(0x20040, 3);
        let memory = Arc::new(parking_lot::Mutex::new(memory));
        let mut puller = Puller::new(Arc::clone(&memory), std::ptr::null_mut(), std::ptr::null_mut());
        let mut rasterizer = Box::new(FakeRasterizer {
            flush_needed: true,
            flush_write: Some((Arc::clone(&memory), 0x20040, 7)),
            ..Default::default()
        });
        puller.bind_rasterizer(rasterizer.as_ref());
        assert_eq!(puller.read_semaphore_word(0x20040), 7);
        let expected = (0x9000_0040, 4, CacheType::BUFFER_CACHE);
        assert_eq!(*rasterizer.flush_checks.lock().unwrap(), vec![expected]);
        assert_eq!(*rasterizer.flush_calls.lock().unwrap(), vec![expected]);

        // A clean buffer is read directly, with no download or other cache flush.
        rasterizer.flush_needed = false;
        memory.lock().write::<u32>(0x20040, 11);
        assert_eq!(puller.read_semaphore_word(0x20040), 11);
        assert_eq!(rasterizer.flush_checks.lock().unwrap().len(), 2);
        assert_eq!(rasterizer.flush_calls.lock().unwrap().len(), 1);

        // Unmapped addresses must not ask the rasterizer to flush device address zero.
        assert_eq!(puller.read_semaphore_word(0x30000), 0);
        assert_eq!(rasterizer.flush_checks.lock().unwrap().len(), 2);
        assert_eq!(rasterizer.flush_calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn semaphore_execute_acquires_release_fences_once_and_never_emit_a_release() {
        let mut puller = Puller::default();
        let fake = Box::new(FakeRasterizer::default());
        let releases = fake.release_fence_calls.clone();
        let queries = fake.query_calls.clone();
        puller.bind_rasterizer(fake.as_ref());
        // Unmapped reads return zero; include unmet compares and wrapping signed subtraction.
        for payload in [0, 1, 0x80000000, u32::MAX] {
            puller.regs.reg_array[0x19] = payload;
            for operation in [0, 2, 3, 4, 5, 6, 7] {
                puller.process_semaphore_execute(operation | 0x100);
            }
        }
        assert_eq!(*releases.lock().unwrap(), vec![true; 28]);
        assert!(queries.lock().unwrap().is_empty());
    }

    #[test]
    fn unknown_engine_method_logs_like_upstream_without_debug_asserts() {
        let mut puller = Puller::default();
        puller.bound_engines[3] = EngineID::from_raw(0x1234);

        puller.call_engine_method(&MethodCall::new(0x40, 0xCAFE_BABE, 3, 1));
    }

    #[test]
    fn unknown_engine_multi_method_logs_like_upstream_without_debug_asserts() {
        let mut puller = Puller::default();
        puller.bound_engines[4] = EngineID::from_raw(0x1234);

        puller.call_engine_multi_method(0x40, 4, &[0xCAFE_BABE], 1, 1);
    }
}

// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of video_core/engines/puller.h and puller.cpp
//!
//! The Puller is the front-end GPU command processor. It reads methods from
//! the GPFIFO command stream and dispatches them either to built-in puller
//! methods (semaphores, fences, sync points) or to bound engine subchannels.

use crate::control::channel_state::ChannelState;
use crate::engines::engine_interface::EngineInterface;
use crate::engines::engine_interface::EngineTypes;
use crate::query_cache::types::{QueryPropertiesFlags, QueryType};
use crate::rasterizer_interface::{RasterizerHandle, RasterizerInterface};
use std::sync::Arc;

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

/// The GPU command puller.
///
/// Corresponds to the C++ `Puller` class. Dispatches method calls to either
/// built-in puller methods or bound engine subchannels.
pub struct Puller {
    pub regs: PullerRegs,
    /// Mapping of subchannels to bound engine IDs.
    pub bound_engines: [EngineID; NUM_SUBCHANNELS],
    memory_manager: Arc<parking_lot::Mutex<crate::memory_manager::MemoryManager>>,
    dma_pusher: *mut crate::dma_pusher::DmaPusher,
    rasterizer: Option<RasterizerHandle>,
    channel_state: *mut ChannelState,
}

impl Puller {
    pub fn new(
        memory_manager: Arc<parking_lot::Mutex<crate::memory_manager::MemoryManager>>,
        dma_pusher: *mut crate::dma_pusher::DmaPusher,
        channel_state: *mut ChannelState,
    ) -> Self {
        Self {
            regs: PullerRegs::default(),
            bound_engines: [EngineID::default(); NUM_SUBCHANNELS],
            memory_manager,
            dma_pusher,
            rasterizer: None,
            channel_state,
        }
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
        let mut bytes = [0u8; 4];
        self.memory_manager
            .lock()
            .read_block_unsafe(gpu_va, &mut bytes);
        u32::from_le_bytes(bytes)
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
                self.with_rasterizer_mut(|rasterizer| rasterizer.signal_reference());
            }
            Some(BufferMethods::SyncpointOperation) => {
                self.process_fence_action_method();
            }
            Some(BufferMethods::WaitForIdle) => {
                self.with_rasterizer_mut(|rasterizer| rasterizer.wait_for_idle());
            }
            Some(BufferMethods::SemaphoreOperation) => {
                self.process_semaphore_trigger_method();
            }
            Some(BufferMethods::NonStallInterrupt) => {
                log::error!("Special puller engine method NonStallInterrupt not implemented");
            }
            Some(BufferMethods::MemOpA) => {
                log::error!("Memory Operation A");
            }
            Some(BufferMethods::MemOpB) => {
                self.with_rasterizer_mut(|rasterizer| rasterizer.invalidate_gpu_cache());
            }
            Some(BufferMethods::MemOpC) | Some(BufferMethods::MemOpD) => {
                log::error!("Memory Operation C,D");
            }
            Some(BufferMethods::SemaphoreAcquire) => {
                self.process_semaphore_acquire();
            }
            Some(BufferMethods::SemaphoreRelease) => {
                self.process_semaphore_release();
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
        let eid = EngineID::from_raw(engine);
        self.bound_engines[method_call.subchannel as usize] = eid;
        match eid {
            EngineID::FermiTwodA
            | EngineID::MaxwellB
            | EngineID::KeplerComputeB
            | EngineID::KeplerInlineToMemoryB
            | EngineID::MaxwellDmaCopyA
            | EngineID::Nv01Timer => {
                let channel_state = unsafe { &mut *self.channel_state };
                let dma_pusher = unsafe { self.dma_pusher.as_mut() };
                if let Some(dma_pusher) = dma_pusher {
                    match eid {
                        EngineID::FermiTwodA => {
                            if let Some(engine) = channel_state.fermi_2d.as_mut() {
                                dma_pusher.bind_subchannel(
                                    engine.as_mut(),
                                    method_call.subchannel,
                                    EngineTypes::Fermi2D,
                                );
                            }
                        }
                        EngineID::MaxwellB => {
                            if let Some(engine) = channel_state.maxwell_3d.as_mut() {
                                dma_pusher.bind_subchannel(
                                    engine.as_mut(),
                                    method_call.subchannel,
                                    EngineTypes::Maxwell3D,
                                );
                            }
                        }
                        EngineID::KeplerComputeB => {
                            if let Some(engine) = channel_state.kepler_compute.as_mut() {
                                dma_pusher.bind_subchannel(
                                    engine.as_mut(),
                                    method_call.subchannel,
                                    EngineTypes::KeplerCompute,
                                );
                            }
                        }
                        EngineID::KeplerInlineToMemoryB => {
                            if let Some(engine) = channel_state.kepler_memory.as_mut() {
                                dma_pusher.bind_subchannel(
                                    engine.as_mut(),
                                    method_call.subchannel,
                                    EngineTypes::KeplerMemory,
                                );
                            }
                        }
                        EngineID::MaxwellDmaCopyA => {
                            if let Some(engine) = channel_state.maxwell_dma.as_mut() {
                                dma_pusher.bind_subchannel(
                                    engine.as_mut(),
                                    method_call.subchannel,
                                    EngineTypes::MaxwellDMA,
                                );
                            }
                        }
                        EngineID::Nv01Timer => {
                            if let Some(engine) = channel_state.nv01_timer.as_mut() {
                                dma_pusher.bind_subchannel(
                                    engine.as_mut(),
                                    method_call.subchannel,
                                    EngineTypes::Nv01Timer,
                                );
                            }
                        }
                        _ => unreachable!("known engine ids are handled by the outer match"),
                    }
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
                self.with_rasterizer_mut(|rasterizer| rasterizer.release_fences(true));
            }
            FenceOperation::Increment => {
                let syncpoint_id = action.syncpoint_id();
                self.with_rasterizer_mut(|rasterizer| rasterizer.signal_sync_point(syncpoint_id));
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
            self.with_rasterizer_mut(|rasterizer| {
                rasterizer.query(
                    sequence_address,
                    QueryType::Payload as u32,
                    QueryPropertiesFlags::HAS_TIMEOUT,
                    payload,
                    0,
                )
            });
        } else {
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

    /// Process a semaphore acquire.
    ///
    /// Corresponds to `Puller::ProcessSemaphoreAcquire`.
    fn process_semaphore_acquire(&mut self) {
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
        self.with_rasterizer_mut(|rasterizer| {
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
        accelerate_dma: crate::rasterizer_interface::TestAccelerateDMA,
        syncpoints: StdArc<StdMutex<Vec<u32>>>,
        wait_for_idle_calls: StdArc<StdMutex<u32>>,
        reference_calls: StdArc<StdMutex<u32>>,
        release_fence_calls: StdArc<StdMutex<Vec<bool>>>,
        query_calls: StdArc<StdMutex<Vec<(u64, u32, QueryPropertiesFlags, u32)>>>,
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
            self.syncpoints.lock().unwrap().push(value);
        }
        fn signal_reference(&mut self) {
            *self.reference_calls.lock().unwrap() += 1;
        }
        fn release_fences(&mut self, force: bool) {
            self.release_fence_calls.lock().unwrap().push(force);
        }
        fn flush_all(&mut self) {}
        fn flush_region(&mut self, _addr: u64, _size: u64, _which: crate::cache_types::CacheType) {}
        fn must_flush_region(
            &self,
            _addr: u64,
            _size: u64,
            _which: crate::cache_types::CacheType,
        ) -> bool {
            false
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
        fn invalidate_gpu_cache(&mut self) {}
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

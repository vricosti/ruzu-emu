// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Kepler Compute engine (NV class B1C0).
//!
//! Handles compute shader dispatch. When the game writes the LAUNCH register,
//! a Queue Meta Data (QMD) descriptor is read from GPU memory, parsed, and
//! forwarded immediately to the bound rasterizer, matching upstream.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

use super::engine_interface::{EngineHandle, EngineInterface, EngineInterfaceState};
use super::engine_upload;
use crate::memory_manager::MemoryManager;
use crate::rasterizer_interface::{RasterizerHandle, RasterizerInterface};
use crate::textures::texture::{TicEntry, TscEntry};

/// Port of kepler_compute.cpp's diagnostic draw budget after an upscale fallback.
pub static G_NX2_TRACE_DRAWS_AFTER_UPSCALE: AtomicU32 = AtomicU32::new(0);

// ── Register offset constants (method addresses) ────────────────────────────

/// Upload register block, matching upstream `ASSERT_REG_POSITION(upload, 0x60)`.
const UPLOAD_REG_OFFSET: u32 = 0x60;

/// Upload exec register, matching upstream `ASSERT_REG_POSITION(exec_upload, 0x6C)`.
const EXEC_UPLOAD: u32 = 0x6C;

/// Upload data register, matching upstream `ASSERT_REG_POSITION(data_upload, 0x6D)`.
const DATA_UPLOAD: u32 = 0x6D;

/// Dispatch trigger — any write to this method launches compute immediately.
const LAUNCH: u32 = 0xAF;

/// Upstream `KeplerCompute::Regs::NUM_REGS`.
const KEPLER_COMPUTE_REG_COUNT: usize = 0xCF8;

/// QMD address register — bits [31:0] of `regs[0xAD]`, shifted left by 8.
const LAUNCH_DESC_LOC: u32 = 0xAD;

/// `LaunchParams::grid_dim_x` word index, matching upstream `LAUNCH_REG_INDEX(grid_dim_x)`.
const LAUNCH_GRID_DIM_X_INDEX: u64 = 0x0C;

/// Texture sampler pool base: +0 addr_high, +1 addr_low, +2 limit.
const TSC_BASE: u32 = 0x557;

/// Texture image pool base: +0 addr_high, +1 addr_low, +2 limit.
const TIC_BASE: u32 = 0x55D;

/// Shader code base address: +0 addr_high, +1 addr_low.
const CODE_LOC_BASE: u32 = 0x582;

/// Texture constant buffer index.
const TEX_CB_INDEX: u32 = 0x982;

// ── QMD constants and types ─────────────────────────────────────────────────

/// Word count of a Queue Meta Data descriptor (256 bytes / 4).
pub const NUM_LAUNCH_PARAMETERS: usize = 0x40;

/// Raw 256-byte launch descriptor layout read by upstream `ProcessLaunch`.
/// Bit-field aliases are decoded by [`LaunchParams::from_words`].
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct LaunchParamsLayout {
    _padding_0: [u32; 0x8],
    program_start: u32,
    _padding_1: [u32; 0x2],
    linked_tsc: u32,
    grid_dim_x: u32,
    grid_dim_y: u32,
    _padding_2: [u32; 0x3],
    shared_alloc: u32,
    block_dim_x: u32,
    block_dim_y: u32,
    const_buffer_enable_mask: u32,
    _padding_3: [u32; 0x8],
    const_buffer_config: [u32; NUM_CONST_BUFFERS * 2],
    local_pos_alloc: u32,
    local_neg_alloc: u32,
    local_crs_alloc: u32,
    _padding_4: [u32; 0x10],
}

const _: () = {
    assert!(std::mem::size_of::<LaunchParamsLayout>() == NUM_LAUNCH_PARAMETERS * 4);
    assert!(std::mem::offset_of!(LaunchParamsLayout, program_start) == 0x8 * 4);
    assert!(std::mem::offset_of!(LaunchParamsLayout, grid_dim_x) == 0xC * 4);
    assert!(std::mem::offset_of!(LaunchParamsLayout, shared_alloc) == 0x11 * 4);
    assert!(std::mem::offset_of!(LaunchParamsLayout, block_dim_x) == 0x12 * 4);
    assert!(std::mem::offset_of!(LaunchParamsLayout, const_buffer_enable_mask) == 0x14 * 4);
    assert!(std::mem::offset_of!(LaunchParamsLayout, const_buffer_config) == 0x1D * 4);
};

impl LaunchParamsLayout {
    fn as_bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: the layout is `repr(C)`, contains only `u32` arrays/fields,
        // and is fully zero-initialized before the upstream-sized raw read.
        unsafe {
            std::slice::from_raw_parts_mut(
                std::ptr::from_mut(self).cast::<u8>(),
                std::mem::size_of::<Self>(),
            )
        }
    }

    fn words(&self) -> &[u32; NUM_LAUNCH_PARAMETERS] {
        // SAFETY: the compile-time size/alignment assertions above establish
        // an exact contiguous array of 0x40 `u32` words.
        unsafe { &*std::ptr::from_ref(self).cast::<[u32; NUM_LAUNCH_PARAMETERS]>() }
    }
}

/// Corresponds to upstream `KeplerCompute::NumConstBuffers`.
pub const NUM_CONST_BUFFERS: usize = 8;

/// Constant buffer configuration from a QMD descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConstBufferConfig {
    pub address: u64,
    pub size: u32,
}

/// Parsed Queue Meta Data (QMD) descriptor — 256-byte structure at a GPU VA.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LaunchParams {
    pub program_start: u32,
    pub linked_tsc: bool,
    pub grid_dim_x: u32,
    pub grid_dim_y: u32,
    pub grid_dim_z: u32,
    pub shared_alloc: u32,
    pub block_dim_x: u32,
    pub block_dim_y: u32,
    pub block_dim_z: u32,
    pub const_buffer_enable_mask: u32,
    pub cache_layout: u32,
    pub const_buffers: [ConstBufferConfig; NUM_CONST_BUFFERS],
    pub local_pos_alloc: u32,
    pub barrier_alloc: u32,
    pub local_neg_alloc: u32,
    pub local_crs_alloc: u32,
    pub gpr_alloc: u32,
    pub sass_version: u32,
}

impl LaunchParams {
    /// Parse a QMD from 64 little-endian u32 words.
    pub fn from_words(w: &[u32; NUM_LAUNCH_PARAMETERS]) -> Self {
        // Word 0x08: program_start (full word).
        let program_start = w[0x08];

        // Word 0x0B: linked_tsc in bit [30].
        let linked_tsc = ((w[0x0B] >> 30) & 1) != 0;

        // Word 0x0C: grid_dim_x in bits [30:0].
        let grid_dim_x = w[0x0C] & 0x7FFF_FFFF;

        // Word 0x0D: grid_dim_y in bits [15:0], grid_dim_z in bits [31:16].
        let grid_dim_y = w[0x0D] & 0xFFFF;
        let grid_dim_z = (w[0x0D] >> 16) & 0xFFFF;

        // Word 0x11: shared_alloc in bits [17:0].
        let shared_alloc = w[0x11] & 0x3FFFF;

        // Word 0x12: block_dim_x in bits [31:16].
        let block_dim_x = (w[0x12] >> 16) & 0xFFFF;

        // Word 0x13: block_dim_y in bits [15:0], block_dim_z in bits [31:16].
        let block_dim_y = w[0x13] & 0xFFFF;
        let block_dim_z = (w[0x13] >> 16) & 0xFFFF;

        // Word 0x14: const_buffer_enable_mask in bits [7:0], cache_layout in bits [30:29].
        let const_buffer_enable_mask = w[0x14] & 0xFF;
        let cache_layout = (w[0x14] >> 29) & 0x3;

        // Words 0x1D..0x2C: 8 constant buffer configs, 2 words each.
        // Per CB: word0 = addr_low, word1 = addr_high[7:0] | size[31:15].
        let mut const_buffers = [ConstBufferConfig::default(); NUM_CONST_BUFFERS];
        for i in 0..NUM_CONST_BUFFERS {
            let base = 0x1D + i * 2;
            let addr_low = w[base] as u64;
            let word1 = w[base + 1];
            let addr_high = (word1 & 0xFF) as u64;
            let size = (word1 >> 15) & 0x1FFFF;
            const_buffers[i] = ConstBufferConfig {
                address: (addr_high << 32) | addr_low,
                size,
            };
        }

        // Word 0x2D: local_pos_alloc in bits [19:0], barrier_alloc in bits [31:27].
        let local_pos_alloc = w[0x2D] & 0xFFFFF;
        let barrier_alloc = (w[0x2D] >> 27) & 0x1F;

        // Word 0x2E: local_neg_alloc in bits [19:0], gpr_alloc in bits [28:24].
        let local_neg_alloc = w[0x2E] & 0xFFFFF;
        let gpr_alloc = (w[0x2E] >> 24) & 0x1F;

        // Word 0x2F: local_crs_alloc in bits [19:0], sass_version in bits [28:24].
        let local_crs_alloc = w[0x2F] & 0xFFFFF;
        let sass_version = (w[0x2F] >> 24) & 0x1F;

        Self {
            program_start,
            linked_tsc,
            grid_dim_x,
            grid_dim_y,
            grid_dim_z,
            shared_alloc,
            block_dim_x,
            block_dim_y,
            block_dim_z,
            const_buffer_enable_mask,
            cache_layout,
            const_buffers,
            local_pos_alloc,
            barrier_alloc,
            local_neg_alloc,
            local_crs_alloc,
            gpr_alloc,
            sass_version,
        }
    }
}

/// A recorded compute dispatch with all relevant state.
#[derive(Debug, Clone, Default)]
pub struct DispatchCall {
    pub launch_description: LaunchParams,
    pub launch_desc_loc: u64,
    pub indirect_compute_address: Option<u64>,
    pub code_address: u64,
    pub tsc_address: u64,
    pub tsc_limit: u32,
    pub tic_address: u64,
    pub tic_limit: u32,
    pub tex_cb_index: u32,
}

#[derive(Debug, Clone, Copy, Default)]
struct UploadInfo {
    upload_address: u64,
    exec_address: u64,
    copy_size: u32,
    was_dirty: bool,
}

/// Corresponds to upstream's public anonymous `state` member.
#[derive(Debug, Clone, Default)]
pub struct State {
    pub write_offset: u32,
    pub copy_size: u32,
    pub inner_buffer: Vec<u8>,
}

// ── Engine struct ───────────────────────────────────────────────────────────

/// KeplerCompute::FrontState. The Arc-owned allocation lets the frontend access
/// only its copied state while the backend exclusively borrows KeplerCompute.
pub(crate) struct FrontState {
    registers: [u32; 0x1000],
    executing_macro: Option<u32>,
    parameters: Vec<u32>,
    compute_uploaded: bool,
    bind_pointer: u32,
    // Owning equivalents of upstream's native_context and macro pointers.
    native_context: Option<crate::r#macro::SharedNativeMacroContext>,
    macro_owner: crate::r#macro::SharedNativeMacroContext,
    macro_compute: bool,
    native_data_ram: Option<Arc<Mutex<crate::macro_turing::DataRam>>>,
}

pub struct KeplerCompute {
    /// Native method register bank from KeplerCompute's upstream owner.
    native_registers: [u32; 0x1000],
    native_qmd: crate::native_qmd::NativeQmdScheduler,
    native_data_ram: Option<Arc<Mutex<crate::macro_turing::DataRam>>>,
    native_mode: bool,
    native_context: Option<crate::r#macro::SharedNativeMacroContext>,
    // Owning handle for upstream native_macro/native_macro_positions, with selected engine.
    native_macro_context: Option<crate::r#macro::SharedNativeMacroContext>,
    native_macro_compute: bool,
    native_compute_uploaded: bool,
    native_plain_methods: Box<[bool; 0xe00]>,
    native_parameters: Vec<u32>,
    native_memo_parameters: Vec<u32>,
    native_executing_macro: Option<u32>,
    native_executing_macro_logged: Option<u32>,
    native_bind_pointer: u32,
    native_mme_dumped: bool,
    front_macros: bool,
    front: Option<Arc<crate::gpu_split::FrontOwned<FrontState>>>,
    regs: Box<[u32; KEPLER_COMPUTE_REG_COUNT]>,
    interface_state: EngineInterfaceState,
    memory_manager: Arc<Mutex<MemoryManager>>,
    upload_state: engine_upload::State,
    upload_address: u64,
    upload_dirty: bool,
    uploads: Vec<UploadInfo>,
    /// Port of upstream owner-local `launch_description`.
    pub launch_description: LaunchParams,
    /// Preserved owner-local scratch state from upstream.
    pub state: State,
    /// Port of upstream owner-local `indirect_compute`.
    indirect_compute: Option<u64>,
    rasterizer: Option<RasterizerHandle>,
    /// Ocarina of Time's movie network: a pass that reads one colour image and stores none
    /// retains its input here; the pass that stores an image upscaling it is replaced by a plain
    /// copy (the network's matrix passes cost tens of milliseconds per frame). Cleared after the
    /// copy. Port of Lichii `native_upscale_input` (aae0b063f7).
    native_upscale_input: Option<TicEntry>,
    /// Native dispatches left to skip while a retained network is being replaced (bounded, so a
    /// non-network pass cannot silence the engine forever).
    network_skip_remaining: u32,
    /// Set when the replacement copy fails, so the full network runs from then on.
    native_upscale_disabled: bool,
}

impl KeplerCompute {
    /// Corresponds to upstream `KeplerCompute(Core::System&, MemoryManager&)`.
    /// Rust stores the upstream `MemoryManager&` owner directly here; the wider
    /// `System&` dependency remains outside this bounded slice.
    pub fn new(memory_manager: Arc<Mutex<MemoryManager>>) -> Self {
        Self {
            native_registers: [0; 0x1000],
            native_qmd: {
                let mut scheduler = crate::native_qmd::NativeQmdScheduler::default();
                if std::env::var_os("EDEN_NX2_QMD_TRACE").is_some() {
                    let memory = Arc::clone(&memory_manager);
                    scheduler.trace = Some(Box::new(move |event, address, value| {
                        log::info!(
                            "QMD trace {event} {address:#x} {value} (as {})",
                            memory.lock().get_id()
                        );
                    }));
                }
                scheduler
            },
            native_data_ram: None,
            native_mode: false,
            native_context: None,
            native_macro_context: None,
            native_macro_compute: false,
            native_compute_uploaded: false,
            native_plain_methods: {
                let mut methods = Box::new([true; 0xe00]);
                for method in [
                    0x45,
                    0x46,
                    0x47,
                    0x48,
                    0xae,
                    0xaf,
                    0xb0,
                    0x107,
                    0x6c3,
                    EXEC_UPLOAD as usize,
                    DATA_UPLOAD as usize,
                ] {
                    methods[method] = false;
                }
                if std::env::var_os("EDEN_NATIVE_DRAW_DIAGNOSTICS").is_some() {
                    methods[0x5a] = false;
                    methods[0xc6..=0x108].fill(false);
                }
                methods
            },
            native_parameters: Vec::new(),
            native_memo_parameters: Vec::new(),
            native_executing_macro: None,
            native_executing_macro_logged: None,
            native_bind_pointer: 0,
            native_mme_dumped: false,
            front_macros: false,
            front: None,
            regs: Box::new([0u32; KEPLER_COMPUTE_REG_COUNT]),
            interface_state: {
                let mut state = EngineInterfaceState::new();
                state.execution_mask[EXEC_UPLOAD as usize] = true;
                state.execution_mask[DATA_UPLOAD as usize] = true;
                state.execution_mask[LAUNCH as usize] = true;
                state
            },
            upload_state: engine_upload::State::new_with_memory_manager(Arc::clone(
                &memory_manager,
            )),
            memory_manager,
            upload_address: 0,
            upload_dirty: false,
            uploads: Vec::new(),
            launch_description: LaunchParams::default(),
            state: State::default(),
            indirect_compute: None,
            rasterizer: None,
            native_upscale_input: None,
            network_skip_remaining: 0,
            native_upscale_disabled: false,
        }
    }

    /// Port of SetNativeDataRam; the RAM remains owned by the channel.
    pub fn set_native_data_ram(&mut self, ram: Option<Arc<Mutex<crate::macro_turing::DataRam>>>) {
        self.native_data_ram = ram;
    }

    /// Port of SetNativeMacroMode. The shared handle owns the initial graphics engine and
    /// positions; context controls the later compute-upload switch. Handles replace raw spans.
    pub fn set_native_macro_mode(
        &mut self,
        shared_macro: Option<crate::r#macro::SharedNativeMacroContext>,
        context: Option<crate::r#macro::SharedNativeMacroContext>,
    ) {
        let same = match (&self.native_context, &context) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.native_compute_uploaded = false;
        }
        self.native_context = context;
        self.native_mode = shared_macro.is_some();
        self.upload_state
            .set_native_partial_transfers(self.native_mode);
        self.native_macro_context = shared_macro;
        self.native_macro_compute = false;
        self.interface_state.execution_mask.fill(self.native_mode);
        if !self.native_mode {
            for method in [EXEC_UPLOAD, DATA_UPLOAD, LAUNCH] {
                self.interface_state.execution_mask[method as usize] = true;
            }
        }
    }

    pub fn is_native_mode(&self) -> bool {
        self.native_mode
    }

    pub fn front_runs_macros(&self) -> bool {
        self.native_mode
    }

    /// Port of Front. Drain occurs before creating any Rust engine reference.
    ///
    /// # Safety
    /// `engine` must be a live, stable engine owned by this split's channel.
    /// The caller must be its sole command producer, with no engine references
    /// alive across this call. Cache the returned frontend allocation: subsequent
    /// dispatch must borrow that allocation, never the concurrently running back.
    #[cold]
    #[inline(never)]
    pub(crate) unsafe fn front(
        engine: *mut Self,
        split: &crate::gpu_split::GpuSplitFrontend,
    ) -> Arc<crate::gpu_split::FrontOwned<FrontState>> {
        // Upstream checks front before draining. We always drain on acquiring a
        // handle so even that field is never read through a concurrent &mut Self.
        split.drain();
        let engine = unsafe { &mut *engine };
        if engine.front.is_none() {
            let state = FrontState {
                registers: engine.native_registers,
                executing_macro: engine.native_executing_macro,
                parameters: std::mem::take(&mut engine.native_parameters),
                compute_uploaded: engine.native_compute_uploaded,
                bind_pointer: engine.native_bind_pointer,
                native_context: engine.native_context.clone(),
                macro_owner: engine.native_macro_context.clone()
                    .expect("native compute requires macro owner"),
                macro_compute: engine.native_macro_compute,
                native_data_ram: engine.native_data_ram.clone(),
            };
            engine.native_executing_macro = None;
            engine.front = Some(Arc::new(crate::gpu_split::FrontOwned::new(state)));
            engine.front_macros = true;
        }
        engine.front.as_ref().unwrap().clone()
    }

    // Mechanical ownership selector for C++'s raw context/macro pointers.
    fn front_macro_owner(fs: &FrontState) -> &crate::r#macro::SharedNativeMacroContext {
        fs.native_context.as_ref().unwrap_or(&fs.macro_owner)
    }

    fn front_macro_engine<'a>(
        fs: &FrontState,
        context: &'a mut crate::r#macro::NativeMacroContext,
    ) -> &'a mut crate::r#macro::MacroEngine {
        let compute = if fs.native_context.is_some() {
            fs.compute_uploaded
        } else {
            fs.macro_compute
        };
        if compute { &mut context.compute_engine } else { &mut context.engine }
    }

    fn front_macro_positions<'a>(
        fs: &FrontState,
        context: &'a mut crate::r#macro::NativeMacroContext,
    ) -> &'a mut [u32; 0x80] {
        let compute = if fs.native_context.is_some() {
            fs.compute_uploaded
        } else {
            fs.macro_compute
        };
        if compute { &mut context.compute_positions } else { &mut context.positions }
    }

    /// Port of FrontApply. No macro-context lock survives this call: enqueue,
    /// ring-space waits and Drain must happen only after it has returned.
    fn front_apply(fs: &mut FrontState, method: u32, argument: u32) {
        if method == 0x45 || method == 0x47 {
            fs.compute_uploaded = true;
        }
        fs.registers[method as usize] = argument;
        match method {
            0x45 | 0x46 => {
                let owner = Self::front_macro_owner(fs).clone();
                let guard = owner.lock();
                let mut context = guard.borrow_mut();
                let engine = Self::front_macro_engine(fs, &mut context);
                if method == 0x45 { engine.clear_code(argument); }
                else { engine.add_code(0, argument); }
            }
            0x47 => fs.bind_pointer = argument,
            0x48 => {
                let owner = Self::front_macro_owner(fs).clone();
                let guard = owner.lock();
                let mut context = guard.borrow_mut();
                let positions = Self::front_macro_positions(fs, &mut context);
                if (fs.bind_pointer as usize) < positions.len() {
                    positions[fs.bind_pointer as usize] = argument;
                    fs.bind_pointer += 1;
                } else {
                    log::error!("Native compute macro bind out of bounds");
                }
            }
            _ => {}
        }
    }

    /// FrontCallMethod operates only on the retained frontend state, never on
    /// the engine allocation that the backend is concurrently mutating.
    pub(crate) fn front_call_method(
        fs: &mut FrontState, engine: EngineHandle,
        split: &crate::gpu_split::GpuSplitFrontend,
        method: u32, argument: u32, is_last_call: bool, segment: Option<u64>,
    ) {
        if method as usize >= fs.registers.len() {
            split.method(engine, method, argument, is_last_call, segment, crate::gpu_split::Writes::Anywhere);
            return;
        }
        if method >= 0xe00 {
            if fs.executing_macro.is_none() {
                if method & 1 != 0 {
                    log::error!("Native compute macro starts on argument method");
                    return;
                }
                fs.executing_macro = Some(method);
            }
            fs.parameters.push(argument);
            if is_last_call {
                Self::front_run_macro(fs, engine, split);
            }
            return;
        }
        Self::front_apply(fs, method, argument);
        split.method(engine, method, argument, is_last_call, segment, crate::gpu_split::Writes::Anywhere);
    }

    pub(crate) fn front_call_multi_method(
        fs: &mut FrontState, engine: EngineHandle,
        split: &crate::gpu_split::GpuSplitFrontend,
        method: u32, words: &[u32], amount: u32, methods_pending: u32,
        segment: Option<u64>,
    ) {
        let words = &words[..amount as usize];
        if method >= 0xe00 && (method as usize) < fs.registers.len() {
            for (i, &argument) in words.iter().enumerate() {
                Self::front_call_method(fs, engine, split, method, argument,
                    methods_pending.wrapping_sub(i as u32) <= 1, segment);
            }
            return;
        }
        if (method as usize) < fs.registers.len() {
            for &argument in words {
                Self::front_apply(fs, method, argument);
            }
        }
        split.multi_method(engine, method, words, amount, methods_pending, segment, crate::gpu_split::Writes::Anywhere);
    }

    // Out of line: its local data RAM copy must not size the stack frame of
    // every front method call (front_call_method), probed page by page.
    #[inline(never)]
    fn front_run_macro(
        fs: &mut FrontState, engine: EngineHandle,
        split: &crate::gpu_split::GpuSplitFrontend,
    ) {
        #[cfg(target_arch = "x86_64")]
        {
            // Upstream reaches the context, the JIT and the channel's data RAM through raw
            // pointers / shared_ptr members used in place. Rust borrows the allocations
            // through their owning handles instead of cloning the Arcs per call.
            // SAFETY: FrontState::native_context, macro_owner and native_data_ram are set once
            // by KeplerCompute::front and never reassigned; `fs` (and so these Arcs) outlives
            // this call, and nothing below touches those fields through `fs`.
            let owner = unsafe { &*Arc::as_ptr(Self::front_macro_owner(fs)) };
            // Keep the original macro identity for restoring RAM and reporting
            // failures even if an emitted upload changes the frontend selector.
            let compute = if fs.native_context.is_some() {
                fs.compute_uploaded
            } else { fs.macro_compute };
            let _profile = crate::nx2_profile::Scope::new(crate::nx2_profile::Zone::Macro);
            let (start, mut jit, kept_ram, mut local_ram) = {
                let guard = owner.lock();
                let mut context = guard.borrow_mut();
                let positions = Self::front_macro_positions(fs, &mut context);
                let start = positions[((fs.executing_macro.unwrap() - 0xe00) / 2) as usize
                    % positions.len()];
                fs.executing_macro = None;
                let macro_engine = Self::front_macro_engine(fs, &mut context);
                // Upstream stores the same pointer every call; the owning handle is only
                // replaced when it names another RAM (no reference count traffic).
                if let Some(ram) = &fs.native_data_ram {
                    if !macro_engine.native_interpreter.active_dram.as_ref()
                        .is_some_and(|active| Arc::ptr_eq(active, ram))
                    {
                        macro_engine.native_interpreter.active_dram = Some(ram.clone());
                    }
                }
                // SAFETY: native_jit is created once and never replaced while the engine
                // lives (as MacroEngine::execute_native borrows it); the engine lives in the
                // context `owner` keeps alive.
                let jit = unsafe { &*Arc::as_ptr(macro_engine.native_jit.get_or_insert_with(|| {
                    Arc::new(std::sync::Mutex::new(Default::default()))
                })) };
                // One JIT lock for Prepare and Execute (as execute_native); only the front
                // runs this engine's macros while the split runs.
                let mut jit = jit.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                jit.prepare(&macro_engine.native_code, macro_engine.native_code_generation)
                    .expect("native compute frontend macro JIT preparation failed");
                // The channel's RAM, which active_dram now names, is borrowed from
                // FrontState below. Without one, the owning handle active_dram holds
                // (another engine's RAM) is kept for the call, as before.
                let kept_ram = if fs.native_data_ram.is_none() {
                    macro_engine.native_interpreter.active_dram.clone()
                } else { None };
                let local_ram = (fs.native_data_ram.is_none() && kept_ram.is_none()).then(|| {
                    std::mem::replace(&mut macro_engine.native_interpreter.dram, [0; 0xc00])
                });
                (start, jit, kept_ram, local_ram)
            }; // Release the context before Execute can enqueue or wait for the back.
            let ram_owner: Option<&Mutex<crate::macro_turing::DataRam>> = match &kept_ram {
                Some(ram) => Some(&**ram),
                // SAFETY: see `owner` above.
                None => fs.native_data_ram.as_ref().map(|ram| unsafe { &*Arc::as_ptr(ram) }),
            };
            let mut parameters = std::mem::take(&mut fs.parameters);
            let mut shared_ram = ram_owner.map(|ram| ram.lock());
            let ram = match shared_ram.as_deref_mut() {
                Some(ram) => ram,
                None => local_ram.as_mut().unwrap(),
            };
            let front = fs as *mut FrontState;
            // Kept: on a panic out of Execute it puts the leased interpreter RAM (no
            // active_dram) and the parameter vector back before re-raising it.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                jit.execute(start, &mut parameters, ram,
                    // Read/emit callbacks run sequentially. Parameters and RAM
                    // are leased separately, and no whole-state borrow spans a callback.
                    |address| unsafe {
                        (*front).registers.get(address as usize).copied().unwrap_or(0)
                    },
                    |address, value, _, _| {
                        if address >= 0xe00 { return false; }
                        unsafe { Self::front_apply(&mut *front, address, value); }
                        split.macro_write(engine, address, value, crate::gpu_split::Writes::Anywhere);
                        true
                    },
                )
            }));
            drop(shared_ram);
            drop(jit);
            // Upstream has nothing to put back and sets native_execution_failed in place:
            // the context lock is only taken when there is something to write.
            let failed = matches!(&result, Ok(result) if !result.success);
            if local_ram.is_some() || failed {
                let guard = owner.lock();
                let mut context = guard.borrow_mut();
                let macro_engine = if compute { &mut context.compute_engine }
                    else { &mut context.engine };
                if let Some(ram) = local_ram { macro_engine.native_interpreter.dram = ram; }
                if let Ok(result) = &result {
                    if !result.success {
                        macro_engine.native_execution_failed = true;
                        log::error!("Native MME (JIT, split front) failed: compute start={start:#x}, consumed={}, error={}", result.consumed, result.error);
                    }
                }
            }
            parameters.clear();
            fs.parameters = parameters;
            if let Err(payload) = result { std::panic::resume_unwind(payload); }
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            fs.executing_macro = None;
            fs.parameters.clear();
        }
    }

    /// Port of ProcessNativeMethod. The retained reentrant context lock allows emitted
    /// register writes to upload code without borrowing the selected macro engine across callbacks.
    fn process_native_method(&mut self, method: u32, argument: u32, is_last_call: bool) -> bool {
        use crate::native_query_trace::trace_native_query;
        if self.native_context.is_some() && !self.front_macros {
            if method == 0x45 || method == 0x47 {
                self.native_compute_uploaded = true;
            }
            // Upstream re-points native_macro at the context on every method. The
            // owning handle is replaced only when it names another context, so a
            // method write does not touch the shared reference count.
            let same = match (&self.native_macro_context, &self.native_context) {
                (Some(current), Some(context)) => Arc::ptr_eq(current, context),
                _ => false,
            };
            if !same {
                self.native_macro_context = self.native_context.clone();
            }
            self.native_macro_compute = self.native_compute_uploaded;
        }
        if method as usize >= self.native_registers.len() {
            log::error!("Native compute method out of range: {method:#x}");
            return true;
        }
        // Borrow the owner instead of cloning it for every method. The handle is
        // only replaced by set_native_macro_mode (channel setup) or by the
        // re-pointing above, which is a no-op for the nested calls a macro emits
        // (same native_context), so the allocation outlives this guard.
        let owner = Arc::as_ptr(
            self.native_macro_context
                .as_ref()
                .expect("native compute requires macro owner"),
        );
        let this = self as *mut Self;
        // Upstream dereferences native_macro / native_macro_positions only on the paths
        // that use them (macro calls, code and bind uploads, failure flags); the shared
        // context is locked on exactly those paths. Most methods reaching here (upload
        // words, semaphores, QMD launches) never touch it, so they no longer lock it, and
        // the front may run macros while the back dispatches.
        //
        // Every `selected`/`positions` dereference below happens while a guard from
        // `lock_context` is alive in the same branch (the reentrant lock nests for the
        // register writes a macro emits). No RefMut spans callbacks.
        let lock_context = || unsafe { (*owner).lock() };
        let selected = |context: *mut crate::r#macro::NativeMacroContext| unsafe {
            if (*this).native_macro_compute {
                std::ptr::addr_of_mut!((*context).compute_engine)
            } else {
                std::ptr::addr_of_mut!((*context).engine)
            }
        };
        let positions = |context: *mut crate::r#macro::NativeMacroContext| unsafe {
            if (*this).native_macro_compute {
                std::ptr::addr_of_mut!((*context).compute_positions)
            } else {
                std::ptr::addr_of_mut!((*context).positions)
            }
        };
        // Mechanical byte-copy adaptation for the source's owner-local ofstream dumps.
        let dump_words = |path: String, words: &[u32]| {
            let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
            let _ = std::fs::write(path, bytes);
        };
        if method >= 0xe00 {
            if self.native_executing_macro.is_none() {
                if method & 1 != 0 {
                    log::error!("Native compute macro starts on argument method");
                    return true;
                }
                self.native_executing_macro = Some(method);
            }
            self.native_parameters.push(argument);
            if !is_last_call {
                return true;
            }
            let entry = ((self.native_executing_macro.unwrap() - 0xe00) / 2) as usize % 0x80;
            self.native_executing_macro_logged = self.native_executing_macro.take();
            static WATCH: OnceLock<u32> = OnceLock::new();
            let watch = *WATCH.get_or_init(|| {
                std::env::var("EDEN_NX2_SEM_WATCH")
                    .ok()
                    .and_then(|v| std::ffi::CString::new(v).ok())
                    .map_or(0, |v| unsafe {
                        libc::strtoull(v.as_ptr(), std::ptr::null_mut(), 16) as u32
                    })
            });
            let watched = watch != 0
                && self
                    .native_parameters
                    .iter()
                    .any(|parameter| parameter & !0xf == watch);
            let guard = lock_context();
            let context = guard.as_ptr();
            let start = unsafe { (*positions(context))[entry] };
            if watched {
                log::info!(
                    "Semaphore watch: compute macro start={start:#x} parameters={:08x?}",
                    self.native_parameters
                );
            }
            static MME_DUMP: OnceLock<bool> = OnceLock::new();
            let mme_dump =
                *MME_DUMP.get_or_init(|| std::env::var_os("EDEN_NX2_MME_DUMP").is_some());
            if mme_dump {
                if !self.native_mme_dumped && start == 0xda {
                    self.native_mme_dumped = true;
                    dump_words(format!("native-compute-mme-{this:p}.bin"), unsafe {
                        &(*selected(context)).native_code
                    });
                }
                log::info!(
                    "MME call engine {this:p} method {:#x} start={start:#x} parameters={:08x?}",
                    self.native_executing_macro_logged.unwrap(),
                    self.native_parameters
                );
            }
            let engine = selected(context);
            // Upstream assigns the active_dram pointer; the owning handle is only
            // replaced when it names other storage (no refcount churn per call).
            if let Some(ram) = &self.native_data_ram {
                unsafe {
                    let active = &mut (*engine).native_interpreter.active_dram;
                    if !active.as_ref().is_some_and(|current| Arc::ptr_eq(current, ram)) {
                        *active = Some(ram.clone());
                    }
                }
            }
            #[cfg(target_arch = "x86_64")]
            let use_jit = {
                static USE_JIT: OnceLock<bool> = OnceLock::new();
                *USE_JIT.get_or_init(|| {
                    std::env::var_os("EDEN_NX2_MME_JIT")
                        .is_none_or(|v| !v.to_string_lossy().starts_with('0'))
                }) && !watched
            };
            #[cfg(not(target_arch = "x86_64"))]
            let use_jit = false;
            // Lease interpreter and FIFO so Rust callbacks can observe live uploads
            // safely. As in MacroEngine::execute_native, the 12 KiB interpreter
            // (and its own RAM) is not leased when the JIT runs on active_dram:
            // the JIT only uses active_dram, so moving and zeroing the
            // interpreter on every compute macro call is avoided.
            let shared = unsafe { (*engine).native_interpreter.active_dram.is_some() };
            let mut interpreter = (!use_jit || !shared)
                .then(|| unsafe { std::mem::take(&mut (*engine).native_interpreter) });
            let mut parameters = std::mem::take(&mut self.native_parameters);
            let ram_owner = match &interpreter {
                Some(interpreter) => interpreter.active_dram.clone(),
                None => unsafe { (*engine).native_interpreter.active_dram.clone() },
            };
            let mut shared_ram = ram_owner.as_ref().map(|ram| ram.lock());
            let mut local_ram = interpreter
                .as_mut()
                .map(|interpreter| std::mem::replace(&mut interpreter.dram, [0; 0xc00]));
            let ram = match shared_ram.as_deref_mut() {
                Some(shared) => shared,
                None => local_ram
                    .as_mut()
                    .expect("the interpreter is leased when there is no active_dram"),
            };
            let dram0_before = ram[0x27];
            let mut emit = |address: u32,
                            value: u32,
                            _fifo: &mut Vec<u32>,
                            _ram: &mut crate::macro_turing::DataRam| {
                static WRITES: AtomicU32 = AtomicU32::new(0);
                static LOG_EMITS: OnceLock<bool> = OnceLock::new();
                let log_emits = *LOG_EMITS
                    .get_or_init(|| std::env::var_os("EDEN_NX2_SEM_WATCH_EMITS").is_some());
                if watched && log_emits {
                    log::info!("Semaphore watch: compute macro emit {address:#x} = {value:#x}");
                }
                if WRITES.fetch_add(1, Ordering::Relaxed) < 4096 {
                    log::debug!("Native compute MME emit method={address:#x}, value={value:#x}");
                }
                if address >= 0xe00 {
                    return false;
                }
                let _scope = crate::nx2_profile::Scope::new(crate::nx2_profile::Zone::Method);
                unsafe {
                    (*this).call_method(address, value, true);
                }
                true
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                #[cfg(target_arch = "x86_64")]
                {
                    if use_jit {
                        // native_jit is created once and never replaced while the
                        // engine (held by the context guard) lives: borrow it.
                        let jit = &*Arc::as_ptr((*engine).native_jit.get_or_insert_with(|| {
                            Arc::new(std::sync::Mutex::new(Default::default()))
                        }));
                        let mut jit = jit
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        jit.prepare(&(*engine).native_code, (*engine).native_code_generation)
                            .expect("native compute macro JIT preparation failed");
                        let result = jit.execute(
                            start,
                            &mut parameters,
                            ram,
                            |address| {
                                (*this)
                                    .native_registers
                                    .get(address as usize)
                                    .copied()
                                    .unwrap_or(0)
                            },
                            &mut emit,
                        );
                        if !result.success {
                            (*selected(context)).native_execution_failed = true;
                            log::error!("Native MME (JIT) failed: compute start={start:#x}, consumed={}/{}, error={}", result.consumed, parameters.len(), result.error);
                        }
                        return;
                    }
                }
                let interpreter = interpreter
                    .as_mut()
                    .expect("the interpreter is leased whenever the JIT is not used");
                static USE_MEMO: OnceLock<bool> = OnceLock::new();
                let use_memo = *USE_MEMO.get_or_init(|| {
                    std::env::var_os("EDEN_NX2_MME_MEMO")
                        .is_none_or(|v| !v.to_string_lossy().starts_with('0'))
                });
                if use_memo && !watched {
                    (*engine)
                        .native_memo
                        .synchronize((*engine).native_code_generation);
                    if let Some(effects) = (*engine).native_memo.find(start, &parameters, ram) {
                        for pair in effects.data_ram_writes.chunks_exact(2) {
                            ram[pair[0] as usize] = pair[1];
                        }
                        for pair in effects.writes.chunks_exact(2) {
                            emit(pair[0], pair[1], &mut parameters, ram);
                        }
                        return;
                    }
                    (*engine).native_writes.clear();
                }
                (*this).native_memo_parameters.clone_from(&parameters);
                let generation = (*engine).native_code_generation;
                let result = interpreter.execute_with_code_reader(
                    0x3000,
                    |base| {
                        let code = std::ptr::addr_of!((*engine).native_code).cast::<u32>();
                        [*code.add(base), *code.add(base + 1), *code.add(base + 2)]
                    },
                    start,
                    &mut parameters,
                    |address| {
                        (*this)
                            .native_registers
                            .get(address as usize)
                            .copied()
                            .unwrap_or(0)
                    },
                    |address, value, fifo, ram| {
                        if use_memo {
                            (*selected(context)).native_writes.push((address, value));
                        }
                        emit(address, value, fifo, ram)
                    },
                    Some(ram),
                );
                let current = selected(context);
                let state = if current == engine {
                    &*interpreter
                } else {
                    &(*current).native_interpreter
                };
                if use_memo
                    && !watched
                    && result.success
                    && !state.read_registers
                    && !state.reread_data_ram
                    && generation == (*current).native_code_generation
                {
                    (*engine).native_memo.store(
                        start,
                        &(*this).native_memo_parameters,
                        &state.data_ram_reads,
                        &state.data_ram_writes,
                        &(*current).native_writes,
                    );
                }
                static MME_DUMP_AFTER: OnceLock<bool> = OnceLock::new();
                if *MME_DUMP_AFTER.get_or_init(|| std::env::var_os("EDEN_NX2_MME_DUMP").is_some())
                    && ram[0x27] != dram0_before
                {
                    log::info!("MME data RAM[0x27] {dram0_before:#x} -> {:#x} by engine {this:p} macro start={:#x}", ram[0x27], (*positions(context))[entry]);
                }
                if !result.success {
                    (*current).native_execution_failed = true;
                    static DIAGNOSTICS: OnceLock<bool> = OnceLock::new();
                    if *DIAGNOSTICS
                        .get_or_init(|| std::env::var_os("EDEN_NATIVE_DRAW_DIAGNOSTICS").is_some())
                    {
                        dump_words("native-compute-mme.bin".into(), &(*current).native_code);
                    }
                    log::error!("Native MME failed: compute start={:#x}, pc={:#x}, consumed={}/{}, error={}, parameters={parameters:08x?}", (*positions(context))[entry], result.pc, result.consumed, parameters.len(), result.error);
                }
            }));
            if let Some(mut interpreter) = interpreter {
                if let Some(local_ram) = local_ram {
                    interpreter.dram = local_ram;
                }
                unsafe {
                    (*engine).native_interpreter = interpreter;
                }
            }
            parameters.clear();
            self.native_parameters = parameters;
            if let Err(payload) = result {
                std::panic::resume_unwind(payload);
            }
            return true;
        }
        self.native_registers[method as usize] = argument;
        if method == 0x6c3 {
            self.process_native_report_semaphore(argument);
            return true;
        }
        // Upstream reads memory_manager.GetID() without a lock; here it takes the
        // memory-manager lock (shared with the command-list reads), so it is read
        // only when the record is written: the 67 inline-QMD words of every
        // dispatch would otherwise lock it 67 times for a disabled trace.
        if (method == 0x5a || (0xc6..=0x108).contains(&method))
            && crate::native_query_trace::native_query_trace_enabled()
        {
            let asid = self.memory_manager.lock().get_id() as u64;
            trace_native_query(12, asid, method as u64, 0, 0, argument as u64, 0);
        }
        if method == 0x107 {
            let address = (((self.native_registers[0xc6] as u64) << 32)
                | self.native_registers[0xc7] as u64)
                .wrapping_shl(8);
            let program = ((self.native_registers[0xc8 + 49] as u64 & 0x1ffff) << 32)
                | self.native_registers[0xc8 + 48] as u64;
            let control = self.native_registers[0xa7];
            log::debug!("Native inline QMD: address={address:#x}, program={program:#x}, control={control:#x}, grid={}/{}/{}",self.native_registers[0xc8+12],self.native_registers[0xc8+13]&0xffff,self.native_registers[0xc8+14]&0xffff);
            static DIAGNOSTICS: OnceLock<bool> = OnceLock::new();
            if *DIAGNOSTICS
                .get_or_init(|| std::env::var_os("EDEN_NATIVE_DRAW_DIAGNOSTICS").is_some())
            {
                dump_words(
                    "native-inline-qmd.bin".into(),
                    &self.native_registers[0xc8..0x108],
                );
                dump_words(
                    "native-inline-compute-registers.bin".into(),
                    &self.native_registers,
                );
                if self
                    .memory_manager
                    .lock()
                    .is_fully_mapped_range(program, 0x10000)
                {
                    let mut code = vec![0u8; 0x10000];
                    self.memory_manager
                        .lock()
                        .read_block_unsafe(program, &mut code);
                    let _ = std::fs::write("native-inline-compute-shader.bin", code);
                }
            }
            if !self
                .memory_manager
                .lock()
                .is_fully_mapped_range(address, 256)
            {
                unsafe {
                    let guard = lock_context();
                    (*selected(guard.as_ptr())).native_execution_failed = true;
                }
                log::error!("Unmapped native inline QMD destination");
                return true;
            }
            // Upstream passes a byte span over the register words; no copy.
            let bytes: &[u8] = bytemuck::cast_slice(&self.native_registers[0xc8..0x108]);
            unsafe {
                self.rasterizer
                    .expect("native inline QMD requires rasterizer")
                    .as_mut()
            }
            .accelerate_inline_to_memory(address, 256, &bytes);
            if control & (1 << 24) != 0 || self.native_registers[0xcc] & (1 << 12) == 0 {
                let previous = self.native_registers[0xad];
                self.native_registers[0xad] = (address >> 8) as u32;
                self.process_native_method(0xb0, 3, true);
                self.native_registers[0xad] = previous;
            }
            return true;
        }
        if self.front_macros && (0x45..=0x48).contains(&method) {
            return true;
        }
        match method {
            0x45 => {
                unsafe {
                    let guard = lock_context();
                    (*selected(guard.as_ptr())).clear_code(argument);
                }
                true
            }
            0x46 => {
                unsafe {
                    let guard = lock_context();
                    (*selected(guard.as_ptr())).add_code(0, argument);
                }
                true
            }
            0x47 => {
                self.native_bind_pointer = argument;
                true
            }
            0x48 => {
                if self.native_bind_pointer < 0x80 {
                    unsafe {
                        let guard = lock_context();
                        (*positions(guard.as_ptr()))[self.native_bind_pointer as usize] = argument;
                    }
                    self.native_bind_pointer += 1;
                } else {
                    log::error!("Native compute macro bind out of bounds");
                }
                true
            }
            0xae | 0xaf | 0xb0 => {
                // As above: the ID and the translations are trace record arguments only.
                let trace = crate::native_query_trace::native_query_trace_enabled();
                let asid = if trace {
                    self.memory_manager.lock().get_id() as u64
                } else {
                    0
                };
                let address = (self.native_registers[0xad] as u64) << 8;
                if trace {
                    let cpu = self
                        .memory_manager
                        .lock()
                        .gpu_to_cpu_address(address)
                        .unwrap_or(0);
                    trace_native_query(9, asid, address, cpu, method, argument as u64, 0);
                }
                if method == 0xae
                    || (method == 0xaf && argument & 2 == 0)
                    || (method == 0xb0 && matches!(argument & 15, 0 | 1))
                {
                    return true;
                }
                static DIAGNOSTICS: OnceLock<bool> = OnceLock::new();
                let diagnostics = *DIAGNOSTICS
                    .get_or_init(|| std::env::var_os("EDEN_NATIVE_DRAW_DIAGNOSTICS").is_some());
                if argument & 15 == 12 && diagnostics {
                    static CAPTURED: AtomicU32 = AtomicU32::new(0);
                    if CAPTURED.fetch_add(1, Ordering::Relaxed) == 0
                        && self
                            .memory_manager
                            .lock()
                            .is_fully_mapped_range(address, 256)
                    {
                        let mut bytes = [0u8; 256];
                        self.memory_manager
                            .lock()
                            .read_block_unsafe(address, &mut bytes);
                        let words: Vec<u32> = bytes
                            .chunks_exact(4)
                            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                            .collect();
                        let _ = std::fs::write("native-increment-qmd.bin", bytes);
                        log::warn!("Native increment dependency descriptor: address={address:#x}, words={words:08x?}");
                    }
                }
                let memory = Arc::clone(&self.memory_manager);
                let rasterizer = self.rasterizer.expect("native QMD requires rasterizer");
                let mut scheduler = std::mem::take(&mut self.native_qmd);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    scheduler.signal_with_dispatch(address,argument,
                    |address,descriptor| {
                        if !memory.lock().is_fully_mapped_range(address,256) { return false; }
                        let mut bytes=[0u8;256]; memory.lock().read_block_unsafe(address,&mut bytes);
                        for (word,bytes) in descriptor.iter_mut().zip(bytes.chunks_exact(4)) { *word=u32::from_le_bytes(bytes.try_into().unwrap()); }
                        static LOGGED:AtomicU32=AtomicU32::new(0);
                        if descriptor[53]&(1<<23)!=0 && LOGGED.load(Ordering::Relaxed)<8 {
                            LOGGED.fetch_add(1,Ordering::Relaxed);
                            log::info!("Native QMD {address:#x} releases: {:08x?} | {:08x?} | {:08x?}",&descriptor[24..28],&descriptor[28..32],&descriptor[52..56]);
                        }
                        for slot in 0..2 {
                            if !trace {break;}
                            let flags=descriptor[25+slot*4]; if flags&(1<<23)==0 {continue;}
                            let destination=((flags as u64&0xff)<<32)|descriptor[24+slot*4] as u64;
                            let cpu=memory.lock().gpu_to_cpu_address(destination).unwrap_or(0);
                            trace_native_query(7+slot as u32,asid,destination,cpu,descriptor[4],descriptor[26+slot*4] as u64,flags);
                        }
                        true
                    }, crate::native_qmd::FlaggedRelease(|address:u64,payload:u32,flags:u32| {
                        if !memory.lock().is_fully_mapped_range(address,4) {return false;}
                        let rasterizer=unsafe{rasterizer.as_mut()};
                        // NVC6C0_QMDV03_00_RELEASEn word 1 (clc6c0qmd.h): bit 19 is
                        // MEMBAR_TYPE (FE_SYSMEMBAR), a memory barrier ahead of the
                        // release write; it does not change the value written. Only
                        // REDUCTION_ENABLE (bit 26) selects a reduction, and the
                        // scheduler accepts RED_ADD alone. Bit 19 used to be read
                        // here as a max reduction.
                        if flags&(1<<26)!=0 {
                            let memory=Arc::clone(&memory);
                            rasterizer.signal_fence(Box::new(move || {
                                let mut mm=memory.lock();
                                let old=mm.read::<u32>(address);
                                let value=old.wrapping_add(payload);
                                mm.write::<u32>(address,value);
                                let cpu=mm.gpu_to_cpu_address(address); drop(mm);
                                if let Some(cpu)=cpu {crate::nx2::Nx2SemaphoreWaiters::get().on_write(cpu,value);}
                                static WATCH:OnceLock<u64>=OnceLock::new();
                                let watch=*WATCH.get_or_init(||std::env::var("EDEN_NX2_SEM_WATCH").ok().and_then(|v|std::ffi::CString::new(v).ok()).map_or(0,|v|unsafe{libc::strtoull(v.as_ptr(),std::ptr::null_mut(),16) as u64}));
                                if watch!=0 && address&!0xf==watch {log::info!("Semaphore watch: QMD release add {address:#x} -> {value:#x}");}
                                trace_native_query(1,asid,address,cpu.unwrap_or(0),0,value as u64,1<<26);
                            }));
                        } else {
                            static WATCH:OnceLock<u64>=OnceLock::new();
                            let watch=*WATCH.get_or_init(||std::env::var("EDEN_NX2_SEM_WATCH").ok().and_then(|v|std::ffi::CString::new(v).ok()).map_or(0,|v|unsafe{libc::strtoull(v.as_ptr(),std::ptr::null_mut(),16) as u64}));
                            if watch!=0 && address&!0xf==watch&!0xf {log::info!("Semaphore watch: QMD release {address:#x} = {payload:#x} flags {flags:#x}");}
                            rasterizer.query(address,crate::query_cache::types::QueryType::Payload as u32,crate::query_cache::types::QueryPropertiesFlags::IS_A_FENCE,payload,0);
                        }
                        true
                    }), |qmd:&crate::native_qmd::Descriptor|unsafe{(*this).dispatch_native_compute(qmd)})
                }));
                self.native_qmd = scheduler;
                let error = match result {
                    Ok(error) => error,
                    Err(payload) => std::panic::resume_unwind(payload),
                };
                if error.is_empty() {
                    log::debug!("Native QMD control action={argument:#x}, QMD={address:#x}");
                    return true;
                }
                unsafe {
                    let guard = lock_context();
                    (*selected(guard.as_ptr())).native_execution_failed = true;
                }
                let mut qmd = [0u32; 128];
                let mapped = self
                    .memory_manager
                    .lock()
                    .is_fully_mapped_range(address, 512);
                if mapped {
                    let mut bytes = [0u8; 512];
                    self.memory_manager
                        .lock()
                        .read_block_unsafe(address, &mut bytes);
                    for (w, b) in qmd.iter_mut().zip(bytes.chunks_exact(4)) {
                        *w = u32::from_le_bytes(b.try_into().unwrap());
                    }
                }
                let program = ((qmd[49] as u64) << 32) | qmd[48] as u64;
                static FAILURES: OnceLock<Mutex<std::collections::HashMap<(u64, String), u32>>> =
                    OnceLock::new();
                let mut failures = FAILURES.get_or_init(Default::default).lock();
                let count = failures.entry((program, error.into())).or_default();
                let previous = *count;
                *count = count.wrapping_add(1);
                drop(failures);
                if previous >= 2 {
                    return true;
                }
                log::error!("Native QMD failed: {error}");
                if diagnostics && mapped {
                    dump_words("native-qmd.bin".into(), &qmd);
                    let guard = lock_context();
                    dump_words("native-dispatch-mme.bin".into(), unsafe {
                        &(*selected(guard.as_ptr())).native_code
                    });
                    drop(guard);
                    dump_words(
                        "native-dispatch-registers.bin".into(),
                        &self.native_registers,
                    );
                }
                log::error!("Native compute dispatch unsupported: method={method:#x}, value={argument:#x}, QMD={address:#x}, words={qmd:08x?}");
                true
            }
            _ => {
                if (method as usize) < KEPLER_COMPUTE_REG_COUNT {
                    self.regs[method as usize] = argument;
                }
                method != EXEC_UPLOAD && method != DATA_UPLOAD
            }
        }
    }

    /// Port of KeplerCompute::DispatchNativeCompute.
    fn dispatch_native_compute(&mut self, qmd: &crate::native_qmd::Descriptor) -> bool {
        use shader_recompiler::exception::{
            InvalidArgument, LogicError, NotImplementedException, RuntimeError, ShaderException,
        };
        // Assign into the original LaunchParams bitfields, including their narrower widths.
        self.launch_description = LaunchParams::default();
        self.launch_description.grid_dim_x = qmd[12] & 0x7fffffff;
        self.launch_description.grid_dim_y = qmd[13] & 0xffff;
        self.launch_description.grid_dim_z = qmd[14] & 0xffff;
        self.launch_description.shared_alloc = qmd[17] & 0x3ffff;
        self.launch_description.block_dim_x = qmd[18] >> 16;
        self.launch_description.block_dim_y = qmd[19] & 0xffff;
        self.launch_description.block_dim_z = qmd[19] >> 16;
        self.launch_description.const_buffer_enable_mask = qmd[20] & 0xff;
        self.launch_description.local_pos_alloc = qmd[23] & 0xfffff;
        self.launch_description.local_crs_alloc = qmd[50] & 0xfffff;
        self.regs[CODE_LOC_BASE as usize + 1] = qmd[48];
        self.regs[CODE_LOC_BASE as usize] = qmd[49] & 0x1ffff;
        for slot in 0..NUM_CONST_BUFFERS {
            let high = qmd[33 + slot * 2];
            if high & 0x1ff00 != 0 {
                log::error!("Native compute constant address exceeds supported GPU width");
                return false;
            }
            self.launch_description.const_buffers[slot] = ConstBufferConfig {
                address: ((high as u64 & 0xff) << 32) | qmd[32 + slot * 2] as u64,
                size: ((high >> 19) & 0x1fff) << 4,
            };
        }
        if self.launch_description.block_dim_x == 0
            || self.launch_description.block_dim_y == 0
            || self.launch_description.block_dim_z == 0
        {
            return false;
        }
        // Ocarina of Time's movie network (Lichii aae0b063f7, #721, port of Eglantine 92f119195f):
        // a pass that reads one colour image and stores none retains its colour and opens a skip
        // window; the pass that stores an image upscaling it is replaced by a plain copy and closes
        // it. The gameplay network is left alone: its pair (+0xc0 -> +0xd0) carries the tonemap.
        // Lichii also requires `!dlss_chain.active`; ruzu has no Fire Emblem DLSS chain, so that
        // term is always true here (DIFF.md).
        if !self.native_upscale_disabled
            && common::settings::get_current_program_id() == crate::oot_network::PROGRAM_ID
            && (self.launch_description.const_buffer_enable_mask & 1) != 0
        {
            use super::fermi_2d::{Config, Filter, MemoryLayout, Operation, Surface};
            use super::native_upscale::find_native_upscale_output;
            use crate::gpu::RenderTargetFormat as RTFormat;
            use crate::textures::texture::TextureFormat;

            let cbuf = self.launch_description.const_buffers[0].clone();
            let cbuf_address = cbuf.address;
            fn upscale_pair(from: &Option<TicEntry>, to: &Option<TicEntry>, same_format: bool) -> bool {
                let (Some(from), Some(to)) = (from, to) else {
                    return false;
                };
                if (same_format && from.format() != to.format())
                    || from.address() == 0
                    || to.address() == 0
                    || from.address() == to.address()
                    || !from.is_block_linear()
                    || !to.is_block_linear()
                {
                    return false;
                }
                let (from_width, from_height) = (u64::from(from.width()), u64::from(from.height()));
                let (to_width, to_height) = (u64::from(to.width()), u64::from(to.height()));
                let (cross_a, cross_b) = (to_width * from_height, to_height * from_width);
                from_width >= 64
                    && to_width > from_width
                    && to_width <= from_width * 4
                    && cross_a.abs_diff(cross_b) * 32 <= cross_a
            }
            let (reads, stores) = self.native_image_handles(self.code_address());
            let mut replacement_input: Option<TicEntry> = None;
            let mut replacement_output: Option<TicEntry> = None;
            {
                let image = |offset: u32| -> Option<TicEntry> {
                    if u64::from(offset) + 4 > u64::from(cbuf.size) {
                        return None;
                    }
                    let handle = self
                        .memory_manager
                        .lock()
                        .read::<u32>(cbuf_address + u64::from(offset));
                    if handle == 0 || (handle & 0xfffff) > self.tic_limit() {
                        return None;
                    }
                    Some(self.get_tic_entry(handle & 0xfffff))
                };
                if self.native_upscale_input.is_some()
                    && self.network_skip_remaining > 0
                    && !stores.is_empty()
                {
                    if let Some(store) = find_native_upscale_output(
                        &self.native_upscale_input,
                        &stores,
                        &image,
                        upscale_pair,
                    ) {
                        replacement_input = self.native_upscale_input;
                        replacement_output = image(store);
                    }
                }
            }
            let mut skipping = false;
            if reads.len() == 1 && stores.is_empty() && reads[0] == 0x58 {
                // Ocarina's movie network reads its colour at +0x58; anchoring the window there
                // keeps gameplay compute untouched.
                self.remember_native_upscale_input();
                if self.native_upscale_input.is_some() {
                    self.network_skip_remaining = 32;
                    skipping = true;
                }
            } else if replacement_input.is_none() && self.network_skip_remaining > 0 {
                self.network_skip_remaining -= 1;
                if self.network_skip_remaining == 0 {
                    self.native_upscale_input = None;
                }
                skipping = true;
            }
            if let (Some(input), Some(output)) = (replacement_input, replacement_output) {
                let surface = |tic: &TicEntry| -> Option<Surface> {
                    let format = match TextureFormat::from_raw(tic.format()) {
                        Some(TextureFormat::B10G11R11) => RTFormat::B10G11R11Float,
                        Some(TextureFormat::R16G16B16A16) => RTFormat::R16G16B16A16Float,
                        Some(TextureFormat::A8B8G8R8) => RTFormat::A8B8G8R8Unorm,
                        Some(TextureFormat::A2B10G10R10) => RTFormat::A2B10G10R10Unorm,
                        _ => return None,
                    };
                    if tic.address() == 0 || !tic.is_block_linear() {
                        return None;
                    }
                    Some(Surface {
                        format: format as u32,
                        linear: MemoryLayout::BlockLinear as u32,
                        block_dimensions: (tic.block_width() & 0xf)
                            | ((tic.block_height() & 0xf) << 4)
                            | ((tic.block_depth() & 0xf) << 8),
                        depth: 1,
                        width: tic.width(),
                        height: tic.height(),
                        addr_upper: (tic.address() >> 32) as u32,
                        addr_lower: tic.address() as u32,
                        ..Surface::default()
                    })
                };
                if let (Some(source), Some(dest)) = (surface(&input), surface(&output)) {
                    // Does the program decode sRGB itself? (Ocarina's movie network does.)
                    let decodes_srgb = {
                        let program = self.code_address();
                        let mut bytes = [0u8; 0x400 * 4];
                        let (mut threshold, mut exponent) = (false, false);
                        let mut offset = 0u64;
                        while offset < 0x20000 && !(threshold && exponent) {
                            let memory = self.memory_manager.lock();
                            if !memory.is_fully_mapped_range(program + offset, bytes.len() as u64) {
                                break;
                            }
                            memory.read_block(program + offset, &mut bytes);
                            drop(memory);
                            let word = |index: usize| {
                                u32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap())
                            };
                            for index in (0..0x400).step_by(4) {
                                let opcode = word(index) & 0xfff;
                                threshold |= opcode == 0x434 && word(index + 1) == 0x292d292d;
                                exponent |= opcode == 0x832 && word(index + 1) == 0x40cc40cc;
                            }
                            offset += bytes.len() as u64;
                        }
                        threshold && exponent
                    };
                    let config = Config {
                        operation: if decodes_srgb {
                            Operation::SrcCopySrgbToLinear
                        } else {
                            Operation::SrcCopy
                        },
                        filter: Filter::Bilinear,
                        must_accelerate: true,
                        dst_x0: 0,
                        dst_y0: 0,
                        dst_x1: output.width() as i32,
                        dst_y1: output.height() as i32,
                        src_x0: 0,
                        src_y0: 0,
                        src_x1: input.width() as i32,
                        src_y1: input.height() as i32,
                    };
                    let rasterizer = self
                        .rasterizer
                        .map(|handle| unsafe { handle.as_mut() })
                        .expect("native compute requires a rasterizer");
                    let done = rasterizer.accelerate_surface_copy(&source, &dest, &config);
                    log::debug!(
                        "Native network pass {:#x} replaced by input copy {}->{} ({}): {}",
                        self.code_address(),
                        input.address(),
                        output.address(),
                        if decodes_srgb { "sRGB" } else { "plain" },
                        done
                    );
                    self.native_upscale_input = None;
                    self.network_skip_remaining = 0;
                    if done {
                        return true;
                    }
                    // Run the real pass, and the whole network on later frames.
                    self.native_upscale_disabled = true;
                    skipping = false;
                }
            }
            if skipping {
                return true;
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Existing Rust rasterizer adapter snapshots the bound engine's launch state.
            let dispatch = DispatchCall {
                launch_description: self.launch_description.clone(),
                launch_desc_loc: self.launch_desc_address(),
                indirect_compute_address: self.indirect_compute,
                code_address: self.code_address(),
                tsc_address: self.tsc_address(),
                tsc_limit: self.tsc_limit(),
                tic_address: self.tic_address(),
                tic_limit: self.tic_limit(),
                tex_cb_index: self.tex_cb_index(),
            };
            let rasterizer = self
                .rasterizer
                .map(|handle| unsafe { handle.as_mut() })
                .expect("native compute requires a rasterizer");
            rasterizer.dispatch_compute(&dispatch);
        }));
        if let Err(payload) = result {
            // Catch only Shader::Exception descendants, never general Rust failures.
            let message = if let Some(error) = payload.downcast_ref::<ShaderException>() {
                error.to_string()
            } else if let Some(error) = payload.downcast_ref::<LogicError>() {
                error.to_string()
            } else if let Some(error) = payload.downcast_ref::<RuntimeError>() {
                error.to_string()
            } else if let Some(error) = payload.downcast_ref::<NotImplementedException>() {
                error.to_string()
            } else if let Some(error) = payload.downcast_ref::<InvalidArgument>() {
                error.to_string()
            } else {
                std::panic::resume_unwind(payload)
            };
            static LOGGED: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
            if LOGGED
                .get_or_init(Default::default)
                .lock()
                .insert(message.clone())
            {
                log::error!("Native compute shader rejected: {message}");
            }
            if message.contains("matrix multiply") {
                self.describe_native_resources(qmd);
                log::error!(
                    "Native tensor dispatch is unsupported; no validated replacement was available"
                );
            }
            return false;
        }
        true
    }

    /// Port of KeplerCompute::DescribeNativeResources.
    fn describe_native_resources(&self, qmd: &crate::native_qmd::Descriptor) {
        static DESCRIBED: AtomicU32 = AtomicU32::new(0);
        if DESCRIBED.fetch_add(1, Ordering::Relaxed) >= 2 {
            return;
        }
        let cbuf = &self.launch_description.const_buffers[0];
        log::info!("Native resources: cbuf0 {:#x} size {:#x}, grid {}x{}x{}, block {}x{}x{}, tic pool {:#x}",
            cbuf.address, cbuf.size, qmd[12], qmd[13] & 0xffff, qmd[14] & 0xffff,
            self.launch_description.block_dim_x, self.launch_description.block_dim_y,
            self.launch_description.block_dim_z, self.tic_address());
        for offset in (0x80..0x120).step_by(4) {
            let handle = self
                .memory_manager
                .lock()
                .read::<u32>(cbuf.address + offset);
            let tic_index = handle & 0xfffff;
            if handle == 0 || tic_index > self.tic_limit() {
                continue;
            }
            let tic = self.get_tic_entry(tic_index);
            log::info!("  handle @{offset:#x} = {handle:#x}: tic {tic_index} address {:#x} {}x{} format {:#x} types {}/{}/{}/{}",
                tic.address(), tic.width(), tic.height(), tic.format(), tic.r_type(), tic.g_type(), tic.b_type(), tic.a_type());
        }
    }

    /// Port of Lichii `KeplerCompute::NativeImageHandles` (aae0b063f7): the cbuf0 byte offsets of
    /// the texture handles a native program reads (TEX, TLD, TLD4, TXD) and the surface handles it
    /// stores to (SUST.P), from its code up to the first unconditional EXIT. Cached per program
    /// address for the process lifetime, like upstream's function-local static.
    fn native_image_handles(&self, program: u64) -> (Vec<u32>, Vec<u32>) {
        static CACHE: OnceLock<Mutex<std::collections::HashMap<u64, (Vec<u32>, Vec<u32>)>>> =
            OnceLock::new();
        let cache = CACHE.get_or_init(Default::default);
        if let Some(handles) = cache.lock().get(&program) {
            return handles.clone();
        }
        let mut handles: (Vec<u32>, Vec<u32>) = (Vec::new(), Vec::new());
        let add = |list: &mut Vec<u32>, offset: u32| {
            if !list.contains(&offset) {
                list.push(offset);
            }
        };
        let mut bytes = [0u8; 0x400 * 4];
        let mut exited = false;
        let mut offset = 0u64;
        while offset < 0x40000 && !exited {
            let memory = self.memory_manager.lock();
            if !memory.is_fully_mapped_range(program + offset, bytes.len() as u64) {
                break;
            }
            memory.read_block(program + offset, &mut bytes);
            drop(memory);
            let word = |index: usize| {
                u32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap())
            };
            let mut index = 0;
            while index < 0x400 && !exited {
                let opcode = word(index) & 0xfff;
                let binding = (word(index + 1) >> 22) & 0x1f;
                let handle = ((word(index + 1) >> 8) & 0x3fff) * 4;
                match opcode {
                    0xb60 | 0xb63 | 0xb66 | 0xb6d => {
                        if binding == 0 {
                            add(&mut handles.0, handle);
                        }
                    }
                    0xb9b => {
                        if binding == 0 {
                            add(&mut handles.1, handle);
                        }
                    }
                    // EXIT, unconditional when guarded by PT
                    0x94d => exited = ((word(index) >> 12) & 0xf) == 7,
                    _ => {}
                }
                index += 4;
            }
            offset += bytes.len() as u64;
        }
        cache.lock().entry(program).or_insert(handles).clone()
    }

    /// Port of Lichii `KeplerCompute::RememberNativeUpscaleInput` (aae0b063f7).
    fn remember_native_upscale_input(&mut self) {
        self.native_upscale_input = None;
        let (reads, stores) = self.native_image_handles(self.code_address());
        // An image-fed matrix pass writes its intermediate results to buffers. Retain its colour
        // input: the final pass's sampled image is another intermediate that a skipped network
        // never produced (Ocarina's movie network reads its colour at +0x58).
        if reads.len() != 1 || !stores.is_empty() {
            return;
        }
        let cbuf = &self.launch_description.const_buffers[0];
        if u64::from(reads[0]) + 4 > u64::from(cbuf.size) {
            return;
        }
        let address = cbuf.address;
        let handle = self
            .memory_manager
            .lock()
            .read::<u32>(address + u64::from(reads[0]));
        if handle == 0 || (handle & 0xfffff) > self.tic_limit() {
            return;
        }
        let input = self.get_tic_entry(handle & 0xfffff);
        if input.address() != 0
            && input.is_block_linear()
            && input.width() >= 64
            && input.height() >= 64
        {
            self.native_upscale_input = Some(input);
        }
    }

    /// Port of ProcessNativeReportSemaphore: SET_REPORT_SEMAPHORE_A-D.
    fn process_native_report_semaphore(&mut self, argument: u32) {
        use crate::query_cache::types::{QueryPropertiesFlags, QueryType};
        use std::sync::atomic::{AtomicU32, Ordering};

        let address = (u64::from(self.native_registers[0x6c0] & 0x1ffffff) << 32)
            | u64::from(self.native_registers[0x6c1]);
        let payload = self.native_registers[0x6c2];
        let operation = argument & 3;
        let one_word = (argument >> 28) & 1 != 0;
        static LOGGED: AtomicU32 = AtomicU32::new(0);
        if LOGGED.fetch_add(1, Ordering::Relaxed) < 32 {
            log::info!("Native compute report semaphore {address:#x} = {payload:#x}, control {argument:#x}");
        }
        if operation != 0 && operation != 2 {
            log::warn!("Native compute report semaphore operation {operation} not supported");
            return;
        }
        let rasterizer = self
            .rasterizer
            .map(|handle| unsafe { handle.as_mut() })
            .expect("native compute semaphore requires a rasterizer");
        if (argument >> 3) & 1 != 0 {
            let reduction = (argument >> 9) & 7;
            // Keep the exact memory owner alive until the fence. The source
            // captures this solely to reach memory_manager; no engine state is
            // read by the callback, and the value must be read at fence time.
            let memory = Arc::clone(&self.memory_manager);
            rasterizer.signal_fence(Box::new(move || {
                let mut memory = memory.lock();
                let old_value = memory.read::<u32>(address);
                let value = match reduction {
                    0 => old_value.wrapping_add(payload),
                    1 => old_value.min(payload),
                    2 => old_value.max(payload),
                    3 => {
                        if old_value >= payload {
                            0
                        } else {
                            old_value.wrapping_add(1)
                        }
                    }
                    4 => {
                        if old_value == 0 || old_value > payload {
                            payload
                        } else {
                            old_value - 1
                        }
                    }
                    5 => old_value & payload,
                    6 => old_value | payload,
                    _ => old_value ^ payload,
                };
                memory.write::<u32>(address, value);
            }));
            return;
        }
        rasterizer.query(
            address,
            QueryType::Payload as u32,
            if one_word {
                QueryPropertiesFlags::IS_A_FENCE
            } else {
                QueryPropertiesFlags::HAS_TIMEOUT
            },
            payload,
            0,
        );
    }

    /// Corresponds to upstream `KeplerCompute::CallMethod`.
    pub fn call_method(&mut self, method: u32, argument: u32, _is_last_call: bool) {
        let idx = method as usize;
        if self.native_mode {
            if idx < self.native_plain_methods.len() && self.native_plain_methods[idx] {
                self.native_registers[idx] = argument;
                if idx < KEPLER_COMPUTE_REG_COUNT {
                    self.regs[idx] = argument;
                }
                return;
            }
            if self.process_native_method(method, argument, _is_last_call) {
                return;
            }
        }
        if idx >= KEPLER_COMPUTE_REG_COUNT {
            log::error!("Compute register out of range: {method:#x}");
            return;
        }
        self.regs[idx] = argument;

        match method {
            EXEC_UPLOAD => {
                let regs = self.upload_registers();
                let info = UploadInfo {
                    upload_address: self.upload_address,
                    exec_address: self.upload_state.exec_target_address(&regs),
                    copy_size: self.upload_state.get_upload_size(),
                    was_dirty: self.upload_dirty,
                };
                self.uploads.push(info);
                self.upload_state
                    .process_exec(&regs, self.exec_upload_linear());
            }
            DATA_UPLOAD => {
                self.upload_address = self.interface_state.current_dma_segment;
                self.upload_dirty = self.interface_state.current_dirty;
                self.interface_state.current_dirty = false;
                let regs = self.upload_registers();
                self.upload_state
                    .process_data_word(&regs, argument, _is_last_call);
            }
            LAUNCH => {
                let qmd_addr = self.launch_desc_address();
                for data in &self.uploads {
                    let offset = data.exec_address.wrapping_sub(qmd_addr);
                    if offset / std::mem::size_of::<u32>() as u64 == LAUNCH_GRID_DIM_X_INDEX {
                        let source_dirty = self
                            .memory_manager
                            .lock()
                            .is_memory_dirty(data.upload_address, data.copy_size as u64);
                        if data.was_dirty || source_dirty {
                            self.indirect_compute = Some(data.upload_address);
                        }
                    }
                }
                self.uploads.clear();
                self.process_launch();
                self.indirect_compute = None;
            }
            _ => {}
        }
    }

    /// Corresponds to upstream `KeplerCompute::CallMultiMethod`.
    pub fn call_multi_method(
        &mut self,
        method: u32,
        args: &[u32],
        amount: u32,
        methods_pending: u32,
    ) {
        assert!(
            (amount as usize) <= args.len(),
            "KeplerCompute multi-method amount exceeds the argument span"
        );
        if self.native_mode {
            for (i, &arg) in args.iter().take(amount as usize).enumerate() {
                self.call_method(method, arg, methods_pending.wrapping_sub(i as u32) <= 1);
            }
            return;
        }
        if method == DATA_UPLOAD {
            self.upload_address = self.interface_state.current_dma_segment;
            self.upload_dirty = self.interface_state.current_dirty;
            self.interface_state.current_dirty = false;
            let regs = self.upload_registers();
            self.upload_state
                .process_data_multi(&regs, &args[..amount as usize]);
            return;
        }

        for (i, &arg) in args[..amount as usize].iter().enumerate() {
            self.call_method(method, arg, methods_pending.wrapping_sub(i as u32) <= 1);
        }
    }

    /// Corresponds to `KeplerCompute::BindRasterizer`.
    pub fn bind_rasterizer(&mut self, rasterizer: &dyn RasterizerInterface) {
        self.rasterizer = Some(RasterizerHandle::from_ref(rasterizer));
        self.upload_state.bind_rasterizer(rasterizer);
    }

    /// Corresponds to upstream `KeplerCompute::ProcessLaunch`.
    fn process_launch(&mut self) {
        let qmd_addr = self.launch_desc_address();
        let mut raw = LaunchParamsLayout::default();
        self.memory_manager
            .lock()
            .read_block_unsafe(qmd_addr, raw.as_bytes_mut());
        self.launch_description = LaunchParams::from_words(raw.words());

        let dispatch = DispatchCall {
            launch_description: self.launch_description.clone(),
            launch_desc_loc: qmd_addr,
            indirect_compute_address: self.indirect_compute,
            code_address: self.code_address(),
            tsc_address: self.tsc_address(),
            tsc_limit: self.tsc_limit(),
            tic_address: self.tic_address(),
            tic_limit: self.tic_limit(),
            tex_cb_index: self.tex_cb_index(),
        };
        log::debug!(
            "KeplerCompute: dispatch grid=({},{},{}) block=({},{},{}) code=0x{:X}",
            dispatch.launch_description.grid_dim_x,
            dispatch.launch_description.grid_dim_y,
            dispatch.launch_description.grid_dim_z,
            dispatch.launch_description.block_dim_x,
            dispatch.launch_description.block_dim_y,
            dispatch.launch_description.block_dim_z,
            dispatch.code_address,
        );
        // SAFETY: ChannelState binds the single renderer owner before GPU
        // execution and the GPU thread serializes all engine callbacks. This
        // is the Rust expression of upstream's non-owning mutable pointer.
        let rasterizer = self
            .rasterizer
            .map(|handle| unsafe { handle.as_mut() })
            .expect("KeplerCompute must be bound to a rasterizer before launch");
        rasterizer.dispatch_compute(&dispatch);
    }

    // ── Register accessors ──────────────────────────────────────────────

    /// GPU VA of the QMD descriptor (regs[0xAD] << 8).
    pub fn launch_desc_address(&self) -> u64 {
        (self.regs[LAUNCH_DESC_LOC as usize] as u64) << 8
    }

    fn upload_registers(&self) -> engine_upload::Registers {
        engine_upload::Registers {
            line_length_in: self.regs[UPLOAD_REG_OFFSET as usize],
            line_count: self.regs[(UPLOAD_REG_OFFSET + 1) as usize],
            dest: engine_upload::DestRegisters {
                address_high: self.regs[(UPLOAD_REG_OFFSET + 2) as usize],
                address_low: self.regs[(UPLOAD_REG_OFFSET + 3) as usize],
                pitch: self.regs[(UPLOAD_REG_OFFSET + 4) as usize],
                block_dims: self.regs[(UPLOAD_REG_OFFSET + 5) as usize],
                width: self.regs[(UPLOAD_REG_OFFSET + 6) as usize],
                height: self.regs[(UPLOAD_REG_OFFSET + 7) as usize],
                depth: self.regs[(UPLOAD_REG_OFFSET + 8) as usize],
                layer: self.regs[(UPLOAD_REG_OFFSET + 9) as usize],
                x: self.regs[(UPLOAD_REG_OFFSET + 10) as usize],
                y: self.regs[(UPLOAD_REG_OFFSET + 11) as usize],
            },
        }
    }

    fn exec_upload_linear(&self) -> bool {
        (self.regs[EXEC_UPLOAD as usize] & 1) != 0
    }

    /// Shader code base address from CODE_LOC_BASE registers.
    pub fn code_address(&self) -> u64 {
        let high = self.regs[CODE_LOC_BASE as usize] as u64;
        let low = self.regs[(CODE_LOC_BASE + 1) as usize] as u64;
        (high << 32) | low
    }

    /// Texture sampler pool address.
    pub fn tsc_address(&self) -> u64 {
        let high = self.regs[TSC_BASE as usize] as u64;
        let low = self.regs[(TSC_BASE + 1) as usize] as u64;
        (high << 32) | low
    }

    /// Texture sampler pool limit.
    pub fn tsc_limit(&self) -> u32 {
        self.regs[(TSC_BASE + 2) as usize]
    }

    /// Texture image pool address.
    pub fn tic_address(&self) -> u64 {
        let high = self.regs[TIC_BASE as usize] as u64;
        let low = self.regs[(TIC_BASE + 1) as usize] as u64;
        (high << 32) | low
    }

    /// Texture image pool limit.
    pub fn tic_limit(&self) -> u32 {
        self.regs[(TIC_BASE + 2) as usize]
    }

    /// Texture constant buffer index.
    pub fn tex_cb_index(&self) -> u32 {
        self.regs[TEX_CB_INDEX as usize]
    }

    /// Port of upstream `KeplerCompute::GetIndirectComputeAddress`.
    pub fn get_indirect_compute_address(&self) -> Option<u64> {
        self.indirect_compute
    }

    /// Port of upstream `KeplerCompute::GetTICEntry`.
    #[allow(dead_code)]
    fn get_tic_entry(&self, tic_index: u32) -> TicEntry {
        let tic_address_gpu = self
            .tic_address()
            .wrapping_add((tic_index as u64).wrapping_mul(std::mem::size_of::<TicEntry>() as u64));
        let mut bytes = [0u8; std::mem::size_of::<TicEntry>()];
        self.memory_manager
            .lock()
            .read_block_unsafe(tic_address_gpu, &mut bytes);
        unsafe { std::ptr::read_unaligned(bytes.as_ptr() as *const TicEntry) }
    }

    /// Port of upstream `KeplerCompute::GetTSCEntry`.
    #[allow(dead_code)]
    fn get_tsc_entry(&self, tsc_index: u32) -> TscEntry {
        let tsc_address_gpu = self
            .tsc_address()
            .wrapping_add((tsc_index as u64).wrapping_mul(std::mem::size_of::<TscEntry>() as u64));
        let mut bytes = [0u8; std::mem::size_of::<TscEntry>()];
        self.memory_manager
            .lock()
            .read_block_unsafe(tsc_address_gpu, &mut bytes);
        unsafe { std::ptr::read_unaligned(bytes.as_ptr() as *const TscEntry) }
    }

    /// Port of reading upstream `launch_description`.
    pub fn launch_description(&self) -> &LaunchParams {
        &self.launch_description
    }
}

#[cfg(test)]
impl Default for KeplerCompute {
    fn default() -> Self {
        Self::new(Arc::new(Mutex::new(MemoryManager::new(0))))
    }
}

impl EngineInterface for KeplerCompute {
    fn call_method(&mut self, method: u32, method_argument: u32, is_last_call: bool) {
        KeplerCompute::call_method(self, method, method_argument, is_last_call);
    }

    fn call_multi_method(
        &mut self,
        method: u32,
        base_start: &[u32],
        amount: u32,
        methods_pending: u32,
    ) {
        KeplerCompute::call_multi_method(self, method, base_start, amount, methods_pending);
    }

    fn consume_sink_impl(&mut self) {
        for &(method, value) in &self.interface_state.method_sink {
            let idx = method as usize;
            if idx < KEPLER_COMPUTE_REG_COUNT {
                self.regs[idx] = value;
            }
        }
        self.interface_state.method_sink.clear();
    }

    fn interface_state(&self) -> &super::engine_interface::EngineInterfaceState {
        &self.interface_state
    }

    fn interface_state_mut(&mut self) -> &mut super::engine_interface::EngineInterfaceState {
        &mut self.interface_state
    }

}

#[cfg(test)]
impl KeplerCompute {
    fn write_reg(&mut self, method: u32, value: u32) {
        if method != LAUNCH {
            log::trace!("KeplerCompute: reg[0x{:X}] = 0x{:X}", method, value);
        }
        self.call_method(method, value, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::draw_manager::{Maxwell3DClearView, Maxwell3DDrawView};
    use crate::engines::fermi_2d::{Config as Fermi2DConfig, Surface as Fermi2DSurface};
    use crate::query_cache::types::QueryPropertiesFlags;
    use crate::rasterizer_interface::RasterizerDownloadArea;
    use std::cell::{Cell, RefCell};

    fn new_test_engine() -> KeplerCompute {
        KeplerCompute::new(Arc::new(Mutex::new(MemoryManager::new(0))))
    }

    #[cfg(target_arch = "x86_64")]
    fn front_instruction(fields: &[(usize, usize, u32)]) -> [u32; 3] {
        let mut bits = 0u128;
        for low in [15, 20, 25, 51, 56, 61] { bits |= 24u128 << low; }
        for &(low, count, value) in fields {
            let mask = ((1u128 << count) - 1) << low;
            bits = (bits & !mask) | (((value as u128) << low) & mask);
        }
        [(bits >> 64) as u32, (bits >> 32) as u32, bits as u32]
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn front_macro_executes_with_shared_or_local_ram_and_ordered_backend_writes() {
        for shared in [false, true] {
            let context = Arc::new(parking_lot::ReentrantMutex::new(RefCell::new(
                crate::r#macro::NativeMacroContext::default(),
            )));
            {
                let guard = context.lock();
                let mut state = guard.borrow_mut();
                state.engine.set_native_turing(true);
                state.compute_engine.set_native_turing(true);
                state.compute_engine.native_interpreter.dram[0x28] = 0xabcdef;
            }
            let ram = Arc::new(Mutex::new([0; 0xc00]));
            ram.lock()[0x28] = 0xabcdef;
            let mut engine = Box::new(new_test_engine());
            engine.set_native_macro_mode(Some(context.clone()), Some(context.clone()));
            if shared { engine.set_native_data_ram(Some(ram.clone())); }
            let handle = EngineHandle::from_ref(&*engine);
            let split = crate::gpu_split::GpuSplitFrontend::new(true);
            let front = unsafe { KeplerCompute::front(&mut *engine, &split) };
            // The engine allocation stays alive and unborrowed until stop joins.
            split.start_back(|| {});
            {
                let mut fs = unsafe { front.get(&split) };
                KeplerCompute::front_call_method(&mut fs, handle, &split, 0x45, 0, true, None);
                let code = [
                    front_instruction(&[(10, 5, 31), (20, 5, 25), (30, 16, 0x27),
                        (25, 5, 26), (66, 16, 42)]),
                    front_instruction(&[(0, 1, 1), (10, 5, 30), (20, 5, 25),
                        (30, 16, 0x27), (66, 16, 0x200), (82, 3, 6), (85, 4, 1)]),
                    front_instruction(&[]),
                ].concat();
                KeplerCompute::front_call_multi_method(&mut fs, handle, &split,
                    0x46, &code, code.len() as u32, code.len() as u32, None);
                KeplerCompute::front_call_multi_method(&mut fs, handle, &split,
                    0x200, &[13, 17], 2, 2, Some(0x1234));
                KeplerCompute::front_call_method(&mut fs, handle, &split, 0xe01, 99, true, None);
                assert!(fs.parameters.is_empty());
                assert_eq!(fs.executing_macro, None);
                KeplerCompute::front_call_method(&mut fs, handle, &split, 0xe00, 7, false, None);
                assert_eq!(fs.registers[0x200], 17);
                assert_eq!(fs.parameters, [7]);
                KeplerCompute::front_call_multi_method(&mut fs, handle, &split,
                    0xe01, &[8, 9], 2, 2, None);
                assert!(fs.parameters.is_empty());
                assert_eq!(fs.executing_macro, None);
                assert_eq!(fs.registers[0x200], 42);
            }
            split.drain();
            split.stop().unwrap();
            assert_eq!(engine.native_registers[0x200], 42);
            assert!(engine.native_parameters.is_empty());
            let guard = context.lock();
            let state = guard.borrow();
            assert_eq!(state.compute_engine.native_code_generation, 9);
            assert!(!state.compute_engine.native_execution_failed);
            if shared {
                assert_eq!(&ram.lock()[0x27..=0x28], &[42, 0xabcdef]);
            } else {
                assert_eq!(&state.compute_engine.native_interpreter.dram[0x27..=0x28],
                    &[42, 0xabcdef]);
            }
        }
    }

    // Backend (non-split) counterpart: the interpreter is no longer leased when
    // the JIT runs on active_dram, and still is for the engine-owned RAM.
    #[test]
    #[cfg(target_arch = "x86_64")]
    fn native_macro_updates_shared_or_engine_ram_without_leasing_for_jit() {
        for shared in [false, true] {
            let context = Arc::new(parking_lot::ReentrantMutex::new(RefCell::new(
                crate::r#macro::NativeMacroContext::default(),
            )));
            {
                let guard = context.lock();
                let mut state = guard.borrow_mut();
                state.engine.set_native_turing(true);
                state.compute_engine.set_native_turing(true);
                state.compute_engine.native_interpreter.dram[0x28] = 0xabcdef;
            }
            let ram = Arc::new(Mutex::new([0; 0xc00]));
            ram.lock()[0x28] = 0xabcdef;
            let mut engine = Box::new(new_test_engine());
            engine.set_native_macro_mode(Some(context.clone()), Some(context.clone()));
            if shared { engine.set_native_data_ram(Some(ram.clone())); }
            engine.call_method(0x45, 0, true);
            let code = [
                front_instruction(&[(10, 5, 31), (20, 5, 25), (30, 16, 0x27),
                    (25, 5, 26), (66, 16, 42)]),
                front_instruction(&[(0, 1, 1), (10, 5, 30), (20, 5, 25),
                    (30, 16, 0x27), (66, 16, 0x200), (82, 3, 6), (85, 4, 1)]),
                front_instruction(&[]),
            ].concat();
            engine.call_multi_method(0x46, &code, code.len() as u32, code.len() as u32);
            engine.call_multi_method(0x200, &[13, 17], 2, 2);
            assert_eq!(engine.native_registers[0x200], 17);
            engine.call_method(0xe00, 7, false);
            engine.call_multi_method(0xe01, &[8, 9], 2, 2);
            assert!(engine.native_parameters.is_empty());
            assert_eq!(engine.native_registers[0x200], 42);
            let guard = context.lock();
            let state = guard.borrow();
            assert!(!state.compute_engine.native_execution_failed);
            assert_eq!(state.compute_engine.native_jit.is_some(),
                std::env::var_os("EDEN_NX2_MME_JIT")
                    .is_none_or(|v| !v.to_string_lossy().starts_with('0')));
            if shared {
                assert_eq!(&ram.lock()[0x27..=0x28], &[42, 0xabcdef]);
                assert_eq!(state.compute_engine.native_interpreter.dram[0x27], 0);
            } else {
                assert_eq!(&state.compute_engine.native_interpreter.dram[0x27..=0x28],
                    &[42, 0xabcdef]);
            }
        }
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn front_macro_reads_front_registers_before_backend_execution() {
        let context = Arc::new(parking_lot::ReentrantMutex::new(RefCell::new(
            crate::r#macro::NativeMacroContext::default(),
        )));
        {
            let guard = context.lock();
            let mut state = guard.borrow_mut();
            state.engine.set_native_turing(true);
            let code = [
                front_instruction(&[(10, 5, 22), (20, 5, 25), (30, 16, 0x201),
                    (66, 16, 0x202), (82, 3, 6), (85, 4, 1)]),
                front_instruction(&[(0, 1, 1), (10, 5, 22), (20, 5, 25),
                    (30, 16, 0x1234), (66, 16, 0x203), (82, 3, 6), (85, 4, 1)]),
                front_instruction(&[]),
            ].concat();
            for word in code { state.engine.add_code(0, word); }
        }
        let mut engine = Box::new(new_test_engine());
        engine.set_native_macro_mode(Some(context.clone()), Some(context));
        engine.native_registers[0x201] = 9;
        engine.native_registers[0x203] = 99;
        let handle = EngineHandle::from_ref(&*engine);
        let split = crate::gpu_split::GpuSplitFrontend::new(true);
        let front = unsafe { KeplerCompute::front(&mut *engine, &split) };
        // Start from a fresh ring but hold registration before command execution.
        // On assertion failure, dropping go also releases the waiting worker.
        let (go, wait) = std::sync::mpsc::channel();
        split.start_back(move || { let _ = wait.recv(); });
        {
            let mut fs = unsafe { front.get(&split) };
            KeplerCompute::front_call_method(&mut fs, handle, &split, 0x201, 81, true, None);
            KeplerCompute::front_call_method(&mut fs, handle, &split, 0xe00, 0, true, None);
            assert_eq!(fs.registers[0x202], 81);
            assert_eq!(fs.registers[0x203], 0);
        }
        assert_eq!(engine.native_registers[0x201], 9);
        assert_eq!(engine.native_registers[0x202], 0);
        assert_eq!(engine.native_registers[0x203], 99);
        go.send(()).unwrap();
        split.drain();
        split.stop().unwrap();
        assert_eq!(engine.native_registers[0x201], 81);
        assert_eq!(engine.native_registers[0x202], 81);
        assert_eq!(engine.native_registers[0x203], 0);
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn front_macro_rejects_recursive_emits_and_reports_failure_on_selected_engine() {
        let context = Arc::new(parking_lot::ReentrantMutex::new(RefCell::new(
            crate::r#macro::NativeMacroContext::default(),
        )));
        {
            let guard = context.lock();
            let mut state = guard.borrow_mut();
            state.engine.set_native_turing(true);
            state.compute_engine.set_native_turing(true);
            let code = [
                // Switch the frontend selector during execution. Failure must
                // still be reported on the macro that began this execution.
                front_instruction(&[(30, 16, 0x45), (66, 16, 0),
                    (82, 3, 5), (85, 4, 6)]),
                front_instruction(&[(0, 1, 1), (30, 16, 0xe00), (66, 16, 1),
                    (82, 3, 5), (85, 4, 6)]),
                front_instruction(&[]),
            ].concat();
            for word in code { state.engine.add_code(0, word); }
            state.engine.native_interpreter.dram[5] = 123;
        }
        let mut engine = Box::new(new_test_engine());
        engine.set_native_macro_mode(Some(context.clone()), Some(context.clone()));
        let handle = EngineHandle::from_ref(&*engine);
        let split = crate::gpu_split::GpuSplitFrontend::new(true);
        let front = unsafe { KeplerCompute::front(&mut *engine, &split) };
        let mut fs = unsafe { front.get(&split) };
        KeplerCompute::front_call_method(&mut fs, handle, &split, 0xe00, 0, true, None);
        assert!(fs.parameters.is_empty());
        assert_eq!(fs.executing_macro, None);
        assert_eq!(fs.registers[0xe00], 0);
        assert!(fs.compute_uploaded);
        let guard = context.lock();
        let state = guard.borrow();
        assert!(state.engine.native_execution_failed);
        assert!(!state.compute_engine.native_execution_failed);
        assert_eq!(state.engine.native_interpreter.dram[5], 123);
    }

    #[test]
    fn front_takeover_moves_fifo_and_keeps_independent_registers() {
        let context = Arc::new(parking_lot::ReentrantMutex::new(RefCell::new(
            crate::r#macro::NativeMacroContext::default(),
        )));
        let mut engine = Box::new(new_test_engine());
        engine.set_native_macro_mode(Some(context.clone()), Some(context));
        engine.native_registers[0x200] = 7;
        engine.native_parameters = vec![11, 19];
        let allocation = engine.native_parameters.as_ptr();
        engine.native_executing_macro = Some(0xe00);
        engine.native_bind_pointer = 5;
        let split = crate::gpu_split::GpuSplitFrontend::new(true);
        // No backend commands exist, and no reference spans Front's drain.
        let front = unsafe { KeplerCompute::front(&mut *engine, &split) };
        assert!(engine.front_macros);
        assert!(engine.native_parameters.is_empty());
        assert_eq!(engine.native_executing_macro, None);
        {
            let mut fs = unsafe { front.get(&split) };
            assert_eq!(fs.parameters.as_ptr(), allocation);
            assert_eq!(fs.parameters, [11, 19]);
            assert_eq!(fs.executing_macro, Some(0xe00));
            assert_eq!(fs.bind_pointer, 5);
            assert_eq!(fs.registers[0x200], 7);
            KeplerCompute::front_apply(&mut fs, 0x200, 23);
            assert_eq!(fs.registers[0x200], 23);
            assert_eq!(engine.native_registers[0x200], 7);
        }
        let again = unsafe { KeplerCompute::front(&mut *engine, &split) };
        assert!(Arc::ptr_eq(&front, &again));
        assert_eq!(unsafe { again.get(&split) }.registers[0x200], 23);
    }

    #[test]
    fn front_uploads_select_compute_and_backend_does_not_repeat_uploads() {
        let context = Arc::new(parking_lot::ReentrantMutex::new(RefCell::new(
            crate::r#macro::NativeMacroContext::default(),
        )));
        {
            let guard = context.lock();
            let mut state = guard.borrow_mut();
            state.engine.set_native_turing(true);
            state.compute_engine.set_native_turing(true);
        }
        let mut engine = Box::new(new_test_engine());
        engine.set_native_macro_mode(Some(context.clone()), Some(context.clone()));
        let split = crate::gpu_split::GpuSplitFrontend::new(true);
        let front = unsafe { KeplerCompute::front(&mut *engine, &split) };
        let mut fs = unsafe { front.get(&split) };
        // Before compute's first pointer upload, code is shared with graphics.
        KeplerCompute::front_apply(&mut fs, 0x46, 0xa);
        for (method, value) in [(0x45, 2), (0x46, 0xb), (0x47, 127), (0x48, 9), (0x48, 99)] {
            KeplerCompute::front_apply(&mut fs, method, value);
            engine.call_method(method, value, true);
            assert_eq!(engine.native_registers[method as usize], value);
        }
        assert!(fs.compute_uploaded);
        assert_eq!(fs.bind_pointer, 128); // Failed binding does not advance.
        assert_eq!(fs.registers[0x48], 99); // Register write precedes validation.
        let guard = context.lock();
        let state = guard.borrow();
        assert_eq!(state.engine.native_code[0], 0xa);
        assert_eq!(state.engine.native_write_offset, 1);
        assert_eq!(state.compute_engine.native_code[6], 0xb);
        assert_eq!(state.compute_engine.native_write_offset, 7);
        assert_eq!(state.compute_positions[127], 9);
        assert_eq!(state.positions[127], 0);
    }

    #[test]
    fn front_without_native_context_keeps_original_engine_and_releases_lock() {
        let owner = Arc::new(parking_lot::ReentrantMutex::new(RefCell::new(
            crate::r#macro::NativeMacroContext::default(),
        )));
        {
            let guard = owner.lock();
            let mut state = guard.borrow_mut();
            state.engine.set_native_turing(true);
            state.compute_engine.set_native_turing(true);
        }
        let mut engine = Box::new(new_test_engine());
        engine.set_native_macro_mode(Some(owner.clone()), None);
        let split = crate::gpu_split::GpuSplitFrontend::new(true);
        let front = unsafe { KeplerCompute::front(&mut *engine, &split) };
        let mut fs = unsafe { front.get(&split) };
        for (method, value) in [(0x45, 3), (0x46, 0x123), (0x47, 7), (0x48, 15)] {
            KeplerCompute::front_apply(&mut fs, method, value);
        }
        // Still holding the frontend state must not block a worker's context
        // access. Reentrant try_lock on this thread would miss that regression.
        let unlocked = std::thread::spawn(move || {
            let guard = owner.try_lock().expect("FrontApply retained the macro lock");
            let state = guard.borrow();
            assert_eq!(state.engine.native_code[9], 0x123);
            assert_eq!(state.positions[7], 15);
            assert_eq!(state.compute_engine.native_code[9], 0);
            assert_eq!(state.compute_positions[7], 0);
        });
        unlocked.join().unwrap();
        assert!(fs.compute_uploaded);
    }

    #[test]
    fn native_methods_lock_the_macro_context_only_where_they_use_it() {
        // Upstream ProcessNativeMethod dereferences native_macro only on macro, code,
        // bind and failure paths. A method outside them must not wait for a context
        // another thread (the split's front running a macro) holds.
        let context = Arc::new(parking_lot::ReentrantMutex::new(RefCell::new(
            crate::r#macro::NativeMacroContext::default(),
        )));
        let mut engine = Box::new(new_test_engine());
        engine.set_native_macro_mode(Some(context.clone()), Some(context.clone()));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let holder = {
            let context = context.clone();
            std::thread::spawn(move || {
                let _guard = context.lock();
                ready_tx.send(()).unwrap();
                // Bounded so that a regression fails instead of hanging the test.
                done_rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok()
            })
        };
        ready_rx.recv().unwrap();
        engine.call_method(0x47, 9, true); // bind pointer
        engine.call_method(0xad, 0x70, true); // plain QMD address register
        engine.call_method(0xae, 0, true); // QMD action without a launch
        engine.call_method(0xb0, 1, true);
        done_tx.send(()).unwrap();
        assert!(holder.join().unwrap(), "a context-free method waited for the macro context");
        assert_eq!(engine.native_bind_pointer, 9);
        assert!(engine.native_compute_uploaded);
        // A bind upload does use the context, and still writes the selected positions.
        engine.call_method(0x48, 0x1234, true);
        assert_eq!(engine.native_bind_pointer, 10);
        let guard = context.lock();
        let state = guard.borrow();
        assert_eq!(state.compute_positions[9], 0x1234);
        assert_eq!(state.positions[9], 0);
    }

    #[test]
    fn consume_sink_preserves_allocation_order_and_register_bounds_like_upstream() {
        let mut engine = new_test_engine();
        engine.interface_state.method_sink.reserve(32);
        let capacity = engine.interface_state.method_sink.capacity();
        let pointer = engine.interface_state.method_sink.as_ptr();
        for value in [7, 19] {
            for (method, argument) in [
                (0x20, 1),
                (KEPLER_COMPUTE_REG_COUNT as u32 - 1, value),
                (KEPLER_COMPUTE_REG_COUNT as u32, 0xbad),
                (u32::MAX, 0xbad),
                (0x20, value),
            ] {
                engine.push_method_sink(method, argument);
            }
            engine.consume_sink();
            assert_eq!(engine.regs[0x20], value);
            assert_eq!(engine.regs[KEPLER_COMPUTE_REG_COUNT - 1], value);
            assert!(engine.interface_state.method_sink.is_empty());
            assert_eq!(engine.interface_state.method_sink.capacity(), capacity);
            assert_eq!(engine.interface_state.method_sink.as_ptr(), pointer);
        }
    }

    #[test]
    fn native_method_routing_runs_shared_macros_qmd_and_fenced_releases() {
        const CHILD: &str = "RUZU_NATIVE_COMPUTE_ROUTING_CHILD";
        let Ok(mode) = std::env::var(CHILD) else {
            for mode in ["jit", "memo", "interpreter"] {
                let status = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        std::thread::current().name().unwrap(),
                        "--test-threads=1",
                    ])
                    .env(CHILD, mode)
                    .env("EDEN_NX2_MME_JIT", if mode == "jit" { "1" } else { "0" })
                    .env("EDEN_NX2_MME_MEMO", if mode == "memo" { "1" } else { "0" })
                    .env_remove("EDEN_NX2_SEM_WATCH")
                    .env_remove("EDEN_NX2_MME_DUMP")
                    .env_remove("EDEN_NATIVE_DRAW_DIAGNOSTICS")
                    .env_remove("EDEN_NX2_REJECT")
                    .env_remove("EDEN_NX2_SHADER_DUMP")
                    .env_remove("EDEN_NATIVE_SHADER_OVERRIDE_DIR")
                    .status()
                    .unwrap();
                assert!(status.success(), "child {mode}");
            }
            return;
        };
        let context = Arc::new(parking_lot::ReentrantMutex::new(std::cell::RefCell::new(
            crate::r#macro::NativeMacroContext::default(),
        )));
        let encode = |fields: &[(usize, usize, u32)]| {
            let mut bits = 0u128;
            for low in [15, 20, 25, 51, 56, 61] {
                bits |= 24u128 << low;
            }
            for &(low, count, value) in fields {
                let mask = ((1u128 << count) - 1) << low;
                bits = (bits & !mask) | (((value as u128) << low) & mask);
            }
            [(bits >> 64) as u32, (bits >> 32) as u32, bits as u32]
        };
        let code = |value| {
            [
                encode(&[
                    (0, 1, 1),
                    (30, 16, 0x200),
                    (66, 16, value),
                    (82, 3, 5),
                    (85, 4, 6),
                ]),
                encode(&[]),
            ]
            .concat()
        };
        {
            let guard = context.lock();
            let mut context = guard.borrow_mut();
            context.engine.set_native_turing(true);
            context.compute_engine.set_native_turing(true);
            context.engine.clear_code(0);
            for word in code(0x55) {
                context.engine.add_code(0, word);
            }
        }
        let mut backing = vec![0u8; 0x1000];
        let mut qmd = [0u32; 64];
        qmd[12] = 1;
        qmd[13] = 1;
        qmd[14] = 1;
        qmd[18] = (2 << 16) | 0x30;
        qmd[19] = (1 << 16) | 1;
        for (slot, address, payload, flags) in [
            (24, 0x7800, 7, 1 << 26),
            (28, 0x7804, 4, 1 << 19),
            (52, 0x7808, 99, 0),
        ] {
            qmd[slot] = address;
            qmd[slot + 1] = (1 << 23) | (1 << 30) | flags;
            qmd[slot + 2] = payload;
        }
        for base in [0x100, 0x200] {
            for (index, word) in qmd.iter().enumerate() {
                backing[base + index * 4..base + index * 4 + 4]
                    .copy_from_slice(&word.to_le_bytes());
            }
        }
        let mut engine = new_owner_backed_engine(&backing, 0x7000);
        let rasterizer = TestRasterizer::new(0, 0);
        rasterizer.defer_fences.set(true);
        engine.bind_rasterizer(&rasterizer);
        engine.set_native_macro_mode(Some(context.clone()), Some(context.clone()));
        assert!(engine.is_native_mode());
        assert!(engine.interface_state.execution_mask.iter().all(|&bit| bit));
        let ram = Arc::new(Mutex::new([0u32; 0xc00]));
        ram.lock()[0x27] = 0xabcdef;
        engine.set_native_data_ram(Some(ram.clone()));
        engine.call_method(0xe01, 0, true);
        assert!(engine.native_executing_macro.is_none());
        engine.call_method(0xe00, 0, true);
        assert_eq!(engine.native_registers[0x200], 0x55);
        assert!(!engine.native_compute_uploaded);
        engine.call_method(0x47, 0, true);
        engine.call_method(0x48, 0, true);
        engine.call_method(0x45, 0, true);
        let words = code(0x99);
        engine.call_multi_method(0x46, &words, words.len() as u32, words.len() as u32);
        assert!(engine.native_compute_uploaded);
        engine.call_method(0xe00, 7, false);
        assert_eq!(engine.native_registers[0x200], 0x55);
        engine.call_multi_method(0xe01, &[8], 1, 1);
        assert_eq!(engine.native_registers[0x200], 0x99);
        engine.call_method(0x200, 0, true);
        engine.call_method(0xe00, 7, false);
        engine.call_method(0xe01, 8, true);
        assert_eq!(engine.native_registers[0x200], 0x99);
        assert!(engine.native_parameters.is_empty());
        assert_eq!(ram.lock()[0x27], 0xabcdef);
        {
            let guard = context.lock();
            let context = guard.borrow();
            assert_eq!(context.engine.native_code[..6], code(0x55));
            assert_eq!(context.compute_engine.native_code[..6], code(0x99));
            assert!(!context.compute_engine.native_execution_failed);
            if mode == "memo" {
                assert!(context.compute_engine.native_memo.size() > 0);
            }
            if mode == "interpreter" {
                assert_eq!(context.compute_engine.native_memo.size(), 0);
            }
            #[cfg(target_arch = "x86_64")]
            assert_eq!(context.compute_engine.native_jit.is_some(), mode == "jit");
        }
        engine.set_native_data_ram(None);
        engine.call_method(0xe00, 7, false);
        engine.call_method(0xe01, 8, true);
        assert!(Arc::ptr_eq(
            context
                .lock()
                .borrow()
                .compute_engine
                .native_interpreter
                .active_dram
                .as_ref()
                .unwrap(),
            &ram
        ));
        engine.set_native_macro_mode(Some(context.clone()), Some(context.clone()));
        assert!(engine.native_compute_uploaded);
        engine.call_method(0x48, 3, true);
        assert!(engine.native_macro_compute);
        engine.call_method(0xd00, 0x1234, true);
        assert_eq!(engine.native_registers[0xd00], 0x1234);
        engine.call_method(0x1000, 1, true); // logged unsupported register, not a host panic
        engine.push_method_sink(0xd00, 42);
        engine.consume_sink_impl();
        // Native QMD ignores upload-only/action0/action1, then dispatches and releases at fences.
        engine.call_method(0xad, 0x71, true);
        for (method, value) in [(0xae, 3), (0xaf, 0), (0xb0, 0), (0xb0, 1)] {
            engine.call_method(method, value, true);
        }
        assert_eq!(rasterizer.dispatches.get(), 0);
        engine.memory_manager.lock().write::<u32>(0x7800, 1);
        engine.memory_manager.lock().write::<u32>(0x7804, 2);
        engine.call_method(0xb0, 3, true);
        assert_eq!(rasterizer.dispatches.get(), 1);
        // Only the REDUCTION_ENABLE release is deferred as a read-modify-write.
        // The MEMBAR_TYPE (bit 19) release is a plain payload write, like the
        // release with no flags.
        assert_eq!(rasterizer.pending_fences.borrow().len(), 1);
        {
            let queries = rasterizer.queries.borrow();
            let plain: Vec<_> = queries.iter().rev().take(2).map(|query| (query.0, query.3)).collect();
            assert_eq!(plain, [(0x7808, 99), (0x7804, 4)]);
        }
        engine.memory_manager.lock().write::<u32>(0x7800, 10);
        let waiters = crate::nx2::Nx2SemaphoreWaiters::get();
        waiters.add(
            0xc001,
            0x7800,
            17,
            crate::nx2::Nx2SemaphoreWaiters::ACQUIRE_EQUAL,
        );
        loop {
            let fence = rasterizer.pending_fences.borrow_mut().pop_front();
            let Some(fence) = fence else {
                break;
            };
            fence();
        }
        assert_eq!(engine.memory_manager.lock().read::<u32>(0x7800), 17);
        assert!(waiters.latched(0xc001));
        waiters.remove(0xc001);
        waiters.remove(0xc002);
        // Inline upload keeps the original AD across its optional recursive dispatch.
        engine.native_registers[0xc8..0x108].copy_from_slice(&qmd);
        engine.call_method(0xc6, 0, true);
        engine.call_method(0xc7, 0x72, true);
        engine.call_method(0xa7, 1 << 24, true);
        engine.call_method(0x107, qmd[63], true);
        assert_eq!(rasterizer.dispatches.get(), 2);
        assert_eq!(engine.native_registers[0xad], 0x71);
        let uploads = rasterizer.inline_uploads.borrow();
        let upload = uploads.last().unwrap();
        assert_eq!((upload.0, upload.1), (0x7200, 256));
        assert_eq!(
            upload.2,
            qmd.iter()
                .flat_map(|word| word.to_le_bytes())
                .collect::<Vec<_>>()
        );
        drop(uploads);
        engine.call_method(0xad, 0xdead, true);
        engine.call_method(0xb0, 3, true);
        assert!(
            context
                .lock()
                .borrow()
                .compute_engine
                .native_execution_failed
        );
        // The native compute environment now lowers real code at offset zero and starts at SPH size.
        for (index, word) in [0x794du32, 0, 0x03800000, 0].into_iter().enumerate() {
            backing[0x400 + index * 4..0x404 + index * 4].copy_from_slice(&word.to_le_bytes());
        }
        engine.call_method(CODE_LOC_BASE, 0, true);
        engine.call_method(CODE_LOC_BASE + 1, 0x7400, true);
        let env = crate::shader_environment::ComputeEnvironment::from_kepler_compute(
            &engine,
            engine.memory_manager.clone(),
        );
        assert_eq!(env.generic_environment().source_size_bytes(), 16);
        assert_eq!(env.generic_environment().start_address(), 0x50);
        assert_eq!(env.generic_environment().cached_size_bytes(), 120);
        let other = Arc::new(parking_lot::ReentrantMutex::new(std::cell::RefCell::new(
            crate::r#macro::NativeMacroContext::default(),
        )));
        engine.set_native_macro_mode(Some(other.clone()), Some(other));
        assert!(!engine.native_compute_uploaded);
        engine.set_native_macro_mode(None, None);
        assert!(!engine.is_native_mode());
        assert_eq!(
            engine
                .interface_state
                .execution_mask
                .iter()
                .enumerate()
                .filter_map(|(i, &v)| v.then_some(i))
                .collect::<Vec<_>>(),
            [EXEC_UPLOAD as usize, DATA_UPLOAD as usize, LAUNCH as usize]
        );
    }

    #[test]
    fn upload_dirty_snapshot_survives_until_indirect_launch() {
        for multi in [false, true] {
            for was_dirty in [false, true] {
                for source_dirty in [false, true] {
                    let backing = vec![0u8; 0x2000];
                    let mut engine = new_owner_backed_engine(&backing, 0x10000);
                    let rasterizer = TestRasterizer::new(if source_dirty { 0x11000 } else { 0 }, 4);
                    engine.memory_manager.lock().bind_rasterizer(&rasterizer);
                    engine.bind_rasterizer(&rasterizer);
                    engine.regs[LAUNCH_DESC_LOC as usize] = 0x100;
                    engine.regs[UPLOAD_REG_OFFSET as usize] = 4;
                    engine.regs[(UPLOAD_REG_OFFSET + 1) as usize] = 1;
                    engine.regs[(UPLOAD_REG_OFFSET + 3) as usize] = 0x10030;
                    engine.call_method(EXEC_UPLOAD, 1, true);
                    engine.set_current_dma_segment(0x11000);
                    engine.set_current_dirty(was_dirty);
                    if multi {
                        engine.call_multi_method(DATA_UPLOAD, &[42], 1, 1);
                    } else {
                        engine.call_method(DATA_UPLOAD, 42, true);
                    }
                    assert_eq!(engine.upload_dirty, was_dirty);
                    assert!(!engine.current_dirty());
                    engine.call_method(EXEC_UPLOAD, 1, true);
                    assert_eq!(engine.uploads.last().unwrap().was_dirty, was_dirty);
                    engine.call_method(LAUNCH, 1, true);
                    assert_eq!(
                        rasterizer
                            .last_dispatch
                            .borrow()
                            .as_ref()
                            .unwrap()
                            .indirect_compute_address,
                        (was_dirty || source_dirty).then_some(0x11000)
                    );
                    assert!(engine.uploads.is_empty());
                    assert_eq!(engine.get_indirect_compute_address(), None);
                }
            }
        }
    }

    fn new_owner_backed_engine(backing: &[u8], device_addr: u64) -> KeplerCompute {
        let device_memory = Arc::new(
            crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager::default(),
        );
        device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            device_addr,
            backing.as_ptr(),
            0x4000_0000,
            backing.len(),
            1,
            true,
        );
        let memory_manager = Arc::new(Mutex::new(
            MemoryManager::new_with_geometry_and_device_memory(
                1,
                Arc::clone(&device_memory),
                32,
                0x1_0000_0000,
                16,
                12,
            ),
        ));
        memory_manager
            .lock()
            .map(device_addr, device_addr, backing.len() as u64, 0, false);
        KeplerCompute::new(memory_manager)
    }

    #[test]
    fn native_dispatch_decodes_qmd_bitfields_rejects_invalid_buffers_and_preserves_failures() {
        let mut engine = new_test_engine();
        let rasterizer = TestRasterizer::new(0, 0);
        engine.bind_rasterizer(&rasterizer);
        engine.launch_description.program_start = 0x1234;
        engine.launch_description.linked_tsc = true;
        engine.launch_description.local_neg_alloc = 99;
        engine.indirect_compute = Some(0x123456);
        let mut qmd = [0u32; 64];
        qmd[12] = 0xfedcba98;
        qmd[13] = 0xdead1234;
        qmd[14] = 0xbeef5678;
        qmd[17] = u32::MAX;
        qmd[18] = 0x0020abcd;
        qmd[19] = 0x00030004;
        qmd[20] = 0xffff00a5;
        qmd[23] = 0xabfedcba;
        qmd[50] = 0xcdabcdef;
        qmd[48] = 0x87654321;
        qmd[49] = 0xfffe1234;
        for slot in 0..8 {
            qmd[32 + slot * 2] = 0x80000000 + slot as u32;
            qmd[33 + slot * 2] = 0xfff80080 + slot as u32;
        }
        assert!(engine.dispatch_native_compute(&qmd));
        let dispatch = rasterizer.last_dispatch.borrow().as_ref().unwrap().clone();
        let launch = &dispatch.launch_description;
        assert_eq!(
            (launch.grid_dim_x, launch.grid_dim_y, launch.grid_dim_z),
            (0x7edcba98, 0x1234, 0x5678)
        );
        assert_eq!(
            (launch.block_dim_x, launch.block_dim_y, launch.block_dim_z),
            (32, 4, 3)
        );
        assert_eq!(
            (
                launch.shared_alloc,
                launch.const_buffer_enable_mask,
                launch.local_pos_alloc,
                launch.local_crs_alloc
            ),
            (0x3ffff, 0xa5, 0xedcba, 0xbcdef)
        );
        assert_eq!(dispatch.code_address, 0x1234_87654321);
        assert_eq!(dispatch.indirect_compute_address, Some(0x123456));
        assert_eq!(
            (
                launch.program_start,
                launch.linked_tsc,
                launch.local_neg_alloc
            ),
            (0, false, 0)
        );
        for slot in 0..8 {
            assert_eq!(
                launch.const_buffers[slot],
                ConstBufferConfig {
                    address: ((0x80 + slot as u64) << 32) | (0x80000000 + slot as u64),
                    size: 0x1fff0
                }
            );
        }
        qmd[37] |= 0x100; // reject slot 2, even with its enable bit cleared
        qmd[20] = 0;
        assert!(!engine.dispatch_native_compute(&qmd));
        assert_eq!(
            engine.launch_description.const_buffers[1],
            launch.const_buffers[1]
        );
        assert_eq!(
            engine.launch_description.const_buffers[2],
            ConstBufferConfig::default()
        );
        assert_eq!(rasterizer.dispatches.get(), 1);
        qmd[37] &= !0x100;
        for (word, mask) in [(18, 0xffff), (19, 0xffff0000), (19, 0xffff)] {
            let old = qmd[word];
            qmd[word] &= mask;
            assert!(!engine.dispatch_native_compute(&qmd));
            qmd[word] = old;
        }
        assert_eq!(rasterizer.dispatches.get(), 1);
        qmd[12] = 0;
        qmd[13] = 0;
        qmd[14] = 0;
        assert!(engine.dispatch_native_compute(&qmd)); // upstream rejects zero block dimensions, not grid dimensions
        rasterizer.dispatch_failure.set(1);
        assert!(!engine.dispatch_native_compute(&qmd));
        assert!(rasterizer.copies.borrow().is_empty());
        rasterizer.dispatch_failure.set(3);
        let error = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            engine.dispatch_native_compute(&qmd)
        }))
        .unwrap_err();
        assert_eq!(
            *error.downcast_ref::<&str>().unwrap(),
            "unrelated rasterizer panic"
        );
    }

    #[test]
    fn native_matrix_multiply_rejection_is_reported_without_a_stand_in() {
        // Lichii f6a8f51324 removed the plain-upscale stand-in for rejected tensor programs:
        // the rejection is described and the dispatch reports failure, with no copy.
        let backing = vec![0u8; 0x1000];
        let mut engine = new_owner_backed_engine(&backing, 0x7000);
        let rasterizer = TestRasterizer::new(0, 0);
        rasterizer.copy_result.set(true);
        engine.bind_rasterizer(&rasterizer);
        let mut qmd = [0u32; 64];
        qmd[18] = 1 << 16;
        qmd[19] = 1 | (1 << 16);
        qmd[32] = 0x7000;
        rasterizer.dispatch_failure.set(2);
        assert!(!engine.dispatch_native_compute(&qmd));
        assert_eq!(rasterizer.dispatches.get(), 1);
        assert!(rasterizer.copies.borrow().is_empty());
    }

    #[test]
    fn native_report_semaphore_reductions_read_at_fence_and_capture_parameters() {
        let backing = vec![0u8; 0x1000];
        let mut engine = new_owner_backed_engine(&backing, 0x7000);
        let rasterizer = TestRasterizer::new(0, 0);
        rasterizer.defer_fences.set(true);
        engine.bind_rasterizer(&rasterizer);
        let cases = [
            (0, u32::MAX, 2, 1),
            (1, u32::MAX, 2, 2),
            (1, 1, 2, 1),
            (2, 1, 2, 2),
            (2, u32::MAX, 2, u32::MAX),
            (3, 3, 4, 4),
            (3, 4, 4, 0),
            (3, 5, 4, 0),
            (4, 0, 4, 4),
            (4, 5, 4, 4),
            (4, 4, 4, 3),
            (4, 1, 4, 0),
            (5, 0x33, 0x55, 0x11),
            (6, 0x33, 0x55, 0x77),
            (7, 0x33, 0x55, 0x66),
        ];
        for (reduction, value_at_fence, payload, expected) in cases {
            engine
                .memory_manager
                .lock()
                .write::<u32>(0x7000, 0xdeadbeef);
            engine.native_registers[0x6c0] = 0;
            engine.native_registers[0x6c1] = 0x7000;
            engine.native_registers[0x6c2] = payload;
            engine.process_native_report_semaphore(2 | (1 << 3) | (reduction << 9) | (1 << 28));
            assert_eq!(engine.memory_manager.lock().read::<u32>(0x7000), 0xdeadbeef);
            assert_eq!(rasterizer.pending_fences.borrow().len(), 1);
            // Later register writes do not alter the enqueued address/payload;
            // GPU memory changes, however, are observed when the fence runs.
            engine.native_registers[0x6c1] = 0x7004;
            engine.native_registers[0x6c2] = 999;
            engine
                .memory_manager
                .lock()
                .write::<u32>(0x7000, value_at_fence);
            let callback = rasterizer.pending_fences.borrow_mut().pop_front().unwrap();
            callback();
            assert_eq!(
                engine.memory_manager.lock().read::<u32>(0x7000),
                expected,
                "reduction {reduction}"
            );
            assert_eq!(engine.memory_manager.lock().read::<u32>(0x7004), 0);
        }
        assert!(rasterizer.queries.borrow().is_empty());
    }

    #[test]
    fn native_report_semaphore_query_flags_address_and_unsupported_operations() {
        let mut engine = new_test_engine();
        assert!(engine.native_registers.iter().all(|&word| word == 0));
        // Unsupported operations must return before touching the rasterizer.
        for operation in [1, 3] {
            engine.process_native_report_semaphore(operation | 8);
        }
        let rasterizer = TestRasterizer::new(0, 0);
        rasterizer.defer_fences.set(true);
        engine.bind_rasterizer(&rasterizer);
        engine.native_registers[0x6c0] = u32::MAX;
        engine.native_registers[0x6c1] = 0x12345678;
        engine.native_registers[0x6c2] = 0xfedcba98;
        for operation in [0, 2] {
            for one_word in [false, true] {
                engine.process_native_report_semaphore(operation | (u32::from(one_word) << 28));
                assert_eq!(
                    rasterizer.queries.borrow().last().copied(),
                    Some((
                        0x1ffffff_12345678,
                        crate::query_cache::types::QueryType::Payload as u32,
                        if one_word {
                            QueryPropertiesFlags::IS_A_FENCE
                        } else {
                            QueryPropertiesFlags::HAS_TIMEOUT
                        },
                        0xfedcba98,
                        0,
                    ))
                );
            }
        }
        for operation in [1, 3] {
            engine.process_native_report_semaphore(operation | 8);
        }
        assert_eq!(rasterizer.queries.borrow().len(), 4);
        assert!(rasterizer.pending_fences.borrow().is_empty());
    }

    /// Writes one 128-bit native instruction (first two words) at `at`.
    fn put_instruction(backing: &mut [u8], at: usize, word0: u32, word1: u32) {
        backing[at..at + 4].copy_from_slice(&word0.to_le_bytes());
        backing[at + 4..at + 8].copy_from_slice(&word1.to_le_bytes());
    }

    /// Image handle operand (cbuf0 byte offset, binding 0) of a texture or surface instruction.
    fn handle_operand(offset: u32) -> u32 {
        (offset / 4) << 8
    }

    /// Writes a block-linear 2D TIC entry at pool index `index`.
    fn put_tic(backing: &mut [u8], pool: usize, index: usize, format: u32, address: u32, size: (u32, u32)) {
        let words = [
            format,
            address,
            3 << 21,
            1 | (2 << 3),
            (size.0 - 1) | (1 << 23),
            size.1 - 1,
            0,
            0,
        ];
        for (word, value) in words.iter().enumerate() {
            let at = pool + index * 32 + word * 4;
            backing[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
    }

    const TEX: u32 = 0xb60;
    const SUST_P: u32 = 0xb9b;
    const EXIT_PT: u32 = 0x94d | (7 << 12);

    #[test]
    fn native_image_handles_lists_binding_zero_images_up_to_the_unconditional_exit() {
        const BASE: u64 = 0x31_0000;
        let mut backing = vec![0u8; 0x3000];
        // Reads: TEX, TLD, TLD4, TXD (a repeated offset is listed once); a bound (binding 1)
        // texture is not a cbuf0 handle. Store: SUST.P. A predicated EXIT does not end the scan.
        put_instruction(&mut backing, 0x00, TEX, handle_operand(0x58));
        put_instruction(&mut backing, 0x10, 0xb63, handle_operand(0x60));
        put_instruction(&mut backing, 0x20, 0xb66, handle_operand(0x58));
        put_instruction(&mut backing, 0x30, 0xb6d, handle_operand(0x70) | (1 << 22));
        put_instruction(&mut backing, 0x40, 0x94d | (0 << 12), 0);
        put_instruction(&mut backing, 0x50, 0xb6d, handle_operand(0x64));
        put_instruction(&mut backing, 0x60, SUST_P, handle_operand(0x68));
        put_instruction(&mut backing, 0x70, SUST_P, handle_operand(0x68));
        put_instruction(&mut backing, 0x80, EXIT_PT, 0);
        put_instruction(&mut backing, 0x90, TEX, handle_operand(0x80));
        // The scan crosses 4 KiB block boundaries until the exit.
        put_instruction(&mut backing, 0x1000, SUST_P, handle_operand(0x90));
        put_instruction(&mut backing, 0x1010, EXIT_PT, 0);
        let engine = new_owner_backed_engine(&backing, BASE);
        assert_eq!(
            engine.native_image_handles(BASE),
            (vec![0x58, 0x60, 0x64], vec![0x68])
        );
        backing[0x80..0x90].fill(0);
        let engine = new_owner_backed_engine(&backing, BASE + 0x10_0000);
        assert_eq!(
            engine.native_image_handles(BASE + 0x10_0000),
            (vec![0x58, 0x60, 0x64, 0x80], vec![0x68, 0x90])
        );
        // Results are cached per program address, like upstream's static map.
        backing[0x00..0x10].fill(0);
        assert_eq!(
            engine.native_image_handles(BASE + 0x10_0000),
            (vec![0x58, 0x60, 0x64, 0x80], vec![0x68, 0x90])
        );
    }

    #[test]
    fn remember_native_upscale_input_retains_only_a_single_large_block_linear_read() {
        const BASE: u64 = 0x52_0000;
        let mut backing = vec![0u8; 0x4000];
        put_instruction(&mut backing, 0x1000, TEX, handle_operand(0x58));
        put_instruction(&mut backing, 0x1010, EXIT_PT, 0);
        put_tic(&mut backing, 0x800, 1, 0x21, 0x9000, (960, 540));
        put_tic(&mut backing, 0x800, 2, 0x21, 0x9000, (63, 540));
        put_tic(&mut backing, 0x800, 3, 0x21, 0, (960, 540));
        backing[0x58..0x5c].copy_from_slice(&1u32.to_le_bytes());
        let mut engine = new_owner_backed_engine(&backing, BASE);
        engine.regs[TIC_BASE as usize + 1] = (BASE + 0x800) as u32;
        engine.regs[TIC_BASE as usize + 2] = 3;
        engine.regs[CODE_LOC_BASE as usize + 1] = (BASE + 0x1000) as u32;
        engine.launch_description.const_buffers[0] = ConstBufferConfig {
            address: BASE,
            size: 0x100,
        };
        engine.remember_native_upscale_input();
        let input = engine.native_upscale_input.expect("input retained");
        assert_eq!((input.width(), input.height(), input.address()), (960, 540, 0x9000));
        for (handle, size) in [(2u32, 0x100u32), (3, 0x100), (4, 0x100), (0, 0x100), (1, 0x58)] {
            backing[0x58..0x5c].copy_from_slice(&handle.to_le_bytes());
            engine.launch_description.const_buffers[0].size = size;
            engine.remember_native_upscale_input();
            assert!(engine.native_upscale_input.is_none(), "handle {handle} size {size:#x}");
        }
    }

    /// Ocarina of Time's movie network replacement (Lichii aae0b063f7), run in a child process
    /// because it reads the global current program id.
    #[test]
    fn oot_movie_network_is_skipped_and_its_output_pass_replaced_by_a_copy() {
        const CHILD: &str = "RUZU_OOT_MOVIE_NETWORK_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", std::thread::current().name().unwrap(), "--test-threads=1"])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        const BASE: u64 = 0x74_0000;
        const PREPASS: u64 = BASE + 0x2000;
        const MIDDLE: u64 = BASE + 0x3000;
        const OUTPUT: u64 = BASE + 0x4000;
        const OUTPUT_PLAIN: u64 = BASE + 0x5000;
        const GAMEPLAY: u64 = BASE + 0x6000;
        let mut backing = vec![0u8; 0x8000];
        put_instruction(&mut backing, 0x2000, TEX, handle_operand(0x58));
        put_instruction(&mut backing, 0x2010, EXIT_PT, 0);
        put_instruction(&mut backing, 0x3000, EXIT_PT, 0);
        // The output pass decodes sRGB itself (HSETP2 0.04045, HMUL2 2.4) before its store.
        put_instruction(&mut backing, 0x4000, TEX, handle_operand(0x60));
        put_instruction(&mut backing, 0x4010, 0x434, 0x292d292d);
        put_instruction(&mut backing, 0x4020, 0x832, 0x40cc40cc);
        put_instruction(&mut backing, 0x4030, SUST_P, handle_operand(0x68));
        put_instruction(&mut backing, 0x4040, EXIT_PT, 0);
        put_instruction(&mut backing, 0x5000, TEX, handle_operand(0x60));
        put_instruction(&mut backing, 0x5010, SUST_P, handle_operand(0x68));
        put_instruction(&mut backing, 0x5020, EXIT_PT, 0);
        put_instruction(&mut backing, 0x6000, TEX, handle_operand(0xc0));
        put_instruction(&mut backing, 0x6010, SUST_P, handle_operand(0xd0));
        put_instruction(&mut backing, 0x6020, EXIT_PT, 0);
        const POOL: usize = 0x1000;
        put_tic(&mut backing, POOL, 1, 0x21, 0x10_0000, (960, 540)); // movie colour
        put_tic(&mut backing, POOL, 2, 0x03, 0x20_0000, (960, 540)); // network intermediate
        put_tic(&mut backing, POOL, 3, 0x08, 0x30_0000, (1920, 1080)); // upscaled output
        put_tic(&mut backing, POOL, 4, 0x08, 0x40_0000, (3841, 2160)); // more than 4x
        for (offset, handle) in [(0x58usize, 1u32), (0x60, 2), (0x68, 3), (0xc0, 1), (0xd0, 3)] {
            backing[offset..offset + 4].copy_from_slice(&handle.to_le_bytes());
        }
        let mut engine = new_owner_backed_engine(&backing, BASE);
        engine.regs[TIC_BASE as usize + 1] = BASE as u32 + POOL as u32;
        engine.regs[TIC_BASE as usize + 2] = 4;
        let rasterizer = TestRasterizer::new(0, 0);
        rasterizer.copy_result.set(true);
        engine.bind_rasterizer(&rasterizer);
        let qmd = |program: u64, cbuf_enabled: bool| {
            let mut qmd = [0u32; 64];
            qmd[18] = 1 << 16;
            qmd[19] = 1 | (1 << 16);
            qmd[20] = u32::from(cbuf_enabled);
            qmd[32] = BASE as u32;
            qmd[33] = (0x100 >> 4) << 19;
            qmd[48] = program as u32;
            qmd
        };
        let dispatches = || rasterizer.dispatches.get();
        common::settings::set_current_program_id(crate::oot_network::PROGRAM_ID);

        // The prepass opens the window; the network's passes are skipped; the output pass is
        // replaced by a bilinear copy of the retained colour that decodes sRGB like the program.
        assert!(engine.dispatch_native_compute(&qmd(PREPASS, true)));
        assert_eq!(engine.network_skip_remaining, 32);
        for _ in 0..3 {
            assert!(engine.dispatch_native_compute(&qmd(MIDDLE, true)));
        }
        assert_eq!(engine.network_skip_remaining, 29);
        assert!(engine.dispatch_native_compute(&qmd(OUTPUT, true)));
        assert_eq!(dispatches(), 0);
        {
            let copies = rasterizer.copies.borrow();
            assert_eq!(copies.len(), 1);
            let (src, dst, config) = copies[0];
            assert_eq!((src.address(), src.width, src.height), (0x10_0000, 960, 540));
            assert_eq!((dst.address(), dst.width, dst.height), (0x30_0000, 1920, 1080));
            assert_eq!(src.format, crate::gpu::RenderTargetFormat::B10G11R11Float as u32);
            assert_eq!(dst.format, crate::gpu::RenderTargetFormat::A8B8G8R8Unorm as u32);
            assert_eq!((src.block_width(), src.block_height(), src.depth), (1, 2, 1));
            assert_eq!(config.operation, super::super::fermi_2d::Operation::SrcCopySrgbToLinear);
            assert_eq!(config.filter, super::super::fermi_2d::Filter::Bilinear);
            assert!(config.must_accelerate);
            assert_eq!(
                (config.src_x0, config.src_y0, config.src_x1, config.src_y1),
                (0, 0, 960, 540)
            );
            assert_eq!(
                (config.dst_x0, config.dst_y0, config.dst_x1, config.dst_y1),
                (0, 0, 1920, 1080)
            );
        }
        assert!(engine.native_upscale_input.is_none());
        assert_eq!(engine.network_skip_remaining, 0);
        // The window is closed: later passes run.
        assert!(engine.dispatch_native_compute(&qmd(MIDDLE, true)));
        assert_eq!(dispatches(), 1);

        // A program without the sRGB decode gets a plain copy.
        assert!(engine.dispatch_native_compute(&qmd(PREPASS, true)));
        assert!(engine.dispatch_native_compute(&qmd(OUTPUT_PLAIN, true)));
        assert_eq!(
            rasterizer.copies.borrow()[1].2.operation,
            super::super::fermi_2d::Operation::SrcCopy
        );
        assert_eq!(dispatches(), 1);

        // An output more than 4x wider than the input is not an upscale of it: skipped as a
        // network pass, the window then expires after 32 dispatches and drops the input.
        backing[0x68..0x6c].copy_from_slice(&4u32.to_le_bytes());
        assert!(engine.dispatch_native_compute(&qmd(PREPASS, true)));
        for _ in 0..32 {
            assert!(engine.dispatch_native_compute(&qmd(OUTPUT_PLAIN, true)));
        }
        assert_eq!(dispatches(), 1);
        assert!(engine.native_upscale_input.is_none());
        assert!(engine.dispatch_native_compute(&qmd(OUTPUT_PLAIN, true)));
        assert_eq!(dispatches(), 2);
        backing[0x68..0x6c].copy_from_slice(&3u32.to_le_bytes());

        // The gameplay network (+0xc0 -> +0xd0) is not touched, nor is a dispatch without cbuf0.
        assert!(engine.dispatch_native_compute(&qmd(GAMEPLAY, true)));
        assert_eq!(dispatches(), 3);
        assert!(engine.dispatch_native_compute(&qmd(PREPASS, false)));
        assert_eq!(dispatches(), 4);
        assert_eq!(engine.network_skip_remaining, 0);

        // Another title runs every pass.
        common::settings::set_current_program_id(crate::oot_network::PROGRAM_ID ^ 1);
        assert!(engine.dispatch_native_compute(&qmd(PREPASS, true)));
        assert!(engine.dispatch_native_compute(&qmd(MIDDLE, true)));
        assert_eq!(dispatches(), 6);
        common::settings::set_current_program_id(crate::oot_network::PROGRAM_ID);

        // A failed copy runs the real output pass and disables the replacement from then on.
        rasterizer.copy_result.set(false);
        let copies = rasterizer.copies.borrow().len();
        assert!(engine.dispatch_native_compute(&qmd(PREPASS, true)));
        assert!(engine.dispatch_native_compute(&qmd(OUTPUT, true)));
        assert_eq!(rasterizer.copies.borrow().len(), copies + 1);
        assert_eq!(dispatches(), 7);
        assert!(engine.native_upscale_disabled);
        assert!(engine.dispatch_native_compute(&qmd(PREPASS, true)));
        assert!(engine.dispatch_native_compute(&qmd(OUTPUT, true)));
        assert_eq!(dispatches(), 9);
        assert_eq!(rasterizer.copies.borrow().len(), copies + 1);
    }

    struct TestRasterizer {
        defer_fences: Cell<bool>,
        pending_fences: RefCell<std::collections::VecDeque<Box<dyn FnOnce() + Send>>>,
        queries: RefCell<Vec<(u64, u32, QueryPropertiesFlags, u32, u32)>>,
        accelerate_dma: crate::rasterizer_interface::TestAccelerateDMA,
        dirty_addr: u64,
        dirty_size: u64,
        dispatches: Cell<u32>,
        dispatch_failure: Cell<u8>,
        last_dispatch: RefCell<Option<DispatchCall>>,
        inline_uploads: RefCell<Vec<(u64, usize, Vec<u8>)>>,
        copies: RefCell<Vec<(Fermi2DSurface, Fermi2DSurface, Fermi2DConfig)>>,
        copy_result: Cell<bool>,
        flushes: RefCell<Vec<(u64, u64, crate::cache_types::CacheType)>>,
        copy_events: RefCell<Vec<&'static str>>,
    }

    impl TestRasterizer {
        fn new(dirty_addr: u64, dirty_size: u64) -> Self {
            Self {
                defer_fences: Cell::new(false),
                pending_fences: RefCell::new(Default::default()),
                queries: RefCell::new(Vec::new()),
                accelerate_dma: Default::default(),
                dirty_addr,
                dirty_size,
                dispatches: Cell::new(0),
                dispatch_failure: Cell::new(0),
                last_dispatch: RefCell::new(None),
                inline_uploads: RefCell::new(Vec::new()),
                copies: RefCell::new(Vec::new()),
                copy_result: Cell::new(false),
                flushes: RefCell::new(Vec::new()),
                copy_events: RefCell::new(Vec::new()),
            }
        }
    }

    impl RasterizerInterface for TestRasterizer {
        fn access_accelerate_dma(
            &mut self,
        ) -> &mut dyn crate::engines::maxwell_dma::AccelerateDMAInterface {
            &mut self.accelerate_dma
        }

        fn draw(&mut self, _draw_view: Maxwell3DDrawView<'_>, _instance_count: u32) {}

        fn draw_texture(
            &mut self,
            _draw_texture_view: crate::engines::draw_manager::Maxwell3DDrawTextureView<'_>,
        ) {
        }

        fn clear(&mut self, _clear_view: Maxwell3DClearView<'_>, _layer_count: u32) {}

        fn dispatch_compute(&mut self, dispatch: &DispatchCall) {
            self.dispatches.set(self.dispatches.get() + 1);
            self.last_dispatch.replace(Some(dispatch.clone()));
            match self.dispatch_failure.get() {
                1 => std::panic::panic_any(shader_recompiler::exception::RuntimeError::new(
                    "native failure",
                )),
                2 => std::panic::panic_any(
                    shader_recompiler::exception::NotImplementedException::new("matrix multiply"),
                ),
                3 => panic!("unrelated rasterizer panic"),
                _ => {}
            }
        }

        fn reset_counter(&mut self, _query_type: u32) {}

        fn query(
            &mut self,
            gpu_addr: u64,
            query_type: u32,
            flags: QueryPropertiesFlags,
            payload: u32,
            subreport: u32,
        ) {
            self.queries
                .borrow_mut()
                .push((gpu_addr, query_type, flags, payload, subreport));
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

        fn signal_fence(&mut self, func: Box<dyn FnOnce() + Send>) {
            if self.defer_fences.get() {
                self.pending_fences.borrow_mut().push_back(func);
            } else {
                func();
            }
        }

        fn sync_operation(&mut self, func: Box<dyn FnOnce() + Send>) {
            func();
        }

        fn signal_sync_point(&mut self, _value: u32) {}

        fn signal_reference(&mut self) {}

        fn release_fences(&mut self, _force: bool) {}

        fn flush_all(&mut self) {}

        fn flush_region(&mut self, addr: u64, size: u64, which: crate::cache_types::CacheType) {
            self.flushes.borrow_mut().push((addr, size, which));
            self.copy_events.borrow_mut().push("flush");
        }

        fn accelerate_surface_copy(
            &mut self,
            src: &Fermi2DSurface,
            dst: &Fermi2DSurface,
            config: &Fermi2DConfig,
        ) -> bool {
            self.copies.borrow_mut().push((*src, *dst, *config));
            self.copy_events.borrow_mut().push("copy");
            self.copy_result.get()
        }

        fn must_flush_region(
            &self,
            addr: u64,
            size: u64,
            _which: crate::cache_types::CacheType,
        ) -> bool {
            addr == self.dirty_addr && size == self.dirty_size
        }

        fn get_flush_area(&self, _addr: u64, _size: u64) -> RasterizerDownloadArea {
            RasterizerDownloadArea {
                start_address: 0,
                end_address: 0,
                preemtive: false,
                in_flight: false,
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

        fn wait_for_idle(&mut self) {}

        fn fragment_barrier(&mut self) {}

        fn tiled_cache_barrier(&mut self) {}

        fn flush_commands(&mut self) {}

        fn tick_frame(&mut self) {}

        fn accelerate_inline_to_memory(&mut self, address: u64, copy_size: usize, memory: &[u8]) {
            self.inline_uploads
                .borrow_mut()
                .push((address, copy_size, memory.to_vec()));
        }
    }

    #[test]
    fn test_write_reg() {
        let mut engine = new_test_engine();
        engine.write_reg(0x100, 0xDEAD);
        assert_eq!(engine.regs[0x100], 0xDEAD);
    }

    #[test]
    fn test_write_reg_high_method() {
        let mut engine = new_test_engine();
        engine.write_reg(TEX_CB_INDEX, 5);
        assert_eq!(engine.regs[TEX_CB_INDEX as usize], 5);
        assert_eq!(engine.tex_cb_index(), 5);
    }

    #[test]
    fn test_launch_desc_address() {
        let mut engine = new_test_engine();
        engine.regs[LAUNCH_DESC_LOC as usize] = 0x1234;
        assert_eq!(engine.launch_desc_address(), 0x1234 << 8);
    }

    #[test]
    fn test_code_address() {
        let mut engine = new_test_engine();
        engine.regs[CODE_LOC_BASE as usize] = 0x0001; // high
        engine.regs[(CODE_LOC_BASE + 1) as usize] = 0x2000; // low
        assert_eq!(engine.code_address(), 0x0001_0000_2000);
    }

    #[test]
    fn test_tsc_address() {
        let mut engine = new_test_engine();
        engine.regs[TSC_BASE as usize] = 0x0002; // high
        engine.regs[(TSC_BASE + 1) as usize] = 0x4000; // low
        engine.regs[(TSC_BASE + 2) as usize] = 64; // limit
        assert_eq!(engine.tsc_address(), 0x0002_0000_4000);
        assert_eq!(engine.tsc_limit(), 64);
    }

    #[test]
    fn test_tic_address() {
        let mut engine = new_test_engine();
        engine.regs[TIC_BASE as usize] = 0x0003; // high
        engine.regs[(TIC_BASE + 1) as usize] = 0x8000; // low
        engine.regs[(TIC_BASE + 2) as usize] = 128; // limit
        assert_eq!(engine.tic_address(), 0x0003_0000_8000);
        assert_eq!(engine.tic_limit(), 128);
    }

    #[test]
    fn test_get_indirect_compute_address_defaults_to_none() {
        let engine = new_test_engine();
        assert_eq!(engine.get_indirect_compute_address(), None);
    }

    #[test]
    fn test_get_tic_entry_reads_from_tic_pool() {
        let mut backing = vec![0u8; 0x1000];
        let entry_offset = 3 * std::mem::size_of::<TicEntry>();
        let raw = &mut backing[entry_offset..entry_offset + std::mem::size_of::<TicEntry>()];
        raw[0..8].copy_from_slice(&0x1111_2222_3333_4444u64.to_le_bytes());
        raw[8..16].copy_from_slice(&0x5555_6666_7777_8888u64.to_le_bytes());
        raw[16..24].copy_from_slice(&0x9999_AAAA_BBBB_CCCCu64.to_le_bytes());
        raw[24..32].copy_from_slice(&0xDDDD_EEEE_FFFF_0001u64.to_le_bytes());

        let mut engine = new_owner_backed_engine(&backing, 0x6000);
        engine.regs[TIC_BASE as usize] = 0;
        engine.regs[(TIC_BASE + 1) as usize] = 0x6000;

        let entry = engine.get_tic_entry(3);

        assert_eq!(entry.raw[0], 0x1111_2222_3333_4444);
        assert_eq!(entry.raw[1], 0x5555_6666_7777_8888);
        assert_eq!(entry.raw[2], 0x9999_AAAA_BBBB_CCCC);
        assert_eq!(entry.raw[3], 0xDDDD_EEEE_FFFF_0001);
    }

    #[test]
    fn test_get_tsc_entry_reads_from_tsc_pool() {
        let mut backing = vec![0u8; 0x1000];
        let entry_offset = 2 * std::mem::size_of::<TscEntry>();
        let raw = &mut backing[entry_offset..entry_offset + std::mem::size_of::<TscEntry>()];
        raw[0..8].copy_from_slice(&0x0123_4567_89AB_CDEFu64.to_le_bytes());
        raw[8..16].copy_from_slice(&0xFEDC_BA98_7654_3210u64.to_le_bytes());
        raw[16..24].copy_from_slice(&0x0F0E_0D0C_0B0A_0908u64.to_le_bytes());
        raw[24..32].copy_from_slice(&0x8070_6050_4030_2010u64.to_le_bytes());

        let mut engine = new_owner_backed_engine(&backing, 0x5000);
        engine.regs[TSC_BASE as usize] = 0;
        engine.regs[(TSC_BASE + 1) as usize] = 0x5000;

        let entry = engine.get_tsc_entry(2);

        assert_eq!(entry.raw[0], 0x0123_4567_89AB_CDEF);
        assert_eq!(entry.raw[1], 0xFEDC_BA98_7654_3210);
        assert_eq!(entry.raw[2], 0x0F0E_0D0C_0B0A_0908);
        assert_eq!(entry.raw[3], 0x8070_6050_4030_2010);
    }

    #[test]
    fn test_qmd_from_words_basic() {
        let mut w = [0u32; NUM_LAUNCH_PARAMETERS];
        w[0x08] = 0x500; // program_start
        w[0x0C] = 64; // grid_dim_x
        w[0x0D] = 32 | (16 << 16); // grid_dim_y=32, grid_dim_z=16
        w[0x11] = 0x1000; // shared_alloc
        w[0x12] = 256 << 16; // block_dim_x=256
        w[0x13] = 4 | (2 << 16); // block_dim_y=4, block_dim_z=2
        w[0x14] = 2 << 29; // cache_layout
        w[0x2D] = 0x400 | (7 << 27); // local_pos_alloc, barrier_alloc
        w[0x2E] = 0x300 | (12 << 24); // local_neg_alloc, gpr_alloc
        w[0x2F] = 0x200 | (4 << 24); // local_crs_alloc, sass_version

        let qmd = LaunchParams::from_words(&w);
        assert_eq!(qmd.program_start, 0x500);
        assert_eq!(qmd.grid_dim_x, 64);
        assert_eq!(qmd.grid_dim_y, 32);
        assert_eq!(qmd.grid_dim_z, 16);
        assert_eq!(qmd.shared_alloc, 0x1000);
        assert_eq!(qmd.block_dim_x, 256);
        assert_eq!(qmd.block_dim_y, 4);
        assert_eq!(qmd.block_dim_z, 2);
        assert_eq!(qmd.cache_layout, 2);
        assert_eq!(qmd.local_pos_alloc, 0x400);
        assert_eq!(qmd.barrier_alloc, 7);
        assert_eq!(qmd.local_neg_alloc, 0x300);
        assert_eq!(qmd.gpr_alloc, 12);
        assert_eq!(qmd.local_crs_alloc, 0x200);
        assert_eq!(qmd.sass_version, 4);
    }

    #[test]
    fn test_qmd_from_words_const_buffers() {
        let mut w = [0u32; NUM_LAUNCH_PARAMETERS];
        w[0x14] = 0b0000_0101; // enable CB 0 and CB 2

        // CB 0 at words 0x1D, 0x1E: addr_low=0x1000, word1: addr_high=1, size=512
        w[0x1D] = 0x1000;
        w[0x1E] = 1 | (512 << 15); // addr_high[7:0]=1, size in bits[31:15]

        // CB 2 at words 0x21, 0x22: addr_low=0x2000, word1: addr_high=0, size=256
        w[0x21] = 0x2000;
        w[0x22] = 0 | (256 << 15);

        let qmd = LaunchParams::from_words(&w);
        assert_eq!(qmd.const_buffer_enable_mask, 0b0000_0101);
        assert_eq!(qmd.const_buffers[0].address, 0x0001_0000_1000);
        assert_eq!(qmd.const_buffers[0].size, 512);
        assert_eq!(qmd.const_buffers[2].address, 0x2000);
        assert_eq!(qmd.const_buffers[2].size, 256);
        // Unused CB should be zero.
        assert_eq!(qmd.const_buffers[1].address, 0);
        assert_eq!(qmd.const_buffers[1].size, 0);
    }

    #[test]
    fn test_qmd_from_words_linked_tsc_and_local_crs_alloc() {
        let mut w = [0u32; NUM_LAUNCH_PARAMETERS];
        w[0x0B] = 1 << 30;
        w[0x2F] = 0x54321;

        let qmd = LaunchParams::from_words(&w);
        assert!(qmd.linked_tsc);
        assert_eq!(qmd.local_crs_alloc, 0x54321);
    }

    #[test]
    fn test_qmd_from_words_zeros() {
        let w = [0u32; NUM_LAUNCH_PARAMETERS];
        let qmd = LaunchParams::from_words(&w);
        assert_eq!(qmd.grid_dim_x, 0);
        assert_eq!(qmd.grid_dim_y, 0);
        assert_eq!(qmd.grid_dim_z, 0);
        assert_eq!(qmd.block_dim_x, 0);
        assert_eq!(qmd.block_dim_y, 0);
        assert_eq!(qmd.block_dim_z, 0);
        assert_eq!(qmd.shared_alloc, 0);
        assert_eq!(qmd.program_start, 0);
        assert_eq!(qmd.const_buffer_enable_mask, 0);
        assert_eq!(qmd.cache_layout, 0);
        assert_eq!(qmd.local_pos_alloc, 0);
        assert_eq!(qmd.barrier_alloc, 0);
        assert_eq!(qmd.local_neg_alloc, 0);
        assert_eq!(qmd.gpr_alloc, 0);
        assert_eq!(qmd.sass_version, 0);
        for cb in &qmd.const_buffers {
            assert_eq!(cb.address, 0);
            assert_eq!(cb.size, 0);
        }
    }

    /// Helper: build a QMD byte blob from words for owner-backed launch tests.
    fn make_qmd_bytes(w: &[u32; NUM_LAUNCH_PARAMETERS]) -> Vec<u8> {
        let mut bytes = vec![0u8; NUM_LAUNCH_PARAMETERS * 4];
        for (i, &word) in w.iter().enumerate() {
            bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn test_launch_creates_dispatch() {
        let mut qmd_words = [0u32; NUM_LAUNCH_PARAMETERS];
        qmd_words[0x08] = 0x200; // program_start
        qmd_words[0x0C] = 8; // grid_dim_x
        qmd_words[0x0D] = 4 | (2 << 16); // grid_dim_y=4, z=2
        qmd_words[0x12] = 128 << 16; // block_dim_x=128
        qmd_words[0x13] = 1 | (1 << 16); // block_dim_y=1, z=1
        let qmd_bytes = make_qmd_bytes(&qmd_words);
        let mut backing = vec![0u8; 0x1000];
        backing[..qmd_bytes.len()].copy_from_slice(&qmd_bytes);
        let mut engine = new_owner_backed_engine(&backing, 0x10000);
        let rasterizer = TestRasterizer::new(0, 0);
        engine.bind_rasterizer(&rasterizer);

        // Set up code address.
        engine.regs[CODE_LOC_BASE as usize] = 0x0001;
        engine.regs[(CODE_LOC_BASE + 1) as usize] = 0xA000;

        // Set up TSC/TIC pools.
        engine.regs[TSC_BASE as usize] = 0;
        engine.regs[(TSC_BASE + 1) as usize] = 0x5000;
        engine.regs[(TSC_BASE + 2) as usize] = 32;
        engine.regs[TIC_BASE as usize] = 0;
        engine.regs[(TIC_BASE + 1) as usize] = 0x6000;
        engine.regs[(TIC_BASE + 2) as usize] = 64;
        engine.regs[TEX_CB_INDEX as usize] = 2;

        // Prepare QMD at address 0x100 << 8 = 0x10000.
        engine.regs[LAUNCH_DESC_LOC as usize] = 0x100;

        // Upstream dispatches synchronously from the LAUNCH method.
        engine.write_reg(LAUNCH, 1);

        assert_eq!(rasterizer.dispatches.get(), 1);
        let dispatch = rasterizer.last_dispatch.borrow();
        let d = dispatch.as_ref().expect("LAUNCH must dispatch immediately");
        assert_eq!(d.launch_desc_loc, 0x10000);
        assert_eq!(d.code_address, 0x0001_0000_A000);
        assert_eq!(d.tsc_address, 0x5000);
        assert_eq!(d.tsc_limit, 32);
        assert_eq!(d.tic_address, 0x6000);
        assert_eq!(d.tic_limit, 64);
        assert_eq!(d.tex_cb_index, 2);
        assert_eq!(d.launch_description.grid_dim_x, 8);
        assert_eq!(d.launch_description.grid_dim_y, 4);
        assert_eq!(d.launch_description.grid_dim_z, 2);
        assert_eq!(d.launch_description.block_dim_x, 128);
        assert_eq!(d.launch_description.program_start, 0x200);
        assert_eq!(engine.launch_description.program_start, 0x200);
        assert_eq!(engine.launch_description.grid_dim_x, 8);
        assert_eq!(engine.launch_description.block_dim_x, 128);
    }

    #[test]
    fn upload_state_tracks_indirect_compute_like_upstream() {
        let mut backing = vec![0u8; 0x4000];
        let launch_desc = 0x20000;
        let device_memory = Arc::new(
            crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager::default(),
        );
        device_memory.smmu_set_physical_base_for_test(backing.as_mut_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            0x8000_0000,
            backing.as_mut_ptr(),
            0x4000_0000,
            backing.len(),
            1,
            true,
        );
        let memory_manager = Arc::new(Mutex::new(
            MemoryManager::new_with_geometry_and_device_memory(
                1,
                Arc::clone(&device_memory),
                32,
                0x1_0000_0000,
                16,
                12,
            ),
        ));
        memory_manager
            .lock()
            .map(0x10000, 0x8000_0000, 0x4000, 0, false);
        memory_manager
            .lock()
            .map(launch_desc, 0x8000_1000, 0x1000, 0, false);

        let mut engine = KeplerCompute::new(Arc::clone(&memory_manager));
        let rasterizer = TestRasterizer::new(0x8000_0000, 4);
        memory_manager.lock().bind_rasterizer(&rasterizer);
        engine.bind_rasterizer(&rasterizer);
        engine.set_current_dma_segment(0x10000);

        engine.regs[LAUNCH_DESC_LOC as usize] = (launch_desc >> 8) as u32;
        engine.call_method(UPLOAD_REG_OFFSET, 4, true);
        engine.call_method((UPLOAD_REG_OFFSET + 1) as u32, 1, true);
        engine.call_method((UPLOAD_REG_OFFSET + 2) as u32, 0, true);
        engine.call_method(
            (UPLOAD_REG_OFFSET + 3) as u32,
            launch_desc as u32 + 0x0C * 4,
            true,
        );
        engine.call_method((UPLOAD_REG_OFFSET + 4) as u32, 4, true);
        engine.call_method(EXEC_UPLOAD, 1, true);
        engine.call_method(DATA_UPLOAD, 0x0000_0042, true);
        assert_eq!(
            rasterizer.inline_uploads.borrow().as_slice(),
            &[(
                launch_desc + 0x0C * 4,
                4,
                0x0000_0042u32.to_ne_bytes().to_vec(),
            )]
        );
        engine.call_method(EXEC_UPLOAD, 1, true);

        engine.call_method(LAUNCH, 1, true);

        assert_eq!(rasterizer.dispatches.get(), 1);
        assert_eq!(engine.get_indirect_compute_address(), None);
        assert!(engine.uploads.is_empty());
    }

    #[test]
    fn test_multiple_dispatches() {
        let backing = vec![0u8; 0x2000];
        let mut engine = new_owner_backed_engine(&backing, 0x10000);
        let rasterizer = TestRasterizer::new(0, 0);
        engine.bind_rasterizer(&rasterizer);
        engine.regs[LAUNCH_DESC_LOC as usize] = 0x100;

        // First launch.
        engine.write_reg(LAUNCH, 1);

        // Second launch (update QMD address).
        engine.regs[LAUNCH_DESC_LOC as usize] = 0x101;
        engine.write_reg(LAUNCH, 1);
        assert_eq!(rasterizer.dispatches.get(), 2);
        assert_eq!(
            rasterizer
                .last_dispatch
                .borrow()
                .as_ref()
                .expect("second LAUNCH must dispatch")
                .launch_desc_loc,
            0x101 << 8
        );
    }

    #[test]
    fn test_bind_rasterizer_stores_reference() {
        let syncpoints =
            std::sync::Arc::new(crate::host1x::syncpoint_manager::SyncpointManager::new());
        let rasterizer = crate::renderer_null::null_rasterizer::RasterizerNull::new(syncpoints);
        let mut engine = new_test_engine();
        assert!(engine.rasterizer.is_none());
        engine.bind_rasterizer(&rasterizer);
        assert!(engine.rasterizer.is_some());
    }

    #[test]
    fn test_call_method_launch_dispatches_immediately() {
        let backing = vec![0u8; 0x1000];
        let mut engine = new_owner_backed_engine(&backing, 0x12300);
        let rasterizer = TestRasterizer::new(0, 0);
        engine.bind_rasterizer(&rasterizer);
        engine.regs[LAUNCH_DESC_LOC as usize] = 0x123;
        engine.call_method(LAUNCH, 1, true);
        assert_eq!(rasterizer.dispatches.get(), 1);
    }

    #[test]
    fn test_constructor_keeps_memory_manager_owner() {
        let memory_manager = Arc::new(Mutex::new(MemoryManager::new(0)));
        let engine = KeplerCompute::new(Arc::clone(&memory_manager));
        assert!(Arc::ptr_eq(&engine.memory_manager, &memory_manager));
    }
}

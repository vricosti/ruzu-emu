// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Kepler Compute engine (NV class B1C0).
//!
//! Handles compute shader dispatch. When the game writes the LAUNCH register,
//! a Queue Meta Data (QMD) descriptor is read from GPU memory, parsed, and
//! forwarded immediately to the bound rasterizer, matching upstream.

use std::sync::Arc;

use parking_lot::Mutex;

use super::engine_interface::{EngineInterface, EngineInterfaceState};
use super::engine_upload;
use crate::memory_manager::MemoryManager;
use crate::rasterizer_interface::{RasterizerHandle, RasterizerInterface};
use crate::textures::texture::{TicEntry, TscEntry};

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
}

/// Corresponds to upstream's public anonymous `state` member.
#[derive(Debug, Clone, Default)]
pub struct State {
    pub write_offset: u32,
    pub copy_size: u32,
    pub inner_buffer: Vec<u8>,
}

// ── Engine struct ───────────────────────────────────────────────────────────

pub struct KeplerCompute {
    regs: Box<[u32; KEPLER_COMPUTE_REG_COUNT]>,
    interface_state: EngineInterfaceState,
    memory_manager: Arc<Mutex<MemoryManager>>,
    upload_state: engine_upload::State,
    upload_address: u64,
    uploads: Vec<UploadInfo>,
    /// Port of upstream owner-local `launch_description`.
    pub launch_description: LaunchParams,
    /// Preserved owner-local scratch state from upstream.
    pub state: State,
    /// Port of upstream owner-local `indirect_compute`.
    indirect_compute: Option<u64>,
    rasterizer: Option<RasterizerHandle>,
}

impl KeplerCompute {
    /// Corresponds to upstream `KeplerCompute(Core::System&, MemoryManager&)`.
    /// Rust stores the upstream `MemoryManager&` owner directly here; the wider
    /// `System&` dependency remains outside this bounded slice.
    pub fn new(memory_manager: Arc<Mutex<MemoryManager>>) -> Self {
        Self {
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
            uploads: Vec::new(),
            launch_description: LaunchParams::default(),
            state: State::default(),
            indirect_compute: None,
            rasterizer: None,
        }
    }

    /// Corresponds to upstream `KeplerCompute::CallMethod`.
    pub fn call_method(&mut self, method: u32, argument: u32, _is_last_call: bool) {
        let idx = method as usize;
        assert!(
            idx < KEPLER_COMPUTE_REG_COUNT,
            "Invalid KeplerCompute register, increase the size of the register array"
        );
        self.regs[idx] = argument;

        match method {
            EXEC_UPLOAD => {
                let regs = self.upload_registers();
                let info = UploadInfo {
                    upload_address: self.upload_address,
                    exec_address: self.upload_state.exec_target_address(&regs),
                    copy_size: self.upload_state.get_upload_size(),
                };
                self.uploads.push(info);
                self.upload_state
                    .process_exec(&regs, self.exec_upload_linear());
            }
            DATA_UPLOAD => {
                self.upload_address = self.interface_state.current_dma_segment;
                let regs = self.upload_registers();
                self.upload_state
                    .process_data_word(&regs, argument, _is_last_call);
            }
            LAUNCH => {
                let qmd_addr = self.launch_desc_address();
                for data in &self.uploads {
                    let offset = data.exec_address.wrapping_sub(qmd_addr);
                    if offset / std::mem::size_of::<u32>() as u64 == LAUNCH_GRID_DIM_X_INDEX
                        && self
                            .memory_manager
                            .lock()
                            .is_memory_dirty(data.upload_address, data.copy_size as u64)
                    {
                        self.indirect_compute = Some(data.upload_address);
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
        if method == DATA_UPLOAD {
            self.upload_address = self.interface_state.current_dma_segment;
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
        let sink = std::mem::take(&mut self.interface_state.method_sink);
        for (method, value) in sink {
            let idx = method as usize;
            assert!(idx < KEPLER_COMPUTE_REG_COUNT);
            self.regs[idx] = value;
        }
    }

    fn has_pending_methods(&self) -> bool {
        !self.interface_state.method_sink.is_empty()
    }

    fn execution_mask(&self) -> &[bool] {
        &self.interface_state.execution_mask
    }

    fn push_method_sink(&mut self, method: u32, value: u32) {
        self.interface_state.method_sink.push((method, value));
    }

    fn set_current_dma_segment(&mut self, segment: u64) {
        self.interface_state.current_dma_segment = segment;
    }

    fn current_dirty(&self) -> bool {
        self.interface_state.current_dirty
    }

    fn set_current_dirty(&mut self, dirty: bool) {
        self.interface_state.current_dirty = dirty;
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

    struct TestRasterizer {
        accelerate_dma: crate::rasterizer_interface::TestAccelerateDMA,
        dirty_addr: u64,
        dirty_size: u64,
        dispatches: Cell<u32>,
        last_dispatch: RefCell<Option<DispatchCall>>,
        inline_uploads: RefCell<Vec<(u64, usize, Vec<u8>)>>,
    }

    impl TestRasterizer {
        fn new(dirty_addr: u64, dirty_size: u64) -> Self {
            Self {
                accelerate_dma: Default::default(),
                dirty_addr,
                dirty_size,
                dispatches: Cell::new(0),
                last_dispatch: RefCell::new(None),
                inline_uploads: RefCell::new(Vec::new()),
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
        }

        fn reset_counter(&mut self, _query_type: u32) {}

        fn query(
            &mut self,
            _gpu_addr: u64,
            _query_type: u32,
            _flags: QueryPropertiesFlags,
            _payload: u32,
            _subreport: u32,
        ) {
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
            func();
        }

        fn sync_operation(&mut self, func: Box<dyn FnOnce() + Send>) {
            func();
        }

        fn signal_sync_point(&mut self, _value: u32) {}

        fn signal_reference(&mut self) {}

        fn release_fences(&mut self, _force: bool) {}

        fn flush_all(&mut self) {}

        fn flush_region(&mut self, _addr: u64, _size: u64, _which: crate::cache_types::CacheType) {}

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
                preemptive: false,
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

        fn accelerate_surface_copy(
            &mut self,
            _src: &Fermi2DSurface,
            _dst: &Fermi2DSurface,
            _copy_config: &Fermi2DConfig,
        ) -> bool {
            false
        }

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

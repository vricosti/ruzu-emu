// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/engines/kepler_memory.h and kepler_memory.cpp
//!
//! This engine is known as P2MF (Push-to-Memory-From-host). It allows the
//! CPU/GPU command stream to push data directly into GPU virtual memory.
//! Documentation:
//!   https://github.com/envytools/envytools/blob/master/rnndb/graph/gk104_p2mf.xml
//!   https://cgit.freedesktop.org/mesa/mesa/tree/src/gallium/drivers/nouveau/nvc0/nve4_p2mf.xml.h

use std::sync::Arc;

use parking_lot::Mutex;

use super::engine_interface::{EngineInterface, EngineInterfaceState};
use super::engine_upload;
use crate::memory_manager::MemoryManager;
use crate::rasterizer_interface::RasterizerInterface;

// ── Register layout constants ───────────────────────────────────────────────

/// Register offset for the upload register block (matches `ASSERT_REG_POSITION(upload, 0x60)`).
const UPLOAD_REG_OFFSET: usize = 0x60;

/// Register offset for the exec register (matches `ASSERT_REG_POSITION(exec, 0x6C)`).
const EXEC_REG: usize = 0x6C;

/// Register offset for the data register (matches `ASSERT_REG_POSITION(data, 0x6D)`).
const DATA_REG: usize = 0x6D;

// ── Macro equivalent for register indexing ──────────────────────────────────

/// Equivalent of `KEPLERMEMORY_REG_INDEX(field)` — returns the u32 word index
/// of a field within the register file.
/// For named fields we use the constants above directly.

// ── KeplerMemory engine ─────────────────────────────────────────────────────

/// KeplerMemory engine register file.
#[derive(Clone)]
#[repr(C)]
pub struct Regs {
    pub reg_array: [u32; 0x7F],
}

impl Default for Regs {
    fn default() -> Self {
        Self {
            reg_array: [0u32; Self::NUM_REGS],
        }
    }
}

impl Regs {
    /// Total number of registers in the KeplerMemory register file.
    pub const NUM_REGS: usize = 0x7F;

    /// Read the upload registers from the register array.
    pub fn upload(&self) -> engine_upload::Registers {
        engine_upload::Registers {
            line_length_in: self.reg_array[UPLOAD_REG_OFFSET],
            line_count: self.reg_array[UPLOAD_REG_OFFSET + 1],
            dest: engine_upload::DestRegisters {
                address_high: self.reg_array[UPLOAD_REG_OFFSET + 2],
                address_low: self.reg_array[UPLOAD_REG_OFFSET + 3],
                pitch: self.reg_array[UPLOAD_REG_OFFSET + 4],
                block_dims: self.reg_array[UPLOAD_REG_OFFSET + 5],
                width: self.reg_array[UPLOAD_REG_OFFSET + 6],
                height: self.reg_array[UPLOAD_REG_OFFSET + 7],
                depth: self.reg_array[UPLOAD_REG_OFFSET + 8],
                layer: self.reg_array[UPLOAD_REG_OFFSET + 9],
                x: self.reg_array[UPLOAD_REG_OFFSET + 10],
                y: self.reg_array[UPLOAD_REG_OFFSET + 11],
            },
        }
    }

    /// Check whether exec.linear bit is set.
    pub fn exec_linear(&self) -> bool {
        (self.reg_array[EXEC_REG] & 1) != 0
    }
}

/// The KeplerMemory engine (P2MF).
///
/// Corresponds to the C++ `KeplerMemory` class which inherits `EngineInterface`.
pub struct KeplerMemory {
    pub regs: Regs,
    upload_state: engine_upload::State,
    interface_state: EngineInterfaceState,
}

impl KeplerMemory {
    /// Create a new KeplerMemory engine.
    ///
    /// Corresponds to upstream `KeplerMemory(MemoryManager&)`.
    /// Rust stores the upstream `MemoryManager&` owner in `upload_state`.
    pub fn new(memory_manager: Arc<Mutex<MemoryManager>>) -> Self {
        Self {
            regs: Regs::default(),
            upload_state: engine_upload::State::new_with_memory_manager(Arc::clone(
                &memory_manager,
            )),
            interface_state: EngineInterfaceState::new(),
        }
    }

    /// Bind a rasterizer and set up the execution mask.
    ///
    /// Corresponds to `KeplerMemory::BindRasterizer`.
    pub fn bind_rasterizer(&mut self, rasterizer: &dyn RasterizerInterface) {
        self.upload_state.bind_rasterizer(rasterizer);
        self.interface_state.execution_mask.fill(false);
        self.interface_state.execution_mask[EXEC_REG] = true;
        self.interface_state.execution_mask[DATA_REG] = true;
    }

    fn process_upload_word(&mut self, data: u32, is_last_call: bool) {
        let regs = self.regs.upload();
        self.upload_state
            .process_data_word(&regs, data, is_last_call);
    }

    fn process_upload_multi(&mut self, data: &[u32]) {
        let regs = self.regs.upload();
        self.upload_state.process_data_multi(&regs, data);
    }

    /// Write a value to the register identified by method.
    ///
    /// Corresponds to `KeplerMemory::CallMethod`.
    pub fn call_method(&mut self, method: u32, method_argument: u32, is_last_call: bool) {
        let method_idx = method as usize;
        assert!(
            method_idx < Regs::NUM_REGS,
            "Invalid KeplerMemory register, increase the size of the Regs structure"
        );

        self.regs.reg_array[method_idx] = method_argument;

        match method_idx {
            EXEC_REG => {
                let regs = self.regs.upload();
                self.upload_state
                    .process_exec(&regs, self.regs.exec_linear());
            }
            DATA_REG => {
                self.process_upload_word(method_argument, is_last_call);
            }
            _ => {}
        }
    }

    /// Write multiple values to the register identified by method.
    ///
    /// Corresponds to `KeplerMemory::CallMultiMethod`.
    pub fn call_multi_method(
        &mut self,
        method: u32,
        base_start: &[u32],
        amount: u32,
        methods_pending: u32,
    ) {
        let method_idx = method as usize;
        match method_idx {
            DATA_REG => {
                self.process_upload_multi(&base_start[..amount as usize]);
            }
            _ => {
                for i in 0..amount {
                    self.call_method(
                        method,
                        base_start[i as usize],
                        methods_pending.wrapping_sub(i) <= 1,
                    );
                }
            }
        }
    }

    /// Consume the method sink (deferred writes).
    ///
    /// Corresponds to `KeplerMemory::ConsumeSinkImpl`.
    pub fn consume_sink_impl(&mut self) {
        let sink: Vec<(u32, u32)> = self.interface_state.method_sink.drain(..).collect();
        for (method, value) in sink {
            let method_idx = method as usize;
            self.regs.reg_array[method_idx] = value;
        }
    }
}

#[cfg(test)]
impl Default for KeplerMemory {
    fn default() -> Self {
        Self::new(Arc::new(Mutex::new(MemoryManager::new(0))))
    }
}

impl EngineInterface for KeplerMemory {
    fn call_method(&mut self, method: u32, method_argument: u32, is_last_call: bool) {
        KeplerMemory::call_method(self, method, method_argument, is_last_call);
    }

    fn call_multi_method(
        &mut self,
        method: u32,
        base_start: &[u32],
        amount: u32,
        methods_pending: u32,
    ) {
        KeplerMemory::call_multi_method(self, method, base_start, amount, methods_pending);
    }

    fn consume_sink_impl(&mut self) {
        KeplerMemory::consume_sink_impl(self);
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
mod tests {
    use super::*;

    #[test]
    fn register_owner_layout_matches_upstream() {
        assert_eq!(std::mem::size_of::<Regs>(), Regs::NUM_REGS * 4);
        assert_eq!(std::mem::align_of::<Regs>(), 4);
        assert_eq!(UPLOAD_REG_OFFSET, 0x60);
        assert_eq!(EXEC_REG, 0x6c);
        assert_eq!(DATA_REG, 0x6d);
    }

    #[test]
    fn data_register_writes_through_memory_manager() {
        let device_memory = Arc::new(
            crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager::default(),
        );
        let backing = vec![0u8; 0x1000];
        device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            0x8000_0000,
            backing.as_ptr(),
            0x6000,
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
            .map(0x10000, 0x8000_0000, 0x1000, 0, false);

        let mut engine = KeplerMemory::new(Arc::clone(&memory_manager));
        let syncpoints = Arc::new(crate::host1x::syncpoint_manager::SyncpointManager::new());
        let mut rasterizer = crate::renderer_null::null_rasterizer::RasterizerNull::new(syncpoints);
        let upload_memory_manager = Arc::clone(&memory_manager);
        rasterizer.set_inline_upload_callback(move |address, copy_size, memory| {
            upload_memory_manager
                .lock()
                .write_block(address, &memory[..copy_size]);
        });
        engine.bind_rasterizer(&rasterizer);

        engine.call_method(UPLOAD_REG_OFFSET as u32, 4, true);
        engine.call_method((UPLOAD_REG_OFFSET + 1) as u32, 1, true);
        engine.call_method((UPLOAD_REG_OFFSET + 2) as u32, 0, true);
        engine.call_method((UPLOAD_REG_OFFSET + 3) as u32, 0x10000, true);
        engine.call_method((UPLOAD_REG_OFFSET + 4) as u32, 4, true);
        engine.call_method(EXEC_REG as u32, 1, true);
        engine.call_method(DATA_REG as u32, 0x11223344, true);

        assert_eq!(&backing[..4], &[0x44, 0x33, 0x22, 0x11]);
    }
}

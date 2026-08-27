// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/engines/nv01_timer.h`.

use std::sync::Arc;

use parking_lot::Mutex;

use super::engine_interface::{EngineInterface, EngineInterfaceState};
use crate::memory_manager::MemoryManager;

/// Eden declares a 0x48-byte placeholder register block for the timer engine.
pub const NUM_REGS: usize = 0x48 / std::mem::size_of::<u32>();

#[derive(Default)]
pub struct Regs {
    pub reg_array: [u32; NUM_REGS],
}

/// Stub NV01 timer engine.
///
/// Eden intentionally only logs method calls; it does not currently emulate
/// timer registers or consume deferred method writes.
pub struct Nv01Timer {
    pub regs: Regs,
    pub interface_state: EngineInterfaceState,
}

impl Nv01Timer {
    pub fn new(_memory_manager: Arc<Mutex<MemoryManager>>) -> Self {
        Self {
            regs: Regs::default(),
            interface_state: EngineInterfaceState::new(),
        }
    }

    pub fn call_method(&mut self, method: u32, method_argument: u32, is_last_call: bool) {
        log::debug!(
            "method={}, argument={}, is_last_call={}",
            method,
            method_argument,
            is_last_call
        );
    }

    pub fn call_multi_method(
        &mut self,
        method: u32,
        base_start: &[u32],
        amount: u32,
        methods_pending: u32,
    ) {
        log::debug!(
            "method={}, base_start={:p}, amount={}, pending={}",
            method,
            base_start.as_ptr(),
            amount,
            methods_pending
        );
    }

    fn consume_sink_impl(&mut self) {}
}

impl EngineInterface for Nv01Timer {
    fn call_method(&mut self, method: u32, method_argument: u32, is_last_call: bool) {
        Nv01Timer::call_method(self, method, method_argument, is_last_call);
    }

    fn call_multi_method(
        &mut self,
        method: u32,
        base_start: &[u32],
        amount: u32,
        methods_pending: u32,
    ) {
        Nv01Timer::call_multi_method(self, method, base_start, amount, methods_pending);
    }

    fn consume_sink_impl(&mut self) {
        Nv01Timer::consume_sink_impl(self);
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
    fn register_placeholder_matches_eden_size() {
        assert_eq!(std::mem::size_of::<Regs>(), 0x48);
    }

    #[test]
    fn consume_sink_is_intentionally_a_no_op() {
        let mut timer = Nv01Timer::new(Arc::new(Mutex::new(MemoryManager::new(0))));
        timer.push_method_sink(0x40, 7);

        timer.consume_sink();

        assert!(timer.has_pending_methods());
    }
}

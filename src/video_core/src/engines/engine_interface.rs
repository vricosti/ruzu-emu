// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/engines/engine_interface.h
//!
//! Common interface that all GPU engines implement for register writes.

/// GPU virtual address type.
pub type GPUVAddr = u64;

/// Engine type identifiers, matching the C++ `EngineTypes` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum EngineTypes {
    Nv01Timer = 0,
    KeplerCompute = 1,
    Maxwell3D = 2,
    Fermi2D = 3,
    MaxwellDMA = 4,
    KeplerMemory = 5,
}

/// Trait corresponding to the C++ `EngineInterface` base class.
///
/// Each engine accepts single and multi-value register writes, and maintains
/// an execution mask, a method sink for deferred writes, and dirty tracking.
pub trait EngineInterface: Send {
    /// Write a single value to the register identified by `method`.
    fn call_method(&mut self, method: u32, method_argument: u32, is_last_call: bool);

    /// Write multiple values to the register identified by `method`.
    fn call_multi_method(
        &mut self,
        method: u32,
        base_start: &[u32],
        amount: u32,
        methods_pending: u32,
    );

    /// Consume the method sink, flushing deferred writes. Default implementation
    /// calls `call_method` for each entry then clears the sink.
    fn consume_sink(&mut self) {
        if self.has_pending_methods() {
            self.consume_sink_impl();
        }
    }

    /// Internal sink consumption — override in concrete engines.
    fn consume_sink_impl(&mut self);

    /// Whether the upstream-owned method sink contains deferred writes.
    fn has_pending_methods(&self) -> bool;

    /// Access the execution mask. The DmaPusher uses this to decide whether
    /// a method should be executed immediately or deferred to the sink.
    fn execution_mask(&self) -> &[bool];

    /// Push a (method, value) pair onto the method sink for deferred processing.
    fn push_method_sink(&mut self, method: u32, value: u32);

    /// Set the current DMA segment address.
    fn set_current_dma_segment(&mut self, segment: GPUVAddr);

    /// Get the current dirty flag.
    fn current_dirty(&self) -> bool;

    /// Set the current dirty flag.
    fn set_current_dirty(&mut self, dirty: bool);
}

/// Non-owning handle corresponding to upstream `Engines::EngineInterface*`.
///
/// Rust trait objects are fat pointers, so storing them as raw integer pairs at
/// each call site is easy to get wrong. This wrapper centralizes the one
/// lifetime-erasing conversion needed for upstream-style engine binding.
#[derive(Clone, Copy)]
pub struct EngineHandle {
    ptr: *const (dyn EngineInterface + 'static),
}

unsafe impl Send for EngineHandle {}
unsafe impl Sync for EngineHandle {}

impl EngineHandle {
    pub fn from_ref(engine: &dyn EngineInterface) -> Self {
        let ptr = engine as *const dyn EngineInterface;
        Self {
            ptr: unsafe {
                std::mem::transmute::<
                    *const dyn EngineInterface,
                    *const (dyn EngineInterface + 'static),
                >(ptr)
            },
        }
    }

    /// # Safety
    ///
    /// The caller must uphold upstream's non-owning pointer contract: the
    /// engine object must outlive every handle use, and no aliasing mutable use
    /// may happen concurrently.
    pub unsafe fn as_mut<'a>(self) -> &'a mut dyn EngineInterface {
        unsafe { &mut *(self.ptr as *mut (dyn EngineInterface + 'static)) }
    }
}

/// State common to all engines that implement `EngineInterface`.
///
/// Corresponds to the C++ `EngineInterface` member fields:
/// - `execution_mask` (bitset<u16::MAX>)
/// - `method_sink` (vector of (method, value) pairs)
/// - `current_dirty` flag
/// - `current_dma_segment` address
pub struct EngineInterfaceState {
    /// Bitmask indicating which method indices trigger immediate execution.
    /// Index by method number; `true` means the method must be executed
    /// rather than deferred.
    pub execution_mask: Vec<bool>,
    /// Deferred (method, value) pairs accumulated between flushes.
    pub method_sink: Vec<(u32, u32)>,
    /// Whether the engine has dirty state that needs processing.
    pub current_dirty: bool,
    /// Current DMA segment GPU virtual address.
    pub current_dma_segment: GPUVAddr,
}

impl EngineInterfaceState {
    /// Create a new state with an execution mask sized for `u16::MAX` entries.
    pub fn new() -> Self {
        Self {
            execution_mask: vec![false; u16::MAX as usize],
            method_sink: Vec::new(),
            current_dirty: false,
            current_dma_segment: 0,
        }
    }

    /// Consume the sink by calling the provided callback for each deferred write,
    /// then clear the sink. This is the default `ConsumeSinkImpl` behavior.
    pub fn default_consume_sink(&mut self, mut call_method: impl FnMut(u32, u32, bool)) {
        for &(method, value) in &self.method_sink {
            call_method(method, value, true);
        }
        self.method_sink.clear();
    }
}

impl Default for EngineInterfaceState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{EngineInterface, EngineInterfaceState, EngineTypes};

    struct TestEngine {
        state: EngineInterfaceState,
        consume_calls: usize,
    }

    impl TestEngine {
        fn new() -> Self {
            Self {
                state: EngineInterfaceState::new(),
                consume_calls: 0,
            }
        }
    }

    impl EngineInterface for TestEngine {
        fn call_method(&mut self, _method: u32, _method_argument: u32, _is_last_call: bool) {}

        fn call_multi_method(
            &mut self,
            _method: u32,
            _base_start: &[u32],
            _amount: u32,
            _methods_pending: u32,
        ) {
        }

        fn consume_sink_impl(&mut self) {
            self.consume_calls += 1;
            self.state.method_sink.clear();
        }

        fn has_pending_methods(&self) -> bool {
            !self.state.method_sink.is_empty()
        }

        fn execution_mask(&self) -> &[bool] {
            &self.state.execution_mask
        }

        fn push_method_sink(&mut self, method: u32, value: u32) {
            self.state.method_sink.push((method, value));
        }

        fn set_current_dma_segment(&mut self, segment: u64) {
            self.state.current_dma_segment = segment;
        }

        fn current_dirty(&self) -> bool {
            self.state.current_dirty
        }

        fn set_current_dirty(&mut self, dirty: bool) {
            self.state.current_dirty = dirty;
        }
    }

    #[test]
    fn engine_type_discriminants_match_eden() {
        assert_eq!(EngineTypes::Nv01Timer as u32, 0);
        assert_eq!(EngineTypes::KeplerCompute as u32, 1);
        assert_eq!(EngineTypes::Maxwell3D as u32, 2);
        assert_eq!(EngineTypes::Fermi2D as u32, 3);
        assert_eq!(EngineTypes::MaxwellDMA as u32, 4);
        assert_eq!(EngineTypes::KeplerMemory as u32, 5);
    }

    #[test]
    fn consume_sink_only_calls_the_override_for_pending_methods() {
        let mut engine = TestEngine::new();

        engine.consume_sink();
        assert_eq!(engine.consume_calls, 0);

        engine.push_method_sink(0x40, 1);
        engine.consume_sink();
        assert_eq!(engine.consume_calls, 1);
    }
}

use std::sync::Arc;

use super::arch_version::ArchVersion;
use super::coprocessor::Coprocessor;
use crate::exclusive_monitor::ExclusiveMonitor;
use crate::interface::optimization_flags::OptimizationFlag;
use crate::ir::a32_emitter::A32IREmitter;

/// Exception reported through `A32::UserCallbacks::ExceptionRaised`.
///
/// Upstream owner: `interface/A32/config.h::Exception`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum Exception {
    UndefinedInstruction = 0,
    UnpredictableInstruction = 1,
    DecodeError = 2,
    SendEvent = 3,
    SendEventLocal = 4,
    WaitForInterrupt = 5,
    WaitForEvent = 6,
    Yield = 7,
    Breakpoint = 8,
    PreloadData = 9,
    PreloadDataWithIntentToWrite = 10,
    PreloadInstruction = 11,
    NoExecuteFault = 12,
}

impl Exception {
    pub fn as_u32(self) -> u32 {
        self as u32
    }

    pub fn from_u32(value: u32) -> Self {
        match value {
            0 => Self::UndefinedInstruction,
            1 => Self::UnpredictableInstruction,
            2 => Self::DecodeError,
            3 => Self::SendEvent,
            4 => Self::SendEventLocal,
            5 => Self::WaitForInterrupt,
            6 => Self::WaitForEvent,
            7 => Self::Yield,
            8 => Self::Breakpoint,
            9 => Self::PreloadData,
            10 => Self::PreloadDataWithIntentToWrite,
            11 => Self::PreloadInstruction,
            12 => Self::NoExecuteFault,
            _ => unreachable!("invalid A32 exception value {value}"),
        }
    }
}

/// Host callbacks inserted into generated A32 code.
///
/// Upstream owner: `interface/A32/config.h::UserCallbacks`.
pub trait UserCallbacks: Send {
    fn memory_read_code(&self, vaddr: u32) -> Option<u32> {
        Some(self.memory_read_32(vaddr))
    }

    fn pre_code_read_hook(&self, _is_thumb: bool, _pc: u32, _ir: &mut A32IREmitter<'_>) -> bool {
        true
    }

    fn pre_code_translation_hook(&self, _is_thumb: bool, _pc: u32, _ir: &mut A32IREmitter<'_>) {}

    fn get_ticks_for_code(&self, _is_thumb: bool, _vaddr: u32, _instruction: u32) -> u64 {
        1
    }

    fn memory_read_8(&self, vaddr: u32) -> u8;
    fn memory_read_16(&self, vaddr: u32) -> u16;
    fn memory_read_32(&self, vaddr: u32) -> u32;
    fn memory_read_64(&self, vaddr: u32) -> u64;

    fn memory_write_8(&mut self, vaddr: u32, value: u8);
    fn memory_write_16(&mut self, vaddr: u32, value: u16);
    fn memory_write_32(&mut self, vaddr: u32, value: u32);
    fn memory_write_64(&mut self, vaddr: u32, value: u64);

    fn memory_write_exclusive_8(&mut self, _vaddr: u32, _value: u8, _expected: u8) -> bool {
        false
    }

    fn memory_write_exclusive_16(&mut self, _vaddr: u32, _value: u16, _expected: u16) -> bool {
        false
    }

    fn memory_write_exclusive_32(&mut self, _vaddr: u32, _value: u32, _expected: u32) -> bool {
        false
    }

    fn memory_write_exclusive_64(&mut self, _vaddr: u32, _value: u64, _expected: u64) -> bool {
        false
    }

    fn is_read_only_memory(&self, _vaddr: u32) -> bool {
        false
    }

    fn call_svc(&mut self, swi: u32);
    fn exception_raised(&mut self, pc: u32, exception: Exception);

    fn instruction_synchronization_barrier_raised(&mut self) {}

    fn add_ticks(&mut self, ticks: u64);
    fn get_ticks_remaining(&self) -> u64;

    /// Rust lifecycle adapter: called once the owning JIT state has a stable address.
    fn set_halt_reason_ptr(&mut self, _ptr: *const u32) {}

    /// Rust lifecycle adapter: called once the owning JIT state has a stable address.
    fn set_pc_ptr(&mut self, _ptr: *const u32) {}

    /// Rust lifecycle adapter for the A32 upper location descriptor.
    fn set_upper_location_descriptor_ptr(&mut self, _ptr: *const u32) {}
}

/// The 16 configurable A32 coprocessor slots from `A32::UserConfig`.
///
/// Upstream owner: `interface/A32/config.h::UserConfig::coprocessors`.
pub type Coprocessors = [Option<Arc<dyn Coprocessor>>; 16];

pub fn empty_coprocessors() -> Coprocessors {
    [const { None }; 16]
}

/// Configuration for an A32 JIT instance.
///
/// Upstream owner: `interface/A32/config.h::UserConfig`.
pub struct UserConfig {
    pub callbacks: Box<dyn UserCallbacks>,
    pub global_monitor: Option<*mut ExclusiveMonitor>,
    pub page_table: Option<*mut [*mut u8; Self::NUM_PAGE_TABLE_ENTRIES]>,
    pub coprocessors: Coprocessors,
    pub fastmem_pointer: Option<*mut u8>,
    pub optimizations: OptimizationFlag,
    pub code_cache_size: u32,
    pub page_table_pointer_mask_bits: i32,
    pub page_table_log2_stride: usize,
    pub arch_version: ArchVersion,
    pub processor_id: u8,
    pub detect_misaligned_access_via_page_table: u8,
    pub unsafe_optimizations: bool,
    pub absolute_offset_page_table: bool,
    pub only_detect_misalignment_via_page_table_on_page_boundary: bool,
    pub recompile_on_fastmem_failure: bool,
    pub fastmem_exclusive_access: bool,
    pub recompile_on_exclusive_fastmem_failure: bool,
    pub hook_isb: bool,
    pub hook_hint_instructions: bool,
    pub define_unpredictable_behaviour: bool,
    pub wall_clock_cntpct: bool,
    pub check_halt_on_memory_access: bool,
    pub enable_cycle_counting: bool,
    pub always_little_endian: bool,
    pub very_verbose_debugging_output: bool,
}

impl UserConfig {
    pub const PAGE_BITS: usize = 12;
    pub const NUM_PAGE_TABLE_ENTRIES: usize = 1 << (32 - Self::PAGE_BITS);
    pub const DEFAULT_CODE_CACHE_SIZE: u32 = 128 * 1024 * 1024;

    pub fn new(callbacks: Box<dyn UserCallbacks>) -> Self {
        Self {
            callbacks,
            global_monitor: None,
            page_table: None,
            coprocessors: empty_coprocessors(),
            fastmem_pointer: None,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
            code_cache_size: Self::DEFAULT_CODE_CACHE_SIZE,
            page_table_pointer_mask_bits: 0,
            page_table_log2_stride: 3,
            arch_version: ArchVersion::V8,
            processor_id: 0,
            detect_misaligned_access_via_page_table: 0,
            unsafe_optimizations: false,
            absolute_offset_page_table: false,
            only_detect_misalignment_via_page_table_on_page_boundary: false,
            recompile_on_fastmem_failure: true,
            fastmem_exclusive_access: false,
            recompile_on_exclusive_fastmem_failure: true,
            hook_isb: false,
            hook_hint_instructions: false,
            define_unpredictable_behaviour: false,
            wall_clock_cntpct: false,
            check_halt_on_memory_access: false,
            enable_cycle_counting: true,
            always_little_endian: false,
            very_verbose_debugging_output: false,
        }
    }

    pub fn has_optimization(&self, mut flag: OptimizationFlag) -> bool {
        if !self.unsafe_optimizations {
            flag &= OptimizationFlag::ALL_SAFE_OPTIMIZATIONS;
        }
        (flag & self.optimizations) != OptimizationFlag::NO_OPTIMIZATIONS
    }

    pub fn effective_optimizations(&self) -> OptimizationFlag {
        if self.unsafe_optimizations {
            self.optimizations
        } else {
            self.optimizations & OptimizationFlag::ALL_SAFE_OPTIMIZATIONS
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exception_values_and_layout_match_upstream() {
        let values = [
            Exception::UndefinedInstruction,
            Exception::UnpredictableInstruction,
            Exception::DecodeError,
            Exception::SendEvent,
            Exception::SendEventLocal,
            Exception::WaitForInterrupt,
            Exception::WaitForEvent,
            Exception::Yield,
            Exception::Breakpoint,
            Exception::PreloadData,
            Exception::PreloadDataWithIntentToWrite,
            Exception::PreloadInstruction,
            Exception::NoExecuteFault,
        ];
        for (expected, exception) in values.into_iter().enumerate() {
            assert_eq!(exception.as_u32(), expected as u32);
            assert_eq!(Exception::from_u32(expected as u32), exception);
        }
        assert_eq!(std::mem::size_of::<Exception>(), 4);
        assert_eq!(std::mem::align_of::<Exception>(), 4);
    }

    #[test]
    fn empty_registry_has_all_sixteen_upstream_slots() {
        let registry = empty_coprocessors();
        assert_eq!(registry.len(), 16);
        assert!(registry.iter().all(Option::is_none));
    }

    struct DefaultCallbacks;

    impl UserCallbacks for DefaultCallbacks {
        fn memory_read_8(&self, _vaddr: u32) -> u8 {
            0
        }

        fn memory_read_16(&self, _vaddr: u32) -> u16 {
            0
        }

        fn memory_read_32(&self, _vaddr: u32) -> u32 {
            0
        }

        fn memory_read_64(&self, _vaddr: u32) -> u64 {
            0
        }

        fn memory_write_8(&mut self, _vaddr: u32, _value: u8) {}
        fn memory_write_16(&mut self, _vaddr: u32, _value: u16) {}
        fn memory_write_32(&mut self, _vaddr: u32, _value: u32) {}
        fn memory_write_64(&mut self, _vaddr: u32, _value: u64) {}
        fn call_svc(&mut self, _swi: u32) {}
        fn exception_raised(&mut self, _pc: u32, _exception: Exception) {}
        fn add_ticks(&mut self, _ticks: u64) {}

        fn get_ticks_remaining(&self) -> u64 {
            0
        }
    }

    #[test]
    fn exclusive_write_defaults_match_upstream() {
        let mut callbacks = DefaultCallbacks;
        assert_eq!(callbacks.memory_read_code(0), Some(0));
        assert_eq!(callbacks.get_ticks_for_code(false, 0, 0), 1);
        assert!(!callbacks.memory_write_exclusive_8(0, 0, 0));
        assert!(!callbacks.memory_write_exclusive_16(0, 0, 0));
        assert!(!callbacks.memory_write_exclusive_32(0, 0, 0));
        assert!(!callbacks.memory_write_exclusive_64(0, 0, 0));
        assert!(!callbacks.is_read_only_memory(0));
    }

    #[test]
    fn user_config_defaults_match_upstream() {
        let config = UserConfig::new(Box::new(DefaultCallbacks));
        assert_eq!(UserConfig::PAGE_BITS, 12);
        assert_eq!(UserConfig::NUM_PAGE_TABLE_ENTRIES, 1 << 20);
        assert_eq!(config.code_cache_size, 128 * 1024 * 1024);
        assert_eq!(config.page_table_log2_stride, 3);
        assert_eq!(config.arch_version, ArchVersion::V8);
        assert!(config.recompile_on_fastmem_failure);
        assert!(config.recompile_on_exclusive_fastmem_failure);
        assert!(config.enable_cycle_counting);
        assert!(config.has_optimization(OptimizationFlag::BLOCK_LINKING));
        assert!(!config.has_optimization(OptimizationFlag::UNSAFE_UNFUSE_FMA));
    }
}

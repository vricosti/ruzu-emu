use std::sync::Arc;

use super::coprocessor::Coprocessor;
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
}

/// The 16 configurable A32 coprocessor slots from `A32::UserConfig`.
///
/// Upstream owner: `interface/A32/config.h::UserConfig::coprocessors`.
pub type Coprocessors = [Option<Arc<dyn Coprocessor>>; 16];

pub fn empty_coprocessors() -> Coprocessors {
    [const { None }; 16]
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
}

/// Exception reported through `A64::UserCallbacks::ExceptionRaised`.
///
/// Upstream owner: `interface/A64/config.h::Exception`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum Exception {
    UnallocatedEncoding = 0,
    ReservedValue = 1,
    UnpredictableInstruction = 2,
    WaitForInterrupt = 3,
    WaitForEvent = 4,
    SendEvent = 5,
    SendEventLocal = 6,
    Yield = 7,
    Breakpoint = 8,
    NoExecuteFault = 9,
}

/// Data-cache maintenance operation reported by A64 IR.
///
/// Upstream owner: `interface/A64/config.h::DataCacheOperation`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum DataCacheOperation {
    CleanAndInvalidateBySetWay,
    CleanAndInvalidateByVaToPoC,
    CleanBySetWay,
    CleanByVaToPoC,
    CleanByVaToPoU,
    CleanByVaToPoP,
    InvalidateBySetWay,
    InvalidateByVaToPoC,
    ZeroByVa,
}

/// Instruction-cache maintenance operation reported by A64 IR.
///
/// Upstream owner: `interface/A64/config.h::InstructionCacheOperation`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum InstructionCacheOperation {
    InvalidateByVaToPoU,
    InvalidateAllToPoU,
    InvalidateAllToPoUInnerSharable,
}

/// The 128-bit vector value exchanged by A64 memory callbacks.
pub type Vector = [u64; 2];

/// Host callbacks inserted into generated A64 code.
///
/// Upstream owner: `interface/A64/config.h::UserCallbacks`.
pub trait UserCallbacks: Send {
    fn memory_read_code(&self, vaddr: u64) -> Option<u32> {
        Some(self.memory_read_32(vaddr))
    }

    fn memory_read_8(&self, vaddr: u64) -> u8;
    fn memory_read_16(&self, vaddr: u64) -> u16;
    fn memory_read_32(&self, vaddr: u64) -> u32;
    fn memory_read_64(&self, vaddr: u64) -> u64;
    fn memory_read_128(&self, vaddr: u64) -> Vector;

    fn memory_write_8(&mut self, vaddr: u64, value: u8);
    fn memory_write_16(&mut self, vaddr: u64, value: u16);
    fn memory_write_32(&mut self, vaddr: u64, value: u32);
    fn memory_write_64(&mut self, vaddr: u64, value: u64);
    fn memory_write_128(&mut self, vaddr: u64, value: Vector);

    fn memory_write_exclusive_8(&mut self, _vaddr: u64, _value: u8, _expected: u8) -> bool {
        false
    }

    fn memory_write_exclusive_16(&mut self, _vaddr: u64, _value: u16, _expected: u16) -> bool {
        false
    }

    fn memory_write_exclusive_32(&mut self, _vaddr: u64, _value: u32, _expected: u32) -> bool {
        false
    }

    fn memory_write_exclusive_64(&mut self, _vaddr: u64, _value: u64, _expected: u64) -> bool {
        false
    }

    fn memory_write_exclusive_128(
        &mut self,
        _vaddr: u64,
        _value: Vector,
        _expected: Vector,
    ) -> bool {
        false
    }

    fn is_read_only_memory(&self, _vaddr: u64) -> bool {
        false
    }

    fn call_svc(&mut self, swi: u32);
    fn exception_raised(&mut self, pc: u64, exception: Exception);
    fn data_cache_operation_raised(&mut self, _op: DataCacheOperation, _value: u64) {}
    fn instruction_cache_operation_raised(&mut self, _op: InstructionCacheOperation, _value: u64) {}
    fn instruction_synchronization_barrier_raised(&mut self) {}

    fn add_ticks(&mut self, ticks: u64);
    fn get_ticks_remaining(&self) -> u64;
    fn get_cntpct(&self) -> u64;
}

#[cfg(test)]
mod tests {
    use super::{DataCacheOperation, Exception, InstructionCacheOperation, UserCallbacks, Vector};

    #[test]
    fn exception_values_and_layout_match_upstream() {
        let values = [
            Exception::UnallocatedEncoding,
            Exception::ReservedValue,
            Exception::UnpredictableInstruction,
            Exception::WaitForInterrupt,
            Exception::WaitForEvent,
            Exception::SendEvent,
            Exception::SendEventLocal,
            Exception::Yield,
            Exception::Breakpoint,
            Exception::NoExecuteFault,
        ];
        for (expected, exception) in values.into_iter().enumerate() {
            assert_eq!(exception as i32, expected as i32);
        }
        assert_eq!(std::mem::size_of::<Exception>(), 4);
        assert_eq!(std::mem::align_of::<Exception>(), 4);
    }

    #[test]
    fn cache_operation_discriminants_match_upstream() {
        let data = [
            DataCacheOperation::CleanAndInvalidateBySetWay,
            DataCacheOperation::CleanAndInvalidateByVaToPoC,
            DataCacheOperation::CleanBySetWay,
            DataCacheOperation::CleanByVaToPoC,
            DataCacheOperation::CleanByVaToPoU,
            DataCacheOperation::CleanByVaToPoP,
            DataCacheOperation::InvalidateBySetWay,
            DataCacheOperation::InvalidateByVaToPoC,
            DataCacheOperation::ZeroByVa,
        ];
        for (expected, operation) in data.into_iter().enumerate() {
            assert_eq!(operation as usize, expected);
        }
        assert_eq!(std::mem::size_of::<DataCacheOperation>(), 4);
        assert_eq!(std::mem::align_of::<DataCacheOperation>(), 4);

        assert_eq!(InstructionCacheOperation::InvalidateByVaToPoU as i32, 0);
        assert_eq!(InstructionCacheOperation::InvalidateAllToPoU as i32, 1);
        assert_eq!(
            InstructionCacheOperation::InvalidateAllToPoUInnerSharable as i32,
            2
        );
        assert_eq!(std::mem::size_of::<InstructionCacheOperation>(), 4);
        assert_eq!(std::mem::align_of::<InstructionCacheOperation>(), 4);
    }

    struct DefaultCallbacks;

    impl UserCallbacks for DefaultCallbacks {
        fn memory_read_8(&self, _vaddr: u64) -> u8 {
            0
        }

        fn memory_read_16(&self, _vaddr: u64) -> u16 {
            0
        }

        fn memory_read_32(&self, _vaddr: u64) -> u32 {
            0
        }

        fn memory_read_64(&self, _vaddr: u64) -> u64 {
            0
        }

        fn memory_read_128(&self, _vaddr: u64) -> Vector {
            [0; 2]
        }

        fn memory_write_8(&mut self, _vaddr: u64, _value: u8) {}
        fn memory_write_16(&mut self, _vaddr: u64, _value: u16) {}
        fn memory_write_32(&mut self, _vaddr: u64, _value: u32) {}
        fn memory_write_64(&mut self, _vaddr: u64, _value: u64) {}
        fn memory_write_128(&mut self, _vaddr: u64, _value: Vector) {}
        fn call_svc(&mut self, _swi: u32) {}
        fn exception_raised(&mut self, _pc: u64, _exception: Exception) {}
        fn add_ticks(&mut self, _ticks: u64) {}

        fn get_ticks_remaining(&self) -> u64 {
            0
        }

        fn get_cntpct(&self) -> u64 {
            0
        }
    }

    #[test]
    fn callback_defaults_match_upstream() {
        let mut callbacks = DefaultCallbacks;
        assert_eq!(std::mem::size_of::<Vector>(), 16);
        assert_eq!(std::mem::align_of::<Vector>(), 8);
        assert_eq!(callbacks.memory_read_code(0), Some(0));
        assert!(!callbacks.memory_write_exclusive_8(0, 0, 0));
        assert!(!callbacks.memory_write_exclusive_16(0, 0, 0));
        assert!(!callbacks.memory_write_exclusive_32(0, 0, 0));
        assert!(!callbacks.memory_write_exclusive_64(0, 0, 0));
        assert!(!callbacks.memory_write_exclusive_128(0, [0; 2], [0; 2]));
        assert!(!callbacks.is_read_only_memory(0));
    }
}

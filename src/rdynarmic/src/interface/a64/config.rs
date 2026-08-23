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

#[cfg(test)]
mod tests {
    use super::{DataCacheOperation, Exception, InstructionCacheOperation};

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
}

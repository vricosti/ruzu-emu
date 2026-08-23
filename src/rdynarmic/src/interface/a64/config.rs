/// Data-cache maintenance operation reported by A64 IR.
///
/// Upstream owner: `interface/A64/config.h::DataCacheOperation`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
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
#[repr(u8)]
pub enum InstructionCacheOperation {
    InvalidateByVaToPoU,
    InvalidateAllToPoU,
    InvalidateAllToPoUInnerSharable,
}

#[cfg(test)]
mod tests {
    use super::{DataCacheOperation, InstructionCacheOperation};

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

        assert_eq!(InstructionCacheOperation::InvalidateByVaToPoU as u8, 0);
        assert_eq!(InstructionCacheOperation::InvalidateAllToPoU as u8, 1);
        assert_eq!(
            InstructionCacheOperation::InvalidateAllToPoUInnerSharable as u8,
            2
        );
    }
}

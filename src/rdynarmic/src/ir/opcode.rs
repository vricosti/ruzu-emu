use crate::ir::types::Type;
use std::fmt;

/// IR opcodes. Ported from dynarmic's opcodes.inc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
#[allow(non_camel_case_types)]
pub enum Opcode {
    // --- Core ---
    Void,
    Identity,
    Breakpoint,
    CallHostFunction,

    // --- A64 context getters/setters ---
    A64SetCheckBit,
    A64GetCFlag,
    A64GetNZCVRaw,
    A64SetNZCVRaw,
    A64SetNZCV,
    A64GetW,
    A64GetX,
    A64GetS,
    A64GetD,
    A64GetQ,
    A64GetSP,
    A64GetFPCR,
    A64GetFPSR,
    A64SetW,
    A64SetX,
    A64SetS,
    A64SetD,
    A64SetQ,
    A64SetSP,
    A64SetPC,
    A64SetFPCR,
    A64SetFPSR,
    A64CallSupervisor,
    A64ExceptionRaised,
    A64DataCacheOperationRaised,
    A64InstructionCacheOperationRaised,
    A64DataSynchronizationBarrier,
    A64DataMemoryBarrier,
    A64InstructionSynchronizationBarrier,
    A64GetCNTFRQ,
    A64GetCNTPCT,
    A64GetCTR,
    A64GetDCZID,
    A64GetTPIDR,
    A64SetTPIDR,
    A64GetTPIDRRO,

    // --- RSB ---
    PushRSB,

    // --- Flags ---
    GetCarryFromOp,
    GetOverflowFromOp,
    GetGEFromOp,
    GetNZCVFromOp,
    GetNZFromOp,
    GetUpperFromOp,
    GetLowerFromOp,
    GetCFlagFromNZCV,
    NZCVFromPackedFlags,
    // AddWithCarry returns a combined result + flags
    // handled via GetCarryFromOp/GetOverflowFromOp
    // --- Pseudo ---
    SetInsertionPoint,
    GetInsertionPoint,

    // --- ALU 32-bit ---
    Pack2x32To1x64,
    Pack2x64To1x128,
    LeastSignificantWord,
    MostSignificantWord,
    LeastSignificantHalf,
    LeastSignificantByte,
    MostSignificantBit,
    IsZero32,
    IsZero64,
    TestBit,
    ConditionalSelect32,
    ConditionalSelect64,
    ConditionalSelectNZCV,
    LogicalShiftLeft32,
    LogicalShiftLeft64,
    LogicalShiftRight32,
    LogicalShiftRight64,
    ArithmeticShiftRight32,
    ArithmeticShiftRight64,
    BitRotateRight32,
    BitRotateRight64,
    RotateRightExtended,
    LogicalShiftLeftMasked32,
    LogicalShiftLeftMasked64,
    LogicalShiftRightMasked32,
    LogicalShiftRightMasked64,
    ArithmeticShiftRightMasked32,
    ArithmeticShiftRightMasked64,
    RotateRightMasked32,
    RotateRightMasked64,
    Add32,
    Add64,
    Sub32,
    Sub64,
    Mul32,
    Mul64,
    SignedMultiplyHigh64,
    UnsignedMultiplyHigh64,
    UnsignedDiv32,
    UnsignedDiv64,
    SignedDiv32,
    SignedDiv64,
    And32,
    And64,
    AndNot32,
    AndNot64,
    Eor32,
    Eor64,
    Or32,
    Or64,
    Not32,
    Not64,
    SignExtendByteToWord,
    SignExtendHalfToWord,
    SignExtendByteToLong,
    SignExtendHalfToLong,
    SignExtendWordToLong,
    ZeroExtendByteToWord,
    ZeroExtendHalfToWord,
    ZeroExtendByteToLong,
    ZeroExtendHalfToLong,
    ZeroExtendWordToLong,
    ZeroExtendLongToQuad,
    ByteReverseWord,
    ByteReverseHalf,
    ByteReverseDual,
    CountLeadingZeros32,
    CountLeadingZeros64,
    ExtractRegister32,
    ExtractRegister64,
    ReplicateBit32,
    ReplicateBit64,

    // --- Saturated arithmetic ---
    MaxSigned32,
    MaxSigned64,
    MaxUnsigned32,
    MaxUnsigned64,
    MinSigned32,
    MinSigned64,
    MinUnsigned32,
    MinUnsigned64,
    SignedSaturatedAddWithFlag32,
    SignedSaturatedSubWithFlag32,
    SignedSaturatedAdd8,
    SignedSaturatedAdd16,
    SignedSaturatedAdd32,
    SignedSaturatedAdd64,
    SignedSaturatedDoublingMultiplyReturnHigh16,
    SignedSaturatedDoublingMultiplyReturnHigh32,
    SignedSaturatedSub8,
    SignedSaturatedSub16,
    SignedSaturatedSub32,
    SignedSaturatedSub64,
    SignedSaturation,
    UnsignedSaturatedAdd8,
    UnsignedSaturatedAdd16,
    UnsignedSaturatedAdd32,
    UnsignedSaturatedAdd64,
    UnsignedSaturatedSub8,
    UnsignedSaturatedSub16,
    UnsignedSaturatedSub32,
    UnsignedSaturatedSub64,
    UnsignedSaturation,

    // --- Packed operations ---
    PackedAddU8,
    PackedAddS8,
    PackedAddU16,
    PackedAddS16,
    PackedSubU8,
    PackedSubS8,
    PackedSubU16,
    PackedSubS16,
    PackedAddSubU16,
    PackedAddSubS16,
    PackedSubAddU16,
    PackedSubAddS16,
    PackedHalvingAddU8,
    PackedHalvingAddS8,
    PackedHalvingAddU16,
    PackedHalvingAddS16,
    PackedHalvingSubU8,
    PackedHalvingSubS8,
    PackedHalvingSubU16,
    PackedHalvingSubS16,
    PackedHalvingAddSubU16,
    PackedHalvingAddSubS16,
    PackedHalvingSubAddU16,
    PackedHalvingSubAddS16,
    PackedSaturatedAddU8,
    PackedSaturatedAddS8,
    PackedSaturatedSubU8,
    PackedSaturatedSubS8,
    PackedSaturatedAddU16,
    PackedSaturatedAddS16,
    PackedSaturatedSubU16,
    PackedSaturatedSubS16,
    PackedAbsDiffSumU8,
    PackedSelect,

    // --- CRC32 ---
    CRC32Castagnoli8,
    CRC32Castagnoli16,
    CRC32Castagnoli32,
    CRC32Castagnoli64,
    CRC32ISO8,
    CRC32ISO16,
    CRC32ISO32,
    CRC32ISO64,

    // --- AES ---
    AESDecryptSingleRound,
    AESEncryptSingleRound,
    AESInverseMixColumns,
    AESMixColumns,

    // --- SHA ---
    SM4AccessSubstitutionBox,
    SHA256Hash,
    SHA256MessageSchedule0,
    SHA256MessageSchedule1,

    // --- Vector operations ---
    VectorGetElement8,
    VectorGetElement16,
    VectorGetElement32,
    VectorGetElement64,
    VectorSetElement8,
    VectorSetElement16,
    VectorSetElement32,
    VectorSetElement64,
    VectorAbs8,
    VectorAbs16,
    VectorAbs32,
    VectorAbs64,
    VectorAdd8,
    VectorAdd16,
    VectorAdd32,
    VectorAdd64,
    VectorAnd,
    VectorAndNot,
    VectorArithmeticShiftRight8,
    VectorArithmeticShiftRight16,
    VectorArithmeticShiftRight32,
    VectorArithmeticShiftRight64,
    VectorArithmeticVShift8,
    VectorArithmeticVShift16,
    VectorArithmeticVShift32,
    VectorArithmeticVShift64,
    VectorBroadcastLower8,
    VectorBroadcastLower16,
    VectorBroadcastLower32,
    VectorBroadcast8,
    VectorBroadcast16,
    VectorBroadcast32,
    VectorBroadcast64,
    VectorBroadcastElementLower8,
    VectorBroadcastElementLower16,
    VectorBroadcastElementLower32,
    VectorBroadcastElement8,
    VectorBroadcastElement16,
    VectorBroadcastElement32,
    VectorBroadcastElement64,
    VectorCountLeadingZeros8,
    VectorCountLeadingZeros16,
    VectorCountLeadingZeros32,
    VectorDeinterleaveEven8,
    VectorDeinterleaveEven16,
    VectorDeinterleaveEven32,
    VectorDeinterleaveEven64,
    VectorDeinterleaveEvenLower8,
    VectorDeinterleaveEvenLower16,
    VectorDeinterleaveEvenLower32,
    VectorDeinterleaveOdd8,
    VectorDeinterleaveOdd16,
    VectorDeinterleaveOdd32,
    VectorDeinterleaveOdd64,
    VectorDeinterleaveOddLower8,
    VectorDeinterleaveOddLower16,
    VectorDeinterleaveOddLower32,
    VectorEor,
    VectorEqual8,
    VectorEqual16,
    VectorEqual32,
    VectorEqual64,
    VectorEqual128,
    VectorExtract,
    VectorExtractLower,
    VectorRotateWholeVectorRight,
    VectorGreaterEqualSigned8,
    VectorGreaterEqualSigned16,
    VectorGreaterEqualSigned32,
    VectorGreaterEqualSigned64,
    VectorGreaterEqualUnsigned8,
    VectorGreaterEqualUnsigned16,
    VectorGreaterEqualUnsigned32,
    VectorGreaterEqualUnsigned64,
    VectorGreaterS8,
    VectorGreaterS16,
    VectorGreaterS32,
    VectorGreaterS64,
    VectorLessEqualSigned8,
    VectorLessEqualSigned16,
    VectorLessEqualSigned32,
    VectorLessEqualSigned64,
    VectorLessSigned8,
    VectorLessSigned16,
    VectorLessSigned32,
    VectorLessSigned64,
    VectorHalvingAddS8,
    VectorHalvingAddS16,
    VectorHalvingAddS32,
    VectorHalvingAddU8,
    VectorHalvingAddU16,
    VectorHalvingAddU32,
    VectorHalvingSubS8,
    VectorHalvingSubS16,
    VectorHalvingSubS32,
    VectorHalvingSubU8,
    VectorHalvingSubU16,
    VectorHalvingSubU32,
    VectorInterleaveLower8,
    VectorInterleaveLower16,
    VectorInterleaveLower32,
    VectorInterleaveLower64,
    VectorInterleaveUpper8,
    VectorInterleaveUpper16,
    VectorInterleaveUpper32,
    VectorInterleaveUpper64,
    VectorLogicalShiftLeft8,
    VectorLogicalShiftLeft16,
    VectorLogicalShiftLeft32,
    VectorLogicalShiftLeft64,
    VectorLogicalShiftRight8,
    VectorLogicalShiftRight16,
    VectorLogicalShiftRight32,
    VectorLogicalShiftRight64,
    VectorLogicalVShift8,
    VectorLogicalVShift16,
    VectorLogicalVShift32,
    VectorLogicalVShift64,
    VectorMaxS8,
    VectorMaxS16,
    VectorMaxS32,
    VectorMaxS64,
    VectorMaxU8,
    VectorMaxU16,
    VectorMaxU32,
    VectorMaxU64,
    VectorMinS8,
    VectorMinS16,
    VectorMinS32,
    VectorMinS64,
    VectorMinU8,
    VectorMinU16,
    VectorMinU32,
    VectorMinU64,
    VectorMultiply8,
    VectorMultiply16,
    VectorMultiply32,
    VectorMultiply64,
    VectorMultiplySignedWiden8,
    VectorMultiplySignedWiden16,
    VectorMultiplySignedWiden32,
    VectorMultiplyUnsignedWiden8,
    VectorMultiplyUnsignedWiden16,
    VectorMultiplyUnsignedWiden32,
    VectorNarrow16,
    VectorNarrow32,
    VectorNarrow64,
    VectorNot,
    VectorOr,
    VectorPairedAddLower8,
    VectorPairedAddLower16,
    VectorPairedAddLower32,
    VectorPairedAdd8,
    VectorPairedAdd16,
    VectorPairedAdd32,
    VectorPairedAdd64,
    VectorPairedAddSignedWiden8,
    VectorPairedAddSignedWiden16,
    VectorPairedAddSignedWiden32,
    VectorPairedAddUnsignedWiden8,
    VectorPairedAddUnsignedWiden16,
    VectorPairedAddUnsignedWiden32,
    VectorPairedMaxS8,
    VectorPairedMaxS16,
    VectorPairedMaxS32,
    VectorPairedMaxU8,
    VectorPairedMaxU16,
    VectorPairedMaxU32,
    VectorPairedMaxLowerS8,
    VectorPairedMaxLowerS16,
    VectorPairedMaxLowerS32,
    VectorPairedMaxLowerU8,
    VectorPairedMaxLowerU16,
    VectorPairedMaxLowerU32,
    VectorPairedMinS8,
    VectorPairedMinS16,
    VectorPairedMinS32,
    VectorPairedMinU8,
    VectorPairedMinU16,
    VectorPairedMinU32,
    VectorPairedMinLowerS8,
    VectorPairedMinLowerS16,
    VectorPairedMinLowerS32,
    VectorPairedMinLowerU8,
    VectorPairedMinLowerU16,
    VectorPairedMinLowerU32,
    VectorPolynomialMultiply8,
    VectorPolynomialMultiplyLong8,
    VectorPolynomialMultiplyLong64,
    VectorPopulationCount,
    VectorReverseBits,
    VectorReverseElementsInHalfGroups8,
    VectorReverseElementsInWordGroups8,
    VectorReverseElementsInWordGroups16,
    VectorReverseElementsInLongGroups8,
    VectorReverseElementsInLongGroups16,
    VectorReverseElementsInLongGroups32,
    VectorReduceAdd8,
    VectorReduceAdd16,
    VectorReduceAdd32,
    VectorReduceAdd64,
    VectorRoundingHalvingAddS8,
    VectorRoundingHalvingAddS16,
    VectorRoundingHalvingAddS32,
    VectorRoundingHalvingAddU8,
    VectorRoundingHalvingAddU16,
    VectorRoundingHalvingAddU32,
    VectorRoundingShiftLeftS8,
    VectorRoundingShiftLeftS16,
    VectorRoundingShiftLeftS32,
    VectorRoundingShiftLeftS64,
    VectorRoundingShiftLeftU8,
    VectorRoundingShiftLeftU16,
    VectorRoundingShiftLeftU32,
    VectorRoundingShiftLeftU64,
    VectorShuffleHighHalfwords,
    VectorShuffleLowHalfwords,
    VectorShuffleWords,
    VectorSignExtend8,
    VectorSignExtend16,
    VectorSignExtend32,
    VectorSignExtend64,
    VectorSignedAbsoluteDifference8,
    VectorSignedAbsoluteDifference16,
    VectorSignedAbsoluteDifference32,
    VectorSignedMultiply16,
    VectorSignedMultiply32,
    VectorSignedSaturatedAbs8,
    VectorSignedSaturatedAbs16,
    VectorSignedSaturatedAbs32,
    VectorSignedSaturatedAbs64,
    VectorSignedSaturatedAccumulateUnsigned8,
    VectorSignedSaturatedAccumulateUnsigned16,
    VectorSignedSaturatedAccumulateUnsigned32,
    VectorSignedSaturatedAccumulateUnsigned64,
    VectorSignedSaturatedAdd8,
    VectorSignedSaturatedAdd16,
    VectorSignedSaturatedAdd32,
    VectorSignedSaturatedAdd64,
    VectorSignedSaturatedDoublingMultiplyHigh16,
    VectorSignedSaturatedDoublingMultiplyHigh32,
    VectorSignedSaturatedDoublingMultiplyHighRounding16,
    VectorSignedSaturatedDoublingMultiplyHighRounding32,
    VectorSignedSaturatedDoublingMultiplyLong16,
    VectorSignedSaturatedDoublingMultiplyLong32,
    VectorSignedSaturatedNarrowToSigned16,
    VectorSignedSaturatedNarrowToSigned32,
    VectorSignedSaturatedNarrowToSigned64,
    VectorSignedSaturatedNarrowToUnsigned16,
    VectorSignedSaturatedNarrowToUnsigned32,
    VectorSignedSaturatedNarrowToUnsigned64,
    VectorSignedSaturatedNeg8,
    VectorSignedSaturatedNeg16,
    VectorSignedSaturatedNeg32,
    VectorSignedSaturatedNeg64,
    VectorSignedSaturatedSub8,
    VectorSignedSaturatedSub16,
    VectorSignedSaturatedSub32,
    VectorSignedSaturatedSub64,
    VectorSignedSaturatedShiftLeft8,
    VectorSignedSaturatedShiftLeft16,
    VectorSignedSaturatedShiftLeft32,
    VectorSignedSaturatedShiftLeft64,
    VectorSignedSaturatedShiftLeftUnsigned8,
    VectorSignedSaturatedShiftLeftUnsigned16,
    VectorSignedSaturatedShiftLeftUnsigned32,
    VectorSignedSaturatedShiftLeftUnsigned64,
    VectorSub8,
    VectorSub16,
    VectorSub32,
    VectorSub64,
    VectorTable,
    VectorTableLookup64,
    VectorTableLookup128,
    VectorTranspose8,
    VectorTranspose16,
    VectorTranspose32,
    VectorTranspose64,
    VectorUnsignedAbsoluteDifference8,
    VectorUnsignedAbsoluteDifference16,
    VectorUnsignedAbsoluteDifference32,
    VectorUnsignedMultiply16,
    VectorUnsignedMultiply32,
    VectorUnsignedRecipEstimate,
    VectorUnsignedRecipSqrtEstimate,
    VectorUnsignedSaturatedAccumulateSigned8,
    VectorUnsignedSaturatedAccumulateSigned16,
    VectorUnsignedSaturatedAccumulateSigned32,
    VectorUnsignedSaturatedAccumulateSigned64,
    VectorUnsignedSaturatedAdd8,
    VectorUnsignedSaturatedAdd16,
    VectorUnsignedSaturatedAdd32,
    VectorUnsignedSaturatedAdd64,
    VectorUnsignedSaturatedNarrow16,
    VectorUnsignedSaturatedNarrow32,
    VectorUnsignedSaturatedNarrow64,
    VectorUnsignedSaturatedSub8,
    VectorUnsignedSaturatedSub16,
    VectorUnsignedSaturatedSub32,
    VectorUnsignedSaturatedSub64,
    VectorUnsignedSaturatedShiftLeft8,
    VectorUnsignedSaturatedShiftLeft16,
    VectorUnsignedSaturatedShiftLeft32,
    VectorUnsignedSaturatedShiftLeft64,
    VectorZeroExtend8,
    VectorZeroExtend16,
    VectorZeroExtend32,
    VectorZeroExtend64,
    VectorZeroUpper,
    ZeroVector,

    // --- Floating-point scalar operations ---
    FPAbs16,
    FPAbs32,
    FPAbs64,
    FPAdd32,
    FPAdd64,
    FPCompare32,
    FPCompare64,
    FPDiv32,
    FPDiv64,
    FPMax32,
    FPMax64,
    FPMaxNumeric32,
    FPMaxNumeric64,
    FPMin32,
    FPMin64,
    FPMinNumeric32,
    FPMinNumeric64,
    FPMul32,
    FPMul64,
    FPMulAdd16,
    FPMulAdd32,
    FPMulAdd64,
    FPMulSub16,
    FPMulSub32,
    FPMulSub64,
    FPMulX32,
    FPMulX64,
    FPNeg16,
    FPNeg32,
    FPNeg64,
    FPRecipEstimate16,
    FPRecipEstimate32,
    FPRecipEstimate64,
    FPRecipExponent16,
    FPRecipExponent32,
    FPRecipExponent64,
    FPRecipStepFused16,
    FPRecipStepFused32,
    FPRecipStepFused64,
    FPRoundInt16,
    FPRoundInt32,
    FPRoundInt64,
    FPRSqrtEstimate16,
    FPRSqrtEstimate32,
    FPRSqrtEstimate64,
    FPRSqrtStepFused16,
    FPRSqrtStepFused32,
    FPRSqrtStepFused64,
    FPSqrt32,
    FPSqrt64,
    FPSub32,
    FPSub64,

    // --- Floating-point conversions ---
    FPHalfToDouble,
    FPHalfToSingle,
    FPSingleToDouble,
    FPSingleToHalf,
    FPDoubleToHalf,
    FPDoubleToSingle,
    FPDoubleToFixedS16,
    FPDoubleToFixedS32,
    FPDoubleToFixedS64,
    FPDoubleToFixedU16,
    FPDoubleToFixedU32,
    FPDoubleToFixedU64,
    FPHalfToFixedS16,
    FPHalfToFixedS32,
    FPHalfToFixedS64,
    FPHalfToFixedU16,
    FPHalfToFixedU32,
    FPHalfToFixedU64,
    FPSingleToFixedS16,
    FPSingleToFixedS32,
    FPSingleToFixedS64,
    FPSingleToFixedU16,
    FPSingleToFixedU32,
    FPSingleToFixedU64,
    FPFixedU16ToSingle,
    FPFixedS16ToSingle,
    FPFixedU16ToDouble,
    FPFixedS16ToDouble,
    FPFixedU32ToSingle,
    FPFixedS32ToSingle,
    FPFixedU32ToDouble,
    FPFixedS32ToDouble,
    FPFixedU64ToDouble,
    FPFixedU64ToSingle,
    FPFixedS64ToDouble,
    FPFixedS64ToSingle,

    // --- Floating-point vector operations ---
    FPVectorAbs16,
    FPVectorAbs32,
    FPVectorAbs64,
    FPVectorAdd32,
    FPVectorAdd64,
    FPVectorDiv32,
    FPVectorDiv64,
    FPVectorEqual16,
    FPVectorEqual32,
    FPVectorEqual64,
    FPVectorFromHalf32,
    FPVectorFromSignedFixed32,
    FPVectorFromSignedFixed64,
    FPVectorFromUnsignedFixed32,
    FPVectorFromUnsignedFixed64,
    FPVectorGreater32,
    FPVectorGreater64,
    FPVectorGreaterEqual32,
    FPVectorGreaterEqual64,
    FPVectorMax32,
    FPVectorMax64,
    FPVectorMaxNumeric32,
    FPVectorMaxNumeric64,
    FPVectorMin32,
    FPVectorMin64,
    FPVectorMinNumeric32,
    FPVectorMinNumeric64,
    FPVectorMul32,
    FPVectorMul64,
    FPVectorMulAdd16,
    FPVectorMulAdd32,
    FPVectorMulAdd64,
    FPVectorMulX32,
    FPVectorMulX64,
    FPVectorNeg16,
    FPVectorNeg32,
    FPVectorNeg64,
    FPVectorPairedAdd32,
    FPVectorPairedAdd64,
    FPVectorPairedAddLower32,
    FPVectorPairedAddLower64,
    FPVectorRecipEstimate16,
    FPVectorRecipEstimate32,
    FPVectorRecipEstimate64,
    FPVectorRecipStepFused16,
    FPVectorRecipStepFused32,
    FPVectorRecipStepFused64,
    FPVectorRoundInt16,
    FPVectorRoundInt32,
    FPVectorRoundInt64,
    FPVectorRSqrtEstimate16,
    FPVectorRSqrtEstimate32,
    FPVectorRSqrtEstimate64,
    FPVectorRSqrtStepFused16,
    FPVectorRSqrtStepFused32,
    FPVectorRSqrtStepFused64,
    FPVectorSqrt32,
    FPVectorSqrt64,
    FPVectorSub32,
    FPVectorSub64,
    FPVectorToHalf32,
    FPVectorToSignedFixed16,
    FPVectorToSignedFixed32,
    FPVectorToSignedFixed64,
    FPVectorToUnsignedFixed16,
    FPVectorToUnsignedFixed32,
    FPVectorToUnsignedFixed64,

    // --- A64 Memory access ---
    A64ClearExclusive,
    A64ReadMemory8,
    A64ReadMemory16,
    A64ReadMemory32,
    A64ReadMemory64,
    A64ReadMemory128,
    A64ExclusiveReadMemory8,
    A64ExclusiveReadMemory16,
    A64ExclusiveReadMemory32,
    A64ExclusiveReadMemory64,
    A64ExclusiveReadMemory128,
    A64WriteMemory8,
    A64WriteMemory16,
    A64WriteMemory32,
    A64WriteMemory64,
    A64WriteMemory128,
    A64ExclusiveWriteMemory8,
    A64ExclusiveWriteMemory16,
    A64ExclusiveWriteMemory32,
    A64ExclusiveWriteMemory64,
    A64ExclusiveWriteMemory128,

    // --- A32 context getters/setters ---
    A32SetCheckBit,
    A32GetCFlag,
    A32GetRegister,
    A32SetRegister,
    A32GetExtendedRegister32,
    A32GetExtendedRegister64,
    A32SetExtendedRegister32,
    A32SetExtendedRegister64,
    A32GetVector,
    A32SetVector,
    A32GetCpsr,
    A32SetCpsr,
    A32SetCpsrNZCVRaw,
    A32SetCpsrNZCV,
    A32SetCpsrNZCVQ,
    A32SetCpsrNZ,
    A32SetCpsrNZC,
    A32OrQFlag,
    A32GetGEFlags,
    A32SetGEFlags,
    A32SetGEFlagsCompressed,
    A32BXWritePC,
    A32UpdateUpperLocationDescriptor,
    A32CallSupervisor,
    /// Debug-only: per-instruction PC execution hook (RUZU_A32_PC_EXEC). Arg is
    /// the guest PC. Lowered to a host call into `a32_pc_trace_hook` after a
    /// guest-register flush, so the callback observes accurate r0-r15. Gated by
    /// an env var at translate time; no-op (not emitted) when unset.
    A32PcExecHook,
    A32ExceptionRaised,
    A32DataSynchronizationBarrier,
    A32DataMemoryBarrier,
    A32InstructionSynchronizationBarrier,
    A32GetFpscr,
    A32SetFpscr,
    A32GetFpscrNZCV,
    A32SetFpscrNZCV,

    // --- A32 Memory ---
    A32ClearExclusive,
    A32ReadMemory8,
    A32ReadMemory16,
    A32ReadMemory32,
    A32ReadMemory64,
    A32ExclusiveReadMemory8,
    A32ExclusiveReadMemory16,
    A32ExclusiveReadMemory32,
    A32ExclusiveReadMemory64,
    A32WriteMemory8,
    A32WriteMemory16,
    A32WriteMemory32,
    A32WriteMemory64,
    A32ExclusiveWriteMemory8,
    A32ExclusiveWriteMemory16,
    A32ExclusiveWriteMemory32,
    A32ExclusiveWriteMemory64,

    // --- A32 Coprocessor ---
    A32CoprocInternalOperation,
    A32CoprocSendOneWord,
    A32CoprocSendTwoWords,
    A32CoprocGetOneWord,
    A32CoprocGetTwoWords,
    A32CoprocLoadWords,
    A32CoprocStoreWords,
}

/// Opcode metadata: return type and argument types.
/// Using const arrays for cache-friendliness.
struct OpcodeInfo {
    ret: Type,
    args: &'static [Type],
}

impl Opcode {
    /// Returns the return type of this opcode.
    pub fn return_type(self) -> Type {
        self.info().ret
    }

    /// Returns the argument types of this opcode.
    pub fn arg_types(self) -> &'static [Type] {
        self.info().args
    }

    /// Returns the number of arguments this opcode takes.
    pub fn num_args(self) -> usize {
        self.info().args.len()
    }

    /// Returns true if this opcode has side effects (writes to state, memory, or control flow).
    pub fn has_side_effects(self) -> bool {
        use Opcode::*;
        matches!(
            self,
            Void | Breakpoint | CallHostFunction | PushRSB |
            A64SetCheckBit | A64SetNZCVRaw | A64SetNZCV |
            A64SetW | A64SetX | A64SetS | A64SetD | A64SetQ |
            A64SetSP | A64SetPC | A64SetFPCR | A64SetFPSR | A64SetTPIDR |
            A64CallSupervisor | A64ExceptionRaised |
            A64DataCacheOperationRaised | A64InstructionCacheOperationRaised |
            A64DataSynchronizationBarrier | A64DataMemoryBarrier |
            A64InstructionSynchronizationBarrier |
            A64ClearExclusive |
            A64ExclusiveReadMemory8 | A64ExclusiveReadMemory16 |
            A64ExclusiveReadMemory32 | A64ExclusiveReadMemory64 |
            A64ExclusiveReadMemory128 |
            A64WriteMemory8 | A64WriteMemory16 | A64WriteMemory32 |
            A64WriteMemory64 | A64WriteMemory128 |
            A64ExclusiveWriteMemory8 | A64ExclusiveWriteMemory16 |
            A64ExclusiveWriteMemory32 | A64ExclusiveWriteMemory64 |
            A64ExclusiveWriteMemory128 |
            // A32 side effects
            A32SetCheckBit | A32SetRegister |
            A32SetExtendedRegister32 | A32SetExtendedRegister64 | A32SetVector |
            A32SetCpsr | A32SetCpsrNZCVRaw | A32SetCpsrNZCV | A32SetCpsrNZCVQ |
            A32SetCpsrNZ | A32SetCpsrNZC | A32OrQFlag |
            A32SetGEFlags | A32SetGEFlagsCompressed |
            A32BXWritePC | A32UpdateUpperLocationDescriptor |
            A32CallSupervisor | A32PcExecHook | A32ExceptionRaised |
            A32DataSynchronizationBarrier | A32DataMemoryBarrier |
            A32InstructionSynchronizationBarrier |
            A32SetFpscr | A32SetFpscrNZCV |
            A32ClearExclusive |
            A32ExclusiveReadMemory8 | A32ExclusiveReadMemory16 |
            A32ExclusiveReadMemory32 | A32ExclusiveReadMemory64 |
            A32WriteMemory8 | A32WriteMemory16 | A32WriteMemory32 | A32WriteMemory64 |
            A32ExclusiveWriteMemory8 | A32ExclusiveWriteMemory16 |
            A32ExclusiveWriteMemory32 | A32ExclusiveWriteMemory64 |
            A32CoprocInternalOperation | A32CoprocSendOneWord | A32CoprocSendTwoWords |
            A32CoprocLoadWords | A32CoprocStoreWords
        )
    }

    /// Returns true if this is a memory read operation.
    pub fn is_memory_read(self) -> bool {
        use Opcode::*;
        matches!(
            self,
            A64ReadMemory8
                | A64ReadMemory16
                | A64ReadMemory32
                | A64ReadMemory64
                | A64ReadMemory128
                | A64ExclusiveReadMemory8
                | A64ExclusiveReadMemory16
                | A64ExclusiveReadMemory32
                | A64ExclusiveReadMemory64
                | A64ExclusiveReadMemory128
                | A32ReadMemory8
                | A32ReadMemory16
                | A32ReadMemory32
                | A32ReadMemory64
                | A32ExclusiveReadMemory8
                | A32ExclusiveReadMemory16
                | A32ExclusiveReadMemory32
                | A32ExclusiveReadMemory64
        )
    }

    /// Returns true if this is a memory write operation.
    pub fn is_memory_write(self) -> bool {
        use Opcode::*;
        matches!(
            self,
            A64WriteMemory8
                | A64WriteMemory16
                | A64WriteMemory32
                | A64WriteMemory64
                | A64WriteMemory128
                | A64ExclusiveWriteMemory8
                | A64ExclusiveWriteMemory16
                | A64ExclusiveWriteMemory32
                | A64ExclusiveWriteMemory64
                | A64ExclusiveWriteMemory128
                | A32WriteMemory8
                | A32WriteMemory16
                | A32WriteMemory32
                | A32WriteMemory64
                | A32ExclusiveWriteMemory8
                | A32ExclusiveWriteMemory16
                | A32ExclusiveWriteMemory32
                | A32ExclusiveWriteMemory64
        )
    }

    /// Returns true if this reads from an A64 core register.
    pub fn reads_from_core_register(self) -> bool {
        use Opcode::*;
        matches!(
            self,
            A32GetRegister
                | A32GetExtendedRegister32
                | A32GetExtendedRegister64
                | A32GetVector
                | A64GetW
                | A64GetX
                | A64GetS
                | A64GetD
                | A64GetQ
                | A64GetSP
        )
    }

    /// Returns true if this writes to an A64 core register.
    pub fn writes_to_core_register(self) -> bool {
        use Opcode::*;
        matches!(
            self,
            A32SetRegister
                | A32SetExtendedRegister32
                | A32SetExtendedRegister64
                | A32SetVector
                | A32BXWritePC
                | A64SetW
                | A64SetX
                | A64SetS
                | A64SetD
                | A64SetQ
                | A64SetSP
                | A64SetPC
        )
    }

    /// Returns true if this reads CPSR/NZCV flags.
    pub fn reads_cpsr(self) -> bool {
        use Opcode::*;
        matches!(
            self,
            A32GetCpsr
                | A32GetCFlag
                | A32GetGEFlags
                | A32UpdateUpperLocationDescriptor
                | A64GetCFlag
                | A64GetNZCVRaw
                | ConditionalSelect32
                | ConditionalSelect64
                | ConditionalSelectNZCV
        )
    }

    /// Returns true if this writes CPSR/NZCV flags.
    /// Matches upstream dynarmic `Inst::WritesToCPSR()`.
    pub fn writes_cpsr(self) -> bool {
        use Opcode::*;
        matches!(
            self,
            A32SetCpsr
                | A32SetCpsrNZCVRaw
                | A32SetCpsrNZCV
                | A32SetCpsrNZCVQ
                | A32SetCpsrNZ
                | A32SetCpsrNZC
                | A32OrQFlag
                | A32SetGEFlags
                | A32SetGEFlagsCompressed
                | A32UpdateUpperLocationDescriptor
                | A64SetNZCVRaw
                | A64SetNZCV
        )
    }

    /// Returns true if this reads FPCR.
    pub fn reads_from_fpcr(self) -> bool {
        matches!(self, Opcode::A64GetFPCR)
    }

    /// Returns true if this writes FPSR.
    pub fn writes_to_fpsr(self) -> bool {
        matches!(self, Opcode::A64SetFPSR)
    }

    /// Returns true if this is an A32-specific opcode.
    pub fn is_a32_opcode(self) -> bool {
        use Opcode::*;
        matches!(
            self,
            A32SetCheckBit
                | A32GetCFlag
                | A32GetRegister
                | A32SetRegister
                | A32GetExtendedRegister32
                | A32GetExtendedRegister64
                | A32SetExtendedRegister32
                | A32SetExtendedRegister64
                | A32GetVector
                | A32SetVector
                | A32GetCpsr
                | A32SetCpsr
                | A32SetCpsrNZCVRaw
                | A32SetCpsrNZCV
                | A32SetCpsrNZCVQ
                | A32SetCpsrNZ
                | A32SetCpsrNZC
                | A32OrQFlag
                | A32GetGEFlags
                | A32SetGEFlags
                | A32SetGEFlagsCompressed
                | A32BXWritePC
                | A32UpdateUpperLocationDescriptor
                | A32CallSupervisor
                | A32PcExecHook
                | A32ExceptionRaised
                | A32DataSynchronizationBarrier
                | A32DataMemoryBarrier
                | A32InstructionSynchronizationBarrier
                | A32GetFpscr
                | A32SetFpscr
                | A32GetFpscrNZCV
                | A32SetFpscrNZCV
                | A32ClearExclusive
                | A32ReadMemory8
                | A32ReadMemory16
                | A32ReadMemory32
                | A32ReadMemory64
                | A32ExclusiveReadMemory8
                | A32ExclusiveReadMemory16
                | A32ExclusiveReadMemory32
                | A32ExclusiveReadMemory64
                | A32WriteMemory8
                | A32WriteMemory16
                | A32WriteMemory32
                | A32WriteMemory64
                | A32ExclusiveWriteMemory8
                | A32ExclusiveWriteMemory16
                | A32ExclusiveWriteMemory32
                | A32ExclusiveWriteMemory64
                | A32CoprocInternalOperation
                | A32CoprocSendOneWord
                | A32CoprocSendTwoWords
                | A32CoprocGetOneWord
                | A32CoprocGetTwoWords
                | A32CoprocLoadWords
                | A32CoprocStoreWords
        )
    }

    fn info(self) -> OpcodeInfo {
        use Opcode::*;
        // Type aliases (avoiding glob import due to Void collision)
        const V: Type = Type::Void;
        const U1: Type = Type::U1;
        const U8: Type = Type::U8;
        const U16: Type = Type::U16;
        const U32: Type = Type::U32;
        const U64: Type = Type::U64;
        const U128: Type = Type::U128;
        const NZCV: Type = Type::NZCVFlags;
        const COND: Type = Type::Cond;
        const A64R: Type = Type::A64Reg;
        const A64V: Type = Type::A64Vec;
        const A32R: Type = Type::A32Reg;
        const A32E: Type = Type::A32ExtReg;
        const OPQ: Type = Type::Opaque;
        const ACC: Type = Type::AccType;
        const COPROC: Type = Type::CoprocInfo;
        match self {
            // Core
            Void => OpcodeInfo { ret: V, args: &[] },
            Identity => OpcodeInfo { ret: OPQ, args: &[OPQ] },
            Breakpoint => OpcodeInfo { ret: V, args: &[] },
            CallHostFunction => OpcodeInfo { ret: V, args: &[U64, OPQ, OPQ, OPQ] },

            // A64 context
            A64SetCheckBit => OpcodeInfo { ret: V, args: &[U1] },
            A64GetCFlag => OpcodeInfo { ret: U1, args: &[] },
            A64GetNZCVRaw => OpcodeInfo { ret: U32, args: &[] },
            A64SetNZCVRaw => OpcodeInfo { ret: V, args: &[U32] },
            A64SetNZCV => OpcodeInfo { ret: V, args: &[NZCV] },
            A64GetW => OpcodeInfo { ret: U32, args: &[A64R] },
            A64GetX => OpcodeInfo { ret: U64, args: &[A64R] },
            A64GetS => OpcodeInfo { ret: U128, args: &[A64V] },
            A64GetD => OpcodeInfo { ret: U128, args: &[A64V] },
            A64GetQ => OpcodeInfo { ret: U128, args: &[A64V] },
            A64GetSP => OpcodeInfo { ret: U64, args: &[] },
            A64GetFPCR => OpcodeInfo { ret: U32, args: &[] },
            A64GetFPSR => OpcodeInfo { ret: U32, args: &[] },
            A64SetW => OpcodeInfo { ret: V, args: &[A64R, U32] },
            A64SetX => OpcodeInfo { ret: V, args: &[A64R, U64] },
            A64SetS => OpcodeInfo { ret: V, args: &[A64V, U128] },
            A64SetD => OpcodeInfo { ret: V, args: &[A64V, U128] },
            A64SetQ => OpcodeInfo { ret: V, args: &[A64V, U128] },
            A64SetSP => OpcodeInfo { ret: V, args: &[U64] },
            A64SetPC => OpcodeInfo { ret: V, args: &[U64] },
            A64SetFPCR => OpcodeInfo { ret: V, args: &[U32] },
            A64SetFPSR => OpcodeInfo { ret: V, args: &[U32] },
            A64CallSupervisor => OpcodeInfo { ret: V, args: &[U32] },
            A64ExceptionRaised => OpcodeInfo { ret: V, args: &[U64, U64] },
            A64DataCacheOperationRaised => OpcodeInfo { ret: V, args: &[U64, U64] },
            A64InstructionCacheOperationRaised => OpcodeInfo { ret: V, args: &[U64, U64] },
            A64DataSynchronizationBarrier => OpcodeInfo { ret: V, args: &[] },
            A64DataMemoryBarrier => OpcodeInfo { ret: V, args: &[] },
            A64InstructionSynchronizationBarrier => OpcodeInfo { ret: V, args: &[] },
            A64GetCNTFRQ => OpcodeInfo { ret: U32, args: &[] },
            A64GetCNTPCT => OpcodeInfo { ret: U64, args: &[] },
            A64GetCTR => OpcodeInfo { ret: U32, args: &[] },
            A64GetDCZID => OpcodeInfo { ret: U32, args: &[] },
            A64GetTPIDR => OpcodeInfo { ret: U64, args: &[] },
            A64SetTPIDR => OpcodeInfo { ret: V, args: &[U64] },
            A64GetTPIDRRO => OpcodeInfo { ret: U64, args: &[] },

            // RSB
            PushRSB => OpcodeInfo { ret: V, args: &[U64] },

            // Flags
            GetCarryFromOp => OpcodeInfo { ret: U1, args: &[OPQ] },
            GetOverflowFromOp => OpcodeInfo { ret: U1, args: &[OPQ] },
            GetGEFromOp => OpcodeInfo { ret: U32, args: &[OPQ] },
            GetNZCVFromOp => OpcodeInfo { ret: NZCV, args: &[OPQ] },
            GetNZFromOp => OpcodeInfo { ret: NZCV, args: &[OPQ] },
            GetUpperFromOp => OpcodeInfo { ret: U128, args: &[OPQ] },
            GetLowerFromOp => OpcodeInfo { ret: U128, args: &[OPQ] },
            GetCFlagFromNZCV => OpcodeInfo { ret: U1, args: &[NZCV] },
            NZCVFromPackedFlags => OpcodeInfo { ret: NZCV, args: &[U32] },

            // Pseudo
            SetInsertionPoint | GetInsertionPoint => OpcodeInfo { ret: V, args: &[] },

            // Pack/extract
            Pack2x32To1x64 => OpcodeInfo { ret: U64, args: &[U32, U32] },
            Pack2x64To1x128 => OpcodeInfo { ret: U128, args: &[U64, U64] },
            LeastSignificantWord => OpcodeInfo { ret: U32, args: &[U64] },
            MostSignificantWord => OpcodeInfo { ret: U32, args: &[U64] },
            LeastSignificantHalf => OpcodeInfo { ret: U16, args: &[U32] },
            LeastSignificantByte => OpcodeInfo { ret: U8, args: &[U32] },
            MostSignificantBit => OpcodeInfo { ret: U1, args: &[U32] },
            IsZero32 => OpcodeInfo { ret: U1, args: &[U32] },
            IsZero64 => OpcodeInfo { ret: U1, args: &[U64] },
            TestBit => OpcodeInfo { ret: U1, args: &[U64, U8] },
            ConditionalSelect32 => OpcodeInfo { ret: U32, args: &[COND, U32, U32] },
            ConditionalSelect64 => OpcodeInfo { ret: U64, args: &[COND, U64, U64] },
            ConditionalSelectNZCV => OpcodeInfo { ret: NZCV, args: &[COND, NZCV, NZCV] },

            // Shifts
            LogicalShiftLeft32 => OpcodeInfo { ret: U32, args: &[U32, U8, U1] },
            LogicalShiftLeft64 => OpcodeInfo { ret: U64, args: &[U64, U8] },
            LogicalShiftRight32 => OpcodeInfo { ret: U32, args: &[U32, U8, U1] },
            LogicalShiftRight64 => OpcodeInfo { ret: U64, args: &[U64, U8] },
            ArithmeticShiftRight32 => OpcodeInfo { ret: U32, args: &[U32, U8, U1] },
            ArithmeticShiftRight64 => OpcodeInfo { ret: U64, args: &[U64, U8] },
            BitRotateRight32 => OpcodeInfo { ret: U32, args: &[U32, U8, U1] },
            BitRotateRight64 => OpcodeInfo { ret: U64, args: &[U64, U8] },
            RotateRightExtended => OpcodeInfo { ret: U32, args: &[U32, U1] },
            LogicalShiftLeftMasked32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            LogicalShiftLeftMasked64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            LogicalShiftRightMasked32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            LogicalShiftRightMasked64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            ArithmeticShiftRightMasked32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            ArithmeticShiftRightMasked64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            RotateRightMasked32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            RotateRightMasked64 => OpcodeInfo { ret: U64, args: &[U64, U64] },

            // ALU 32/64
            Add32 => OpcodeInfo { ret: U32, args: &[U32, U32, U1] },
            Add64 => OpcodeInfo { ret: U64, args: &[U64, U64, U1] },
            Sub32 => OpcodeInfo { ret: U32, args: &[U32, U32, U1] },
            Sub64 => OpcodeInfo { ret: U64, args: &[U64, U64, U1] },
            Mul32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            Mul64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            SignedMultiplyHigh64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            UnsignedMultiplyHigh64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            UnsignedDiv32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            UnsignedDiv64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            SignedDiv32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            SignedDiv64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            And32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            And64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            AndNot32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            AndNot64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            Eor32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            Eor64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            Or32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            Or64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            Not32 => OpcodeInfo { ret: U32, args: &[U32] },
            Not64 => OpcodeInfo { ret: U64, args: &[U64] },

            // Extensions
            SignExtendByteToWord => OpcodeInfo { ret: U32, args: &[U8] },
            SignExtendHalfToWord => OpcodeInfo { ret: U32, args: &[U16] },
            SignExtendByteToLong => OpcodeInfo { ret: U64, args: &[U8] },
            SignExtendHalfToLong => OpcodeInfo { ret: U64, args: &[U16] },
            SignExtendWordToLong => OpcodeInfo { ret: U64, args: &[U32] },
            ZeroExtendByteToWord => OpcodeInfo { ret: U32, args: &[U8] },
            ZeroExtendHalfToWord => OpcodeInfo { ret: U32, args: &[U16] },
            ZeroExtendByteToLong => OpcodeInfo { ret: U64, args: &[U8] },
            ZeroExtendHalfToLong => OpcodeInfo { ret: U64, args: &[U16] },
            ZeroExtendWordToLong => OpcodeInfo { ret: U64, args: &[U32] },
            ZeroExtendLongToQuad => OpcodeInfo { ret: U128, args: &[U64] },

            // Byte reverse
            ByteReverseWord => OpcodeInfo { ret: U32, args: &[U32] },
            ByteReverseHalf => OpcodeInfo { ret: U16, args: &[U16] },
            ByteReverseDual => OpcodeInfo { ret: U64, args: &[U64] },

            // Count/Extract
            CountLeadingZeros32 => OpcodeInfo { ret: U32, args: &[U32] },
            CountLeadingZeros64 => OpcodeInfo { ret: U64, args: &[U64] },
            ExtractRegister32 => OpcodeInfo { ret: U32, args: &[U32, U32, U8] },
            ExtractRegister64 => OpcodeInfo { ret: U64, args: &[U64, U64, U8] },
            ReplicateBit32 => OpcodeInfo { ret: U32, args: &[U32, U8] },
            ReplicateBit64 => OpcodeInfo { ret: U64, args: &[U64, U8] },

            // Min/Max
            MaxSigned32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            MaxSigned64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            MaxUnsigned32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            MaxUnsigned64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            MinSigned32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            MinSigned64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            MinUnsigned32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            MinUnsigned64 => OpcodeInfo { ret: U64, args: &[U64, U64] },

            // Saturated arithmetic
            SignedSaturatedAddWithFlag32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            SignedSaturatedSubWithFlag32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            SignedSaturatedAdd8 => OpcodeInfo { ret: U8, args: &[U8, U8] },
            SignedSaturatedAdd16 => OpcodeInfo { ret: U16, args: &[U16, U16] },
            SignedSaturatedAdd32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            SignedSaturatedAdd64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            SignedSaturatedDoublingMultiplyReturnHigh16 => OpcodeInfo { ret: U16, args: &[U16, U16] },
            SignedSaturatedDoublingMultiplyReturnHigh32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            SignedSaturatedSub8 => OpcodeInfo { ret: U8, args: &[U8, U8] },
            SignedSaturatedSub16 => OpcodeInfo { ret: U16, args: &[U16, U16] },
            SignedSaturatedSub32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            SignedSaturatedSub64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            SignedSaturation => OpcodeInfo { ret: U32, args: &[U32, U8] },
            UnsignedSaturatedAdd8 => OpcodeInfo { ret: U8, args: &[U8, U8] },
            UnsignedSaturatedAdd16 => OpcodeInfo { ret: U16, args: &[U16, U16] },
            UnsignedSaturatedAdd32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            UnsignedSaturatedAdd64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            UnsignedSaturatedSub8 => OpcodeInfo { ret: U8, args: &[U8, U8] },
            UnsignedSaturatedSub16 => OpcodeInfo { ret: U16, args: &[U16, U16] },
            UnsignedSaturatedSub32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            UnsignedSaturatedSub64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            UnsignedSaturation => OpcodeInfo { ret: U32, args: &[U32, U8] },

            // Packed operations (all U32->U32 pairs for ARM packed SIMD)
            PackedAddU8 | PackedAddS8 | PackedAddU16 | PackedAddS16 |
            PackedSubU8 | PackedSubS8 | PackedSubU16 | PackedSubS16 |
            PackedAddSubU16 | PackedAddSubS16 | PackedSubAddU16 | PackedSubAddS16 |
            PackedHalvingAddU8 | PackedHalvingAddS8 | PackedHalvingAddU16 | PackedHalvingAddS16 |
            PackedHalvingSubU8 | PackedHalvingSubS8 | PackedHalvingSubU16 | PackedHalvingSubS16 |
            PackedHalvingAddSubU16 | PackedHalvingAddSubS16 | PackedHalvingSubAddU16 | PackedHalvingSubAddS16 |
            PackedSaturatedAddU8 | PackedSaturatedAddS8 | PackedSaturatedSubU8 | PackedSaturatedSubS8 |
            PackedSaturatedAddU16 | PackedSaturatedAddS16 | PackedSaturatedSubU16 | PackedSaturatedSubS16
                => OpcodeInfo { ret: U32, args: &[U32, U32] },
            PackedAbsDiffSumU8 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            PackedSelect => OpcodeInfo { ret: U32, args: &[U32, U32, U32] },

            // CRC32
            CRC32Castagnoli8 | CRC32Castagnoli16 | CRC32Castagnoli32 |
            CRC32ISO8 | CRC32ISO16 | CRC32ISO32
                => OpcodeInfo { ret: U32, args: &[U32, U32] },
            CRC32Castagnoli64 | CRC32ISO64 => OpcodeInfo { ret: U32, args: &[U32, U64] },

            // AES
            AESDecryptSingleRound | AESEncryptSingleRound |
            AESInverseMixColumns | AESMixColumns
                => OpcodeInfo { ret: U128, args: &[U128] },

            // SHA
            SM4AccessSubstitutionBox => OpcodeInfo { ret: U8, args: &[U8] },
            SHA256Hash => OpcodeInfo { ret: U128, args: &[U128, U128, U128, U1] },
            SHA256MessageSchedule0 => OpcodeInfo { ret: U128, args: &[U128, U128] },
            SHA256MessageSchedule1 => OpcodeInfo { ret: U128, args: &[U128, U128, U128] },

            // Vector get/set element
            VectorGetElement8 => OpcodeInfo { ret: U8, args: &[U128, U8] },
            VectorGetElement16 => OpcodeInfo { ret: U16, args: &[U128, U8] },
            VectorGetElement32 => OpcodeInfo { ret: U32, args: &[U128, U8] },
            VectorGetElement64 => OpcodeInfo { ret: U64, args: &[U128, U8] },
            VectorSetElement8 => OpcodeInfo { ret: U128, args: &[U128, U8, U8] },
            VectorSetElement16 => OpcodeInfo { ret: U128, args: &[U128, U8, U16] },
            VectorSetElement32 => OpcodeInfo { ret: U128, args: &[U128, U8, U32] },
            VectorSetElement64 => OpcodeInfo { ret: U128, args: &[U128, U8, U64] },

            // Unary vector -> vector ops
            VectorAbs8 | VectorAbs16 | VectorAbs32 | VectorAbs64 |
            VectorNarrow16 | VectorNarrow32 | VectorNarrow64 |
            VectorNot |
            VectorCountLeadingZeros8 | VectorCountLeadingZeros16 | VectorCountLeadingZeros32 |
            VectorPopulationCount | VectorReverseBits |
            VectorReverseElementsInHalfGroups8 |
            VectorReverseElementsInWordGroups8 | VectorReverseElementsInWordGroups16 |
            VectorReverseElementsInLongGroups8 | VectorReverseElementsInLongGroups16 | VectorReverseElementsInLongGroups32 |
            VectorReduceAdd8 | VectorReduceAdd16 | VectorReduceAdd32 | VectorReduceAdd64 |
            VectorPairedAddSignedWiden8 | VectorPairedAddSignedWiden16 | VectorPairedAddSignedWiden32 |
            VectorPairedAddUnsignedWiden8 | VectorPairedAddUnsignedWiden16 | VectorPairedAddUnsignedWiden32 |
            VectorSignExtend8 | VectorSignExtend16 | VectorSignExtend32 | VectorSignExtend64 |
            VectorZeroExtend8 | VectorZeroExtend16 | VectorZeroExtend32 | VectorZeroExtend64 |
            VectorZeroUpper |
            VectorUnsignedRecipEstimate | VectorUnsignedRecipSqrtEstimate |
            VectorSignedSaturatedAbs8 | VectorSignedSaturatedAbs16 | VectorSignedSaturatedAbs32 | VectorSignedSaturatedAbs64 |
            VectorSignedSaturatedNeg8 | VectorSignedSaturatedNeg16 | VectorSignedSaturatedNeg32 | VectorSignedSaturatedNeg64 |
            VectorSignedSaturatedNarrowToSigned16 | VectorSignedSaturatedNarrowToSigned32 | VectorSignedSaturatedNarrowToSigned64 |
            VectorSignedSaturatedNarrowToUnsigned16 | VectorSignedSaturatedNarrowToUnsigned32 | VectorSignedSaturatedNarrowToUnsigned64 |
            VectorUnsignedSaturatedNarrow16 | VectorUnsignedSaturatedNarrow32 | VectorUnsignedSaturatedNarrow64
                => OpcodeInfo { ret: U128, args: &[U128] },

            ZeroVector => OpcodeInfo { ret: U128, args: &[] },

            VectorSignedMultiply16 | VectorSignedMultiply32 |
            VectorUnsignedMultiply16 | VectorUnsignedMultiply32
                => OpcodeInfo { ret: V, args: &[U128, U128] },

            // Binary vector -> vector ops
            VectorAdd8 | VectorAdd16 | VectorAdd32 | VectorAdd64 |
            VectorSub8 | VectorSub16 | VectorSub32 | VectorSub64 |
            VectorMultiply8 | VectorMultiply16 | VectorMultiply32 | VectorMultiply64 |
            VectorMultiplySignedWiden8 | VectorMultiplySignedWiden16 | VectorMultiplySignedWiden32 |
            VectorMultiplyUnsignedWiden8 | VectorMultiplyUnsignedWiden16 | VectorMultiplyUnsignedWiden32 |
            VectorAnd | VectorAndNot | VectorEor | VectorOr |
            VectorEqual8 | VectorEqual16 | VectorEqual32 | VectorEqual64 | VectorEqual128 |
            VectorGreaterEqualSigned8 | VectorGreaterEqualSigned16 | VectorGreaterEqualSigned32 | VectorGreaterEqualSigned64 |
            VectorGreaterEqualUnsigned8 | VectorGreaterEqualUnsigned16 | VectorGreaterEqualUnsigned32 | VectorGreaterEqualUnsigned64 |
            VectorGreaterS8 | VectorGreaterS16 | VectorGreaterS32 | VectorGreaterS64 |
            VectorLessEqualSigned8 | VectorLessEqualSigned16 | VectorLessEqualSigned32 | VectorLessEqualSigned64 |
            VectorLessSigned8 | VectorLessSigned16 | VectorLessSigned32 | VectorLessSigned64 |
            VectorHalvingAddS8 | VectorHalvingAddS16 | VectorHalvingAddS32 |
            VectorHalvingAddU8 | VectorHalvingAddU16 | VectorHalvingAddU32 |
            VectorHalvingSubS8 | VectorHalvingSubS16 | VectorHalvingSubS32 |
            VectorHalvingSubU8 | VectorHalvingSubU16 | VectorHalvingSubU32 |
            VectorMaxS8 | VectorMaxS16 | VectorMaxS32 | VectorMaxS64 |
            VectorMaxU8 | VectorMaxU16 | VectorMaxU32 | VectorMaxU64 |
            VectorMinS8 | VectorMinS16 | VectorMinS32 | VectorMinS64 |
            VectorMinU8 | VectorMinU16 | VectorMinU32 | VectorMinU64 |
            VectorPairedAdd8 | VectorPairedAdd16 | VectorPairedAdd32 | VectorPairedAdd64 |
            VectorPairedAddLower8 | VectorPairedAddLower16 | VectorPairedAddLower32 |
            VectorPairedMaxS8 | VectorPairedMaxS16 | VectorPairedMaxS32 |
            VectorPairedMaxU8 | VectorPairedMaxU16 | VectorPairedMaxU32 |
            VectorPairedMaxLowerS8 | VectorPairedMaxLowerS16 | VectorPairedMaxLowerS32 |
            VectorPairedMaxLowerU8 | VectorPairedMaxLowerU16 | VectorPairedMaxLowerU32 |
            VectorPairedMinS8 | VectorPairedMinS16 | VectorPairedMinS32 |
            VectorPairedMinU8 | VectorPairedMinU16 | VectorPairedMinU32 |
            VectorPairedMinLowerS8 | VectorPairedMinLowerS16 | VectorPairedMinLowerS32 |
            VectorPairedMinLowerU8 | VectorPairedMinLowerU16 | VectorPairedMinLowerU32 |
            VectorPolynomialMultiply8 | VectorPolynomialMultiplyLong8 | VectorPolynomialMultiplyLong64 |
            VectorRoundingHalvingAddS8 | VectorRoundingHalvingAddS16 | VectorRoundingHalvingAddS32 |
            VectorRoundingHalvingAddU8 | VectorRoundingHalvingAddU16 | VectorRoundingHalvingAddU32 |
            VectorRoundingShiftLeftS8 | VectorRoundingShiftLeftS16 | VectorRoundingShiftLeftS32 | VectorRoundingShiftLeftS64 |
            VectorRoundingShiftLeftU8 | VectorRoundingShiftLeftU16 | VectorRoundingShiftLeftU32 | VectorRoundingShiftLeftU64 |
            VectorSignedAbsoluteDifference8 | VectorSignedAbsoluteDifference16 | VectorSignedAbsoluteDifference32 |
            VectorUnsignedAbsoluteDifference8 | VectorUnsignedAbsoluteDifference16 | VectorUnsignedAbsoluteDifference32
                => OpcodeInfo { ret: U128, args: &[U128, U128] },

            // Vector shifts: the non-V (scalar) forms take an immediate U8
            // shift amount. Upstream `opcodes.inc` signature:
            // `OPCODE(VectorLogicalShiftLeft32, U128, U128, U8)`.
            VectorArithmeticShiftRight8 | VectorArithmeticShiftRight16 | VectorArithmeticShiftRight32 | VectorArithmeticShiftRight64 |
            VectorLogicalShiftLeft8 | VectorLogicalShiftLeft16 | VectorLogicalShiftLeft32 | VectorLogicalShiftLeft64 |
            VectorLogicalShiftRight8 | VectorLogicalShiftRight16 | VectorLogicalShiftRight32 | VectorLogicalShiftRight64 |
            VectorSignedSaturatedShiftLeftUnsigned8 | VectorSignedSaturatedShiftLeftUnsigned16 | VectorSignedSaturatedShiftLeftUnsigned32 | VectorSignedSaturatedShiftLeftUnsigned64
                => OpcodeInfo { ret: U128, args: &[U128, U8] },

            // Vector-by-vector ops (both operands are U128 lanes). Upstream's
            // VShift variants + the vector-by-vector saturating / rounding
            // shifts all carry `U128, U128` operands.
            VectorArithmeticVShift8 | VectorArithmeticVShift16 | VectorArithmeticVShift32 | VectorArithmeticVShift64 |
            VectorLogicalVShift8 | VectorLogicalVShift16 | VectorLogicalVShift32 | VectorLogicalVShift64 |
            VectorSignedSaturatedAccumulateUnsigned8 | VectorSignedSaturatedAccumulateUnsigned16 |
            VectorSignedSaturatedAccumulateUnsigned32 | VectorSignedSaturatedAccumulateUnsigned64 |
            VectorSignedSaturatedAdd8 | VectorSignedSaturatedAdd16 | VectorSignedSaturatedAdd32 | VectorSignedSaturatedAdd64 |
            VectorSignedSaturatedDoublingMultiplyHigh16 | VectorSignedSaturatedDoublingMultiplyHigh32 |
            VectorSignedSaturatedDoublingMultiplyHighRounding16 | VectorSignedSaturatedDoublingMultiplyHighRounding32 |
            VectorSignedSaturatedDoublingMultiplyLong16 | VectorSignedSaturatedDoublingMultiplyLong32 |
            VectorSignedSaturatedShiftLeft8 | VectorSignedSaturatedShiftLeft16 | VectorSignedSaturatedShiftLeft32 | VectorSignedSaturatedShiftLeft64 |
            VectorSignedSaturatedSub8 | VectorSignedSaturatedSub16 | VectorSignedSaturatedSub32 | VectorSignedSaturatedSub64 |
            VectorUnsignedSaturatedAccumulateSigned8 | VectorUnsignedSaturatedAccumulateSigned16 |
            VectorUnsignedSaturatedAccumulateSigned32 | VectorUnsignedSaturatedAccumulateSigned64 |
            VectorUnsignedSaturatedAdd8 | VectorUnsignedSaturatedAdd16 | VectorUnsignedSaturatedAdd32 | VectorUnsignedSaturatedAdd64 |
            VectorUnsignedSaturatedShiftLeft8 | VectorUnsignedSaturatedShiftLeft16 | VectorUnsignedSaturatedShiftLeft32 | VectorUnsignedSaturatedShiftLeft64 |
            VectorUnsignedSaturatedSub8 | VectorUnsignedSaturatedSub16 | VectorUnsignedSaturatedSub32 | VectorUnsignedSaturatedSub64 |
            VectorInterleaveLower8 | VectorInterleaveLower16 | VectorInterleaveLower32 | VectorInterleaveLower64 |
            VectorInterleaveUpper8 | VectorInterleaveUpper16 | VectorInterleaveUpper32 | VectorInterleaveUpper64 |
            VectorDeinterleaveEven8 | VectorDeinterleaveEven16 | VectorDeinterleaveEven32 | VectorDeinterleaveEven64 |
            VectorDeinterleaveEvenLower8 | VectorDeinterleaveEvenLower16 | VectorDeinterleaveEvenLower32 |
            VectorDeinterleaveOdd8 | VectorDeinterleaveOdd16 | VectorDeinterleaveOdd32 | VectorDeinterleaveOdd64 |
            VectorDeinterleaveOddLower8 | VectorDeinterleaveOddLower16 | VectorDeinterleaveOddLower32
                => OpcodeInfo { ret: U128, args: &[U128, U128] },
            // VectorTranspose: upstream takes (U128, U128, U1) where U1 selects even/odd part
            VectorTranspose8 | VectorTranspose16 | VectorTranspose32 | VectorTranspose64
                => OpcodeInfo { ret: U128, args: &[U128, U128, U1] },

            // Vector broadcast: each takes the scalar type matching its element size.
            // Upstream: VectorBroadcast8 takes U8, VectorBroadcast16 takes U16, etc.
            VectorBroadcastLower8 | VectorBroadcast8
                => OpcodeInfo { ret: U128, args: &[U8] },
            VectorBroadcastLower16 | VectorBroadcast16
                => OpcodeInfo { ret: U128, args: &[U16] },
            VectorBroadcastLower32 | VectorBroadcast32
                => OpcodeInfo { ret: U128, args: &[U32] },
            VectorBroadcast64
                => OpcodeInfo { ret: U128, args: &[U64] },
            VectorBroadcastElementLower8 | VectorBroadcastElementLower16 |
            VectorBroadcastElementLower32 | VectorBroadcastElement8 |
            VectorBroadcastElement16 | VectorBroadcastElement32 |
            VectorBroadcastElement64
                => OpcodeInfo { ret: U128, args: &[U128, U8] },

            // Vector extract
            VectorExtract => OpcodeInfo { ret: U128, args: &[U128, U128, U8] },
            VectorExtractLower => OpcodeInfo { ret: U128, args: &[U128, U128, U8] },
            VectorRotateWholeVectorRight => OpcodeInfo { ret: U128, args: &[U128, U8] },

            // Vector shuffle (imm8 control)
            VectorShuffleHighHalfwords | VectorShuffleLowHalfwords | VectorShuffleWords
                => OpcodeInfo { ret: U128, args: &[U128, U8] },

            // Vector table lookup
            VectorTable => OpcodeInfo { ret: Type::Table, args: &[OPQ, OPQ, OPQ, OPQ] },
            VectorTableLookup64 => OpcodeInfo { ret: U64, args: &[U64, Type::Table, U64] },
            VectorTableLookup128 => OpcodeInfo { ret: U128, args: &[U128, Type::Table, U128] },

            // FP scalar - unary
            FPAbs16 => OpcodeInfo { ret: U16, args: &[U16] },
            FPAbs32 => OpcodeInfo { ret: U32, args: &[U32] },
            FPAbs64 => OpcodeInfo { ret: U64, args: &[U64] },
            FPNeg16 => OpcodeInfo { ret: U16, args: &[U16] },
            FPNeg32 => OpcodeInfo { ret: U32, args: &[U32] },
            FPNeg64 => OpcodeInfo { ret: U64, args: &[U64] },
            FPSqrt32 => OpcodeInfo { ret: U32, args: &[U32] },
            FPSqrt64 => OpcodeInfo { ret: U64, args: &[U64] },
            FPRecipEstimate16 => OpcodeInfo { ret: U16, args: &[U16] },
            FPRecipEstimate32 => OpcodeInfo { ret: U32, args: &[U32] },
            FPRecipEstimate64 => OpcodeInfo { ret: U64, args: &[U64] },
            FPRecipExponent16 => OpcodeInfo { ret: U16, args: &[U16] },
            FPRecipExponent32 => OpcodeInfo { ret: U32, args: &[U32] },
            FPRecipExponent64 => OpcodeInfo { ret: U64, args: &[U64] },
            FPRSqrtEstimate16 => OpcodeInfo { ret: U16, args: &[U16] },
            FPRSqrtEstimate32 => OpcodeInfo { ret: U32, args: &[U32] },
            FPRSqrtEstimate64 => OpcodeInfo { ret: U64, args: &[U64] },

            // FP scalar - binary
            FPAdd32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            FPAdd64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            FPDiv32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            FPDiv64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            FPMax32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            FPMax64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            FPMaxNumeric32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            FPMaxNumeric64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            FPMin32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            FPMin64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            FPMinNumeric32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            FPMinNumeric64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            FPMul32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            FPMul64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            FPMulX32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            FPMulX64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            FPSub32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            FPSub64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            FPRecipStepFused16 => OpcodeInfo { ret: U16, args: &[U16, U16] },
            FPRecipStepFused32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            FPRecipStepFused64 => OpcodeInfo { ret: U64, args: &[U64, U64] },
            FPRSqrtStepFused16 => OpcodeInfo { ret: U16, args: &[U16, U16] },
            FPRSqrtStepFused32 => OpcodeInfo { ret: U32, args: &[U32, U32] },
            FPRSqrtStepFused64 => OpcodeInfo { ret: U64, args: &[U64, U64] },

            // FP compare (returns NZCV)
            FPCompare32 => OpcodeInfo { ret: NZCV, args: &[U32, U32, U1] },
            FPCompare64 => OpcodeInfo { ret: NZCV, args: &[U64, U64, U1] },

            // FP ternary (fused multiply-add/sub)
            FPMulAdd16 => OpcodeInfo { ret: U16, args: &[U16, U16, U16] },
            FPMulAdd32 => OpcodeInfo { ret: U32, args: &[U32, U32, U32] },
            FPMulAdd64 => OpcodeInfo { ret: U64, args: &[U64, U64, U64] },
            FPMulSub16 => OpcodeInfo { ret: U16, args: &[U16, U16, U16] },
            FPMulSub32 => OpcodeInfo { ret: U32, args: &[U32, U32, U32] },
            FPMulSub64 => OpcodeInfo { ret: U64, args: &[U64, U64, U64] },

            // FP rounding
            FPRoundInt16 => OpcodeInfo { ret: U16, args: &[U16, U8, U1] },
            FPRoundInt32 => OpcodeInfo { ret: U32, args: &[U32, U8, U1] },
            FPRoundInt64 => OpcodeInfo { ret: U64, args: &[U64, U8, U1] },

            // FP conversions (2 args: value + rounding)
            FPHalfToDouble => OpcodeInfo { ret: U64, args: &[U16, U8] },
            FPHalfToSingle => OpcodeInfo { ret: U32, args: &[U16, U8] },
            FPSingleToDouble => OpcodeInfo { ret: U64, args: &[U32, U8] },
            FPSingleToHalf => OpcodeInfo { ret: U16, args: &[U32, U8] },
            FPDoubleToHalf => OpcodeInfo { ret: U16, args: &[U64, U8] },
            FPDoubleToSingle => OpcodeInfo { ret: U32, args: &[U64, U8] },

            // FP to fixed-point (3 args: value, fbits, rounding)
            FPDoubleToFixedS16 | FPDoubleToFixedU16 => OpcodeInfo { ret: U16, args: &[U64, U8, U8] },
            FPDoubleToFixedS32 | FPDoubleToFixedU32 => OpcodeInfo { ret: U32, args: &[U64, U8, U8] },
            FPDoubleToFixedS64 | FPDoubleToFixedU64 => OpcodeInfo { ret: U64, args: &[U64, U8, U8] },
            FPHalfToFixedS16 | FPHalfToFixedU16 => OpcodeInfo { ret: U16, args: &[U16, U8, U8] },
            FPHalfToFixedS32 | FPHalfToFixedU32 => OpcodeInfo { ret: U32, args: &[U16, U8, U8] },
            FPHalfToFixedS64 | FPHalfToFixedU64 => OpcodeInfo { ret: U64, args: &[U16, U8, U8] },
            FPSingleToFixedS16 | FPSingleToFixedU16 => OpcodeInfo { ret: U16, args: &[U32, U8, U8] },
            FPSingleToFixedS32 | FPSingleToFixedU32 => OpcodeInfo { ret: U32, args: &[U32, U8, U8] },
            FPSingleToFixedS64 | FPSingleToFixedU64 => OpcodeInfo { ret: U64, args: &[U32, U8, U8] },

            // Fixed-point to FP
            FPFixedU16ToSingle | FPFixedS16ToSingle => OpcodeInfo { ret: U32, args: &[U16, U8, U8] },
            FPFixedU16ToDouble | FPFixedS16ToDouble => OpcodeInfo { ret: U64, args: &[U16, U8, U8] },
            FPFixedU32ToSingle | FPFixedS32ToSingle => OpcodeInfo { ret: U32, args: &[U32, U8, U8] },
            FPFixedU32ToDouble | FPFixedS32ToDouble => OpcodeInfo { ret: U64, args: &[U32, U8, U8] },
            FPFixedU64ToDouble | FPFixedS64ToDouble => OpcodeInfo { ret: U64, args: &[U64, U8, U8] },
            FPFixedU64ToSingle | FPFixedS64ToSingle => OpcodeInfo { ret: U32, args: &[U64, U8, U8] },

            // FP vector unary
            FPVectorAbs16 | FPVectorAbs32 | FPVectorAbs64 |
            FPVectorNeg16 | FPVectorNeg32 | FPVectorNeg64
                => OpcodeInfo { ret: U128, args: &[U128] },

            // FP vector binary (with fpcr_controlled flag)
            FPVectorAdd32 | FPVectorAdd64 |
            FPVectorDiv32 | FPVectorDiv64 |
            FPVectorEqual16 | FPVectorEqual32 | FPVectorEqual64 |
            FPVectorGreater32 | FPVectorGreater64 |
            FPVectorGreaterEqual32 | FPVectorGreaterEqual64 |
            FPVectorMax32 | FPVectorMax64 |
            FPVectorMaxNumeric32 | FPVectorMaxNumeric64 |
            FPVectorMin32 | FPVectorMin64 |
            FPVectorMinNumeric32 | FPVectorMinNumeric64 |
            FPVectorMul32 | FPVectorMul64 |
            FPVectorMulX32 | FPVectorMulX64 |
            FPVectorPairedAdd32 | FPVectorPairedAdd64 |
            FPVectorPairedAddLower32 | FPVectorPairedAddLower64 |
            FPVectorRecipStepFused16 | FPVectorRecipStepFused32 | FPVectorRecipStepFused64 |
            FPVectorRSqrtStepFused16 | FPVectorRSqrtStepFused32 | FPVectorRSqrtStepFused64 |
            FPVectorSub32 | FPVectorSub64
                => OpcodeInfo { ret: U128, args: &[U128, U128, U1] },

            FPVectorSqrt32 | FPVectorSqrt64 |
            FPVectorRecipEstimate16 | FPVectorRecipEstimate32 | FPVectorRecipEstimate64 |
            FPVectorRSqrtEstimate16 | FPVectorRSqrtEstimate32 | FPVectorRSqrtEstimate64
                => OpcodeInfo { ret: U128, args: &[U128, U1] },

            // FP vector conversion
            FPVectorFromHalf32 | FPVectorToHalf32
                => OpcodeInfo { ret: U128, args: &[U128, U8, U1] },

            FPVectorFromSignedFixed32 | FPVectorFromSignedFixed64 |
            FPVectorFromUnsignedFixed32 | FPVectorFromUnsignedFixed64 |
            FPVectorToSignedFixed16 | FPVectorToSignedFixed32 | FPVectorToSignedFixed64 |
            FPVectorToUnsignedFixed16 | FPVectorToUnsignedFixed32 | FPVectorToUnsignedFixed64
                => OpcodeInfo { ret: U128, args: &[U128, U8, U8, U1] },

            FPVectorRoundInt16 | FPVectorRoundInt32 | FPVectorRoundInt64
                => OpcodeInfo { ret: U128, args: &[U128, U8, U1, U1] },

            // FP vector fused multiply-add
            FPVectorMulAdd16 | FPVectorMulAdd32 | FPVectorMulAdd64
                => OpcodeInfo { ret: U128, args: &[U128, U128, U128, U1] },

            // A64 Memory
            A64ClearExclusive => OpcodeInfo { ret: V, args: &[] },
            A64ReadMemory8 => OpcodeInfo { ret: U8, args: &[U64, U64, ACC] },
            A64ReadMemory16 => OpcodeInfo { ret: U16, args: &[U64, U64, ACC] },
            A64ReadMemory32 => OpcodeInfo { ret: U32, args: &[U64, U64, ACC] },
            A64ReadMemory64 => OpcodeInfo { ret: U64, args: &[U64, U64, ACC] },
            A64ReadMemory128 => OpcodeInfo { ret: U128, args: &[U64, U64, ACC] },
            A64ExclusiveReadMemory8 => OpcodeInfo { ret: U8, args: &[U64, U64, ACC] },
            A64ExclusiveReadMemory16 => OpcodeInfo { ret: U16, args: &[U64, U64, ACC] },
            A64ExclusiveReadMemory32 => OpcodeInfo { ret: U32, args: &[U64, U64, ACC] },
            A64ExclusiveReadMemory64 => OpcodeInfo { ret: U64, args: &[U64, U64, ACC] },
            A64ExclusiveReadMemory128 => OpcodeInfo { ret: U128, args: &[U64, U64, ACC] },
            A64WriteMemory8 => OpcodeInfo { ret: V, args: &[U64, U64, U8, ACC] },
            A64WriteMemory16 => OpcodeInfo { ret: V, args: &[U64, U64, U16, ACC] },
            A64WriteMemory32 => OpcodeInfo { ret: V, args: &[U64, U64, U32, ACC] },
            A64WriteMemory64 => OpcodeInfo { ret: V, args: &[U64, U64, U64, ACC] },
            A64WriteMemory128 => OpcodeInfo { ret: V, args: &[U64, U64, U128, ACC] },
            A64ExclusiveWriteMemory8 => OpcodeInfo { ret: U32, args: &[U64, U64, U8, ACC] },
            A64ExclusiveWriteMemory16 => OpcodeInfo { ret: U32, args: &[U64, U64, U16, ACC] },
            A64ExclusiveWriteMemory32 => OpcodeInfo { ret: U32, args: &[U64, U64, U32, ACC] },
            A64ExclusiveWriteMemory64 => OpcodeInfo { ret: U32, args: &[U64, U64, U64, ACC] },
            A64ExclusiveWriteMemory128 => OpcodeInfo { ret: U32, args: &[U64, U64, U128, ACC] },

            // A32 context
            A32SetCheckBit => OpcodeInfo { ret: V, args: &[U1] },
            A32GetCFlag => OpcodeInfo { ret: U1, args: &[] },
            A32GetRegister => OpcodeInfo { ret: U32, args: &[A32R] },
            A32SetRegister => OpcodeInfo { ret: V, args: &[A32R, U32] },
            A32GetExtendedRegister32 => OpcodeInfo { ret: U32, args: &[A32E] },
            A32GetExtendedRegister64 => OpcodeInfo { ret: U64, args: &[A32E] },
            A32SetExtendedRegister32 => OpcodeInfo { ret: V, args: &[A32E, U32] },
            A32SetExtendedRegister64 => OpcodeInfo { ret: V, args: &[A32E, U64] },
            A32GetVector => OpcodeInfo { ret: U128, args: &[A32E] },
            A32SetVector => OpcodeInfo { ret: V, args: &[A32E, U128] },
            A32GetCpsr => OpcodeInfo { ret: U32, args: &[] },
            A32SetCpsr => OpcodeInfo { ret: V, args: &[U32] },
            A32SetCpsrNZCVRaw => OpcodeInfo { ret: V, args: &[U32] },
            A32SetCpsrNZCV => OpcodeInfo { ret: V, args: &[NZCV] },
            A32SetCpsrNZCVQ => OpcodeInfo { ret: V, args: &[U32] },
            A32SetCpsrNZ => OpcodeInfo { ret: V, args: &[NZCV] },
            A32SetCpsrNZC => OpcodeInfo { ret: V, args: &[NZCV, U1] },
            A32OrQFlag => OpcodeInfo { ret: V, args: &[U1] },
            A32GetGEFlags => OpcodeInfo { ret: U32, args: &[] },
            A32SetGEFlags => OpcodeInfo { ret: V, args: &[U32] },
            A32SetGEFlagsCompressed => OpcodeInfo { ret: V, args: &[U32] },
            A32BXWritePC => OpcodeInfo { ret: V, args: &[U32] },
            A32UpdateUpperLocationDescriptor => OpcodeInfo { ret: V, args: &[] },
            A32CallSupervisor => OpcodeInfo { ret: V, args: &[U32] },
            A32PcExecHook => OpcodeInfo { ret: V, args: &[U32, U32, U32, U32, U32] },
            A32ExceptionRaised => OpcodeInfo { ret: V, args: &[U32, U64] },
            A32DataSynchronizationBarrier => OpcodeInfo { ret: V, args: &[] },
            A32DataMemoryBarrier => OpcodeInfo { ret: V, args: &[] },
            A32InstructionSynchronizationBarrier => OpcodeInfo { ret: V, args: &[] },
            A32GetFpscr => OpcodeInfo { ret: U32, args: &[] },
            A32SetFpscr => OpcodeInfo { ret: V, args: &[U32] },
            A32GetFpscrNZCV => OpcodeInfo { ret: U32, args: &[] },
            A32SetFpscrNZCV => OpcodeInfo { ret: V, args: &[NZCV] },

            // A32 Memory (location_descriptor passed as first arg)
            A32ClearExclusive => OpcodeInfo { ret: V, args: &[] },
            A32ReadMemory8 => OpcodeInfo { ret: U8, args: &[U64, U32, ACC] },
            A32ReadMemory16 => OpcodeInfo { ret: U16, args: &[U64, U32, ACC] },
            A32ReadMemory32 => OpcodeInfo { ret: U32, args: &[U64, U32, ACC] },
            A32ReadMemory64 => OpcodeInfo { ret: U64, args: &[U64, U32, ACC] },
            A32ExclusiveReadMemory8 => OpcodeInfo { ret: U8, args: &[U64, U32, ACC] },
            A32ExclusiveReadMemory16 => OpcodeInfo { ret: U16, args: &[U64, U32, ACC] },
            A32ExclusiveReadMemory32 => OpcodeInfo { ret: U32, args: &[U64, U32, ACC] },
            A32ExclusiveReadMemory64 => OpcodeInfo { ret: U64, args: &[U64, U32, ACC] },
            A32WriteMemory8 => OpcodeInfo { ret: V, args: &[U64, U32, U8, ACC] },
            A32WriteMemory16 => OpcodeInfo { ret: V, args: &[U64, U32, U16, ACC] },
            A32WriteMemory32 => OpcodeInfo { ret: V, args: &[U64, U32, U32, ACC] },
            A32WriteMemory64 => OpcodeInfo { ret: V, args: &[U64, U32, U64, ACC] },
            A32ExclusiveWriteMemory8 => OpcodeInfo { ret: U32, args: &[U64, U32, U8, ACC] },
            A32ExclusiveWriteMemory16 => OpcodeInfo { ret: U32, args: &[U64, U32, U16, ACC] },
            A32ExclusiveWriteMemory32 => OpcodeInfo { ret: U32, args: &[U64, U32, U32, ACC] },
            A32ExclusiveWriteMemory64 => OpcodeInfo { ret: U32, args: &[U64, U32, U64, ACC] },

            // A32 Coprocessor
            A32CoprocInternalOperation => OpcodeInfo { ret: V, args: &[COPROC] },
            A32CoprocSendOneWord => OpcodeInfo { ret: V, args: &[COPROC, U32] },
            A32CoprocSendTwoWords => OpcodeInfo { ret: V, args: &[COPROC, U32, U32] },
            A32CoprocGetOneWord => OpcodeInfo { ret: U32, args: &[COPROC] },
            A32CoprocGetTwoWords => OpcodeInfo { ret: U64, args: &[COPROC] },
            A32CoprocLoadWords => OpcodeInfo { ret: V, args: &[COPROC, U32, U1] },
            A32CoprocStoreWords => OpcodeInfo { ret: V, args: &[COPROC, U32, U1] },
        }
    }
}

impl fmt::Display for Opcode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_opcode_metadata() {
        assert_eq!(Opcode::Void.return_type(), Type::Void);
        assert_eq!(Opcode::Void.num_args(), 0);

        assert_eq!(Opcode::Add32.return_type(), Type::U32);
        assert_eq!(Opcode::Add32.num_args(), 3);
        assert_eq!(Opcode::Add32.arg_types(), &[Type::U32, Type::U32, Type::U1]);

        assert_eq!(Opcode::A64GetX.return_type(), Type::U64);
        assert_eq!(Opcode::A64GetX.num_args(), 1);
        assert_eq!(Opcode::A64GetX.arg_types(), &[Type::A64Reg]);

        assert_eq!(Opcode::A64SetX.return_type(), Type::Void);
        assert_eq!(Opcode::A64SetX.num_args(), 2);

        assert_eq!(Opcode::A64ReadMemory64.return_type(), Type::U64);
        assert_eq!(Opcode::A64ReadMemory64.num_args(), 3);

        for opcode in [
            Opcode::VectorSignedMultiply16,
            Opcode::VectorSignedMultiply32,
            Opcode::VectorUnsignedMultiply16,
            Opcode::VectorUnsignedMultiply32,
        ] {
            assert_eq!(opcode.return_type(), Type::Void);
            assert_eq!(opcode.arg_types(), &[Type::U128, Type::U128]);
        }

        for opcode in [
            Opcode::CRC32Castagnoli8,
            Opcode::CRC32Castagnoli16,
            Opcode::CRC32Castagnoli32,
            Opcode::CRC32ISO8,
            Opcode::CRC32ISO16,
            Opcode::CRC32ISO32,
        ] {
            assert_eq!(opcode.return_type(), Type::U32);
            assert_eq!(opcode.arg_types(), &[Type::U32, Type::U32]);
        }
        for opcode in [Opcode::CRC32Castagnoli64, Opcode::CRC32ISO64] {
            assert_eq!(opcode.return_type(), Type::U32);
            assert_eq!(opcode.arg_types(), &[Type::U32, Type::U64]);
        }
    }

    #[test]
    fn test_vector_operand_metadata_matches_upstream_groups() {
        for opcode in [
            Opcode::VectorAdd8,
            Opcode::VectorAnd,
            Opcode::VectorEqual128,
            Opcode::VectorGreaterS64,
            Opcode::VectorHalvingSubU32,
            Opcode::VectorMaxS64,
            Opcode::VectorMinU64,
            Opcode::VectorMultiply64,
            Opcode::VectorMultiplySignedWiden32,
            Opcode::VectorMultiplyUnsignedWiden32,
            Opcode::VectorPairedAdd64,
            Opcode::VectorPairedAddLower32,
            Opcode::VectorPairedMaxLowerU32,
            Opcode::VectorPairedMinLowerS32,
            Opcode::VectorPolynomialMultiplyLong64,
            Opcode::VectorRoundingHalvingAddU32,
            Opcode::VectorRoundingShiftLeftS64,
            Opcode::VectorSignedAbsoluteDifference32,
            Opcode::VectorSub64,
            Opcode::VectorUnsignedAbsoluteDifference32,
        ] {
            assert_eq!(opcode.return_type(), Type::U128);
            assert_eq!(opcode.arg_types(), &[Type::U128, Type::U128]);
        }

        for opcode in [
            Opcode::VectorPairedAddSignedWiden8,
            Opcode::VectorPairedAddSignedWiden16,
            Opcode::VectorPairedAddSignedWiden32,
            Opcode::VectorPairedAddUnsignedWiden8,
            Opcode::VectorPairedAddUnsignedWiden16,
            Opcode::VectorPairedAddUnsignedWiden32,
        ] {
            assert_eq!(opcode.return_type(), Type::U128);
            assert_eq!(opcode.arg_types(), &[Type::U128]);
        }

        for opcode in [
            Opcode::VectorArithmeticShiftRight8,
            Opcode::VectorLogicalShiftLeft32,
            Opcode::VectorLogicalShiftRight64,
            Opcode::VectorSignedSaturatedShiftLeftUnsigned16,
        ] {
            assert_eq!(opcode.return_type(), Type::U128);
            assert_eq!(opcode.arg_types(), &[Type::U128, Type::U8]);
        }
    }

    #[test]
    fn test_opcode_side_effects() {
        assert!(Opcode::A64SetX.has_side_effects());
        assert!(Opcode::A64WriteMemory64.has_side_effects());
        assert!(Opcode::A64ExclusiveReadMemory32.has_side_effects());
        assert!(Opcode::A32ExclusiveReadMemory32.has_side_effects());
        assert!(Opcode::PushRSB.has_side_effects());
        assert!(!Opcode::Add32.has_side_effects());
        assert!(!Opcode::A64GetX.has_side_effects());
    }

    #[test]
    fn test_opcode_core_register_classification_matches_upstream() {
        assert!(Opcode::A64GetS.reads_from_core_register());
        assert!(Opcode::A64GetD.reads_from_core_register());
        assert!(Opcode::A64GetQ.reads_from_core_register());
        assert!(Opcode::A64SetS.writes_to_core_register());
        assert!(Opcode::A64SetD.writes_to_core_register());
        assert!(Opcode::A64SetQ.writes_to_core_register());
        assert!(Opcode::A32GetGEFlags.reads_cpsr());
    }

    #[test]
    fn test_opcode_memory_classification() {
        assert!(Opcode::A64ReadMemory32.is_memory_read());
        assert!(!Opcode::A64ReadMemory32.is_memory_write());
        assert!(Opcode::A64WriteMemory64.is_memory_write());
        assert!(!Opcode::A64WriteMemory64.is_memory_read());
    }
}

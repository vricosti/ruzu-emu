// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! IR opcode definitions for the shader recompiler.
//!
//! Matches zuyu's `opcodes.inc`, covering the operations translated by the
//! Maxwell shader frontend.

use super::types::Type;
use std::fmt;

/// IR opcode enum. Each variant corresponds to a single IR instruction type.
///
/// Naming follows zuyu: `FPAdd32` = 32-bit float add, `IAdd32` = 32-bit int add.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(non_camel_case_types)]
pub enum Opcode {
    // ── Control / Meta ────────────────────────────────────────────────
    /// Phi node (SSA merge).
    Phi,
    /// Identity passthrough (will be removed by optimization).
    Identity,
    /// No-op void instruction.
    Void,
    /// Condition reference.
    ConditionRef,
    /// Keep a value alive (reference).
    Reference,
    /// Phi move (parallel copy for phi resolution).
    PhiMove,

    // ── Special ───────────────────────────────────────────────────────
    /// Function prologue marker.
    Prologue,
    /// Function epilogue marker.
    Epilogue,
    /// Convergence join point.
    Join,
    /// Demote fragment to helper invocation (discard).
    DemoteToHelperInvocation,
    /// Emit vertex (geometry shader).
    EmitVertex,
    /// End primitive (geometry shader).
    EndPrimitive,

    // ── Barriers ──────────────────────────────────────────────────────
    Barrier,
    WorkgroupMemoryBarrier,
    DeviceMemoryBarrier,

    // ── Context getters/setters ───────────────────────────────────────
    GetRegister,
    SetRegister,
    GetPred,
    SetPred,
    GetGotoVariable,
    SetGotoVariable,
    GetIndirectBranchVariable,
    SetIndirectBranchVariable,

    // ── Constant buffer loads ─────────────────────────────────────────
    GetCbufU8,
    GetCbufS8,
    GetCbufU16,
    GetCbufS16,
    GetCbufU32,
    GetCbufF32,
    GetCbufU32x2,

    // ── Attribute I/O ─────────────────────────────────────────────────
    GetAttribute,
    GetAttributeU32,
    SetAttribute,
    GetAttributeIndexed,
    SetAttributeIndexed,
    GetPatch,
    SetPatch,
    SetFragColor,
    SetSampleMask,
    SetFragDepth,

    // ── Condition flags ───────────────────────────────────────────────
    GetZFlag,
    GetSFlag,
    GetCFlag,
    GetOFlag,
    SetZFlag,
    SetSFlag,
    SetCFlag,
    SetOFlag,

    // ── System values ─────────────────────────────────────────────────
    WorkgroupId,
    LocalInvocationId,
    InvocationId,
    InvocationInfo,
    SampleId,
    IsHelperInvocation,
    SRWScaleFactorXY,
    SRWScaleFactorZ,
    YDirection,
    ResolutionDownFactor,
    IsTextureScaled,
    IsImageScaled,
    RenderArea,

    // ── Undefined values ──────────────────────────────────────────────
    UndefU1,
    UndefU8,
    UndefU16,
    UndefU32,
    UndefU64,

    // ── Global memory ─────────────────────────────────────────────────
    LoadGlobalU8,
    LoadGlobalS8,
    LoadGlobalU16,
    LoadGlobalS16,
    LoadGlobal32,
    LoadGlobal64,
    LoadGlobal128,
    WriteGlobalU8,
    WriteGlobalS8,
    WriteGlobalU16,
    WriteGlobalS16,
    WriteGlobal32,
    WriteGlobal64,
    WriteGlobal128,

    // ── Storage buffer ────────────────────────────────────────────────
    LoadStorageU8,
    LoadStorageS8,
    LoadStorageU16,
    LoadStorageS16,
    LoadStorage32,
    LoadStorage64,
    LoadStorage128,
    WriteStorageU8,
    WriteStorageS8,
    WriteStorageU16,
    WriteStorageS16,
    WriteStorage32,
    WriteStorage64,
    WriteStorage128,

    // ── Local memory ──────────────────────────────────────────────────
    LoadLocal,
    WriteLocal,

    // ── Shared memory ─────────────────────────────────────────────────
    LoadSharedU8,
    LoadSharedS8,
    LoadSharedU16,
    LoadSharedS16,
    LoadSharedU32,
    LoadSharedU64,
    LoadSharedU128,
    WriteSharedU8,
    WriteSharedU16,
    WriteSharedU32,
    WriteSharedU64,
    WriteSharedU128,

    // ── Vector composite ──────────────────────────────────────────────
    CompositeConstructU32x2,
    CompositeConstructU32x3,
    CompositeConstructU32x4,
    CompositeExtractU32x2,
    CompositeExtractU32x3,
    CompositeExtractU32x4,
    CompositeInsertU32x2,
    CompositeInsertU32x3,
    CompositeInsertU32x4,
    CompositeConstructF16x2,
    CompositeConstructF16x3,
    CompositeConstructF16x4,
    CompositeExtractF16x2,
    CompositeExtractF16x3,
    CompositeExtractF16x4,
    CompositeInsertF16x2,
    CompositeInsertF16x3,
    CompositeInsertF16x4,
    CompositeConstructF32x2,
    CompositeConstructF32x3,
    CompositeConstructF32x4,
    CompositeExtractF32x2,
    CompositeExtractF32x3,
    CompositeExtractF32x4,
    CompositeInsertF32x2,
    CompositeInsertF32x3,
    CompositeInsertF32x4,
    CompositeConstructF64x2,
    CompositeConstructF64x3,
    CompositeConstructF64x4,
    CompositeExtractF64x2,
    CompositeExtractF64x3,
    CompositeExtractF64x4,
    CompositeInsertF64x2,
    CompositeInsertF64x3,
    CompositeInsertF64x4,

    // ── Select ────────────────────────────────────────────────────────
    SelectU1,
    SelectU8,
    SelectU16,
    SelectU32,
    SelectU64,
    SelectF16,
    SelectF32,
    SelectF64,

    // ── Bitcast / conversion ──────────────────────────────────────────
    BitCastU16F16,
    BitCastU32F32,
    BitCastU64F64,
    BitCastF16U16,
    BitCastF32U32,
    BitCastF64U64,
    PackUint2x32,
    UnpackUint2x32,
    PackFloat2x16,
    UnpackFloat2x16,
    PackHalf2x16,
    UnpackHalf2x16,
    PackDouble2x32,
    UnpackDouble2x32,

    // ── Pseudo-operations (associated with parent instruction) ────────
    GetZeroFromOp,
    GetSignFromOp,
    GetCarryFromOp,
    GetOverflowFromOp,
    GetSparseFromOp,
    GetInBoundsFromOp,

    // ── FP16 arithmetic ───────────────────────────────────────────────
    FPAbs16,
    FPNeg16,
    FPAdd16,
    FPMul16,
    FPFma16,
    FPMin16,
    FPMax16,
    FPSaturate16,
    FPClamp16,
    FPRoundEven16,
    FPFloor16,
    FPCeil16,
    FPTrunc16,

    // ── FP32 arithmetic ───────────────────────────────────────────────
    FPAbs32,
    FPNeg32,
    FPAdd32,
    FPSub32,
    FPMul32,
    FPDiv32,
    FPFma32,
    FPMin32,
    FPMax32,
    FPSaturate32,
    FPClamp32,
    FPRoundEven32,
    FPFloor32,
    FPCeil32,
    FPTrunc32,
    FPRecip32,
    FPRecipSqrt32,
    FPSqrt32,
    FPSin,
    FPCos,
    FPExp2,
    FPLog2,

    // ── FP64 arithmetic ───────────────────────────────────────────────
    FPAbs64,
    FPNeg64,
    FPAdd64,
    FPSub64,
    FPMul64,
    FPDiv64,
    FPFma64,
    FPMin64,
    FPMax64,
    FPSaturate64,
    FPClamp64,
    FPRoundEven64,
    FPFloor64,
    FPCeil64,
    FPTrunc64,
    FPRecip64,
    FPRecipSqrt64,
    FPSqrt64,

    // ── FP comparison (ordered) ───────────────────────────────────────
    FPOrdEqual16,
    FPOrdNotEqual16,
    FPOrdLessThan16,
    FPOrdGreaterThan16,
    FPOrdLessThanEqual16,
    FPOrdGreaterThanEqual16,
    FPOrdEqual32,
    FPOrdNotEqual32,
    FPOrdLessThan32,
    FPOrdGreaterThan32,
    FPOrdLessThanEqual32,
    FPOrdGreaterThanEqual32,
    FPOrdEqual64,
    FPOrdNotEqual64,
    FPOrdLessThan64,
    FPOrdGreaterThan64,
    FPOrdLessThanEqual64,
    FPOrdGreaterThanEqual64,

    // ── FP comparison (unordered) ─────────────────────────────────────
    FPUnordEqual16,
    FPUnordNotEqual16,
    FPUnordLessThan16,
    FPUnordGreaterThan16,
    FPUnordLessThanEqual16,
    FPUnordGreaterThanEqual16,
    FPUnordEqual32,
    FPUnordNotEqual32,
    FPUnordLessThan32,
    FPUnordGreaterThan32,
    FPUnordLessThanEqual32,
    FPUnordGreaterThanEqual32,
    FPUnordEqual64,
    FPUnordNotEqual64,
    FPUnordLessThan64,
    FPUnordGreaterThan64,
    FPUnordLessThanEqual64,
    FPUnordGreaterThanEqual64,

    // ── FP classification ─────────────────────────────────────────────
    FPIsNan16,
    FPIsNan32,
    FPIsNan64,

    // ── Integer arithmetic ────────────────────────────────────────────
    IAdd32,
    IAdd64,
    ISub32,
    ISub64,
    IMul32,
    SDiv32,
    UDiv32,
    INeg32,
    INeg64,
    IAbs32,
    IAbs64,
    ShiftLeftLogical32,
    ShiftLeftLogical64,
    ShiftRightLogical32,
    ShiftRightLogical64,
    ShiftRightArithmetic32,
    ShiftRightArithmetic64,
    BitwiseAnd32,
    BitwiseOr32,
    BitwiseXor32,
    BitwiseNot32,
    BitFieldInsert,
    BitFieldSExtract,
    BitFieldUExtract,
    BitReverse32,
    BitCount32,
    FindSMsb32,
    FindUMsb32,

    // ── Integer min/max ───────────────────────────────────────────────
    SMin32,
    UMin32,
    SMax32,
    UMax32,
    SClamp32,
    UClamp32,

    // ── Integer comparison ────────────────────────────────────────────
    IEqual,
    INotEqual,
    SLessThan,
    ULessThan,
    SLessThanEqual,
    ULessThanEqual,
    SGreaterThan,
    UGreaterThan,
    SGreaterThanEqual,
    UGreaterThanEqual,

    // ── Logic ─────────────────────────────────────────────────────────
    LogicalOr,
    LogicalAnd,
    LogicalXor,
    LogicalNot,

    // ── Conversion ────────────────────────────────────────────────────
    ConvertS16F16,
    ConvertS16F32,
    ConvertS16F64,
    ConvertS32F16,
    ConvertS32F32,
    ConvertS32F64,
    ConvertS64F16,
    ConvertS64F32,
    ConvertS64F64,
    ConvertU16F16,
    ConvertU16F32,
    ConvertU16F64,
    ConvertU32F16,
    ConvertU32F32,
    ConvertU32F64,
    ConvertU64F16,
    ConvertU64F32,
    ConvertU64F64,
    ConvertU64U32,
    ConvertU32U64,
    ConvertF16F32,
    ConvertF32F16,
    ConvertF32F64,
    ConvertF64F32,
    ConvertF16S8,
    ConvertF16S16,
    ConvertF16S32,
    ConvertF16S64,
    ConvertF16U8,
    ConvertF16U16,
    ConvertF16U32,
    ConvertF16U64,
    ConvertF32S8,
    ConvertF32S16,
    ConvertF32S32,
    ConvertF32S64,
    ConvertF32U8,
    ConvertF32U16,
    ConvertF32U32,
    ConvertF32U64,
    ConvertF64S8,
    ConvertF64S16,
    ConvertF64S32,
    ConvertF64S64,
    ConvertF64U8,
    ConvertF64U16,
    ConvertF64U32,
    ConvertF64U64,

    // ── Texture ───────────────────────────────────────────────────────
    /// Implicit LOD texture sample (fragment shader).
    ImageSampleImplicitLod,
    /// Explicit LOD texture sample.
    ImageSampleExplicitLod,
    /// Implicit LOD depth comparison sample.
    ImageSampleDrefImplicitLod,
    /// Explicit LOD depth comparison sample.
    ImageSampleDrefExplicitLod,
    /// Texel fetch (integer coordinates).
    ImageFetch,
    /// Query texture dimensions.
    ImageQueryDimensions,
    /// Texture gather (4 texels).
    ImageGather,
    /// Texture gather with depth comparison.
    ImageGatherDref,
    /// Query LOD.
    ImageQueryLod,
    /// Gradient texture sample.
    ImageGradient,

    // ── Upstream pre-TexturePass texture ops ─────────────────────────
    BoundImageSampleImplicitLod,
    BindlessImageSampleImplicitLod,
    BoundImageSampleExplicitLod,
    BindlessImageSampleExplicitLod,
    BoundImageSampleDrefImplicitLod,
    BindlessImageSampleDrefImplicitLod,
    BoundImageSampleDrefExplicitLod,
    BindlessImageSampleDrefExplicitLod,
    BoundImageGather,
    BindlessImageGather,
    BoundImageGatherDref,
    BindlessImageGatherDref,
    BoundImageFetch,
    BindlessImageFetch,
    BoundImageQueryDimensions,
    BindlessImageQueryDimensions,
    BoundImageQueryLod,
    BindlessImageQueryLod,
    BoundImageGradient,
    BindlessImageGradient,

    // ── Image load/store ──────────────────────────────────────────────
    ImageRead,
    ImageWrite,
    BoundImageRead,
    BindlessImageRead,
    BoundImageWrite,
    BindlessImageWrite,
    ImageAtomicIAdd32,
    ImageAtomicSMin32,
    ImageAtomicUMin32,
    ImageAtomicSMax32,
    ImageAtomicUMax32,
    ImageAtomicInc32,
    ImageAtomicDec32,
    ImageAtomicAnd32,
    ImageAtomicOr32,
    ImageAtomicXor32,
    ImageAtomicExchange32,
    BoundImageAtomicIAdd32,
    BindlessImageAtomicIAdd32,
    BoundImageAtomicSMin32,
    BindlessImageAtomicSMin32,
    BoundImageAtomicUMin32,
    BindlessImageAtomicUMin32,
    BoundImageAtomicSMax32,
    BindlessImageAtomicSMax32,
    BoundImageAtomicUMax32,
    BindlessImageAtomicUMax32,
    BoundImageAtomicInc32,
    BindlessImageAtomicInc32,
    BoundImageAtomicDec32,
    BindlessImageAtomicDec32,
    BoundImageAtomicAnd32,
    BindlessImageAtomicAnd32,
    BoundImageAtomicOr32,
    BindlessImageAtomicOr32,
    BoundImageAtomicXor32,
    BindlessImageAtomicXor32,
    BoundImageAtomicExchange32,
    BindlessImageAtomicExchange32,

    // ── Atomic memory ─────────────────────────────────────────────────
    GlobalAtomicIAdd32,
    GlobalAtomicSMin32,
    GlobalAtomicUMin32,
    GlobalAtomicSMax32,
    GlobalAtomicUMax32,
    GlobalAtomicInc32,
    GlobalAtomicDec32,
    GlobalAtomicAnd32,
    GlobalAtomicOr32,
    GlobalAtomicXor32,
    GlobalAtomicExchange32,
    GlobalAtomicIAdd64,
    GlobalAtomicSMin64,
    GlobalAtomicUMin64,
    GlobalAtomicSMax64,
    GlobalAtomicUMax64,
    GlobalAtomicAnd64,
    GlobalAtomicOr64,
    GlobalAtomicXor64,
    GlobalAtomicExchange64,
    GlobalAtomicIAdd32x2,
    GlobalAtomicSMin32x2,
    GlobalAtomicUMin32x2,
    GlobalAtomicSMax32x2,
    GlobalAtomicUMax32x2,
    GlobalAtomicAnd32x2,
    GlobalAtomicOr32x2,
    GlobalAtomicXor32x2,
    GlobalAtomicExchange32x2,
    GlobalAtomicAddF32,
    GlobalAtomicAddF16x2,
    GlobalAtomicAddF32x2,
    GlobalAtomicMinF16x2,
    GlobalAtomicMinF32x2,
    GlobalAtomicMaxF16x2,
    GlobalAtomicMaxF32x2,
    StorageAtomicIAdd32,
    StorageAtomicSMin32,
    StorageAtomicUMin32,
    StorageAtomicSMax32,
    StorageAtomicUMax32,
    StorageAtomicInc32,
    StorageAtomicDec32,
    StorageAtomicAnd32,
    StorageAtomicOr32,
    StorageAtomicXor32,
    StorageAtomicExchange32,
    StorageAtomicIAdd64,
    StorageAtomicSMin64,
    StorageAtomicUMin64,
    StorageAtomicSMax64,
    StorageAtomicUMax64,
    StorageAtomicAnd64,
    StorageAtomicOr64,
    StorageAtomicXor64,
    StorageAtomicExchange64,
    StorageAtomicIAdd32x2,
    StorageAtomicSMin32x2,
    StorageAtomicUMin32x2,
    StorageAtomicSMax32x2,
    StorageAtomicUMax32x2,
    StorageAtomicAnd32x2,
    StorageAtomicOr32x2,
    StorageAtomicXor32x2,
    StorageAtomicExchange32x2,
    StorageAtomicAddF32,
    StorageAtomicAddF16x2,
    StorageAtomicAddF32x2,
    StorageAtomicMinF16x2,
    StorageAtomicMinF32x2,
    StorageAtomicMaxF16x2,
    StorageAtomicMaxF32x2,
    SharedAtomicIAdd32,
    SharedAtomicSMin32,
    SharedAtomicUMin32,
    SharedAtomicSMax32,
    SharedAtomicUMax32,
    SharedAtomicInc32,
    SharedAtomicDec32,
    SharedAtomicAnd32,
    SharedAtomicOr32,
    SharedAtomicXor32,
    SharedAtomicExchange32,
    SharedAtomicExchange64,
    SharedAtomicExchange32x2,

    // ── Warp / Subgroup ───────────────────────────────────────────────
    VoteAll,
    VoteAny,
    VoteEqual,
    LaneId,
    SubgroupBallot,
    SubgroupEqMask,
    SubgroupLtMask,
    SubgroupLeMask,
    SubgroupGtMask,
    SubgroupGeMask,
    ShuffleIndex,
    ShuffleUp,
    ShuffleDown,
    ShuffleButterfly,
    FSwizzleAdd,
    DPdxFine,
    DPdyFine,
    DPdxCoarse,
    DPdyCoarse,

    // ── Branch / control flow (used internally by structured CF) ──────
    Branch,
    BranchConditional,
    LoopMerge,
    SelectionMerge,
    Return,
    Unreachable,
}

/// Opcode metadata: return type and argument types.
pub struct OpcodeMeta {
    pub name: &'static str,
    pub return_type: Type,
    pub arg_types: &'static [Type],
}

impl Opcode {
    /// Get the return type of this opcode.
    pub fn return_type(self) -> Type {
        self.meta().return_type
    }

    /// Get the number of arguments.
    pub fn num_args(self) -> usize {
        self.meta().arg_types.len()
    }

    /// Get the type of argument at `index`.
    pub fn arg_type(self, index: usize) -> Type {
        self.meta().arg_types[index]
    }

    /// Get the name of this opcode.
    pub fn name(self) -> &'static str {
        self.meta().name
    }

    fn texture_pass_opcode_name(self) -> &'static str {
        match self {
            Opcode::BoundImageSampleImplicitLod => "BoundImageSampleImplicitLod",
            Opcode::BindlessImageSampleImplicitLod => "BindlessImageSampleImplicitLod",
            Opcode::BoundImageSampleExplicitLod => "BoundImageSampleExplicitLod",
            Opcode::BindlessImageSampleExplicitLod => "BindlessImageSampleExplicitLod",
            Opcode::BoundImageSampleDrefImplicitLod => "BoundImageSampleDrefImplicitLod",
            Opcode::BindlessImageSampleDrefImplicitLod => "BindlessImageSampleDrefImplicitLod",
            Opcode::BoundImageSampleDrefExplicitLod => "BoundImageSampleDrefExplicitLod",
            Opcode::BindlessImageSampleDrefExplicitLod => "BindlessImageSampleDrefExplicitLod",
            Opcode::BoundImageGather => "BoundImageGather",
            Opcode::BindlessImageGather => "BindlessImageGather",
            Opcode::BoundImageGatherDref => "BoundImageGatherDref",
            Opcode::BindlessImageGatherDref => "BindlessImageGatherDref",
            Opcode::BoundImageFetch => "BoundImageFetch",
            Opcode::BindlessImageFetch => "BindlessImageFetch",
            Opcode::BoundImageQueryDimensions => "BoundImageQueryDimensions",
            Opcode::BindlessImageQueryDimensions => "BindlessImageQueryDimensions",
            Opcode::BoundImageQueryLod => "BoundImageQueryLod",
            Opcode::BindlessImageQueryLod => "BindlessImageQueryLod",
            Opcode::BoundImageGradient => "BoundImageGradient",
            Opcode::BindlessImageGradient => "BindlessImageGradient",
            Opcode::BoundImageRead => "BoundImageRead",
            Opcode::BindlessImageRead => "BindlessImageRead",
            Opcode::BoundImageWrite => "BoundImageWrite",
            Opcode::BindlessImageWrite => "BindlessImageWrite",
            Opcode::ImageAtomicIAdd32 => "ImageAtomicIAdd32",
            Opcode::ImageAtomicSMin32 => "ImageAtomicSMin32",
            Opcode::ImageAtomicUMin32 => "ImageAtomicUMin32",
            Opcode::ImageAtomicSMax32 => "ImageAtomicSMax32",
            Opcode::ImageAtomicUMax32 => "ImageAtomicUMax32",
            Opcode::ImageAtomicInc32 => "ImageAtomicInc32",
            Opcode::ImageAtomicDec32 => "ImageAtomicDec32",
            Opcode::ImageAtomicAnd32 => "ImageAtomicAnd32",
            Opcode::ImageAtomicOr32 => "ImageAtomicOr32",
            Opcode::ImageAtomicXor32 => "ImageAtomicXor32",
            Opcode::ImageAtomicExchange32 => "ImageAtomicExchange32",
            Opcode::BoundImageAtomicIAdd32 => "BoundImageAtomicIAdd32",
            Opcode::BindlessImageAtomicIAdd32 => "BindlessImageAtomicIAdd32",
            Opcode::BoundImageAtomicSMin32 => "BoundImageAtomicSMin32",
            Opcode::BindlessImageAtomicSMin32 => "BindlessImageAtomicSMin32",
            Opcode::BoundImageAtomicUMin32 => "BoundImageAtomicUMin32",
            Opcode::BindlessImageAtomicUMin32 => "BindlessImageAtomicUMin32",
            Opcode::BoundImageAtomicSMax32 => "BoundImageAtomicSMax32",
            Opcode::BindlessImageAtomicSMax32 => "BindlessImageAtomicSMax32",
            Opcode::BoundImageAtomicUMax32 => "BoundImageAtomicUMax32",
            Opcode::BindlessImageAtomicUMax32 => "BindlessImageAtomicUMax32",
            Opcode::BoundImageAtomicInc32 => "BoundImageAtomicInc32",
            Opcode::BindlessImageAtomicInc32 => "BindlessImageAtomicInc32",
            Opcode::BoundImageAtomicDec32 => "BoundImageAtomicDec32",
            Opcode::BindlessImageAtomicDec32 => "BindlessImageAtomicDec32",
            Opcode::BoundImageAtomicAnd32 => "BoundImageAtomicAnd32",
            Opcode::BindlessImageAtomicAnd32 => "BindlessImageAtomicAnd32",
            Opcode::BoundImageAtomicOr32 => "BoundImageAtomicOr32",
            Opcode::BindlessImageAtomicOr32 => "BindlessImageAtomicOr32",
            Opcode::BoundImageAtomicXor32 => "BoundImageAtomicXor32",
            Opcode::BindlessImageAtomicXor32 => "BindlessImageAtomicXor32",
            Opcode::BoundImageAtomicExchange32 => "BoundImageAtomicExchange32",
            Opcode::BindlessImageAtomicExchange32 => "BindlessImageAtomicExchange32",
            _ => unreachable!("not a texture-pass opcode"),
        }
    }

    /// Whether this is a pseudo-operation (GetZeroFromOp, etc.).
    pub fn is_pseudo_instruction(self) -> bool {
        matches!(
            self,
            Opcode::GetZeroFromOp
                | Opcode::GetSignFromOp
                | Opcode::GetCarryFromOp
                | Opcode::GetOverflowFromOp
                | Opcode::GetSparseFromOp
                | Opcode::GetInBoundsFromOp
        )
    }

    /// Whether this instruction may have side effects.
    pub fn may_have_side_effects(self) -> bool {
        matches!(
            self,
            Opcode::ConditionRef
                | Opcode::Reference
                | Opcode::PhiMove
                | Opcode::Prologue
                | Opcode::Epilogue
                | Opcode::Join
                | Opcode::DemoteToHelperInvocation
                | Opcode::Barrier
                | Opcode::WorkgroupMemoryBarrier
                | Opcode::DeviceMemoryBarrier
                | Opcode::EmitVertex
                | Opcode::EndPrimitive
                | Opcode::SetAttribute
                | Opcode::SetAttributeIndexed
                | Opcode::SetPatch
                | Opcode::SetFragColor
                | Opcode::SetSampleMask
                | Opcode::SetFragDepth
                | Opcode::WriteGlobalU8
                | Opcode::WriteGlobalS8
                | Opcode::WriteGlobalU16
                | Opcode::WriteGlobalS16
                | Opcode::WriteGlobal32
                | Opcode::WriteGlobal64
                | Opcode::WriteGlobal128
                | Opcode::WriteStorageU8
                | Opcode::WriteStorageS8
                | Opcode::WriteStorageU16
                | Opcode::WriteStorageS16
                | Opcode::WriteStorage32
                | Opcode::WriteStorage64
                | Opcode::WriteStorage128
                | Opcode::WriteLocal
                | Opcode::WriteSharedU8
                | Opcode::WriteSharedU16
                | Opcode::WriteSharedU32
                | Opcode::WriteSharedU64
                | Opcode::WriteSharedU128
                | Opcode::GlobalAtomicIAdd32
                | Opcode::GlobalAtomicSMin32
                | Opcode::GlobalAtomicUMin32
                | Opcode::GlobalAtomicSMax32
                | Opcode::GlobalAtomicUMax32
                | Opcode::GlobalAtomicInc32
                | Opcode::GlobalAtomicDec32
                | Opcode::GlobalAtomicAnd32
                | Opcode::GlobalAtomicOr32
                | Opcode::GlobalAtomicXor32
                | Opcode::GlobalAtomicExchange32
                | Opcode::GlobalAtomicIAdd64
                | Opcode::GlobalAtomicSMin64
                | Opcode::GlobalAtomicUMin64
                | Opcode::GlobalAtomicSMax64
                | Opcode::GlobalAtomicUMax64
                | Opcode::GlobalAtomicAnd64
                | Opcode::GlobalAtomicOr64
                | Opcode::GlobalAtomicXor64
                | Opcode::GlobalAtomicExchange64
                | Opcode::GlobalAtomicIAdd32x2
                | Opcode::GlobalAtomicSMin32x2
                | Opcode::GlobalAtomicUMin32x2
                | Opcode::GlobalAtomicSMax32x2
                | Opcode::GlobalAtomicUMax32x2
                | Opcode::GlobalAtomicAnd32x2
                | Opcode::GlobalAtomicOr32x2
                | Opcode::GlobalAtomicXor32x2
                | Opcode::GlobalAtomicExchange32x2
                | Opcode::GlobalAtomicAddF32
                | Opcode::GlobalAtomicAddF16x2
                | Opcode::GlobalAtomicAddF32x2
                | Opcode::GlobalAtomicMinF16x2
                | Opcode::GlobalAtomicMinF32x2
                | Opcode::GlobalAtomicMaxF16x2
                | Opcode::GlobalAtomicMaxF32x2
                | Opcode::StorageAtomicIAdd32
                | Opcode::StorageAtomicSMin32
                | Opcode::StorageAtomicUMin32
                | Opcode::StorageAtomicSMax32
                | Opcode::StorageAtomicUMax32
                | Opcode::StorageAtomicInc32
                | Opcode::StorageAtomicDec32
                | Opcode::StorageAtomicAnd32
                | Opcode::StorageAtomicOr32
                | Opcode::StorageAtomicXor32
                | Opcode::StorageAtomicExchange32
                | Opcode::StorageAtomicIAdd64
                | Opcode::StorageAtomicSMin64
                | Opcode::StorageAtomicUMin64
                | Opcode::StorageAtomicSMax64
                | Opcode::StorageAtomicUMax64
                | Opcode::StorageAtomicAnd64
                | Opcode::StorageAtomicOr64
                | Opcode::StorageAtomicXor64
                | Opcode::StorageAtomicExchange64
                | Opcode::StorageAtomicIAdd32x2
                | Opcode::StorageAtomicSMin32x2
                | Opcode::StorageAtomicUMin32x2
                | Opcode::StorageAtomicSMax32x2
                | Opcode::StorageAtomicUMax32x2
                | Opcode::StorageAtomicAnd32x2
                | Opcode::StorageAtomicOr32x2
                | Opcode::StorageAtomicXor32x2
                | Opcode::StorageAtomicExchange32x2
                | Opcode::StorageAtomicAddF32
                | Opcode::StorageAtomicAddF16x2
                | Opcode::StorageAtomicAddF32x2
                | Opcode::StorageAtomicMinF16x2
                | Opcode::StorageAtomicMinF32x2
                | Opcode::StorageAtomicMaxF16x2
                | Opcode::StorageAtomicMaxF32x2
                | Opcode::SharedAtomicIAdd32
                | Opcode::SharedAtomicSMin32
                | Opcode::SharedAtomicUMin32
                | Opcode::SharedAtomicSMax32
                | Opcode::SharedAtomicUMax32
                | Opcode::SharedAtomicInc32
                | Opcode::SharedAtomicDec32
                | Opcode::SharedAtomicAnd32
                | Opcode::SharedAtomicOr32
                | Opcode::SharedAtomicXor32
                | Opcode::SharedAtomicExchange32
                | Opcode::SharedAtomicExchange64
                | Opcode::SharedAtomicExchange32x2
                | Opcode::ImageWrite
                | Opcode::BoundImageWrite
                | Opcode::BindlessImageWrite
                | Opcode::ImageAtomicIAdd32
                | Opcode::ImageAtomicSMin32
                | Opcode::ImageAtomicUMin32
                | Opcode::ImageAtomicSMax32
                | Opcode::ImageAtomicUMax32
                | Opcode::ImageAtomicInc32
                | Opcode::ImageAtomicDec32
                | Opcode::ImageAtomicAnd32
                | Opcode::ImageAtomicOr32
                | Opcode::ImageAtomicXor32
                | Opcode::ImageAtomicExchange32
                | Opcode::BoundImageAtomicIAdd32
                | Opcode::BindlessImageAtomicIAdd32
                | Opcode::BoundImageAtomicSMin32
                | Opcode::BindlessImageAtomicSMin32
                | Opcode::BoundImageAtomicUMin32
                | Opcode::BindlessImageAtomicUMin32
                | Opcode::BoundImageAtomicSMax32
                | Opcode::BindlessImageAtomicSMax32
                | Opcode::BoundImageAtomicUMax32
                | Opcode::BindlessImageAtomicUMax32
                | Opcode::BoundImageAtomicInc32
                | Opcode::BindlessImageAtomicInc32
                | Opcode::BoundImageAtomicDec32
                | Opcode::BindlessImageAtomicDec32
                | Opcode::BoundImageAtomicAnd32
                | Opcode::BindlessImageAtomicAnd32
                | Opcode::BoundImageAtomicOr32
                | Opcode::BindlessImageAtomicOr32
                | Opcode::BoundImageAtomicXor32
                | Opcode::BindlessImageAtomicXor32
                | Opcode::BoundImageAtomicExchange32
                | Opcode::BindlessImageAtomicExchange32
        )
    }

    /// Get the full metadata for this opcode.
    pub fn meta(self) -> OpcodeMeta {
        use Type::*;
        match self {
            // Control/Meta
            Opcode::Phi => OpcodeMeta {
                name: "Phi",
                return_type: Opaque,
                arg_types: &[],
            },
            Opcode::Identity => OpcodeMeta {
                name: "Identity",
                return_type: Opaque,
                arg_types: &[Opaque],
            },
            Opcode::Void => OpcodeMeta {
                name: "Void",
                return_type: Type::Void,
                arg_types: &[],
            },
            Opcode::ConditionRef => OpcodeMeta {
                name: "ConditionRef",
                return_type: U1,
                arg_types: &[U1],
            },
            Opcode::Reference => OpcodeMeta {
                name: "Reference",
                return_type: Type::Void,
                arg_types: &[Opaque],
            },
            Opcode::PhiMove => OpcodeMeta {
                name: "PhiMove",
                return_type: Type::Void,
                arg_types: &[Opaque, Opaque],
            },

            // Special
            Opcode::Prologue => OpcodeMeta {
                name: "Prologue",
                return_type: Type::Void,
                arg_types: &[],
            },
            Opcode::Epilogue => OpcodeMeta {
                name: "Epilogue",
                return_type: Type::Void,
                arg_types: &[],
            },
            Opcode::Join => OpcodeMeta {
                name: "Join",
                return_type: Type::Void,
                arg_types: &[],
            },
            Opcode::DemoteToHelperInvocation => OpcodeMeta {
                name: "DemoteToHelperInvocation",
                return_type: Type::Void,
                arg_types: &[],
            },
            Opcode::EmitVertex => OpcodeMeta {
                name: "EmitVertex",
                return_type: Type::Void,
                arg_types: &[U32],
            },
            Opcode::EndPrimitive => OpcodeMeta {
                name: "EndPrimitive",
                return_type: Type::Void,
                arg_types: &[U32],
            },

            // Barriers
            Opcode::Barrier => OpcodeMeta {
                name: "Barrier",
                return_type: Type::Void,
                arg_types: &[],
            },
            Opcode::WorkgroupMemoryBarrier => OpcodeMeta {
                name: "WorkgroupMemoryBarrier",
                return_type: Type::Void,
                arg_types: &[],
            },
            Opcode::DeviceMemoryBarrier => OpcodeMeta {
                name: "DeviceMemoryBarrier",
                return_type: Type::Void,
                arg_types: &[],
            },

            // Context getters/setters
            Opcode::GetRegister => OpcodeMeta {
                name: "GetRegister",
                return_type: U32,
                arg_types: &[Reg],
            },
            Opcode::SetRegister => OpcodeMeta {
                name: "SetRegister",
                return_type: Type::Void,
                arg_types: &[Reg, U32],
            },
            Opcode::GetPred => OpcodeMeta {
                name: "GetPred",
                return_type: U1,
                arg_types: &[Pred],
            },
            Opcode::SetPred => OpcodeMeta {
                name: "SetPred",
                return_type: Type::Void,
                arg_types: &[Pred, U1],
            },
            Opcode::GetGotoVariable => OpcodeMeta {
                name: "GetGotoVariable",
                return_type: U1,
                arg_types: &[U32],
            },
            Opcode::SetGotoVariable => OpcodeMeta {
                name: "SetGotoVariable",
                return_type: Type::Void,
                arg_types: &[U32, U1],
            },
            Opcode::GetIndirectBranchVariable => OpcodeMeta {
                name: "GetIndirectBranchVariable",
                return_type: U32,
                arg_types: &[],
            },
            Opcode::SetIndirectBranchVariable => OpcodeMeta {
                name: "SetIndirectBranchVariable",
                return_type: Type::Void,
                arg_types: &[U32],
            },

            // Constant buffer loads
            Opcode::GetCbufU8 => OpcodeMeta {
                name: "GetCbufU8",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::GetCbufS8 => OpcodeMeta {
                name: "GetCbufS8",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::GetCbufU16 => OpcodeMeta {
                name: "GetCbufU16",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::GetCbufS16 => OpcodeMeta {
                name: "GetCbufS16",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::GetCbufU32 => OpcodeMeta {
                name: "GetCbufU32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::GetCbufF32 => OpcodeMeta {
                name: "GetCbufF32",
                return_type: F32,
                arg_types: &[U32, U32],
            },
            Opcode::GetCbufU32x2 => OpcodeMeta {
                name: "GetCbufU32x2",
                return_type: U32x2,
                arg_types: &[U32, U32],
            },

            // Attribute I/O
            Opcode::GetAttribute => OpcodeMeta {
                name: "GetAttribute",
                return_type: F32,
                arg_types: &[Attribute, U32],
            },
            Opcode::GetAttributeU32 => OpcodeMeta {
                name: "GetAttributeU32",
                return_type: U32,
                arg_types: &[Attribute, U32],
            },
            Opcode::SetAttribute => OpcodeMeta {
                name: "SetAttribute",
                return_type: Type::Void,
                arg_types: &[Attribute, F32, U32],
            },
            Opcode::GetAttributeIndexed => OpcodeMeta {
                name: "GetAttributeIndexed",
                return_type: F32,
                arg_types: &[U32, U32],
            },
            Opcode::SetAttributeIndexed => OpcodeMeta {
                name: "SetAttributeIndexed",
                return_type: Type::Void,
                arg_types: &[U32, F32, U32],
            },
            Opcode::GetPatch => OpcodeMeta {
                name: "GetPatch",
                return_type: F32,
                arg_types: &[Patch],
            },
            Opcode::SetPatch => OpcodeMeta {
                name: "SetPatch",
                return_type: Type::Void,
                arg_types: &[Patch, F32],
            },
            Opcode::SetFragColor => OpcodeMeta {
                name: "SetFragColor",
                return_type: Type::Void,
                arg_types: &[U32, U32, F32],
            },
            Opcode::SetSampleMask => OpcodeMeta {
                name: "SetSampleMask",
                return_type: Type::Void,
                arg_types: &[U32],
            },
            Opcode::SetFragDepth => OpcodeMeta {
                name: "SetFragDepth",
                return_type: Type::Void,
                arg_types: &[F32],
            },

            // Condition flags
            Opcode::GetZFlag => OpcodeMeta {
                name: "GetZFlag",
                return_type: U1,
                arg_types: &[Type::Void],
            },
            Opcode::GetSFlag => OpcodeMeta {
                name: "GetSFlag",
                return_type: U1,
                arg_types: &[Type::Void],
            },
            Opcode::GetCFlag => OpcodeMeta {
                name: "GetCFlag",
                return_type: U1,
                arg_types: &[Type::Void],
            },
            Opcode::GetOFlag => OpcodeMeta {
                name: "GetOFlag",
                return_type: U1,
                arg_types: &[Type::Void],
            },
            Opcode::SetZFlag => OpcodeMeta {
                name: "SetZFlag",
                return_type: Type::Void,
                arg_types: &[U1],
            },
            Opcode::SetSFlag => OpcodeMeta {
                name: "SetSFlag",
                return_type: Type::Void,
                arg_types: &[U1],
            },
            Opcode::SetCFlag => OpcodeMeta {
                name: "SetCFlag",
                return_type: Type::Void,
                arg_types: &[U1],
            },
            Opcode::SetOFlag => OpcodeMeta {
                name: "SetOFlag",
                return_type: Type::Void,
                arg_types: &[U1],
            },

            // System values
            Opcode::WorkgroupId => OpcodeMeta {
                name: "WorkgroupId",
                return_type: U32x3,
                arg_types: &[],
            },
            Opcode::LocalInvocationId => OpcodeMeta {
                name: "LocalInvocationId",
                return_type: U32x3,
                arg_types: &[],
            },
            Opcode::InvocationId => OpcodeMeta {
                name: "InvocationId",
                return_type: U32,
                arg_types: &[],
            },
            Opcode::InvocationInfo => OpcodeMeta {
                name: "InvocationInfo",
                return_type: U32,
                arg_types: &[],
            },
            Opcode::SampleId => OpcodeMeta {
                name: "SampleId",
                return_type: U32,
                arg_types: &[],
            },
            Opcode::IsHelperInvocation => OpcodeMeta {
                name: "IsHelperInvocation",
                return_type: U1,
                arg_types: &[],
            },
            Opcode::SRWScaleFactorXY => OpcodeMeta {
                name: "SR_WScaleFactorXY",
                return_type: U32,
                arg_types: &[],
            },
            Opcode::SRWScaleFactorZ => OpcodeMeta {
                name: "SR_WScaleFactorZ",
                return_type: U32,
                arg_types: &[],
            },
            Opcode::YDirection => OpcodeMeta {
                name: "YDirection",
                return_type: F32,
                arg_types: &[],
            },
            Opcode::ResolutionDownFactor => OpcodeMeta {
                name: "ResolutionDownFactor",
                return_type: F32,
                arg_types: &[],
            },
            Opcode::IsTextureScaled => OpcodeMeta {
                name: "IsTextureScaled",
                return_type: U1,
                arg_types: &[U32],
            },
            Opcode::IsImageScaled => OpcodeMeta {
                name: "IsImageScaled",
                return_type: U1,
                arg_types: &[U32],
            },
            Opcode::RenderArea => OpcodeMeta {
                name: "RenderArea",
                return_type: F32x4,
                arg_types: &[],
            },

            // Undefined
            Opcode::UndefU1 => OpcodeMeta {
                name: "UndefU1",
                return_type: U1,
                arg_types: &[],
            },
            Opcode::UndefU8 => OpcodeMeta {
                name: "UndefU8",
                return_type: U8,
                arg_types: &[],
            },
            Opcode::UndefU16 => OpcodeMeta {
                name: "UndefU16",
                return_type: U16,
                arg_types: &[],
            },
            Opcode::UndefU32 => OpcodeMeta {
                name: "UndefU32",
                return_type: U32,
                arg_types: &[],
            },
            Opcode::UndefU64 => OpcodeMeta {
                name: "UndefU64",
                return_type: U64,
                arg_types: &[],
            },

            // Global memory
            Opcode::LoadGlobalU8 => OpcodeMeta {
                name: "LoadGlobalU8",
                return_type: U32,
                arg_types: &[Opaque],
            },
            Opcode::LoadGlobalS8 => OpcodeMeta {
                name: "LoadGlobalS8",
                return_type: U32,
                arg_types: &[Opaque],
            },
            Opcode::LoadGlobalU16 => OpcodeMeta {
                name: "LoadGlobalU16",
                return_type: U32,
                arg_types: &[Opaque],
            },
            Opcode::LoadGlobalS16 => OpcodeMeta {
                name: "LoadGlobalS16",
                return_type: U32,
                arg_types: &[Opaque],
            },
            Opcode::LoadGlobal32 => OpcodeMeta {
                name: "LoadGlobal32",
                return_type: U32,
                arg_types: &[Opaque],
            },
            Opcode::LoadGlobal64 => OpcodeMeta {
                name: "LoadGlobal64",
                return_type: U32x2,
                arg_types: &[Opaque],
            },
            Opcode::LoadGlobal128 => OpcodeMeta {
                name: "LoadGlobal128",
                return_type: U32x4,
                arg_types: &[Opaque],
            },
            Opcode::WriteGlobalU8 => OpcodeMeta {
                name: "WriteGlobalU8",
                return_type: Type::Void,
                arg_types: &[Opaque, U32],
            },
            Opcode::WriteGlobalS8 => OpcodeMeta {
                name: "WriteGlobalS8",
                return_type: Type::Void,
                arg_types: &[Opaque, U32],
            },
            Opcode::WriteGlobalU16 => OpcodeMeta {
                name: "WriteGlobalU16",
                return_type: Type::Void,
                arg_types: &[Opaque, U32],
            },
            Opcode::WriteGlobalS16 => OpcodeMeta {
                name: "WriteGlobalS16",
                return_type: Type::Void,
                arg_types: &[Opaque, U32],
            },
            Opcode::WriteGlobal32 => OpcodeMeta {
                name: "WriteGlobal32",
                return_type: Type::Void,
                arg_types: &[Opaque, U32],
            },
            Opcode::WriteGlobal64 => OpcodeMeta {
                name: "WriteGlobal64",
                return_type: Type::Void,
                arg_types: &[Opaque, U32x2],
            },
            Opcode::WriteGlobal128 => OpcodeMeta {
                name: "WriteGlobal128",
                return_type: Type::Void,
                arg_types: &[Opaque, U32x4],
            },

            // Storage buffer
            Opcode::LoadStorageU8 => OpcodeMeta {
                name: "LoadStorageU8",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::LoadStorageS8 => OpcodeMeta {
                name: "LoadStorageS8",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::LoadStorageU16 => OpcodeMeta {
                name: "LoadStorageU16",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::LoadStorageS16 => OpcodeMeta {
                name: "LoadStorageS16",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::LoadStorage32 => OpcodeMeta {
                name: "LoadStorage32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::LoadStorage64 => OpcodeMeta {
                name: "LoadStorage64",
                return_type: U32x2,
                arg_types: &[U32, U32],
            },
            Opcode::LoadStorage128 => OpcodeMeta {
                name: "LoadStorage128",
                return_type: U32x4,
                arg_types: &[U32, U32],
            },
            Opcode::WriteStorageU8 => OpcodeMeta {
                name: "WriteStorageU8",
                return_type: Type::Void,
                arg_types: &[U32, U32, U32],
            },
            Opcode::WriteStorageS8 => OpcodeMeta {
                name: "WriteStorageS8",
                return_type: Type::Void,
                arg_types: &[U32, U32, U32],
            },
            Opcode::WriteStorageU16 => OpcodeMeta {
                name: "WriteStorageU16",
                return_type: Type::Void,
                arg_types: &[U32, U32, U32],
            },
            Opcode::WriteStorageS16 => OpcodeMeta {
                name: "WriteStorageS16",
                return_type: Type::Void,
                arg_types: &[U32, U32, U32],
            },
            Opcode::WriteStorage32 => OpcodeMeta {
                name: "WriteStorage32",
                return_type: Type::Void,
                arg_types: &[U32, U32, U32],
            },
            Opcode::WriteStorage64 => OpcodeMeta {
                name: "WriteStorage64",
                return_type: Type::Void,
                arg_types: &[U32, U32, U32x2],
            },
            Opcode::WriteStorage128 => OpcodeMeta {
                name: "WriteStorage128",
                return_type: Type::Void,
                arg_types: &[U32, U32, U32x4],
            },

            // Local memory
            Opcode::LoadLocal => OpcodeMeta {
                name: "LoadLocal",
                return_type: U32,
                arg_types: &[U32],
            },
            Opcode::WriteLocal => OpcodeMeta {
                name: "WriteLocal",
                return_type: Type::Void,
                arg_types: &[U32, U32],
            },

            // Shared memory
            Opcode::LoadSharedU8 => OpcodeMeta {
                name: "LoadSharedU8",
                return_type: U32,
                arg_types: &[U32],
            },
            Opcode::LoadSharedS8 => OpcodeMeta {
                name: "LoadSharedS8",
                return_type: U32,
                arg_types: &[U32],
            },
            Opcode::LoadSharedU16 => OpcodeMeta {
                name: "LoadSharedU16",
                return_type: U32,
                arg_types: &[U32],
            },
            Opcode::LoadSharedS16 => OpcodeMeta {
                name: "LoadSharedS16",
                return_type: U32,
                arg_types: &[U32],
            },
            Opcode::LoadSharedU32 => OpcodeMeta {
                name: "LoadSharedU32",
                return_type: U32,
                arg_types: &[U32],
            },
            Opcode::LoadSharedU64 => OpcodeMeta {
                name: "LoadSharedU64",
                return_type: U32x2,
                arg_types: &[U32],
            },
            Opcode::LoadSharedU128 => OpcodeMeta {
                name: "LoadSharedU128",
                return_type: U32x4,
                arg_types: &[U32],
            },
            Opcode::WriteSharedU8 => OpcodeMeta {
                name: "WriteSharedU8",
                return_type: Type::Void,
                arg_types: &[U32, U32],
            },
            Opcode::WriteSharedU16 => OpcodeMeta {
                name: "WriteSharedU16",
                return_type: Type::Void,
                arg_types: &[U32, U32],
            },
            Opcode::WriteSharedU32 => OpcodeMeta {
                name: "WriteSharedU32",
                return_type: Type::Void,
                arg_types: &[U32, U32],
            },
            Opcode::WriteSharedU64 => OpcodeMeta {
                name: "WriteSharedU64",
                return_type: Type::Void,
                arg_types: &[U32, U32x2],
            },
            Opcode::WriteSharedU128 => OpcodeMeta {
                name: "WriteSharedU128",
                return_type: Type::Void,
                arg_types: &[U32, U32x4],
            },

            // Vector composite
            Opcode::CompositeConstructU32x2 => OpcodeMeta {
                name: "CompositeConstructU32x2",
                return_type: U32x2,
                arg_types: &[U32, U32],
            },
            Opcode::CompositeConstructU32x3 => OpcodeMeta {
                name: "CompositeConstructU32x3",
                return_type: U32x3,
                arg_types: &[U32, U32, U32],
            },
            Opcode::CompositeConstructU32x4 => OpcodeMeta {
                name: "CompositeConstructU32x4",
                return_type: U32x4,
                arg_types: &[U32, U32, U32, U32],
            },
            Opcode::CompositeExtractU32x2 => OpcodeMeta {
                name: "CompositeExtractU32x2",
                return_type: U32,
                arg_types: &[U32x2, U32],
            },
            Opcode::CompositeExtractU32x3 => OpcodeMeta {
                name: "CompositeExtractU32x3",
                return_type: U32,
                arg_types: &[U32x3, U32],
            },
            Opcode::CompositeExtractU32x4 => OpcodeMeta {
                name: "CompositeExtractU32x4",
                return_type: U32,
                arg_types: &[U32x4, U32],
            },
            Opcode::CompositeInsertU32x2 => OpcodeMeta {
                name: "CompositeInsertU32x2",
                return_type: U32x2,
                arg_types: &[U32x2, U32, U32],
            },
            Opcode::CompositeInsertU32x3 => OpcodeMeta {
                name: "CompositeInsertU32x3",
                return_type: U32x3,
                arg_types: &[U32x3, U32, U32],
            },
            Opcode::CompositeInsertU32x4 => OpcodeMeta {
                name: "CompositeInsertU32x4",
                return_type: U32x4,
                arg_types: &[U32x4, U32, U32],
            },
            Opcode::CompositeConstructF16x2 => OpcodeMeta {
                name: "CompositeConstructF16x2",
                return_type: F16x2,
                arg_types: &[F16, F16],
            },
            Opcode::CompositeConstructF16x3 => OpcodeMeta {
                name: "CompositeConstructF16x3",
                return_type: F16x3,
                arg_types: &[F16, F16, F16],
            },
            Opcode::CompositeConstructF16x4 => OpcodeMeta {
                name: "CompositeConstructF16x4",
                return_type: F16x4,
                arg_types: &[F16, F16, F16, F16],
            },
            Opcode::CompositeExtractF16x2 => OpcodeMeta {
                name: "CompositeExtractF16x2",
                return_type: F16,
                arg_types: &[F16x2, U32],
            },
            Opcode::CompositeExtractF16x3 => OpcodeMeta {
                name: "CompositeExtractF16x3",
                return_type: F16,
                arg_types: &[F16x3, U32],
            },
            Opcode::CompositeExtractF16x4 => OpcodeMeta {
                name: "CompositeExtractF16x4",
                return_type: F16,
                arg_types: &[F16x4, U32],
            },
            Opcode::CompositeInsertF16x2 => OpcodeMeta {
                name: "CompositeInsertF16x2",
                return_type: F16x2,
                arg_types: &[F16x2, F16, U32],
            },
            Opcode::CompositeInsertF16x3 => OpcodeMeta {
                name: "CompositeInsertF16x3",
                return_type: F16x3,
                arg_types: &[F16x3, F16, U32],
            },
            Opcode::CompositeInsertF16x4 => OpcodeMeta {
                name: "CompositeInsertF16x4",
                return_type: F16x4,
                arg_types: &[F16x4, F16, U32],
            },
            Opcode::CompositeConstructF32x2 => OpcodeMeta {
                name: "CompositeConstructF32x2",
                return_type: F32x2,
                arg_types: &[F32, F32],
            },
            Opcode::CompositeConstructF32x3 => OpcodeMeta {
                name: "CompositeConstructF32x3",
                return_type: F32x3,
                arg_types: &[F32, F32, F32],
            },
            Opcode::CompositeConstructF32x4 => OpcodeMeta {
                name: "CompositeConstructF32x4",
                return_type: F32x4,
                arg_types: &[F32, F32, F32, F32],
            },
            Opcode::CompositeExtractF32x2 => OpcodeMeta {
                name: "CompositeExtractF32x2",
                return_type: F32,
                arg_types: &[F32x2, U32],
            },
            Opcode::CompositeExtractF32x3 => OpcodeMeta {
                name: "CompositeExtractF32x3",
                return_type: F32,
                arg_types: &[F32x3, U32],
            },
            Opcode::CompositeExtractF32x4 => OpcodeMeta {
                name: "CompositeExtractF32x4",
                return_type: F32,
                arg_types: &[F32x4, U32],
            },
            Opcode::CompositeInsertF32x2 => OpcodeMeta {
                name: "CompositeInsertF32x2",
                return_type: F32x2,
                arg_types: &[F32x2, F32, U32],
            },
            Opcode::CompositeInsertF32x3 => OpcodeMeta {
                name: "CompositeInsertF32x3",
                return_type: F32x3,
                arg_types: &[F32x3, F32, U32],
            },
            Opcode::CompositeInsertF32x4 => OpcodeMeta {
                name: "CompositeInsertF32x4",
                return_type: F32x4,
                arg_types: &[F32x4, F32, U32],
            },
            Opcode::CompositeConstructF64x2 => OpcodeMeta {
                name: "CompositeConstructF64x2",
                return_type: F64x2,
                arg_types: &[F64, F64],
            },
            Opcode::CompositeConstructF64x3 => OpcodeMeta {
                name: "CompositeConstructF64x3",
                return_type: F64x3,
                arg_types: &[F64, F64, F64],
            },
            Opcode::CompositeConstructF64x4 => OpcodeMeta {
                name: "CompositeConstructF64x4",
                return_type: F64x4,
                arg_types: &[F64, F64, F64, F64],
            },
            Opcode::CompositeExtractF64x2 => OpcodeMeta {
                name: "CompositeExtractF64x2",
                return_type: F64,
                arg_types: &[F64x2, U32],
            },
            Opcode::CompositeExtractF64x3 => OpcodeMeta {
                name: "CompositeExtractF64x3",
                return_type: F64,
                arg_types: &[F64x3, U32],
            },
            Opcode::CompositeExtractF64x4 => OpcodeMeta {
                name: "CompositeExtractF64x4",
                return_type: F64,
                arg_types: &[F64x4, U32],
            },
            Opcode::CompositeInsertF64x2 => OpcodeMeta {
                name: "CompositeInsertF64x2",
                return_type: F64x2,
                arg_types: &[F64x2, F64, U32],
            },
            Opcode::CompositeInsertF64x3 => OpcodeMeta {
                name: "CompositeInsertF64x3",
                return_type: F64x3,
                arg_types: &[F64x3, F64, U32],
            },
            Opcode::CompositeInsertF64x4 => OpcodeMeta {
                name: "CompositeInsertF64x4",
                return_type: F64x4,
                arg_types: &[F64x4, F64, U32],
            },

            // Select
            Opcode::SelectU1 => OpcodeMeta {
                name: "SelectU1",
                return_type: U1,
                arg_types: &[U1, U1, U1],
            },
            Opcode::SelectU8 => OpcodeMeta {
                name: "SelectU8",
                return_type: U8,
                arg_types: &[U1, U8, U8],
            },
            Opcode::SelectU16 => OpcodeMeta {
                name: "SelectU16",
                return_type: U16,
                arg_types: &[U1, U16, U16],
            },
            Opcode::SelectU32 => OpcodeMeta {
                name: "SelectU32",
                return_type: U32,
                arg_types: &[U1, U32, U32],
            },
            Opcode::SelectU64 => OpcodeMeta {
                name: "SelectU64",
                return_type: U64,
                arg_types: &[U1, U64, U64],
            },
            Opcode::SelectF16 => OpcodeMeta {
                name: "SelectF16",
                return_type: F16,
                arg_types: &[U1, F16, F16],
            },
            Opcode::SelectF32 => OpcodeMeta {
                name: "SelectF32",
                return_type: F32,
                arg_types: &[U1, F32, F32],
            },
            Opcode::SelectF64 => OpcodeMeta {
                name: "SelectF64",
                return_type: F64,
                arg_types: &[U1, F64, F64],
            },

            // Bitcast / conversion
            Opcode::BitCastU16F16 => OpcodeMeta {
                name: "BitCastU16F16",
                return_type: U16,
                arg_types: &[F16],
            },
            Opcode::BitCastU32F32 => OpcodeMeta {
                name: "BitCastU32F32",
                return_type: U32,
                arg_types: &[F32],
            },
            Opcode::BitCastU64F64 => OpcodeMeta {
                name: "BitCastU64F64",
                return_type: U64,
                arg_types: &[F64],
            },
            Opcode::BitCastF16U16 => OpcodeMeta {
                name: "BitCastF16U16",
                return_type: F16,
                arg_types: &[U16],
            },
            Opcode::BitCastF32U32 => OpcodeMeta {
                name: "BitCastF32U32",
                return_type: F32,
                arg_types: &[U32],
            },
            Opcode::BitCastF64U64 => OpcodeMeta {
                name: "BitCastF64U64",
                return_type: F64,
                arg_types: &[U64],
            },
            Opcode::PackUint2x32 => OpcodeMeta {
                name: "PackUint2x32",
                return_type: U64,
                arg_types: &[U32x2],
            },
            Opcode::UnpackUint2x32 => OpcodeMeta {
                name: "UnpackUint2x32",
                return_type: U32x2,
                arg_types: &[U64],
            },
            Opcode::PackFloat2x16 => OpcodeMeta {
                name: "PackFloat2x16",
                return_type: U32,
                arg_types: &[F16x2],
            },
            Opcode::UnpackFloat2x16 => OpcodeMeta {
                name: "UnpackFloat2x16",
                return_type: F16x2,
                arg_types: &[U32],
            },
            Opcode::PackHalf2x16 => OpcodeMeta {
                name: "PackHalf2x16",
                return_type: U32,
                arg_types: &[F32x2],
            },
            Opcode::UnpackHalf2x16 => OpcodeMeta {
                name: "UnpackHalf2x16",
                return_type: F32x2,
                arg_types: &[U32],
            },
            Opcode::PackDouble2x32 => OpcodeMeta {
                name: "PackDouble2x32",
                return_type: F64,
                arg_types: &[U32x2],
            },
            Opcode::UnpackDouble2x32 => OpcodeMeta {
                name: "UnpackDouble2x32",
                return_type: U32x2,
                arg_types: &[F64],
            },

            // Pseudo-operations
            Opcode::GetZeroFromOp => OpcodeMeta {
                name: "GetZeroFromOp",
                return_type: U1,
                arg_types: &[Opaque],
            },
            Opcode::GetSignFromOp => OpcodeMeta {
                name: "GetSignFromOp",
                return_type: U1,
                arg_types: &[Opaque],
            },
            Opcode::GetCarryFromOp => OpcodeMeta {
                name: "GetCarryFromOp",
                return_type: U1,
                arg_types: &[Opaque],
            },
            Opcode::GetOverflowFromOp => OpcodeMeta {
                name: "GetOverflowFromOp",
                return_type: U1,
                arg_types: &[Opaque],
            },
            Opcode::GetSparseFromOp => OpcodeMeta {
                name: "GetSparseFromOp",
                return_type: U1,
                arg_types: &[Opaque],
            },
            Opcode::GetInBoundsFromOp => OpcodeMeta {
                name: "GetInBoundsFromOp",
                return_type: U1,
                arg_types: &[Opaque],
            },

            // FP16 arithmetic
            Opcode::FPAbs16 => OpcodeMeta {
                name: "FPAbs16",
                return_type: F16,
                arg_types: &[F16],
            },
            Opcode::FPNeg16 => OpcodeMeta {
                name: "FPNeg16",
                return_type: F16,
                arg_types: &[F16],
            },
            Opcode::FPAdd16 => OpcodeMeta {
                name: "FPAdd16",
                return_type: F16,
                arg_types: &[F16, F16],
            },
            Opcode::FPMul16 => OpcodeMeta {
                name: "FPMul16",
                return_type: F16,
                arg_types: &[F16, F16],
            },
            Opcode::FPFma16 => OpcodeMeta {
                name: "FPFma16",
                return_type: F16,
                arg_types: &[F16, F16, F16],
            },
            Opcode::FPMin16 => OpcodeMeta {
                name: "FPMin16",
                return_type: F16,
                arg_types: &[F16, F16],
            },
            Opcode::FPMax16 => OpcodeMeta {
                name: "FPMax16",
                return_type: F16,
                arg_types: &[F16, F16],
            },
            Opcode::FPSaturate16 => OpcodeMeta {
                name: "FPSaturate16",
                return_type: F16,
                arg_types: &[F16],
            },
            Opcode::FPClamp16 => OpcodeMeta {
                name: "FPClamp16",
                return_type: F16,
                arg_types: &[F16, F16, F16],
            },
            Opcode::FPRoundEven16 => OpcodeMeta {
                name: "FPRoundEven16",
                return_type: F16,
                arg_types: &[F16],
            },
            Opcode::FPFloor16 => OpcodeMeta {
                name: "FPFloor16",
                return_type: F16,
                arg_types: &[F16],
            },
            Opcode::FPCeil16 => OpcodeMeta {
                name: "FPCeil16",
                return_type: F16,
                arg_types: &[F16],
            },
            Opcode::FPTrunc16 => OpcodeMeta {
                name: "FPTrunc16",
                return_type: F16,
                arg_types: &[F16],
            },

            // FP32 arithmetic
            Opcode::FPAbs32 => OpcodeMeta {
                name: "FPAbs32",
                return_type: F32,
                arg_types: &[F32],
            },
            Opcode::FPNeg32 => OpcodeMeta {
                name: "FPNeg32",
                return_type: F32,
                arg_types: &[F32],
            },
            Opcode::FPAdd32 => OpcodeMeta {
                name: "FPAdd32",
                return_type: F32,
                arg_types: &[F32, F32],
            },
            Opcode::FPSub32 => OpcodeMeta {
                name: "FPSub32",
                return_type: F32,
                arg_types: &[F32, F32],
            },
            Opcode::FPMul32 => OpcodeMeta {
                name: "FPMul32",
                return_type: F32,
                arg_types: &[F32, F32],
            },
            Opcode::FPDiv32 => OpcodeMeta {
                name: "FPDiv32",
                return_type: F32,
                arg_types: &[F32, F32],
            },
            Opcode::FPFma32 => OpcodeMeta {
                name: "FPFma32",
                return_type: F32,
                arg_types: &[F32, F32, F32],
            },
            Opcode::FPMin32 => OpcodeMeta {
                name: "FPMin32",
                return_type: F32,
                arg_types: &[F32, F32],
            },
            Opcode::FPMax32 => OpcodeMeta {
                name: "FPMax32",
                return_type: F32,
                arg_types: &[F32, F32],
            },
            Opcode::FPSaturate32 => OpcodeMeta {
                name: "FPSaturate32",
                return_type: F32,
                arg_types: &[F32],
            },
            Opcode::FPClamp32 => OpcodeMeta {
                name: "FPClamp32",
                return_type: F32,
                arg_types: &[F32, F32, F32],
            },
            Opcode::FPRoundEven32 => OpcodeMeta {
                name: "FPRoundEven32",
                return_type: F32,
                arg_types: &[F32],
            },
            Opcode::FPFloor32 => OpcodeMeta {
                name: "FPFloor32",
                return_type: F32,
                arg_types: &[F32],
            },
            Opcode::FPCeil32 => OpcodeMeta {
                name: "FPCeil32",
                return_type: F32,
                arg_types: &[F32],
            },
            Opcode::FPTrunc32 => OpcodeMeta {
                name: "FPTrunc32",
                return_type: F32,
                arg_types: &[F32],
            },
            Opcode::FPRecip32 => OpcodeMeta {
                name: "FPRecip32",
                return_type: F32,
                arg_types: &[F32],
            },
            Opcode::FPRecipSqrt32 => OpcodeMeta {
                name: "FPRecipSqrt32",
                return_type: F32,
                arg_types: &[F32],
            },
            Opcode::FPSqrt32 => OpcodeMeta {
                name: "FPSqrt32",
                return_type: F32,
                arg_types: &[F32],
            },
            Opcode::FPSin => OpcodeMeta {
                name: "FPSin",
                return_type: F32,
                arg_types: &[F32],
            },
            Opcode::FPCos => OpcodeMeta {
                name: "FPCos",
                return_type: F32,
                arg_types: &[F32],
            },
            Opcode::FPExp2 => OpcodeMeta {
                name: "FPExp2",
                return_type: F32,
                arg_types: &[F32],
            },
            Opcode::FPLog2 => OpcodeMeta {
                name: "FPLog2",
                return_type: F32,
                arg_types: &[F32],
            },

            // FP64 arithmetic
            Opcode::FPAbs64 => OpcodeMeta {
                name: "FPAbs64",
                return_type: F64,
                arg_types: &[F64],
            },
            Opcode::FPNeg64 => OpcodeMeta {
                name: "FPNeg64",
                return_type: F64,
                arg_types: &[F64],
            },
            Opcode::FPAdd64 => OpcodeMeta {
                name: "FPAdd64",
                return_type: F64,
                arg_types: &[F64, F64],
            },
            Opcode::FPSub64 => OpcodeMeta {
                name: "FPSub64",
                return_type: F64,
                arg_types: &[F64, F64],
            },
            Opcode::FPMul64 => OpcodeMeta {
                name: "FPMul64",
                return_type: F64,
                arg_types: &[F64, F64],
            },
            Opcode::FPDiv64 => OpcodeMeta {
                name: "FPDiv64",
                return_type: F64,
                arg_types: &[F64, F64],
            },
            Opcode::FPFma64 => OpcodeMeta {
                name: "FPFma64",
                return_type: F64,
                arg_types: &[F64, F64, F64],
            },
            Opcode::FPMin64 => OpcodeMeta {
                name: "FPMin64",
                return_type: F64,
                arg_types: &[F64, F64],
            },
            Opcode::FPMax64 => OpcodeMeta {
                name: "FPMax64",
                return_type: F64,
                arg_types: &[F64, F64],
            },
            Opcode::FPSaturate64 => OpcodeMeta {
                name: "FPSaturate64",
                return_type: F64,
                arg_types: &[F64],
            },
            Opcode::FPClamp64 => OpcodeMeta {
                name: "FPClamp64",
                return_type: F64,
                arg_types: &[F64, F64, F64],
            },
            Opcode::FPRoundEven64 => OpcodeMeta {
                name: "FPRoundEven64",
                return_type: F64,
                arg_types: &[F64],
            },
            Opcode::FPFloor64 => OpcodeMeta {
                name: "FPFloor64",
                return_type: F64,
                arg_types: &[F64],
            },
            Opcode::FPCeil64 => OpcodeMeta {
                name: "FPCeil64",
                return_type: F64,
                arg_types: &[F64],
            },
            Opcode::FPTrunc64 => OpcodeMeta {
                name: "FPTrunc64",
                return_type: F64,
                arg_types: &[F64],
            },
            Opcode::FPRecip64 => OpcodeMeta {
                name: "FPRecip64",
                return_type: F64,
                arg_types: &[F64],
            },
            Opcode::FPRecipSqrt64 => OpcodeMeta {
                name: "FPRecipSqrt64",
                return_type: F64,
                arg_types: &[F64],
            },
            Opcode::FPSqrt64 => OpcodeMeta {
                name: "FPSqrt64",
                return_type: F64,
                arg_types: &[F64],
            },

            // FP ordered comparison
            Opcode::FPOrdEqual16 => OpcodeMeta {
                name: "FPOrdEqual16",
                return_type: U1,
                arg_types: &[F16, F16],
            },
            Opcode::FPOrdNotEqual16 => OpcodeMeta {
                name: "FPOrdNotEqual16",
                return_type: U1,
                arg_types: &[F16, F16],
            },
            Opcode::FPOrdLessThan16 => OpcodeMeta {
                name: "FPOrdLessThan16",
                return_type: U1,
                arg_types: &[F16, F16],
            },
            Opcode::FPOrdGreaterThan16 => OpcodeMeta {
                name: "FPOrdGreaterThan16",
                return_type: U1,
                arg_types: &[F16, F16],
            },
            Opcode::FPOrdLessThanEqual16 => OpcodeMeta {
                name: "FPOrdLessThanEqual16",
                return_type: U1,
                arg_types: &[F16, F16],
            },
            Opcode::FPOrdGreaterThanEqual16 => OpcodeMeta {
                name: "FPOrdGreaterThanEqual16",
                return_type: U1,
                arg_types: &[F16, F16],
            },
            Opcode::FPOrdEqual32 => OpcodeMeta {
                name: "FPOrdEqual32",
                return_type: U1,
                arg_types: &[F32, F32],
            },
            Opcode::FPOrdNotEqual32 => OpcodeMeta {
                name: "FPOrdNotEqual32",
                return_type: U1,
                arg_types: &[F32, F32],
            },
            Opcode::FPOrdLessThan32 => OpcodeMeta {
                name: "FPOrdLessThan32",
                return_type: U1,
                arg_types: &[F32, F32],
            },
            Opcode::FPOrdGreaterThan32 => OpcodeMeta {
                name: "FPOrdGreaterThan32",
                return_type: U1,
                arg_types: &[F32, F32],
            },
            Opcode::FPOrdLessThanEqual32 => OpcodeMeta {
                name: "FPOrdLessThanEqual32",
                return_type: U1,
                arg_types: &[F32, F32],
            },
            Opcode::FPOrdGreaterThanEqual32 => OpcodeMeta {
                name: "FPOrdGreaterThanEqual32",
                return_type: U1,
                arg_types: &[F32, F32],
            },
            Opcode::FPOrdEqual64 => OpcodeMeta {
                name: "FPOrdEqual64",
                return_type: U1,
                arg_types: &[F64, F64],
            },
            Opcode::FPOrdNotEqual64 => OpcodeMeta {
                name: "FPOrdNotEqual64",
                return_type: U1,
                arg_types: &[F64, F64],
            },
            Opcode::FPOrdLessThan64 => OpcodeMeta {
                name: "FPOrdLessThan64",
                return_type: U1,
                arg_types: &[F64, F64],
            },
            Opcode::FPOrdGreaterThan64 => OpcodeMeta {
                name: "FPOrdGreaterThan64",
                return_type: U1,
                arg_types: &[F64, F64],
            },
            Opcode::FPOrdLessThanEqual64 => OpcodeMeta {
                name: "FPOrdLessThanEqual64",
                return_type: U1,
                arg_types: &[F64, F64],
            },
            Opcode::FPOrdGreaterThanEqual64 => OpcodeMeta {
                name: "FPOrdGreaterThanEqual64",
                return_type: U1,
                arg_types: &[F64, F64],
            },

            // FP unordered comparison
            Opcode::FPUnordEqual16 => OpcodeMeta {
                name: "FPUnordEqual16",
                return_type: U1,
                arg_types: &[F16, F16],
            },
            Opcode::FPUnordNotEqual16 => OpcodeMeta {
                name: "FPUnordNotEqual16",
                return_type: U1,
                arg_types: &[F16, F16],
            },
            Opcode::FPUnordLessThan16 => OpcodeMeta {
                name: "FPUnordLessThan16",
                return_type: U1,
                arg_types: &[F16, F16],
            },
            Opcode::FPUnordGreaterThan16 => OpcodeMeta {
                name: "FPUnordGreaterThan16",
                return_type: U1,
                arg_types: &[F16, F16],
            },
            Opcode::FPUnordLessThanEqual16 => OpcodeMeta {
                name: "FPUnordLessThanEqual16",
                return_type: U1,
                arg_types: &[F16, F16],
            },
            Opcode::FPUnordGreaterThanEqual16 => OpcodeMeta {
                name: "FPUnordGreaterThanEqual16",
                return_type: U1,
                arg_types: &[F16, F16],
            },
            Opcode::FPUnordEqual32 => OpcodeMeta {
                name: "FPUnordEqual32",
                return_type: U1,
                arg_types: &[F32, F32],
            },
            Opcode::FPUnordNotEqual32 => OpcodeMeta {
                name: "FPUnordNotEqual32",
                return_type: U1,
                arg_types: &[F32, F32],
            },
            Opcode::FPUnordLessThan32 => OpcodeMeta {
                name: "FPUnordLessThan32",
                return_type: U1,
                arg_types: &[F32, F32],
            },
            Opcode::FPUnordGreaterThan32 => OpcodeMeta {
                name: "FPUnordGreaterThan32",
                return_type: U1,
                arg_types: &[F32, F32],
            },
            Opcode::FPUnordLessThanEqual32 => OpcodeMeta {
                name: "FPUnordLessThanEqual32",
                return_type: U1,
                arg_types: &[F32, F32],
            },
            Opcode::FPUnordGreaterThanEqual32 => OpcodeMeta {
                name: "FPUnordGreaterThanEqual32",
                return_type: U1,
                arg_types: &[F32, F32],
            },
            Opcode::FPUnordEqual64 => OpcodeMeta {
                name: "FPUnordEqual64",
                return_type: U1,
                arg_types: &[F64, F64],
            },
            Opcode::FPUnordNotEqual64 => OpcodeMeta {
                name: "FPUnordNotEqual64",
                return_type: U1,
                arg_types: &[F64, F64],
            },
            Opcode::FPUnordLessThan64 => OpcodeMeta {
                name: "FPUnordLessThan64",
                return_type: U1,
                arg_types: &[F64, F64],
            },
            Opcode::FPUnordGreaterThan64 => OpcodeMeta {
                name: "FPUnordGreaterThan64",
                return_type: U1,
                arg_types: &[F64, F64],
            },
            Opcode::FPUnordLessThanEqual64 => OpcodeMeta {
                name: "FPUnordLessThanEqual64",
                return_type: U1,
                arg_types: &[F64, F64],
            },
            Opcode::FPUnordGreaterThanEqual64 => OpcodeMeta {
                name: "FPUnordGreaterThanEqual64",
                return_type: U1,
                arg_types: &[F64, F64],
            },

            // FP classification
            Opcode::FPIsNan16 => OpcodeMeta {
                name: "FPIsNan16",
                return_type: U1,
                arg_types: &[F16],
            },
            Opcode::FPIsNan32 => OpcodeMeta {
                name: "FPIsNan32",
                return_type: U1,
                arg_types: &[F32],
            },
            Opcode::FPIsNan64 => OpcodeMeta {
                name: "FPIsNan64",
                return_type: U1,
                arg_types: &[F64],
            },

            // Integer arithmetic
            Opcode::IAdd32 => OpcodeMeta {
                name: "IAdd32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::IAdd64 => OpcodeMeta {
                name: "IAdd64",
                return_type: U64,
                arg_types: &[U64, U64],
            },
            Opcode::ISub32 => OpcodeMeta {
                name: "ISub32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::ISub64 => OpcodeMeta {
                name: "ISub64",
                return_type: U64,
                arg_types: &[U64, U64],
            },
            Opcode::IMul32 => OpcodeMeta {
                name: "IMul32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::SDiv32 => OpcodeMeta {
                name: "SDiv32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::UDiv32 => OpcodeMeta {
                name: "UDiv32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::INeg32 => OpcodeMeta {
                name: "INeg32",
                return_type: U32,
                arg_types: &[U32],
            },
            Opcode::INeg64 => OpcodeMeta {
                name: "INeg64",
                return_type: U64,
                arg_types: &[U64],
            },
            Opcode::IAbs32 => OpcodeMeta {
                name: "IAbs32",
                return_type: U32,
                arg_types: &[U32],
            },
            Opcode::IAbs64 => OpcodeMeta {
                name: "IAbs64",
                return_type: U64,
                arg_types: &[U64],
            },
            Opcode::ShiftLeftLogical32 => OpcodeMeta {
                name: "ShiftLeftLogical32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::ShiftLeftLogical64 => OpcodeMeta {
                name: "ShiftLeftLogical64",
                return_type: U64,
                arg_types: &[U64, U32],
            },
            Opcode::ShiftRightLogical32 => OpcodeMeta {
                name: "ShiftRightLogical32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::ShiftRightLogical64 => OpcodeMeta {
                name: "ShiftRightLogical64",
                return_type: U64,
                arg_types: &[U64, U32],
            },
            Opcode::ShiftRightArithmetic32 => OpcodeMeta {
                name: "ShiftRightArithmetic32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::ShiftRightArithmetic64 => OpcodeMeta {
                name: "ShiftRightArithmetic64",
                return_type: U64,
                arg_types: &[U64, U32],
            },
            Opcode::BitwiseAnd32 => OpcodeMeta {
                name: "BitwiseAnd32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::BitwiseOr32 => OpcodeMeta {
                name: "BitwiseOr32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::BitwiseXor32 => OpcodeMeta {
                name: "BitwiseXor32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::BitwiseNot32 => OpcodeMeta {
                name: "BitwiseNot32",
                return_type: U32,
                arg_types: &[U32],
            },
            Opcode::BitFieldInsert => OpcodeMeta {
                name: "BitFieldInsert",
                return_type: U32,
                arg_types: &[U32, U32, U32, U32],
            },
            Opcode::BitFieldSExtract => OpcodeMeta {
                name: "BitFieldSExtract",
                return_type: U32,
                arg_types: &[U32, U32, U32],
            },
            Opcode::BitFieldUExtract => OpcodeMeta {
                name: "BitFieldUExtract",
                return_type: U32,
                arg_types: &[U32, U32, U32],
            },
            Opcode::BitReverse32 => OpcodeMeta {
                name: "BitReverse32",
                return_type: U32,
                arg_types: &[U32],
            },
            Opcode::BitCount32 => OpcodeMeta {
                name: "BitCount32",
                return_type: U32,
                arg_types: &[U32],
            },
            Opcode::FindSMsb32 => OpcodeMeta {
                name: "FindSMsb32",
                return_type: U32,
                arg_types: &[U32],
            },
            Opcode::FindUMsb32 => OpcodeMeta {
                name: "FindUMsb32",
                return_type: U32,
                arg_types: &[U32],
            },

            // Integer min/max
            Opcode::SMin32 => OpcodeMeta {
                name: "SMin32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::UMin32 => OpcodeMeta {
                name: "UMin32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::SMax32 => OpcodeMeta {
                name: "SMax32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::UMax32 => OpcodeMeta {
                name: "UMax32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::SClamp32 => OpcodeMeta {
                name: "SClamp32",
                return_type: U32,
                arg_types: &[U32, U32, U32],
            },
            Opcode::UClamp32 => OpcodeMeta {
                name: "UClamp32",
                return_type: U32,
                arg_types: &[U32, U32, U32],
            },

            // Integer comparison
            Opcode::IEqual => OpcodeMeta {
                name: "IEqual",
                return_type: U1,
                arg_types: &[U32, U32],
            },
            Opcode::INotEqual => OpcodeMeta {
                name: "INotEqual",
                return_type: U1,
                arg_types: &[U32, U32],
            },
            Opcode::SLessThan => OpcodeMeta {
                name: "SLessThan",
                return_type: U1,
                arg_types: &[U32, U32],
            },
            Opcode::ULessThan => OpcodeMeta {
                name: "ULessThan",
                return_type: U1,
                arg_types: &[U32, U32],
            },
            Opcode::SLessThanEqual => OpcodeMeta {
                name: "SLessThanEqual",
                return_type: U1,
                arg_types: &[U32, U32],
            },
            Opcode::ULessThanEqual => OpcodeMeta {
                name: "ULessThanEqual",
                return_type: U1,
                arg_types: &[U32, U32],
            },
            Opcode::SGreaterThan => OpcodeMeta {
                name: "SGreaterThan",
                return_type: U1,
                arg_types: &[U32, U32],
            },
            Opcode::UGreaterThan => OpcodeMeta {
                name: "UGreaterThan",
                return_type: U1,
                arg_types: &[U32, U32],
            },
            Opcode::SGreaterThanEqual => OpcodeMeta {
                name: "SGreaterThanEqual",
                return_type: U1,
                arg_types: &[U32, U32],
            },
            Opcode::UGreaterThanEqual => OpcodeMeta {
                name: "UGreaterThanEqual",
                return_type: U1,
                arg_types: &[U32, U32],
            },

            // Logic
            Opcode::LogicalOr => OpcodeMeta {
                name: "LogicalOr",
                return_type: U1,
                arg_types: &[U1, U1],
            },
            Opcode::LogicalAnd => OpcodeMeta {
                name: "LogicalAnd",
                return_type: U1,
                arg_types: &[U1, U1],
            },
            Opcode::LogicalXor => OpcodeMeta {
                name: "LogicalXor",
                return_type: U1,
                arg_types: &[U1, U1],
            },
            Opcode::LogicalNot => OpcodeMeta {
                name: "LogicalNot",
                return_type: U1,
                arg_types: &[U1],
            },

            // Conversions
            Opcode::ConvertS16F16 => OpcodeMeta {
                name: "ConvertS16F16",
                return_type: U32,
                arg_types: &[F16],
            },
            Opcode::ConvertS16F32 => OpcodeMeta {
                name: "ConvertS16F32",
                return_type: U32,
                arg_types: &[F32],
            },
            Opcode::ConvertS16F64 => OpcodeMeta {
                name: "ConvertS16F64",
                return_type: U32,
                arg_types: &[F64],
            },
            Opcode::ConvertS32F16 => OpcodeMeta {
                name: "ConvertS32F16",
                return_type: U32,
                arg_types: &[F16],
            },
            Opcode::ConvertS32F32 => OpcodeMeta {
                name: "ConvertS32F32",
                return_type: U32,
                arg_types: &[F32],
            },
            Opcode::ConvertS32F64 => OpcodeMeta {
                name: "ConvertS32F64",
                return_type: U32,
                arg_types: &[F64],
            },
            Opcode::ConvertS64F16 => OpcodeMeta {
                name: "ConvertS64F16",
                return_type: U64,
                arg_types: &[F16],
            },
            Opcode::ConvertS64F32 => OpcodeMeta {
                name: "ConvertS64F32",
                return_type: U64,
                arg_types: &[F32],
            },
            Opcode::ConvertS64F64 => OpcodeMeta {
                name: "ConvertS64F64",
                return_type: U64,
                arg_types: &[F64],
            },
            Opcode::ConvertU16F16 => OpcodeMeta {
                name: "ConvertU16F16",
                return_type: U32,
                arg_types: &[F16],
            },
            Opcode::ConvertU16F32 => OpcodeMeta {
                name: "ConvertU16F32",
                return_type: U32,
                arg_types: &[F32],
            },
            Opcode::ConvertU16F64 => OpcodeMeta {
                name: "ConvertU16F64",
                return_type: U32,
                arg_types: &[F64],
            },
            Opcode::ConvertU32F16 => OpcodeMeta {
                name: "ConvertU32F16",
                return_type: U32,
                arg_types: &[F16],
            },
            Opcode::ConvertU32F32 => OpcodeMeta {
                name: "ConvertU32F32",
                return_type: U32,
                arg_types: &[F32],
            },
            Opcode::ConvertU32F64 => OpcodeMeta {
                name: "ConvertU32F64",
                return_type: U32,
                arg_types: &[F64],
            },
            Opcode::ConvertU64F16 => OpcodeMeta {
                name: "ConvertU64F16",
                return_type: U64,
                arg_types: &[F16],
            },
            Opcode::ConvertU64F32 => OpcodeMeta {
                name: "ConvertU64F32",
                return_type: U64,
                arg_types: &[F32],
            },
            Opcode::ConvertU64F64 => OpcodeMeta {
                name: "ConvertU64F64",
                return_type: U64,
                arg_types: &[F64],
            },
            Opcode::ConvertU64U32 => OpcodeMeta {
                name: "ConvertU64U32",
                return_type: U64,
                arg_types: &[U32],
            },
            Opcode::ConvertU32U64 => OpcodeMeta {
                name: "ConvertU32U64",
                return_type: U32,
                arg_types: &[U64],
            },
            Opcode::ConvertF16F32 => OpcodeMeta {
                name: "ConvertF16F32",
                return_type: F16,
                arg_types: &[F32],
            },
            Opcode::ConvertF32F16 => OpcodeMeta {
                name: "ConvertF32F16",
                return_type: F32,
                arg_types: &[F16],
            },
            Opcode::ConvertF32F64 => OpcodeMeta {
                name: "ConvertF32F64",
                return_type: F32,
                arg_types: &[F64],
            },
            Opcode::ConvertF64F32 => OpcodeMeta {
                name: "ConvertF64F32",
                return_type: F64,
                arg_types: &[F32],
            },
            Opcode::ConvertF16S8 => OpcodeMeta {
                name: "ConvertF16S8",
                return_type: F16,
                arg_types: &[U32],
            },
            Opcode::ConvertF16S16 => OpcodeMeta {
                name: "ConvertF16S16",
                return_type: F16,
                arg_types: &[U32],
            },
            Opcode::ConvertF16S32 => OpcodeMeta {
                name: "ConvertF16S32",
                return_type: F16,
                arg_types: &[U32],
            },
            Opcode::ConvertF16S64 => OpcodeMeta {
                name: "ConvertF16S64",
                return_type: F16,
                arg_types: &[U64],
            },
            Opcode::ConvertF16U8 => OpcodeMeta {
                name: "ConvertF16U8",
                return_type: F16,
                arg_types: &[U32],
            },
            Opcode::ConvertF16U16 => OpcodeMeta {
                name: "ConvertF16U16",
                return_type: F16,
                arg_types: &[U32],
            },
            Opcode::ConvertF16U32 => OpcodeMeta {
                name: "ConvertF16U32",
                return_type: F16,
                arg_types: &[U32],
            },
            Opcode::ConvertF16U64 => OpcodeMeta {
                name: "ConvertF16U64",
                return_type: F16,
                arg_types: &[U64],
            },
            Opcode::ConvertF32S8 => OpcodeMeta {
                name: "ConvertF32S8",
                return_type: F32,
                arg_types: &[U32],
            },
            Opcode::ConvertF32S16 => OpcodeMeta {
                name: "ConvertF32S16",
                return_type: F32,
                arg_types: &[U32],
            },
            Opcode::ConvertF32S32 => OpcodeMeta {
                name: "ConvertF32S32",
                return_type: F32,
                arg_types: &[U32],
            },
            Opcode::ConvertF32S64 => OpcodeMeta {
                name: "ConvertF32S64",
                return_type: F32,
                arg_types: &[U64],
            },
            Opcode::ConvertF32U8 => OpcodeMeta {
                name: "ConvertF32U8",
                return_type: F32,
                arg_types: &[U32],
            },
            Opcode::ConvertF32U16 => OpcodeMeta {
                name: "ConvertF32U16",
                return_type: F32,
                arg_types: &[U32],
            },
            Opcode::ConvertF32U32 => OpcodeMeta {
                name: "ConvertF32U32",
                return_type: F32,
                arg_types: &[U32],
            },
            Opcode::ConvertF32U64 => OpcodeMeta {
                name: "ConvertF32U64",
                return_type: F32,
                arg_types: &[U64],
            },
            Opcode::ConvertF64S8 => OpcodeMeta {
                name: "ConvertF64S8",
                return_type: F64,
                arg_types: &[U32],
            },
            Opcode::ConvertF64S16 => OpcodeMeta {
                name: "ConvertF64S16",
                return_type: F64,
                arg_types: &[U32],
            },
            Opcode::ConvertF64S32 => OpcodeMeta {
                name: "ConvertF64S32",
                return_type: F64,
                arg_types: &[U32],
            },
            Opcode::ConvertF64S64 => OpcodeMeta {
                name: "ConvertF64S64",
                return_type: F64,
                arg_types: &[U64],
            },
            Opcode::ConvertF64U8 => OpcodeMeta {
                name: "ConvertF64U8",
                return_type: F64,
                arg_types: &[U32],
            },
            Opcode::ConvertF64U16 => OpcodeMeta {
                name: "ConvertF64U16",
                return_type: F64,
                arg_types: &[U32],
            },
            Opcode::ConvertF64U32 => OpcodeMeta {
                name: "ConvertF64U32",
                return_type: F64,
                arg_types: &[U32],
            },
            Opcode::ConvertF64U64 => OpcodeMeta {
                name: "ConvertF64U64",
                return_type: F64,
                arg_types: &[U64],
            },

            // Texture
            Opcode::ImageSampleImplicitLod => OpcodeMeta {
                name: "ImageSampleImplicitLod",
                return_type: F32x4,
                arg_types: &[Opaque, Opaque, F32, Opaque],
            },
            Opcode::ImageSampleExplicitLod => OpcodeMeta {
                name: "ImageSampleExplicitLod",
                return_type: F32x4,
                arg_types: &[Opaque, Opaque, F32, Opaque],
            },
            Opcode::ImageSampleDrefImplicitLod => OpcodeMeta {
                name: "ImageSampleDrefImplicitLod",
                return_type: F32,
                arg_types: &[Opaque, Opaque, F32, F32, Opaque],
            },
            Opcode::ImageSampleDrefExplicitLod => OpcodeMeta {
                name: "ImageSampleDrefExplicitLod",
                return_type: F32,
                arg_types: &[Opaque, Opaque, F32, F32, Opaque],
            },
            Opcode::ImageFetch => OpcodeMeta {
                name: "ImageFetch",
                return_type: F32x4,
                arg_types: &[Opaque, Opaque, Opaque, U32, U32],
            },
            Opcode::ImageQueryDimensions => OpcodeMeta {
                name: "ImageQueryDimensions",
                return_type: U32x4,
                arg_types: &[Opaque, U32, U1],
            },
            Opcode::ImageGather => OpcodeMeta {
                name: "ImageGather",
                return_type: F32x4,
                arg_types: &[Opaque, Opaque, Opaque, Opaque],
            },
            Opcode::ImageGatherDref => OpcodeMeta {
                name: "ImageGatherDref",
                return_type: F32x4,
                arg_types: &[Opaque, Opaque, Opaque, Opaque, F32],
            },
            Opcode::ImageQueryLod => OpcodeMeta {
                name: "ImageQueryLod",
                return_type: F32x4,
                arg_types: &[Opaque, Opaque],
            },
            Opcode::ImageGradient => OpcodeMeta {
                name: "ImageGradient",
                return_type: F32x4,
                arg_types: &[Opaque, Opaque, Opaque, Opaque, F32],
            },
            Opcode::BoundImageSampleImplicitLod | Opcode::BindlessImageSampleImplicitLod => {
                OpcodeMeta {
                    name: self.texture_pass_opcode_name(),
                    return_type: F32x4,
                    arg_types: &[Opaque, Opaque, F32, Opaque],
                }
            }
            Opcode::BoundImageSampleExplicitLod | Opcode::BindlessImageSampleExplicitLod => {
                OpcodeMeta {
                    name: self.texture_pass_opcode_name(),
                    return_type: F32x4,
                    arg_types: &[Opaque, Opaque, F32, Opaque],
                }
            }
            Opcode::BoundImageSampleDrefImplicitLod
            | Opcode::BindlessImageSampleDrefImplicitLod => OpcodeMeta {
                name: self.texture_pass_opcode_name(),
                return_type: F32,
                arg_types: &[Opaque, Opaque, F32, F32, Opaque],
            },
            Opcode::BoundImageSampleDrefExplicitLod
            | Opcode::BindlessImageSampleDrefExplicitLod => OpcodeMeta {
                name: self.texture_pass_opcode_name(),
                return_type: F32,
                arg_types: &[Opaque, Opaque, F32, F32, Opaque],
            },
            Opcode::BoundImageGather | Opcode::BindlessImageGather => OpcodeMeta {
                name: self.texture_pass_opcode_name(),
                return_type: F32x4,
                arg_types: &[Opaque, Opaque, Opaque, Opaque],
            },
            Opcode::BoundImageGatherDref | Opcode::BindlessImageGatherDref => OpcodeMeta {
                name: self.texture_pass_opcode_name(),
                return_type: F32x4,
                arg_types: &[Opaque, Opaque, Opaque, Opaque, F32],
            },
            Opcode::BoundImageFetch | Opcode::BindlessImageFetch => OpcodeMeta {
                name: self.texture_pass_opcode_name(),
                return_type: F32x4,
                arg_types: &[Opaque, Opaque, Opaque, U32, U32],
            },
            Opcode::BoundImageQueryDimensions | Opcode::BindlessImageQueryDimensions => {
                OpcodeMeta {
                    name: self.texture_pass_opcode_name(),
                    return_type: U32x4,
                    arg_types: &[Opaque, U32, U1],
                }
            }
            Opcode::BoundImageQueryLod | Opcode::BindlessImageQueryLod => OpcodeMeta {
                name: self.texture_pass_opcode_name(),
                return_type: F32x2,
                arg_types: &[Opaque, Opaque],
            },
            Opcode::BoundImageGradient | Opcode::BindlessImageGradient => OpcodeMeta {
                name: self.texture_pass_opcode_name(),
                return_type: F32x4,
                arg_types: &[Opaque, Opaque, Opaque, Opaque, F32],
            },

            // Image load/store
            Opcode::ImageRead => OpcodeMeta {
                name: "ImageRead",
                return_type: U32x4,
                arg_types: &[Opaque, Opaque],
            },
            Opcode::ImageWrite => OpcodeMeta {
                name: "ImageWrite",
                return_type: Type::Void,
                arg_types: &[Opaque, Opaque, U32x4],
            },
            Opcode::BoundImageRead | Opcode::BindlessImageRead => OpcodeMeta {
                name: self.texture_pass_opcode_name(),
                return_type: U32x4,
                arg_types: &[Opaque, Opaque],
            },
            Opcode::BoundImageWrite | Opcode::BindlessImageWrite => OpcodeMeta {
                name: self.texture_pass_opcode_name(),
                return_type: Type::Void,
                arg_types: &[Opaque, Opaque, U32x4],
            },
            Opcode::ImageAtomicIAdd32
            | Opcode::ImageAtomicSMin32
            | Opcode::ImageAtomicUMin32
            | Opcode::ImageAtomicSMax32
            | Opcode::ImageAtomicUMax32
            | Opcode::ImageAtomicInc32
            | Opcode::ImageAtomicDec32
            | Opcode::ImageAtomicAnd32
            | Opcode::ImageAtomicOr32
            | Opcode::ImageAtomicXor32
            | Opcode::ImageAtomicExchange32
            | Opcode::BoundImageAtomicIAdd32
            | Opcode::BindlessImageAtomicIAdd32
            | Opcode::BoundImageAtomicSMin32
            | Opcode::BindlessImageAtomicSMin32
            | Opcode::BoundImageAtomicUMin32
            | Opcode::BindlessImageAtomicUMin32
            | Opcode::BoundImageAtomicSMax32
            | Opcode::BindlessImageAtomicSMax32
            | Opcode::BoundImageAtomicUMax32
            | Opcode::BindlessImageAtomicUMax32
            | Opcode::BoundImageAtomicInc32
            | Opcode::BindlessImageAtomicInc32
            | Opcode::BoundImageAtomicDec32
            | Opcode::BindlessImageAtomicDec32
            | Opcode::BoundImageAtomicAnd32
            | Opcode::BindlessImageAtomicAnd32
            | Opcode::BoundImageAtomicOr32
            | Opcode::BindlessImageAtomicOr32
            | Opcode::BoundImageAtomicXor32
            | Opcode::BindlessImageAtomicXor32
            | Opcode::BoundImageAtomicExchange32
            | Opcode::BindlessImageAtomicExchange32 => OpcodeMeta {
                name: self.texture_pass_opcode_name(),
                return_type: U32,
                arg_types: &[Opaque, Opaque, U32],
            },

            // Atomic global
            Opcode::GlobalAtomicIAdd32 => OpcodeMeta {
                name: "GlobalAtomicIAdd32",
                return_type: U32,
                arg_types: &[U64, U32],
            },
            Opcode::GlobalAtomicSMin32 => OpcodeMeta {
                name: "GlobalAtomicSMin32",
                return_type: U32,
                arg_types: &[U64, U32],
            },
            Opcode::GlobalAtomicUMin32 => OpcodeMeta {
                name: "GlobalAtomicUMin32",
                return_type: U32,
                arg_types: &[U64, U32],
            },
            Opcode::GlobalAtomicSMax32 => OpcodeMeta {
                name: "GlobalAtomicSMax32",
                return_type: U32,
                arg_types: &[U64, U32],
            },
            Opcode::GlobalAtomicUMax32 => OpcodeMeta {
                name: "GlobalAtomicUMax32",
                return_type: U32,
                arg_types: &[U64, U32],
            },
            Opcode::GlobalAtomicInc32 => OpcodeMeta {
                name: "GlobalAtomicInc32",
                return_type: U32,
                arg_types: &[U64, U32],
            },
            Opcode::GlobalAtomicDec32 => OpcodeMeta {
                name: "GlobalAtomicDec32",
                return_type: U32,
                arg_types: &[U64, U32],
            },
            Opcode::GlobalAtomicAnd32 => OpcodeMeta {
                name: "GlobalAtomicAnd32",
                return_type: U32,
                arg_types: &[U64, U32],
            },
            Opcode::GlobalAtomicOr32 => OpcodeMeta {
                name: "GlobalAtomicOr32",
                return_type: U32,
                arg_types: &[U64, U32],
            },
            Opcode::GlobalAtomicXor32 => OpcodeMeta {
                name: "GlobalAtomicXor32",
                return_type: U32,
                arg_types: &[U64, U32],
            },
            Opcode::GlobalAtomicExchange32 => OpcodeMeta {
                name: "GlobalAtomicExchange32",
                return_type: U32,
                arg_types: &[U64, U32],
            },
            Opcode::GlobalAtomicIAdd64 => OpcodeMeta {
                name: "GlobalAtomicIAdd64",
                return_type: U64,
                arg_types: &[U64, U64],
            },
            Opcode::GlobalAtomicSMin64 => OpcodeMeta {
                name: "GlobalAtomicSMin64",
                return_type: U64,
                arg_types: &[U64, U64],
            },
            Opcode::GlobalAtomicUMin64 => OpcodeMeta {
                name: "GlobalAtomicUMin64",
                return_type: U64,
                arg_types: &[U64, U64],
            },
            Opcode::GlobalAtomicSMax64 => OpcodeMeta {
                name: "GlobalAtomicSMax64",
                return_type: U64,
                arg_types: &[U64, U64],
            },
            Opcode::GlobalAtomicUMax64 => OpcodeMeta {
                name: "GlobalAtomicUMax64",
                return_type: U64,
                arg_types: &[U64, U64],
            },
            Opcode::GlobalAtomicAnd64 => OpcodeMeta {
                name: "GlobalAtomicAnd64",
                return_type: U64,
                arg_types: &[U64, U64],
            },
            Opcode::GlobalAtomicOr64 => OpcodeMeta {
                name: "GlobalAtomicOr64",
                return_type: U64,
                arg_types: &[U64, U64],
            },
            Opcode::GlobalAtomicXor64 => OpcodeMeta {
                name: "GlobalAtomicXor64",
                return_type: U64,
                arg_types: &[U64, U64],
            },
            Opcode::GlobalAtomicExchange64 => OpcodeMeta {
                name: "GlobalAtomicExchange64",
                return_type: U64,
                arg_types: &[U64, U64],
            },
            Opcode::GlobalAtomicIAdd32x2 => OpcodeMeta {
                name: "GlobalAtomicIAdd32x2",
                return_type: U32x2,
                arg_types: &[U32x2, U32x2],
            },
            Opcode::GlobalAtomicSMin32x2 => OpcodeMeta {
                name: "GlobalAtomicSMin32x2",
                return_type: U32x2,
                arg_types: &[U32x2, U32x2],
            },
            Opcode::GlobalAtomicUMin32x2 => OpcodeMeta {
                name: "GlobalAtomicUMin32x2",
                return_type: U32x2,
                arg_types: &[U32x2, U32x2],
            },
            Opcode::GlobalAtomicSMax32x2 => OpcodeMeta {
                name: "GlobalAtomicSMax32x2",
                return_type: U32x2,
                arg_types: &[U32x2, U32x2],
            },
            Opcode::GlobalAtomicUMax32x2 => OpcodeMeta {
                name: "GlobalAtomicUMax32x2",
                return_type: U32x2,
                arg_types: &[U32x2, U32x2],
            },
            Opcode::GlobalAtomicAnd32x2 => OpcodeMeta {
                name: "GlobalAtomicAnd32x2",
                return_type: U32x2,
                arg_types: &[U32x2, U32x2],
            },
            Opcode::GlobalAtomicOr32x2 => OpcodeMeta {
                name: "GlobalAtomicOr32x2",
                return_type: U32x2,
                arg_types: &[U32x2, U32x2],
            },
            Opcode::GlobalAtomicXor32x2 => OpcodeMeta {
                name: "GlobalAtomicXor32x2",
                return_type: U32x2,
                arg_types: &[U32x2, U32x2],
            },
            Opcode::GlobalAtomicExchange32x2 => OpcodeMeta {
                name: "GlobalAtomicExchange32x2",
                return_type: U32x2,
                arg_types: &[U32x2, U32x2],
            },
            Opcode::GlobalAtomicAddF32 => OpcodeMeta {
                name: "GlobalAtomicAddF32",
                return_type: F32,
                arg_types: &[U64, F32],
            },
            Opcode::GlobalAtomicAddF16x2 => OpcodeMeta {
                name: "GlobalAtomicAddF16x2",
                return_type: U32,
                arg_types: &[U64, F16x2],
            },
            Opcode::GlobalAtomicAddF32x2 => OpcodeMeta {
                name: "GlobalAtomicAddF32x2",
                return_type: U32,
                arg_types: &[U64, F32x2],
            },
            Opcode::GlobalAtomicMinF16x2 => OpcodeMeta {
                name: "GlobalAtomicMinF16x2",
                return_type: U32,
                arg_types: &[U64, F16x2],
            },
            Opcode::GlobalAtomicMinF32x2 => OpcodeMeta {
                name: "GlobalAtomicMinF32x2",
                return_type: U32,
                arg_types: &[U64, F32x2],
            },
            Opcode::GlobalAtomicMaxF16x2 => OpcodeMeta {
                name: "GlobalAtomicMaxF16x2",
                return_type: U32,
                arg_types: &[U64, F16x2],
            },
            Opcode::GlobalAtomicMaxF32x2 => OpcodeMeta {
                name: "GlobalAtomicMaxF32x2",
                return_type: U32,
                arg_types: &[U64, F32x2],
            },

            // Atomic storage
            Opcode::StorageAtomicIAdd32 => OpcodeMeta {
                name: "StorageAtomicIAdd32",
                return_type: U32,
                arg_types: &[U32, U32, U32],
            },
            Opcode::StorageAtomicSMin32 => OpcodeMeta {
                name: "StorageAtomicSMin32",
                return_type: U32,
                arg_types: &[U32, U32, U32],
            },
            Opcode::StorageAtomicUMin32 => OpcodeMeta {
                name: "StorageAtomicUMin32",
                return_type: U32,
                arg_types: &[U32, U32, U32],
            },
            Opcode::StorageAtomicSMax32 => OpcodeMeta {
                name: "StorageAtomicSMax32",
                return_type: U32,
                arg_types: &[U32, U32, U32],
            },
            Opcode::StorageAtomicUMax32 => OpcodeMeta {
                name: "StorageAtomicUMax32",
                return_type: U32,
                arg_types: &[U32, U32, U32],
            },
            Opcode::StorageAtomicInc32 => OpcodeMeta {
                name: "StorageAtomicInc32",
                return_type: U32,
                arg_types: &[U32, U32, U32],
            },
            Opcode::StorageAtomicDec32 => OpcodeMeta {
                name: "StorageAtomicDec32",
                return_type: U32,
                arg_types: &[U32, U32, U32],
            },
            Opcode::StorageAtomicAnd32 => OpcodeMeta {
                name: "StorageAtomicAnd32",
                return_type: U32,
                arg_types: &[U32, U32, U32],
            },
            Opcode::StorageAtomicOr32 => OpcodeMeta {
                name: "StorageAtomicOr32",
                return_type: U32,
                arg_types: &[U32, U32, U32],
            },
            Opcode::StorageAtomicXor32 => OpcodeMeta {
                name: "StorageAtomicXor32",
                return_type: U32,
                arg_types: &[U32, U32, U32],
            },
            Opcode::StorageAtomicExchange32 => OpcodeMeta {
                name: "StorageAtomicExchange32",
                return_type: U32,
                arg_types: &[U32, U32, U32],
            },
            Opcode::StorageAtomicIAdd64 => OpcodeMeta {
                name: "StorageAtomicIAdd64",
                return_type: U64,
                arg_types: &[U32, U32, U64],
            },
            Opcode::StorageAtomicSMin64 => OpcodeMeta {
                name: "StorageAtomicSMin64",
                return_type: U64,
                arg_types: &[U32, U32, U64],
            },
            Opcode::StorageAtomicUMin64 => OpcodeMeta {
                name: "StorageAtomicUMin64",
                return_type: U64,
                arg_types: &[U32, U32, U64],
            },
            Opcode::StorageAtomicSMax64 => OpcodeMeta {
                name: "StorageAtomicSMax64",
                return_type: U64,
                arg_types: &[U32, U32, U64],
            },
            Opcode::StorageAtomicUMax64 => OpcodeMeta {
                name: "StorageAtomicUMax64",
                return_type: U64,
                arg_types: &[U32, U32, U64],
            },
            Opcode::StorageAtomicAnd64 => OpcodeMeta {
                name: "StorageAtomicAnd64",
                return_type: U64,
                arg_types: &[U32, U32, U64],
            },
            Opcode::StorageAtomicOr64 => OpcodeMeta {
                name: "StorageAtomicOr64",
                return_type: U64,
                arg_types: &[U32, U32, U64],
            },
            Opcode::StorageAtomicXor64 => OpcodeMeta {
                name: "StorageAtomicXor64",
                return_type: U64,
                arg_types: &[U32, U32, U64],
            },
            Opcode::StorageAtomicExchange64 => OpcodeMeta {
                name: "StorageAtomicExchange64",
                return_type: U64,
                arg_types: &[U32, U32, U64],
            },
            Opcode::StorageAtomicIAdd32x2 => OpcodeMeta {
                name: "StorageAtomicIAdd32x2",
                return_type: U32x2,
                arg_types: &[U32, U32, U32x2],
            },
            Opcode::StorageAtomicSMin32x2 => OpcodeMeta {
                name: "StorageAtomicSMin32x2",
                return_type: U32x2,
                arg_types: &[U32, U32, U32x2],
            },
            Opcode::StorageAtomicUMin32x2 => OpcodeMeta {
                name: "StorageAtomicUMin32x2",
                return_type: U32x2,
                arg_types: &[U32, U32, U32x2],
            },
            Opcode::StorageAtomicSMax32x2 => OpcodeMeta {
                name: "StorageAtomicSMax32x2",
                return_type: U32x2,
                arg_types: &[U32, U32, U32x2],
            },
            Opcode::StorageAtomicUMax32x2 => OpcodeMeta {
                name: "StorageAtomicUMax32x2",
                return_type: U32x2,
                arg_types: &[U32, U32, U32x2],
            },
            Opcode::StorageAtomicAnd32x2 => OpcodeMeta {
                name: "StorageAtomicAnd32x2",
                return_type: U32x2,
                arg_types: &[U32, U32, U32x2],
            },
            Opcode::StorageAtomicOr32x2 => OpcodeMeta {
                name: "StorageAtomicOr32x2",
                return_type: U32x2,
                arg_types: &[U32, U32, U32x2],
            },
            Opcode::StorageAtomicXor32x2 => OpcodeMeta {
                name: "StorageAtomicXor32x2",
                return_type: U32x2,
                arg_types: &[U32, U32, U32x2],
            },
            Opcode::StorageAtomicExchange32x2 => OpcodeMeta {
                name: "StorageAtomicExchange32x2",
                return_type: U32x2,
                arg_types: &[U32, U32, U32x2],
            },
            Opcode::StorageAtomicAddF32 => OpcodeMeta {
                name: "StorageAtomicAddF32",
                return_type: F32,
                arg_types: &[U32, U32, F32],
            },
            Opcode::StorageAtomicAddF16x2 => OpcodeMeta {
                name: "StorageAtomicAddF16x2",
                return_type: U32,
                arg_types: &[U32, U32, F16x2],
            },
            Opcode::StorageAtomicAddF32x2 => OpcodeMeta {
                name: "StorageAtomicAddF32x2",
                return_type: U32,
                arg_types: &[U32, U32, F32x2],
            },
            Opcode::StorageAtomicMinF16x2 => OpcodeMeta {
                name: "StorageAtomicMinF16x2",
                return_type: U32,
                arg_types: &[U32, U32, F16x2],
            },
            Opcode::StorageAtomicMinF32x2 => OpcodeMeta {
                name: "StorageAtomicMinF32x2",
                return_type: U32,
                arg_types: &[U32, U32, F32x2],
            },
            Opcode::StorageAtomicMaxF16x2 => OpcodeMeta {
                name: "StorageAtomicMaxF16x2",
                return_type: U32,
                arg_types: &[U32, U32, F16x2],
            },
            Opcode::StorageAtomicMaxF32x2 => OpcodeMeta {
                name: "StorageAtomicMaxF32x2",
                return_type: U32,
                arg_types: &[U32, U32, F32x2],
            },

            // Atomic shared
            Opcode::SharedAtomicIAdd32 => OpcodeMeta {
                name: "SharedAtomicIAdd32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::SharedAtomicSMin32 => OpcodeMeta {
                name: "SharedAtomicSMin32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::SharedAtomicUMin32 => OpcodeMeta {
                name: "SharedAtomicUMin32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::SharedAtomicSMax32 => OpcodeMeta {
                name: "SharedAtomicSMax32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::SharedAtomicUMax32 => OpcodeMeta {
                name: "SharedAtomicUMax32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::SharedAtomicInc32 => OpcodeMeta {
                name: "SharedAtomicInc32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::SharedAtomicDec32 => OpcodeMeta {
                name: "SharedAtomicDec32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::SharedAtomicAnd32 => OpcodeMeta {
                name: "SharedAtomicAnd32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::SharedAtomicOr32 => OpcodeMeta {
                name: "SharedAtomicOr32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::SharedAtomicXor32 => OpcodeMeta {
                name: "SharedAtomicXor32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::SharedAtomicExchange32 => OpcodeMeta {
                name: "SharedAtomicExchange32",
                return_type: U32,
                arg_types: &[U32, U32],
            },
            Opcode::SharedAtomicExchange64 => OpcodeMeta {
                name: "SharedAtomicExchange64",
                return_type: U64,
                arg_types: &[U32, U64],
            },
            Opcode::SharedAtomicExchange32x2 => OpcodeMeta {
                name: "SharedAtomicExchange32x2",
                return_type: U32x2,
                arg_types: &[U32, U32x2],
            },

            // Warp / Subgroup
            Opcode::VoteAll => OpcodeMeta {
                name: "VoteAll",
                return_type: U1,
                arg_types: &[U1],
            },
            Opcode::VoteAny => OpcodeMeta {
                name: "VoteAny",
                return_type: U1,
                arg_types: &[U1],
            },
            Opcode::VoteEqual => OpcodeMeta {
                name: "VoteEqual",
                return_type: U1,
                arg_types: &[U1],
            },
            Opcode::SubgroupBallot => OpcodeMeta {
                name: "SubgroupBallot",
                return_type: U32,
                arg_types: &[U1],
            },
            Opcode::LaneId => OpcodeMeta {
                name: "LaneId",
                return_type: U32,
                arg_types: &[],
            },
            Opcode::SubgroupEqMask => OpcodeMeta {
                name: "SubgroupEqMask",
                return_type: U32,
                arg_types: &[],
            },
            Opcode::SubgroupLtMask => OpcodeMeta {
                name: "SubgroupLtMask",
                return_type: U32,
                arg_types: &[],
            },
            Opcode::SubgroupLeMask => OpcodeMeta {
                name: "SubgroupLeMask",
                return_type: U32,
                arg_types: &[],
            },
            Opcode::SubgroupGtMask => OpcodeMeta {
                name: "SubgroupGtMask",
                return_type: U32,
                arg_types: &[],
            },
            Opcode::SubgroupGeMask => OpcodeMeta {
                name: "SubgroupGeMask",
                return_type: U32,
                arg_types: &[],
            },
            Opcode::ShuffleIndex => OpcodeMeta {
                name: "ShuffleIndex",
                return_type: U32,
                arg_types: &[U32, U32, U32, U32],
            },
            Opcode::ShuffleUp => OpcodeMeta {
                name: "ShuffleUp",
                return_type: U32,
                arg_types: &[U32, U32, U32, U32],
            },
            Opcode::ShuffleDown => OpcodeMeta {
                name: "ShuffleDown",
                return_type: U32,
                arg_types: &[U32, U32, U32, U32],
            },
            Opcode::ShuffleButterfly => OpcodeMeta {
                name: "ShuffleButterfly",
                return_type: U32,
                arg_types: &[U32, U32, U32, U32],
            },
            Opcode::FSwizzleAdd => OpcodeMeta {
                name: "FSwizzleAdd",
                return_type: F32,
                arg_types: &[F32, F32, U32],
            },
            Opcode::DPdxFine => OpcodeMeta {
                name: "DPdxFine",
                return_type: F32,
                arg_types: &[F32],
            },
            Opcode::DPdyFine => OpcodeMeta {
                name: "DPdyFine",
                return_type: F32,
                arg_types: &[F32],
            },
            Opcode::DPdxCoarse => OpcodeMeta {
                name: "DPdxCoarse",
                return_type: F32,
                arg_types: &[F32],
            },
            Opcode::DPdyCoarse => OpcodeMeta {
                name: "DPdyCoarse",
                return_type: F32,
                arg_types: &[F32],
            },

            // Branch / control flow
            Opcode::Branch => OpcodeMeta {
                name: "Branch",
                return_type: Type::Void,
                arg_types: &[],
            },
            Opcode::BranchConditional => OpcodeMeta {
                name: "BranchConditional",
                return_type: Type::Void,
                arg_types: &[U1],
            },
            Opcode::LoopMerge => OpcodeMeta {
                name: "LoopMerge",
                return_type: Type::Void,
                arg_types: &[],
            },
            Opcode::SelectionMerge => OpcodeMeta {
                name: "SelectionMerge",
                return_type: Type::Void,
                arg_types: &[],
            },
            Opcode::Return => OpcodeMeta {
                name: "Return",
                return_type: Type::Void,
                arg_types: &[],
            },
            Opcode::Unreachable => OpcodeMeta {
                name: "Unreachable",
                return_type: Type::Void,
                arg_types: &[],
            },
        }
    }
}

impl fmt::Display for Opcode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[cfg(test)]
mod tests {
    use crate::ir::types::Type;

    use super::Opcode;

    #[test]
    fn register_ssa_ops_are_not_side_effects() {
        assert!(!Opcode::SetRegister.may_have_side_effects());
        assert!(!Opcode::SetPred.may_have_side_effects());
        assert!(!Opcode::SetZFlag.may_have_side_effects());
        assert!(!Opcode::GetRegister.may_have_side_effects());
    }

    #[test]
    fn guest_visible_outputs_remain_side_effects() {
        assert!(Opcode::SetAttribute.may_have_side_effects());
        assert!(Opcode::SetFragColor.may_have_side_effects());
        assert!(Opcode::WriteGlobal32.may_have_side_effects());
        assert!(Opcode::Barrier.may_have_side_effects());
    }

    #[test]
    fn image_query_lod_matches_the_upstream_four_component_result() {
        assert_eq!(Opcode::ImageQueryLod.meta().return_type, Type::F32x4);
    }
}

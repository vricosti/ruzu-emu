// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Main GLASM emission entry point.
//!
//! Maps to upstream `backend/glasm/emit_glasm.cpp`.
//! Walks the IR program and dispatches to per-opcode emit functions.

use crate::ir;
use crate::ir::opcodes::Opcode;

use super::glasm_emit_context::EmitContext;

/// Walk an IR program and emit GLASM instructions.
///
/// Matches the main loop in upstream `EmitGLASM`.
pub fn emit_program(ctx: &mut EmitContext, program: &ir::Program) {
    for block in &program.blocks {
        for inst in block.iter() {
            emit_inst(ctx, inst);
        }
    }
    // Append program end
    ctx.add_line("END;");
}

/// Emit a single IR instruction as GLASM.
fn emit_inst(ctx: &mut EmitContext, inst: &ir::instruction::Inst) {
    match inst.opcode {
        Opcode::GlobalAtomicCompareExchange32
        | Opcode::GlobalAtomicCompareExchange64
        | Opcode::StorageAtomicCompareExchange32
        | Opcode::StorageAtomicCompareExchange64 => {
            std::panic::panic_any(crate::exception::NotImplementedException::new(
                "GLASM compare-exchange",
            ));
        }
        // Arithmetic - float
        Opcode::FPAdd32 => ctx.add_line("ADD.F RC.x,RC.x,RC.y;"),
        Opcode::FPMul32 => ctx.add_line("MUL.F RC.x,RC.x,RC.y;"),
        Opcode::FPFma32 => ctx.add_line("MAD.F RC.x,RC.x,RC.y,RC.z;"),
        Opcode::FPMax32 => ctx.add_line("MAX.F RC.x,RC.x,RC.y;"),
        Opcode::FPMin32 => ctx.add_line("MIN.F RC.x,RC.x,RC.y;"),
        Opcode::FPNeg32 => ctx.add_line("MOV.F RC.x,-RC.x;"),
        Opcode::FPAbs32 => ctx.add_line("MOV.F RC.x,|RC.x|;"),
        Opcode::FPSaturate32 => ctx.add_line("MOV.F.SAT RC.x,RC.x;"),
        Opcode::FPRoundEven32 => ctx.add_line("ROUND.F RC.x,RC.x;"),
        Opcode::FPFloor32 => ctx.add_line("FLR.F RC.x,RC.x;"),
        Opcode::FPCeil32 => ctx.add_line("CEIL.F RC.x,RC.x;"),
        Opcode::FPTrunc32 => ctx.add_line("TRUNC.F RC.x,RC.x;"),
        Opcode::FPSin => ctx.add_line("SIN RC.x,RC.x;"),
        Opcode::FPCos => ctx.add_line("COS RC.x,RC.x;"),
        Opcode::FPExp2 => ctx.add_line("EX2 RC.x,RC.x;"),
        Opcode::FPLog2 => ctx.add_line("LG2 RC.x,RC.x;"),
        Opcode::FPRecip32 => ctx.add_line("RCP RC.x,RC.x;"),
        Opcode::FPRecipSqrt32 => ctx.add_line("RSQ RC.x,RC.x;"),
        Opcode::FPSqrt32 => {
            ctx.add_line("RSQ RC.x,RC.x;");
            ctx.add_line("RCP RC.x,RC.x;");
        }

        // Arithmetic - integer
        Opcode::IAdd32 => ctx.add_line("ADD.S RC.x,RC.x,RC.y;"),
        Opcode::ISub32 => ctx.add_line("SUB.S RC.x,RC.x,RC.y;"),
        Opcode::IMul32 => ctx.add_line("MUL.S RC.x,RC.x,RC.y;"),
        Opcode::SDiv32 => ctx.add_line("DIV.S RC.x,RC.x,RC.y;"),
        Opcode::UDiv32 => ctx.add_line("DIV.U RC.x,RC.x,RC.y;"),
        Opcode::INeg32 => ctx.add_line("MOV.S RC.x,-RC.x;"),
        Opcode::IAbs32 => ctx.add_line("ABS.S RC.x,RC.x;"),
        Opcode::ShiftLeftLogical32 => ctx.add_line("SHL.U RC.x,RC.x,RC.y;"),
        Opcode::ShiftRightLogical32 => ctx.add_line("SHR.U RC.x,RC.x,RC.y;"),
        Opcode::ShiftRightArithmetic32 => ctx.add_line("SHR.S RC.x,RC.x,RC.y;"),
        Opcode::BitwiseAnd32 => ctx.add_line("AND.S RC.x,RC.x,RC.y;"),
        Opcode::BitwiseOr32 => ctx.add_line("OR.S RC.x,RC.x,RC.y;"),
        Opcode::BitwiseXor32 => ctx.add_line("XOR.S RC.x,RC.x,RC.y;"),
        Opcode::BitwiseNot32 => ctx.add_line("NOT.S RC.x,RC.x;"),
        Opcode::BitReverse32 => ctx.add_line("BFR RC.x,RC.x;"),
        Opcode::BitCount32 => ctx.add_line("BTC RC.x,RC.x;"),
        Opcode::SMin32 => ctx.add_line("MIN.S RC.x,RC.x,RC.y;"),
        Opcode::UMin32 => ctx.add_line("MIN.U RC.x,RC.x,RC.y;"),
        Opcode::SMax32 => ctx.add_line("MAX.S RC.x,RC.x,RC.y;"),
        Opcode::UMax32 => ctx.add_line("MAX.U RC.x,RC.x,RC.y;"),
        Opcode::SClamp32 => {
            ctx.add_line("MIN.S RC.x,RC.x,RC.z;");
            ctx.add_line("MAX.S RC.x,RC.x,RC.y;");
        }
        Opcode::UClamp32 => {
            ctx.add_line("MIN.U RC.x,RC.x,RC.z;");
            ctx.add_line("MAX.U RC.x,RC.x,RC.y;");
        }

        // Comparisons - float
        Opcode::FPOrdLessThan32 => ctx.add_line("SLT.F RC.x,RC.x,RC.y;"),
        Opcode::FPOrdEqual32 => ctx.add_line("SEQ.F RC.x,RC.x,RC.y;"),
        Opcode::FPOrdLessThanEqual32 => ctx.add_line("SLE.F RC.x,RC.x,RC.y;"),
        Opcode::FPOrdGreaterThan32 => ctx.add_line("SGT.F RC.x,RC.x,RC.y;"),
        Opcode::FPOrdGreaterThanEqual32 => ctx.add_line("SGE.F RC.x,RC.x,RC.y;"),
        Opcode::FPOrdNotEqual32 => ctx.add_line("SNE.F RC.x,RC.x,RC.y;"),
        Opcode::FPUnordLessThan32 => ctx.add_line("SLT.F RC.x,RC.x,RC.y;"),
        Opcode::FPUnordEqual32 => ctx.add_line("SEQ.F RC.x,RC.x,RC.y;"),
        Opcode::FPUnordLessThanEqual32 => ctx.add_line("SLE.F RC.x,RC.x,RC.y;"),
        Opcode::FPUnordGreaterThan32 => ctx.add_line("SGT.F RC.x,RC.x,RC.y;"),
        Opcode::FPUnordGreaterThanEqual32 => ctx.add_line("SGE.F RC.x,RC.x,RC.y;"),
        Opcode::FPUnordNotEqual32 => ctx.add_line("SNE.F RC.x,RC.x,RC.y;"),
        Opcode::FPIsNan32 => ctx.add_line("SNE.F RC.x,RC.x,RC.x;"),

        // Comparisons - integer
        Opcode::SLessThan => ctx.add_line("SLT.S RC.x,RC.x,RC.y;"),
        Opcode::ULessThan => ctx.add_line("SLT.U RC.x,RC.x,RC.y;"),
        Opcode::IEqual => ctx.add_line("SEQ.S RC.x,RC.x,RC.y;"),
        Opcode::SLessThanEqual => ctx.add_line("SLE.S RC.x,RC.x,RC.y;"),
        Opcode::ULessThanEqual => ctx.add_line("SLE.U RC.x,RC.x,RC.y;"),
        Opcode::SGreaterThan => ctx.add_line("SGT.S RC.x,RC.x,RC.y;"),
        Opcode::UGreaterThan => ctx.add_line("SGT.U RC.x,RC.x,RC.y;"),
        Opcode::INotEqual => ctx.add_line("SNE.U RC.x,RC.x,RC.y;"),
        Opcode::SGreaterThanEqual => ctx.add_line("SGE.S RC.x,RC.x,RC.y;"),
        Opcode::UGreaterThanEqual => ctx.add_line("SGE.U RC.x,RC.x,RC.y;"),

        // Logical
        Opcode::LogicalOr => ctx.add_line("OR.S RC.x,RC.x,RC.y;"),
        Opcode::LogicalAnd => ctx.add_line("AND.S RC.x,RC.x,RC.y;"),
        Opcode::LogicalXor => ctx.add_line("XOR.S RC.x,RC.x,RC.y;"),
        Opcode::LogicalNot => ctx.add_line("SEQ.S RC.x,RC.x,0;"),

        // Select
        Opcode::SelectU32 | Opcode::SelectF32 => ctx.add_line("CMP.S RC.x,RC.x,RC.y,RC.z;"),

        // Undefined
        Opcode::UndefU1 | Opcode::UndefU8 | Opcode::UndefU16 | Opcode::UndefU32 => {
            ctx.add_line("MOV.S RC.x,0;")
        }

        // Barriers
        Opcode::Barrier => ctx.add_line("BAR;"),
        Opcode::WorkgroupMemoryBarrier => ctx.add_line("MEMBAR.CTA;"),
        Opcode::DeviceMemoryBarrier => ctx.add_line("MEMBAR;"),

        // Control flow
        Opcode::DemoteToHelperInvocation => ctx.add_line("KIL TR.x;"),

        // Bitwise conversion (identity in GLASM)
        Opcode::BitCastU32F32
        | Opcode::BitCastF32U32
        | Opcode::BitCastU16F16
        | Opcode::BitCastF16U16
        | Opcode::BitCastU64F64
        | Opcode::BitCastF64U64
        | Opcode::Identity => {
            // No-op in GLASM: registers are untyped
        }

        // Pack/unpack
        Opcode::PackHalf2x16 => ctx.add_line("PK2H RC.x,RC;"),
        Opcode::UnpackHalf2x16 => ctx.add_line("UP2H RC.xy,RC.x;"),

        // Shared memory
        Opcode::LoadSharedU8 => ctx.add_line("LDS.U8 RC,shared_mem[RC.x];"),
        Opcode::LoadSharedS8 => ctx.add_line("LDS.S8 RC,shared_mem[RC.x];"),
        Opcode::LoadSharedU16 => ctx.add_line("LDS.U16 RC,shared_mem[RC.x];"),
        Opcode::LoadSharedS16 => ctx.add_line("LDS.S16 RC,shared_mem[RC.x];"),
        Opcode::LoadSharedU32 => ctx.add_line("LDS.U32 RC,shared_mem[RC.x];"),
        Opcode::LoadSharedU64 => ctx.add_line("LDS.U32X2 RC,shared_mem[RC.x];"),
        Opcode::LoadSharedU128 => ctx.add_line("LDS.U32X4 RC,shared_mem[RC.x];"),
        Opcode::WriteSharedU8 => ctx.add_line("STS.U8 RC.y,shared_mem[RC.x];"),
        Opcode::WriteSharedU16 => ctx.add_line("STS.U16 RC.y,shared_mem[RC.x];"),
        Opcode::WriteSharedU32 => ctx.add_line("STS.U32 RC.y,shared_mem[RC.x];"),
        Opcode::WriteSharedU64 => ctx.add_line("STS.U32X2 RC,shared_mem[RC.x];"),
        Opcode::WriteSharedU128 => ctx.add_line("STS.U32X4 RC,shared_mem[RC.x];"),

        // Warp operations
        Opcode::InvocationId => {
            let stage_name = ctx.stage_name;
            ctx.add_fmt(format!("MOV.S RC.x,{}.threadid;", stage_name));
        }
        Opcode::SRWScaleFactorXY => {
            super::emit_glasm_context_get_set::emit_sr_w_scale_factor_xy(ctx);
        }
        Opcode::SRWScaleFactorZ => {
            super::emit_glasm_context_get_set::emit_sr_w_scale_factor_z(ctx);
        }
        Opcode::VoteAll => ctx.add_line("TGALL.S RC.x,RC.x;"),
        Opcode::VoteAny => ctx.add_line("TGANY.S RC.x,RC.x;"),
        Opcode::VoteEqual => ctx.add_line("TGEQ.S RC.x,RC.x;"),
        Opcode::SubgroupBallot => ctx.add_line("TGBALLOT RC.x,RC.x;"),
        Opcode::SubgroupEqMask => {
            let sn = ctx.stage_name;
            ctx.add_fmt(format!("MOV.U RC,{}.threadeqmask;", sn));
        }
        Opcode::SubgroupLtMask => {
            let sn = ctx.stage_name;
            ctx.add_fmt(format!("MOV.U RC,{}.threadltmask;", sn));
        }
        Opcode::SubgroupLeMask => {
            let sn = ctx.stage_name;
            ctx.add_fmt(format!("MOV.U RC,{}.threadlemask;", sn));
        }
        Opcode::SubgroupGtMask => {
            let sn = ctx.stage_name;
            ctx.add_fmt(format!("MOV.U RC,{}.threadgtmask;", sn));
        }
        Opcode::SubgroupGeMask => {
            let sn = ctx.stage_name;
            ctx.add_fmt(format!("MOV.U RC,{}.threadgemask;", sn));
        }

        // Special
        Opcode::Prologue | Opcode::Epilogue | Opcode::Void | Opcode::Phi | Opcode::PhiMove => {
            // No-op or handled separately
        }

        Opcode::EmitVertex => ctx.add_line("EMIT;"),
        Opcode::EndPrimitive => ctx.add_line("ENDPRIM;"),

        // Attributes and context
        Opcode::GetAttribute | Opcode::SetAttribute | Opcode::GetCbufU32 | Opcode::GetCbufF32 => {
            // Complex operations handled through context_get_set
            ctx.add_line(&format!(
                "; {} (complex, context-dependent)",
                inst.opcode.name()
            ));
        }

        // Conversion
        Opcode::ConvertF32S32 => ctx.add_line("CVT.F32.S32 RC.x,RC.x;"),
        Opcode::ConvertF32U32 => ctx.add_line("CVT.F32.U32 RC.x,RC.x;"),
        Opcode::ConvertS32F32 => ctx.add_line("CVT.S32.F32 RC.x,RC.x;"),
        Opcode::ConvertU32F32 => ctx.add_line("CVT.U32.F32 RC.x,RC.x;"),
        Opcode::ConvertF32F64 => ctx.add_line("CVT.F32.F64 RC.x,RC.x;"),
        Opcode::ConvertF64F32 => ctx.add_line("CVT.F64.F32 RC.x,RC.x;"),

        // Image/texture operations
        Opcode::ImageRead
        | Opcode::ImageWrite
        | Opcode::ImageFetch
        | Opcode::ImageQueryDimensions
        | Opcode::ImageSampleImplicitLod
        | Opcode::ImageSampleExplicitLod
        | Opcode::ImageSampleDrefImplicitLod
        | Opcode::ImageSampleDrefExplicitLod
        | Opcode::ImageGather
        | Opcode::ImageGatherDref => {
            ctx.add_line(&format!("; {} (image op, complex)", inst.opcode.name()));
        }
        Opcode::IsTextureScaled => {
            super::emit_glasm_image::emit_is_texture_scaled(ctx, inst);
        }
        Opcode::IsImageScaled => {
            super::emit_glasm_image::emit_is_image_scaled(ctx, inst);
        }

        // Memory operations
        Opcode::LoadGlobalU8
        | Opcode::LoadGlobalS8
        | Opcode::LoadGlobalU16
        | Opcode::LoadGlobalS16
        | Opcode::LoadGlobal32
        | Opcode::LoadGlobal64
        | Opcode::LoadGlobal128
        | Opcode::WriteGlobalU8
        | Opcode::WriteGlobalS8
        | Opcode::WriteGlobalU16
        | Opcode::WriteGlobalS16
        | Opcode::WriteGlobal32
        | Opcode::WriteGlobal64
        | Opcode::WriteGlobal128
        | Opcode::LoadStorageU8
        | Opcode::LoadStorageS8
        | Opcode::LoadStorageU16
        | Opcode::LoadStorageS16
        | Opcode::LoadStorage32
        | Opcode::LoadStorage64
        | Opcode::LoadStorage128
        | Opcode::WriteStorageU8
        | Opcode::WriteStorageS8
        | Opcode::WriteStorageU16
        | Opcode::WriteStorageS16
        | Opcode::WriteStorage32
        | Opcode::WriteStorage64
        | Opcode::WriteStorage128 => {
            ctx.add_line(&format!("; {} (memory op, complex)", inst.opcode.name()));
        }

        // Composite operations
        Opcode::CompositeConstructU32x2
        | Opcode::CompositeConstructU32x3
        | Opcode::CompositeConstructU32x4
        | Opcode::CompositeConstructF32x2
        | Opcode::CompositeConstructF32x3
        | Opcode::CompositeConstructF32x4
        | Opcode::CompositeExtractU32x2
        | Opcode::CompositeExtractU32x3
        | Opcode::CompositeExtractU32x4
        | Opcode::CompositeExtractF32x2
        | Opcode::CompositeExtractF32x3
        | Opcode::CompositeExtractF32x4
        | Opcode::CompositeInsertF32x2
        | Opcode::CompositeInsertF32x3
        | Opcode::CompositeInsertF32x4 => {
            ctx.add_line(&format!("; {} (composite, complex)", inst.opcode.name()));
        }

        Opcode::CompositeInsertF64x2
        | Opcode::CompositeInsertF64x3
        | Opcode::CompositeInsertF64x4 => {
            panic!(
                "{:?} not implemented in GLASM backend (upstream NotImplemented)",
                inst.opcode
            )
        }

        // Not-implemented / fallback
        _ => {
            ctx.add_line(&format!("; {} (not implemented)", inst.opcode.name()));
        }
    }
}

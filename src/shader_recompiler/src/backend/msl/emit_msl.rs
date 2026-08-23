// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Main native-MSL emission entry point.
//!
//! The file boundary follows Eden's `backend/glsl/emit_glsl.{h,cpp}`. MSL is
//! a new target backend, so there is no upstream MSL source to mirror.

use crate::backend::bindings::Bindings;
use crate::ir;
use crate::ir::opcodes::Opcode;
use crate::ir::program::SyntaxNode;
use crate::ir::value::{InstRef, Value};
use crate::profile::Profile;
use crate::runtime_info::RuntimeInfo;

use super::emit_msl_atomic;
use super::emit_msl_barriers;
use super::emit_msl_bitwise_conversion;
use super::emit_msl_composite;
use super::emit_msl_context_get_set;
use super::emit_msl_convert;
use super::emit_msl_floating_point;
use super::emit_msl_image;
use super::emit_msl_image_atomic;
use super::emit_msl_integer;
use super::emit_msl_logical;
use super::emit_msl_memory;
use super::emit_msl_select;
use super::emit_msl_shared_memory;
use super::msl_emit_context::MslEmitContext;
use super::{MslError, MslOptions, MslShaderArtifact};

fn varying_mask_has_only_position(mask: &[u64; 8]) -> bool {
    mask.iter().enumerate().all(|(word_index, word)| {
        let allowed = if word_index == 0 {
            (0b1111u64) << 28
        } else {
            0
        };
        word & !allowed == 0
    })
}

fn first_unsupported_program_feature(
    program: &ir::Program,
    profile: &Profile,
) -> Option<&'static str> {
    let info = &program.info;

    if program.shared_memory_size != 0 && program.stage != crate::stage::Stage::Compute {
        return Some("shared memory outside a compute shader");
    }
    if (info.uses_workgroup_id || info.uses_local_invocation_id)
        && program.stage != crate::stage::Stage::Compute
    {
        return Some("compute built-in outside a compute shader");
    }
    if (info.uses_sample_id || info.uses_is_helper_invocation)
        && program.stage != crate::stage::Stage::Fragment
    {
        return Some("fragment built-in outside a fragment shader");
    }
    if program.stage != crate::stage::Stage::Compute && program.workgroup_size != [1, 1, 1] {
        return Some("workgroup size");
    }
    if program.output_vertices != 0 || program.invocations != 1 {
        return Some("geometry execution modes");
    }
    if program.is_geometry_passthrough {
        return Some("geometry passthrough");
    }
    let supported_vertex_position_stores = program.stage == crate::stage::Stage::VertexB
        && varying_mask_has_only_position(&info.stores.mask);
    let supported_fragment_colors = program.stage == crate::stage::Stage::Fragment;
    if info.loads.mask.iter().any(|word| *word != 0)
        || (!supported_vertex_position_stores && info.stores.mask.iter().any(|word| *word != 0))
        || info.passthrough.mask.iter().any(|word| *word != 0)
        || info.loads_indexed_attributes
        || info.stores_indexed_attributes
        || (!supported_fragment_colors && info.stores_frag_color.iter().any(|store| *store))
        || info.stores_sample_mask
        || info.stores_frag_depth
        || info.stores_tess_level_outer
        || info.stores_tess_level_inner
        || !info.legacy_stores_mapping.is_empty()
    {
        return Some("stage inputs or outputs");
    }
    if !info.storage_buffers_descriptors.is_empty() && profile.support_descriptor_aliasing {
        return Some("descriptor-aliasing storage buffers");
    }
    if !info.texture_buffer_descriptors.is_empty() || !info.image_buffer_descriptors.is_empty() {
        return Some("resource bindings");
    }
    if !info.constant_buffer_descriptors.is_empty() && profile.support_descriptor_aliasing {
        return Some("descriptor-aliasing constant buffers");
    }
    if info.uses_patches.iter().any(|used| *used) {
        return Some("tessellation patches");
    }
    if info.stores_global_memory
        || info.uses_global_memory
        || info.uses_global_increment
        || info.uses_global_decrement
        || info.uses_atomic_f32_add
        || info.uses_atomic_f16x2_add
        || info.uses_atomic_f16x2_min
        || info.uses_atomic_f16x2_max
        || info.uses_atomic_f32x2_add
        || info.uses_atomic_f32x2_min
        || info.uses_atomic_f32x2_max
        || info.uses_int64_bit_atomics
    {
        return Some("memory operations");
    }
    if info.uses_invocation_id
        || info.uses_invocation_info
        || info.uses_subgroup_invocation_id
        || info.uses_subgroup_shuffles
        || info.uses_subgroup_vote
        || info.uses_subgroup_mask
        || info.requires_layer_emulation
        || info.emulated_layer != 0
        || info.used_clip_distances != 0
    {
        return Some("stage built-ins");
    }
    if info.uses_fp64 {
        return Some("64-bit floating point");
    }
    if info.uses_int64 && !profile.support_int64 {
        return Some("64-bit integers on the selected Metal device");
    }
    if info.uses_fp32_denorms_flush
        || info.uses_fp32_denorms_preserve
        || info.uses_image_1d
        || info.uses_sparse_residency
        || info.uses_demote_to_helper_invocation
        || info.uses_fswzadd
        || info.uses_derivatives
        || info.uses_typeless_image_reads
        || info.uses_typeless_image_writes
        || info.uses_image_buffers
        || info.uses_rescaling_uniform
        || info.uses_cbuf_indirect
        || info.uses_render_area
    {
        return Some("shader capabilities");
    }
    None
}

fn immediate_u32(inst: &crate::ir::instruction::Inst, arg: u32) -> Result<u32, MslError> {
    match inst.arg(arg as usize) {
        Value::ImmU32(value) => Ok(*value),
        _ => Err(MslError::ExpectedImmediate {
            opcode: inst.opcode,
            arg,
            expected: "u32",
        }),
    }
}

fn emit_inst(
    context: &mut MslEmitContext,
    program: &ir::Program,
    inst_ref: InstRef,
) -> Result<(), MslError> {
    let inst = program.block(inst_ref.block).inst(inst_ref.inst);
    match inst.opcode {
        Opcode::Void | Opcode::Prologue | Opcode::Epilogue => Ok(()),
        Opcode::GetZeroFromOp
        | Opcode::GetSignFromOp
        | Opcode::GetCarryFromOp
        | Opcode::GetOverflowFromOp
            if context.is_defined(inst_ref) =>
        {
            Ok(())
        }
        Opcode::Identity => context.emit_identity(program, inst_ref, inst),
        Opcode::CompositeConstructU32x2
        | Opcode::CompositeConstructU32x3
        | Opcode::CompositeConstructU32x4
        | Opcode::CompositeConstructF16x2
        | Opcode::CompositeConstructF16x3
        | Opcode::CompositeConstructF16x4
        | Opcode::CompositeConstructF32x2
        | Opcode::CompositeConstructF32x3
        | Opcode::CompositeConstructF32x4 => {
            emit_msl_composite::emit_construct(context, inst_ref, inst)
        }
        Opcode::CompositeExtractU32x2
        | Opcode::CompositeExtractU32x3
        | Opcode::CompositeExtractU32x4
        | Opcode::CompositeExtractF16x2
        | Opcode::CompositeExtractF16x3
        | Opcode::CompositeExtractF16x4
        | Opcode::CompositeExtractF32x2
        | Opcode::CompositeExtractF32x3
        | Opcode::CompositeExtractF32x4 => {
            emit_msl_composite::emit_extract(context, inst_ref, inst)
        }
        Opcode::CompositeInsertU32x2
        | Opcode::CompositeInsertU32x3
        | Opcode::CompositeInsertU32x4
        | Opcode::CompositeInsertF16x2
        | Opcode::CompositeInsertF16x3
        | Opcode::CompositeInsertF16x4
        | Opcode::CompositeInsertF32x2
        | Opcode::CompositeInsertF32x3
        | Opcode::CompositeInsertF32x4 => emit_msl_composite::emit_insert(context, inst_ref, inst),
        Opcode::SelectU1 => emit_msl_select::emit_select(context, inst_ref, inst, ir::Type::U1),
        Opcode::SelectU32 => emit_msl_select::emit_select(context, inst_ref, inst, ir::Type::U32),
        Opcode::SelectU64 => emit_msl_select::emit_select(context, inst_ref, inst, ir::Type::U64),
        Opcode::SelectF16 => emit_msl_select::emit_select(context, inst_ref, inst, ir::Type::F16),
        Opcode::SelectF32 => emit_msl_select::emit_select(context, inst_ref, inst, ir::Type::F32),
        Opcode::BitCastU32F32 => emit_msl_bitwise_conversion::emit_bitcast(
            context,
            inst_ref,
            inst,
            ir::Type::U32,
            "uint",
        ),
        Opcode::BitCastF32U32 => emit_msl_bitwise_conversion::emit_bitcast(
            context,
            inst_ref,
            inst,
            ir::Type::F32,
            "float",
        ),
        Opcode::LogicalOr => emit_msl_logical::emit_binary(context, inst_ref, inst, "||"),
        Opcode::LogicalAnd => emit_msl_logical::emit_binary(context, inst_ref, inst, "&&"),
        Opcode::LogicalXor => emit_msl_logical::emit_binary(context, inst_ref, inst, "!="),
        Opcode::LogicalNot => emit_msl_logical::emit_not(context, inst_ref, inst),
        Opcode::IAdd32 => emit_msl_integer::emit_iadd_32(context, program, inst_ref, inst),
        Opcode::IAdd64 => emit_msl_integer::emit_binary_64(context, program, inst_ref, inst, "+"),
        Opcode::ISub32 => emit_msl_integer::emit_isub_32(context, program, inst_ref, inst),
        Opcode::ISub64 => emit_msl_integer::emit_binary_64(context, program, inst_ref, inst, "-"),
        Opcode::IMul32 => emit_msl_integer::emit_imul_32(context, program, inst_ref, inst),
        Opcode::SDiv32 => emit_msl_integer::emit_sdiv_32(context, inst_ref, inst),
        Opcode::UDiv32 => emit_msl_integer::emit_udiv_32(context, program, inst_ref, inst),
        Opcode::INeg32 => emit_msl_integer::emit_ineg_32(context, inst_ref, inst),
        Opcode::INeg64 => emit_msl_integer::emit_ineg_64(context, inst_ref, inst),
        Opcode::IAbs32 => emit_msl_integer::emit_iabs_32(context, inst_ref, inst),
        Opcode::IAbs64 => emit_msl_integer::emit_iabs_64(context, inst_ref, inst),
        Opcode::ShiftLeftLogical32 => {
            emit_msl_integer::emit_binary(context, program, inst_ref, inst, "<<")
        }
        Opcode::ShiftLeftLogical64 => {
            emit_msl_integer::emit_binary_64(context, program, inst_ref, inst, "<<")
        }
        Opcode::ShiftRightLogical32 => {
            emit_msl_integer::emit_binary(context, program, inst_ref, inst, ">>")
        }
        Opcode::ShiftRightLogical64 => {
            emit_msl_integer::emit_binary_64(context, program, inst_ref, inst, ">>")
        }
        Opcode::ShiftRightArithmetic32 => {
            emit_msl_integer::emit_shift_right_arithmetic_32(context, inst_ref, inst)
        }
        Opcode::ShiftRightArithmetic64 => {
            emit_msl_integer::emit_shift_right_arithmetic_64(context, inst_ref, inst)
        }
        Opcode::BitwiseAnd32 => {
            emit_msl_integer::emit_bitwise_with_flags(context, program, inst_ref, inst, "&")
        }
        Opcode::BitwiseOr32 => {
            emit_msl_integer::emit_bitwise_with_flags(context, program, inst_ref, inst, "|")
        }
        Opcode::BitwiseXor32 => {
            emit_msl_integer::emit_bitwise_with_flags(context, program, inst_ref, inst, "^")
        }
        Opcode::BitwiseNot32 => emit_msl_integer::emit_not_32(context, inst_ref, inst),
        Opcode::BitFieldInsert => emit_msl_integer::emit_bit_field_insert(context, inst_ref, inst),
        Opcode::BitFieldSExtract => {
            emit_msl_integer::emit_bit_field_extract(context, inst_ref, inst, true)
        }
        Opcode::BitFieldUExtract => {
            emit_msl_integer::emit_bit_field_extract(context, inst_ref, inst, false)
        }
        Opcode::BitReverse32 => {
            emit_msl_integer::emit_unary_intrinsic_32(context, inst_ref, inst, "reverse_bits")
        }
        Opcode::BitCount32 => {
            emit_msl_integer::emit_unary_intrinsic_32(context, inst_ref, inst, "popcount")
        }
        Opcode::FindSMsb32 => emit_msl_integer::emit_find_msb_32(context, inst_ref, inst, true),
        Opcode::FindUMsb32 => emit_msl_integer::emit_find_msb_32(context, inst_ref, inst, false),
        Opcode::SMin32 => emit_msl_integer::emit_min_max(context, inst_ref, inst, "min", true),
        Opcode::UMin32 => emit_msl_integer::emit_min_max(context, inst_ref, inst, "min", false),
        Opcode::SMax32 => emit_msl_integer::emit_min_max(context, inst_ref, inst, "max", true),
        Opcode::UMax32 => emit_msl_integer::emit_min_max(context, inst_ref, inst, "max", false),
        Opcode::SClamp32 => emit_msl_integer::emit_clamp(context, inst_ref, inst, true),
        Opcode::UClamp32 => emit_msl_integer::emit_clamp(context, inst_ref, inst, false),
        Opcode::IEqual => emit_msl_integer::emit_comparison(context, inst_ref, inst, "==", false),
        Opcode::INotEqual => {
            emit_msl_integer::emit_comparison(context, inst_ref, inst, "!=", false)
        }
        Opcode::SLessThan => emit_msl_integer::emit_comparison(context, inst_ref, inst, "<", true),
        Opcode::ULessThan => emit_msl_integer::emit_comparison(context, inst_ref, inst, "<", false),
        Opcode::SLessThanEqual => {
            emit_msl_integer::emit_comparison(context, inst_ref, inst, "<=", true)
        }
        Opcode::ULessThanEqual => {
            emit_msl_integer::emit_comparison(context, inst_ref, inst, "<=", false)
        }
        Opcode::SGreaterThan => {
            emit_msl_integer::emit_comparison(context, inst_ref, inst, ">", true)
        }
        Opcode::UGreaterThan => {
            emit_msl_integer::emit_comparison(context, inst_ref, inst, ">", false)
        }
        Opcode::SGreaterThanEqual => {
            emit_msl_integer::emit_comparison(context, inst_ref, inst, ">=", true)
        }
        Opcode::UGreaterThanEqual => {
            emit_msl_integer::emit_comparison(context, inst_ref, inst, ">=", false)
        }
        Opcode::FPAbs16 => {
            emit_msl_floating_point::emit_intrinsic_16(context, inst_ref, inst, "fabs")
        }
        Opcode::FPNeg16 => {
            emit_msl_floating_point::emit_unary_operator_16(context, inst_ref, inst, "-")
        }
        Opcode::FPAdd16 => {
            emit_msl_floating_point::emit_fp_add_16(context, program, inst_ref, inst)
        }
        Opcode::FPMul16 => {
            emit_msl_floating_point::emit_fp_mul_16(context, program, inst_ref, inst)
        }
        Opcode::FPFma16 => emit_msl_floating_point::emit_fp_fma_16(context, inst_ref, inst),
        Opcode::FPMin16 => {
            emit_msl_floating_point::emit_intrinsic_16(context, inst_ref, inst, "min")
        }
        Opcode::FPMax16 => {
            emit_msl_floating_point::emit_intrinsic_16(context, inst_ref, inst, "max")
        }
        Opcode::FPSaturate16 => {
            emit_msl_floating_point::emit_intrinsic_16(context, inst_ref, inst, "saturate")
        }
        Opcode::FPClamp16 => {
            emit_msl_floating_point::emit_intrinsic_16(context, inst_ref, inst, "clamp")
        }
        Opcode::FPRoundEven16 => {
            emit_msl_floating_point::emit_intrinsic_16(context, inst_ref, inst, "rint")
        }
        Opcode::FPFloor16 => {
            emit_msl_floating_point::emit_intrinsic_16(context, inst_ref, inst, "floor")
        }
        Opcode::FPCeil16 => {
            emit_msl_floating_point::emit_intrinsic_16(context, inst_ref, inst, "ceil")
        }
        Opcode::FPTrunc16 => {
            emit_msl_floating_point::emit_intrinsic_16(context, inst_ref, inst, "trunc")
        }
        Opcode::FPAdd32 => {
            emit_msl_floating_point::emit_fp_add_32(context, program, inst_ref, inst)
        }
        Opcode::FPSub32 => {
            emit_msl_floating_point::emit_binary_operator_32(context, program, inst_ref, inst, "-")
        }
        Opcode::FPMul32 => {
            emit_msl_floating_point::emit_fp_mul_32(context, program, inst_ref, inst)
        }
        Opcode::FPDiv32 => {
            emit_msl_floating_point::emit_binary_operator_32(context, program, inst_ref, inst, "/")
        }
        Opcode::FPFma32 => emit_msl_floating_point::emit_fp_fma_32(context, inst_ref, inst),
        Opcode::FPNeg32 => {
            emit_msl_floating_point::emit_unary_operator_32(context, inst_ref, inst, "-")
        }
        Opcode::FPAbs32 => {
            emit_msl_floating_point::emit_intrinsic_32(context, inst_ref, inst, "fabs")
        }
        Opcode::FPSaturate32 => {
            emit_msl_floating_point::emit_intrinsic_32(context, inst_ref, inst, "saturate")
        }
        Opcode::FPClamp32 => {
            emit_msl_floating_point::emit_intrinsic_32(context, inst_ref, inst, "clamp")
        }
        Opcode::FPMin32 => {
            emit_msl_floating_point::emit_intrinsic_32(context, inst_ref, inst, "min")
        }
        Opcode::FPMax32 => {
            emit_msl_floating_point::emit_intrinsic_32(context, inst_ref, inst, "max")
        }
        Opcode::FPRoundEven32 => {
            emit_msl_floating_point::emit_intrinsic_32(context, inst_ref, inst, "rint")
        }
        Opcode::FPFloor32 => {
            emit_msl_floating_point::emit_intrinsic_32(context, inst_ref, inst, "floor")
        }
        Opcode::FPCeil32 => {
            emit_msl_floating_point::emit_intrinsic_32(context, inst_ref, inst, "ceil")
        }
        Opcode::FPTrunc32 => {
            emit_msl_floating_point::emit_intrinsic_32(context, inst_ref, inst, "trunc")
        }
        Opcode::FPRecip32 => emit_msl_floating_point::emit_recip_32(context, inst_ref, inst),
        Opcode::FPRecipSqrt32 => {
            emit_msl_floating_point::emit_intrinsic_32(context, inst_ref, inst, "rsqrt")
        }
        Opcode::FPSqrt32 => {
            emit_msl_floating_point::emit_intrinsic_32(context, inst_ref, inst, "sqrt")
        }
        Opcode::FPSin => emit_msl_floating_point::emit_intrinsic_32(context, inst_ref, inst, "sin"),
        Opcode::FPCos => emit_msl_floating_point::emit_intrinsic_32(context, inst_ref, inst, "cos"),
        Opcode::FPExp2 => {
            emit_msl_floating_point::emit_intrinsic_32(context, inst_ref, inst, "exp2")
        }
        Opcode::FPLog2 => {
            emit_msl_floating_point::emit_intrinsic_32(context, inst_ref, inst, "log2")
        }
        Opcode::FPOrdEqual32 => {
            emit_msl_floating_point::emit_ordered_comparison_32(context, inst_ref, inst, "==")
        }
        Opcode::FPOrdNotEqual32 => {
            emit_msl_floating_point::emit_ordered_comparison_32(context, inst_ref, inst, "!=")
        }
        Opcode::FPOrdLessThan32 => {
            emit_msl_floating_point::emit_ordered_comparison_32(context, inst_ref, inst, "<")
        }
        Opcode::FPOrdGreaterThan32 => {
            emit_msl_floating_point::emit_ordered_comparison_32(context, inst_ref, inst, ">")
        }
        Opcode::FPOrdLessThanEqual32 => {
            emit_msl_floating_point::emit_ordered_comparison_32(context, inst_ref, inst, "<=")
        }
        Opcode::FPOrdGreaterThanEqual32 => {
            emit_msl_floating_point::emit_ordered_comparison_32(context, inst_ref, inst, ">=")
        }
        Opcode::FPUnordEqual32 => {
            emit_msl_floating_point::emit_unordered_comparison_32(context, inst_ref, inst, "==")
        }
        Opcode::FPUnordNotEqual32 => {
            emit_msl_floating_point::emit_unordered_comparison_32(context, inst_ref, inst, "!=")
        }
        Opcode::FPUnordLessThan32 => {
            emit_msl_floating_point::emit_unordered_comparison_32(context, inst_ref, inst, "<")
        }
        Opcode::FPUnordGreaterThan32 => {
            emit_msl_floating_point::emit_unordered_comparison_32(context, inst_ref, inst, ">")
        }
        Opcode::FPUnordLessThanEqual32 => {
            emit_msl_floating_point::emit_unordered_comparison_32(context, inst_ref, inst, "<=")
        }
        Opcode::FPUnordGreaterThanEqual32 => {
            emit_msl_floating_point::emit_unordered_comparison_32(context, inst_ref, inst, ">=")
        }
        Opcode::FPIsNan32 => emit_msl_floating_point::emit_is_nan_32(context, inst_ref, inst),
        Opcode::FPOrdEqual16 => {
            emit_msl_floating_point::emit_ordered_comparison_16(context, inst_ref, inst, "==")
        }
        Opcode::FPOrdNotEqual16 => {
            emit_msl_floating_point::emit_ordered_comparison_16(context, inst_ref, inst, "!=")
        }
        Opcode::FPOrdLessThan16 => {
            emit_msl_floating_point::emit_ordered_comparison_16(context, inst_ref, inst, "<")
        }
        Opcode::FPOrdGreaterThan16 => {
            emit_msl_floating_point::emit_ordered_comparison_16(context, inst_ref, inst, ">")
        }
        Opcode::FPOrdLessThanEqual16 => {
            emit_msl_floating_point::emit_ordered_comparison_16(context, inst_ref, inst, "<=")
        }
        Opcode::FPOrdGreaterThanEqual16 => {
            emit_msl_floating_point::emit_ordered_comparison_16(context, inst_ref, inst, ">=")
        }
        Opcode::FPUnordEqual16 => {
            emit_msl_floating_point::emit_unordered_comparison_16(context, inst_ref, inst, "==")
        }
        Opcode::FPUnordNotEqual16 => {
            emit_msl_floating_point::emit_unordered_comparison_16(context, inst_ref, inst, "!=")
        }
        Opcode::FPUnordLessThan16 => {
            emit_msl_floating_point::emit_unordered_comparison_16(context, inst_ref, inst, "<")
        }
        Opcode::FPUnordGreaterThan16 => {
            emit_msl_floating_point::emit_unordered_comparison_16(context, inst_ref, inst, ">")
        }
        Opcode::FPUnordLessThanEqual16 => {
            emit_msl_floating_point::emit_unordered_comparison_16(context, inst_ref, inst, "<=")
        }
        Opcode::FPUnordGreaterThanEqual16 => {
            emit_msl_floating_point::emit_unordered_comparison_16(context, inst_ref, inst, ">=")
        }
        Opcode::FPIsNan16 => emit_msl_floating_point::emit_is_nan_16(context, inst_ref, inst),
        Opcode::ConvertS16F16 => emit_msl_convert::emit_convert_s16_f16(context, inst_ref, inst),
        Opcode::ConvertS32F16 => emit_msl_convert::emit_convert_s32_f16(context, inst_ref, inst),
        Opcode::ConvertS64F16 | Opcode::ConvertS64F32 => {
            emit_msl_convert::emit_convert_s64_float(context, inst_ref, inst)
        }
        Opcode::ConvertS32F32 => emit_msl_convert::emit_convert_s32_f32(context, inst_ref, inst),
        Opcode::ConvertU16F16 => emit_msl_convert::emit_convert_u16_f16(context, inst_ref, inst),
        Opcode::ConvertU32F16 => emit_msl_convert::emit_convert_u32_f16(context, inst_ref, inst),
        Opcode::ConvertU64F16 | Opcode::ConvertU64F32 => {
            emit_msl_convert::emit_convert_u64_float(context, inst_ref, inst)
        }
        Opcode::ConvertU32F32 => emit_msl_convert::emit_convert_u32_f32(context, inst_ref, inst),
        Opcode::ConvertU64U32 => emit_msl_convert::emit_convert_u64_u32(context, inst_ref, inst),
        Opcode::ConvertU32U64 => emit_msl_convert::emit_convert_u32_u64(context, inst_ref, inst),
        Opcode::ConvertF16F32 => emit_msl_convert::emit_convert_f16_f32(context, inst_ref, inst),
        Opcode::ConvertF32F16 => emit_msl_convert::emit_convert_f32_f16(context, inst_ref, inst),
        Opcode::ConvertF16S8 => {
            emit_msl_convert::emit_convert_f16_signed(context, inst_ref, inst, 8)
        }
        Opcode::ConvertF16S16 => {
            emit_msl_convert::emit_convert_f16_signed(context, inst_ref, inst, 16)
        }
        Opcode::ConvertF16S32 => {
            emit_msl_convert::emit_convert_f16_signed(context, inst_ref, inst, 32)
        }
        Opcode::ConvertF16S64 => {
            emit_msl_convert::emit_convert_f16_signed(context, inst_ref, inst, 64)
        }
        Opcode::ConvertF16U8 => {
            emit_msl_convert::emit_convert_f16_unsigned(context, inst_ref, inst, 8)
        }
        Opcode::ConvertF16U16 => {
            emit_msl_convert::emit_convert_f16_unsigned(context, inst_ref, inst, 16)
        }
        Opcode::ConvertF16U32 => {
            emit_msl_convert::emit_convert_f16_unsigned(context, inst_ref, inst, 32)
        }
        Opcode::ConvertF16U64 => {
            emit_msl_convert::emit_convert_f16_unsigned(context, inst_ref, inst, 64)
        }
        Opcode::ConvertF32S32 => emit_msl_convert::emit_convert_f32_s32(context, inst_ref, inst),
        Opcode::ConvertF32S64 => emit_msl_convert::emit_convert_f32_s64(context, inst_ref, inst),
        Opcode::ConvertF32U32 => emit_msl_convert::emit_convert_f32_u32(context, inst_ref, inst),
        Opcode::ConvertF32U64 => emit_msl_convert::emit_convert_f32_u64(context, inst_ref, inst),
        Opcode::GetCbufU8
        | Opcode::GetCbufS8
        | Opcode::GetCbufU16
        | Opcode::GetCbufS16
        | Opcode::GetCbufU32
        | Opcode::GetCbufF32
        | Opcode::GetCbufU32x2 => emit_msl_context_get_set::emit_get_cbuf(context, inst_ref, inst),
        Opcode::WorkgroupId => emit_msl_context_get_set::emit_workgroup_id(context, inst_ref),
        Opcode::LocalInvocationId => {
            emit_msl_context_get_set::emit_local_invocation_id(context, inst_ref)
        }
        Opcode::SampleId => emit_msl_context_get_set::emit_sample_id(context, inst_ref),
        Opcode::IsHelperInvocation => {
            emit_msl_context_get_set::emit_is_helper_invocation(context, inst_ref)
        }
        Opcode::LoadLocal => emit_msl_context_get_set::emit_load_local(context, inst_ref, inst),
        Opcode::WriteLocal => emit_msl_context_get_set::emit_write_local(context, inst_ref, inst),
        Opcode::LoadStorageU8
        | Opcode::LoadStorageS8
        | Opcode::LoadStorageU16
        | Opcode::LoadStorageS16
        | Opcode::LoadStorage32
        | Opcode::LoadStorage64
        | Opcode::LoadStorage128 => emit_msl_memory::emit_load_storage(context, inst_ref, inst),
        Opcode::WriteStorageU8
        | Opcode::WriteStorageS8
        | Opcode::WriteStorageU16
        | Opcode::WriteStorageS16
        | Opcode::WriteStorage32
        | Opcode::WriteStorage64
        | Opcode::WriteStorage128 => emit_msl_memory::emit_write_storage(context, inst_ref, inst),
        Opcode::LoadSharedU8
        | Opcode::LoadSharedS8
        | Opcode::LoadSharedU16
        | Opcode::LoadSharedS16
        | Opcode::LoadSharedU32
        | Opcode::LoadSharedU64
        | Opcode::LoadSharedU128 => {
            emit_msl_shared_memory::emit_load_shared(context, inst_ref, inst)
        }
        Opcode::WriteSharedU8
        | Opcode::WriteSharedU16
        | Opcode::WriteSharedU32
        | Opcode::WriteSharedU64
        | Opcode::WriteSharedU128 => {
            emit_msl_shared_memory::emit_write_shared(context, inst_ref, inst)
        }
        Opcode::Barrier => emit_msl_barriers::emit_barrier(context),
        Opcode::SharedAtomicIAdd32
        | Opcode::SharedAtomicSMin32
        | Opcode::SharedAtomicUMin32
        | Opcode::SharedAtomicSMax32
        | Opcode::SharedAtomicUMax32
        | Opcode::SharedAtomicInc32
        | Opcode::SharedAtomicDec32
        | Opcode::SharedAtomicAnd32
        | Opcode::SharedAtomicOr32
        | Opcode::SharedAtomicXor32
        | Opcode::SharedAtomicExchange32 => {
            emit_msl_atomic::emit_shared_atomic(context, inst_ref, inst)
        }
        Opcode::StorageAtomicIAdd32
        | Opcode::StorageAtomicSMin32
        | Opcode::StorageAtomicUMin32
        | Opcode::StorageAtomicSMax32
        | Opcode::StorageAtomicUMax32
        | Opcode::StorageAtomicInc32
        | Opcode::StorageAtomicDec32
        | Opcode::StorageAtomicAnd32
        | Opcode::StorageAtomicOr32
        | Opcode::StorageAtomicXor32
        | Opcode::StorageAtomicExchange32 => {
            emit_msl_atomic::emit_storage_atomic(context, inst_ref, inst)
        }
        Opcode::ImageSampleImplicitLod | Opcode::ImageSampleExplicitLod => {
            emit_msl_image::emit_image_sample(context, inst_ref, inst)
        }
        Opcode::ImageSampleDrefImplicitLod | Opcode::ImageSampleDrefExplicitLod => {
            emit_msl_image::emit_image_sample_dref(context, inst_ref, inst)
        }
        Opcode::ImageGather | Opcode::ImageGatherDref => {
            emit_msl_image::emit_image_gather(context, program, inst_ref, inst)
        }
        Opcode::ImageFetch => emit_msl_image::emit_image_fetch(context, inst_ref, inst),
        Opcode::ImageQueryDimensions => {
            emit_msl_image::emit_image_query_dimensions(context, inst_ref, inst)
        }
        Opcode::ImageQueryLod => emit_msl_image::emit_image_query_lod(context, inst_ref, inst),
        Opcode::ImageGradient => {
            emit_msl_image::emit_image_gradient(context, program, inst_ref, inst)
        }
        Opcode::ImageRead => emit_msl_image::emit_image_read(context, program, inst_ref, inst),
        Opcode::ImageWrite => emit_msl_image::emit_image_write(context, inst_ref, inst),
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
        | Opcode::ImageAtomicExchange32 => {
            emit_msl_image_atomic::emit_image_atomic(context, inst_ref, inst)
        }
        Opcode::SetAttribute => {
            let Value::Attribute(attribute) = inst.arg(0) else {
                return Err(MslError::ExpectedImmediate {
                    opcode: inst.opcode,
                    arg: 0,
                    expected: "attribute",
                });
            };
            if immediate_u32(inst, 2)? != 0 {
                return Err(MslError::UnsupportedProgramFeature(
                    "per-vertex output indexing",
                ));
            }
            if !attribute.is_position() {
                return Err(MslError::UnsupportedAttribute(attribute.0));
            }
            context.emit_set_position(inst_ref, attribute.position_element(), inst.arg(1))
        }
        Opcode::SetFragColor => {
            let render_target = immediate_u32(inst, 0)?;
            let component = immediate_u32(inst, 1)?;
            if render_target >= 8 || component >= 4 {
                return Err(MslError::UnsupportedProgramFeature("fragment output index"));
            }
            context.emit_set_frag_color(inst_ref, render_target, component, inst.arg(2))
        }
        opcode => Err(MslError::UnsupportedOpcode {
            block: inst_ref.block,
            inst: inst_ref.inst,
            opcode,
        }),
    }
}

fn emit_block(
    context: &mut MslEmitContext,
    program: &ir::Program,
    block_index: u32,
) -> Result<(), MslError> {
    for (inst_index, _) in program.block(block_index).indexed_iter() {
        emit_inst(
            context,
            program,
            InstRef {
                block: block_index,
                inst: inst_index,
            },
        )?;
    }
    Ok(())
}

/// Emit native MSL directly from the backend-neutral shader IR.
///
/// The initial supported language is intentionally exact: empty vertex and
/// fragment programs are valid, while every unported instruction is reported
/// as an error. Callers must never substitute a fallback shader for this
/// error.
pub fn emit_msl(
    program: &ir::Program,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
) -> Result<MslShaderArtifact, MslError> {
    let mut bindings = Bindings::default();
    emit_msl_with_options_and_bindings(
        program,
        profile,
        runtime_info,
        &MslOptions::default(),
        &mut bindings,
    )
}

pub fn emit_msl_with_options(
    program: &ir::Program,
    profile: &Profile,
    _runtime_info: &RuntimeInfo,
    options: &MslOptions,
) -> Result<MslShaderArtifact, MslError> {
    let mut bindings = Bindings::default();
    emit_msl_with_options_and_bindings(program, profile, _runtime_info, options, &mut bindings)
}

pub fn emit_msl_with_bindings(
    program: &ir::Program,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
    bindings: &mut Bindings,
) -> Result<MslShaderArtifact, MslError> {
    emit_msl_with_options_and_bindings(
        program,
        profile,
        runtime_info,
        &MslOptions::default(),
        bindings,
    )
}

pub fn emit_msl_with_options_and_bindings(
    program: &ir::Program,
    profile: &Profile,
    _runtime_info: &RuntimeInfo,
    options: &MslOptions,
    bindings: &mut Bindings,
) -> Result<MslShaderArtifact, MslError> {
    if let Some(feature) = first_unsupported_program_feature(program, profile) {
        return Err(MslError::UnsupportedProgramFeature(feature));
    }
    let mut context = MslEmitContext::new(program, profile, options, bindings)?;
    if program.syntax_list.is_empty() {
        for block_index in 0..program.blocks.len() as u32 {
            emit_block(&mut context, program, block_index)?;
        }
    } else {
        for node in &program.syntax_list {
            match node {
                SyntaxNode::Block(block_index) => emit_block(&mut context, program, *block_index)?,
                SyntaxNode::Return => {}
                _ => {
                    return Err(MslError::UnsupportedProgramFeature(
                        "structured control flow",
                    ))
                }
            }
        }
    }
    Ok(context.finish())
}

#[cfg(test)]
mod tests {
    use crate::backend::msl::{MslResourceKind, MslVersion};
    use crate::ir::basic_block::Block;
    use crate::ir::emitter::Emitter;
    use crate::ir::opcodes::Opcode;
    use crate::ir::types::TextureInstInfo;
    use crate::ir::value::Value;
    use crate::profile::Profile;
    use crate::runtime_info::RuntimeInfo;
    use crate::shader_info::{ImageDescriptor, ImageFormat, TextureDescriptor, TextureType};
    use crate::stage::Stage;

    use super::*;

    fn empty_program(stage: Stage) -> ir::Program {
        let mut program = ir::Program::new(stage);
        program.blocks.push(Block::new());
        program
    }

    fn sampled_texture_program(count: u32, explicit_lod: bool) -> ir::Program {
        let mut program = empty_program(Stage::Fragment);
        program.info.texture_descriptors.push(TextureDescriptor {
            texture_type: TextureType::Color2D,
            is_depth: false,
            is_multisample: false,
            is_integer: false,
            has_secondary: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            shift_left: 0,
            secondary_cbuf_index: 0,
            secondary_cbuf_offset: 0,
            secondary_shift_left: 0,
            count,
            size_shift: 0,
        });
        let coords = program.blocks[0].append_new_inst(
            Opcode::CompositeConstructF32x2,
            vec![Value::ImmF32(0.25), Value::ImmF32(0.75)],
        );
        let opcode = if explicit_lod {
            Opcode::ImageSampleExplicitLod
        } else {
            Opcode::ImageSampleImplicitLod
        };
        let sample = program.blocks[0].append_new_inst(
            opcode,
            vec![
                Value::ImmU32(count.saturating_sub(1)),
                Value::Inst(InstRef {
                    block: 0,
                    inst: coords,
                }),
                Value::ImmF32(1.0),
                Value::Void,
            ],
        );
        program.blocks[0].inst_mut(sample).flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::Color2D as u8,
            ..Default::default()
        }
        .to_u32();
        program
    }

    fn storage_coordinates(program: &mut ir::Program, texture_type: TextureType) -> Value {
        let (opcode, values) = match texture_type {
            TextureType::Color1D => return Value::ImmU32(4),
            TextureType::ColorArray1D | TextureType::Color2D => (
                Opcode::CompositeConstructU32x2,
                vec![Value::ImmU32(4), Value::ImmU32(2)],
            ),
            TextureType::ColorArray2D | TextureType::Color3D => (
                Opcode::CompositeConstructU32x3,
                vec![Value::ImmU32(4), Value::ImmU32(2), Value::ImmU32(1)],
            ),
            _ => unreachable!("invalid storage image test dimension"),
        };
        let coords = program.blocks[0].append_new_inst(opcode, values);
        Value::Inst(InstRef {
            block: 0,
            inst: coords,
        })
    }

    fn storage_image_program(
        texture_type: TextureType,
        count: u32,
        is_integer: bool,
        is_read: bool,
        is_written: bool,
    ) -> ir::Program {
        let mut program = empty_program(Stage::Compute);
        let format = if is_integer {
            ImageFormat::R32Uint
        } else {
            ImageFormat::Typeless
        };
        program.info.image_descriptors.push(ImageDescriptor {
            texture_type,
            format,
            is_written,
            is_read,
            is_integer,
            cbuf_index: 0,
            cbuf_offset: 0,
            count,
            size_shift: 0,
        });
        let coords = storage_coordinates(&mut program, texture_type);
        let flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: texture_type as u8,
            image_format: format as u8,
            ..Default::default()
        }
        .to_u32();
        if is_read {
            let read = program.blocks[0].append_new_inst(
                Opcode::ImageRead,
                vec![Value::ImmU32(count.saturating_sub(1)), coords],
            );
            program.blocks[0].inst_mut(read).flags = flags;
        }
        if is_written {
            let color = program.blocks[0].append_new_inst(
                Opcode::CompositeConstructU32x4,
                vec![
                    Value::ImmU32(1),
                    Value::ImmU32(2),
                    Value::ImmU32(3),
                    Value::ImmU32(4),
                ],
            );
            let write = program.blocks[0].append_new_inst(
                Opcode::ImageWrite,
                vec![
                    Value::ImmU32(count.saturating_sub(1)),
                    coords,
                    Value::Inst(InstRef {
                        block: 0,
                        inst: color,
                    }),
                ],
            );
            program.blocks[0].inst_mut(write).flags = flags;
        }
        program
    }

    fn storage_image_atomic_program(index: Value, opcodes: &[Opcode]) -> ir::Program {
        let mut program = empty_program(Stage::Compute);
        program.info.uses_atomic_image_u32 = true;
        program.info.image_descriptors.push(ImageDescriptor {
            texture_type: TextureType::Color2D,
            format: ImageFormat::R32Uint,
            is_written: true,
            is_read: true,
            is_integer: true,
            cbuf_index: 0,
            cbuf_offset: 0,
            count: 1,
            size_shift: 0,
        });
        let coords = storage_coordinates(&mut program, TextureType::Color2D);
        let flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::Color2D as u8,
            image_format: ImageFormat::R32Uint as u8,
            ..Default::default()
        }
        .to_u32();
        for opcode in opcodes {
            let atomic = program.blocks[0]
                .append_new_inst(*opcode, vec![index, coords, Value::ImmU32(0x8000_0001)]);
            program.blocks[0].inst_mut(atomic).flags = flags;
        }
        program
    }

    fn array_2d_fetch_program(is_multisample: bool, with_offset: bool) -> ir::Program {
        let mut program = empty_program(Stage::Fragment);
        program.info.texture_descriptors.push(TextureDescriptor {
            texture_type: TextureType::ColorArray2D,
            is_depth: false,
            is_multisample,
            is_integer: false,
            has_secondary: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            shift_left: 0,
            secondary_cbuf_index: 0,
            secondary_cbuf_offset: 0,
            secondary_shift_left: 0,
            count: 1,
            size_shift: 0,
        });
        let coords = program.blocks[0].append_new_inst(
            Opcode::CompositeConstructU32x3,
            vec![Value::ImmU32(4), Value::ImmU32(2), Value::ImmU32(1)],
        );
        let offset = if with_offset {
            let offset = program.blocks[0].append_new_inst(
                Opcode::CompositeConstructU32x2,
                vec![Value::ImmU32(1), Value::ImmU32(2)],
            );
            Value::Inst(InstRef {
                block: 0,
                inst: offset,
            })
        } else {
            Value::Void
        };
        let fetch = program.blocks[0].append_new_inst(
            Opcode::ImageFetch,
            vec![
                Value::ImmU32(0),
                Value::Inst(InstRef {
                    block: 0,
                    inst: coords,
                }),
                offset,
                Value::ImmU32(3),
                if is_multisample {
                    Value::ImmU32(2)
                } else {
                    Value::Void
                },
            ],
        );
        program.blocks[0].inst_mut(fetch).flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::ColorArray2D as u8,
            ..Default::default()
        }
        .to_u32();
        let query = program.blocks[0].append_new_inst(
            Opcode::ImageQueryDimensions,
            vec![
                Value::ImmU32(0),
                Value::ImmU32(3),
                Value::ImmU1(is_multisample),
            ],
        );
        program.blocks[0].inst_mut(query).flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::ColorArray2D as u8,
            ..Default::default()
        }
        .to_u32();
        program
    }

    fn query_lod_program(texture_type: TextureType) -> ir::Program {
        let mut program = empty_program(Stage::Fragment);
        program.info.texture_descriptors.push(TextureDescriptor {
            texture_type,
            is_depth: false,
            is_multisample: false,
            is_integer: false,
            has_secondary: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            shift_left: 0,
            secondary_cbuf_index: 0,
            secondary_cbuf_offset: 0,
            secondary_shift_left: 0,
            count: 1,
            size_shift: 0,
        });
        let coords = program.blocks[0].append_new_inst(
            Opcode::CompositeConstructF32x3,
            vec![Value::ImmF32(0.25), Value::ImmF32(0.75), Value::ImmF32(1.0)],
        );
        let query = program.blocks[0].append_new_inst(
            Opcode::ImageQueryLod,
            vec![
                Value::ImmU32(0),
                Value::Inst(InstRef {
                    block: 0,
                    inst: coords,
                }),
            ],
        );
        program.blocks[0].inst_mut(query).flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: texture_type as u8,
            ..Default::default()
        }
        .to_u32();
        program
    }

    fn gradient_program(
        texture_type: TextureType,
        num_derivatives: u8,
        with_offset: bool,
        with_lod_clamp: bool,
    ) -> ir::Program {
        let mut program = empty_program(Stage::Fragment);
        program.info.texture_descriptors.push(TextureDescriptor {
            texture_type,
            is_depth: false,
            is_multisample: false,
            is_integer: false,
            has_secondary: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            shift_left: 0,
            secondary_cbuf_index: 0,
            secondary_cbuf_offset: 0,
            secondary_shift_left: 0,
            count: 1,
            size_shift: 0,
        });
        let coords_opcode = match texture_type {
            TextureType::Color1D => Opcode::Identity,
            TextureType::ColorArray1D | TextureType::Color2D | TextureType::Color2DRect => {
                Opcode::CompositeConstructF32x2
            }
            TextureType::ColorArray2D | TextureType::Color3D | TextureType::ColorCube => {
                Opcode::CompositeConstructF32x3
            }
            TextureType::ColorArrayCube => Opcode::CompositeConstructF32x4,
            TextureType::Buffer => unreachable!(),
        };
        let coords = if texture_type == TextureType::Color1D {
            Value::ImmF32(0.25)
        } else {
            let values = [
                Value::ImmF32(0.25),
                Value::ImmF32(0.5),
                Value::ImmF32(0.75),
                Value::ImmF32(1.0),
            ];
            let count = match coords_opcode {
                Opcode::CompositeConstructF32x2 => 2,
                Opcode::CompositeConstructF32x3 => 3,
                Opcode::CompositeConstructF32x4 => 4,
                _ => unreachable!(),
            };
            let coords = program.blocks[0].append_new_inst(coords_opcode, values[..count].to_vec());
            Value::Inst(InstRef {
                block: 0,
                inst: coords,
            })
        };
        let derivatives = program.blocks[0].append_new_inst(
            if num_derivatives == 1 {
                Opcode::CompositeConstructF32x2
            } else {
                Opcode::CompositeConstructF32x4
            },
            if num_derivatives == 1 {
                vec![Value::ImmF32(0.1), Value::ImmF32(0.2)]
            } else {
                vec![
                    Value::ImmF32(0.1),
                    Value::ImmF32(0.2),
                    Value::ImmF32(0.3),
                    Value::ImmF32(0.4),
                ]
            },
        );
        let fourth_argument = if num_derivatives == 3 {
            let second = program.blocks[0].append_new_inst(
                Opcode::CompositeConstructF32x2,
                vec![Value::ImmF32(0.5), Value::ImmF32(0.6)],
            );
            Value::Inst(InstRef {
                block: 0,
                inst: second,
            })
        } else if with_offset {
            let offset = program.blocks[0].append_new_inst(
                Opcode::CompositeConstructU32x2,
                vec![Value::ImmU32(u32::MAX), Value::ImmU32(2)],
            );
            Value::Inst(InstRef {
                block: 0,
                inst: offset,
            })
        } else {
            Value::Void
        };
        let gradient = program.blocks[0].append_new_inst(
            Opcode::ImageGradient,
            vec![
                Value::ImmU32(0),
                coords,
                Value::Inst(InstRef {
                    block: 0,
                    inst: derivatives,
                }),
                fourth_argument,
                if with_lod_clamp {
                    Value::ImmF32(0.5)
                } else {
                    Value::Void
                },
            ],
        );
        program.blocks[0].inst_mut(gradient).flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: texture_type as u8,
            num_derivatives,
            has_lod_clamp: with_lod_clamp,
            ..Default::default()
        }
        .to_u32();
        program
    }

    fn gather_program() -> ir::Program {
        let mut program = empty_program(Stage::Fragment);
        program.info.texture_descriptors.push(TextureDescriptor {
            texture_type: TextureType::Color2D,
            is_depth: false,
            is_multisample: false,
            is_integer: false,
            has_secondary: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            shift_left: 0,
            secondary_cbuf_index: 0,
            secondary_cbuf_offset: 0,
            secondary_shift_left: 0,
            count: 1,
            size_shift: 0,
        });
        let coords = program.blocks[0].append_new_inst(
            Opcode::CompositeConstructF32x2,
            vec![Value::ImmF32(0.25), Value::ImmF32(0.75)],
        );
        let gather = program.blocks[0].append_new_inst(
            Opcode::ImageGather,
            vec![
                Value::ImmU32(0),
                Value::Inst(InstRef {
                    block: 0,
                    inst: coords,
                }),
                Value::Void,
                Value::Void,
            ],
        );
        program.blocks[0].inst_mut(gather).flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::Color2D as u8,
            gather_component: 2,
            ..Default::default()
        }
        .to_u32();
        program
    }

    #[test]
    fn emits_minimal_vertex_entry_point_without_spirv() {
        let artifact = emit_msl(
            &empty_program(Stage::VertexB),
            &Profile::default(),
            &RuntimeInfo::default(),
        )
        .unwrap();

        assert_eq!(artifact.source.stage, Stage::VertexB);
        assert_eq!(artifact.entry_point, "main0");
        assert!(artifact
            .source
            .source
            .contains("vertex MslVertexOut main0()"));
        assert!(artifact
            .source
            .source
            .contains("float4 position [[position]]"));
        assert!(!artifact
            .source
            .source
            .to_ascii_lowercase()
            .contains("spir-v"));
        assert_eq!(artifact.bindings, Default::default());
    }

    #[test]
    fn emits_profile_gated_gather_subpixel_offset() {
        let profile = Profile {
            need_gather_subpixel_offset: true,
            ..Profile::default()
        };
        let artifact = emit_msl(&gather_program(), &profile, &RuntimeInfo::default())
            .expect("2D gather must lower directly to MSL");

        assert!(artifact.source.source.contains("0.001953125f"));
        assert!(artifact.source.source.contains("tex0.get_width(0u)"));
        assert!(artifact.source.source.contains("tex0.get_height(0u)"));
        assert!(artifact.source.source.contains("component::z"));
    }

    #[test]
    fn emits_minimal_fragment_entry_point_without_spirv() {
        let artifact = emit_msl(
            &empty_program(Stage::Fragment),
            &Profile::default(),
            &RuntimeInfo::default(),
        )
        .unwrap();

        assert_eq!(artifact.source.stage, Stage::Fragment);
        assert!(artifact.source.source.contains("fragment void main0()"));
        assert!(!artifact
            .source
            .source
            .to_ascii_lowercase()
            .contains("spir-v"));
    }

    #[test]
    fn emits_fragment_sample_and_helper_invocation_builtins() {
        let mut program = empty_program(Stage::Fragment);
        program.info.uses_sample_id = true;
        program.info.uses_is_helper_invocation = true;
        program.blocks[0].append_new_inst(Opcode::SampleId, vec![]);
        program.blocks[0].append_new_inst(Opcode::IsHelperInvocation, vec![]);

        let artifact = emit_msl(&program, &Profile::default(), &RuntimeInfo::default()).unwrap();
        let source = &artifact.source.source;
        assert!(source.contains("uint sample_id [[sample_id]]"));
        assert!(source.contains("uint v_0_0 = sample_id;"));
        assert!(source.contains("bool v_0_1 = simd_is_helper_thread();"));
    }

    #[test]
    fn emits_minimal_compute_entry_point_and_execution_metadata() {
        let mut program = empty_program(Stage::Compute);
        program.workgroup_size = [8, 4, 2];
        let artifact = emit_msl(&program, &Profile::default(), &RuntimeInfo::default()).unwrap();

        assert_eq!(artifact.source.stage, Stage::Compute);
        assert!(artifact.source.source.contains("kernel void main0()"));
        assert_eq!(artifact.execution.workgroup_size, Some([8, 4, 2]));
    }

    #[test]
    fn emits_compute_grid_and_threadgroup_position_builtins() {
        let mut program = empty_program(Stage::Compute);
        program.info.uses_workgroup_id = true;
        program.info.uses_local_invocation_id = true;
        program.blocks[0].append_new_inst(Opcode::WorkgroupId, vec![]);
        program.blocks[0].append_new_inst(Opcode::LocalInvocationId, vec![]);

        let artifact = emit_msl(&program, &Profile::default(), &RuntimeInfo::default()).unwrap();
        let source = &artifact.source.source;
        assert!(source.contains("uint3 workgroup_id [[threadgroup_position_in_grid]]"));
        assert!(source.contains("uint3 local_invocation_id [[thread_position_in_threadgroup]]"));
        assert!(source.contains("uint3 v_0_0 = workgroup_id;"));
        assert!(source.contains("uint3 v_0_1 = local_invocation_id;"));
    }

    #[test]
    fn emits_private_local_memory_with_upstream_word_indexing() {
        let mut program = empty_program(Stage::Compute);
        program.local_memory_size = 18;
        program.info.uses_local_memory = true;
        let load = program.blocks[0].append_new_inst(Opcode::LoadLocal, vec![Value::ImmU32(2)]);
        program.blocks[0].append_new_inst(
            Opcode::WriteLocal,
            vec![
                Value::ImmU32(3),
                Value::Inst(InstRef {
                    block: 0,
                    inst: load,
                }),
            ],
        );

        let artifact = emit_msl(&program, &Profile::default(), &RuntimeInfo::default()).unwrap();
        let source = &artifact.source.source;
        assert!(source.contains("thread uint lmem[5];"));
        assert!(source.contains("uint v_0_0 = lmem[0x00000002u];"));
        assert!(source.contains("lmem[0x00000003u] = v_0_0;"));
    }

    #[test]
    fn emits_native_color2d_sample_and_descriptor_array_bindings() {
        let profile = Profile {
            unified_descriptor_binding: true,
            ..Profile::default()
        };
        let artifact = emit_msl(
            &sampled_texture_program(2, true),
            &profile,
            &RuntimeInfo::default(),
        )
        .unwrap();

        assert!(artifact
            .source
            .source
            .contains("array<texture2d<float>, 2> tex0 [[texture(0)]]"));
        assert!(artifact
            .source
            .source
            .contains("array<sampler, 2> samp0 [[sampler(0)]]"));
        assert!(artifact.source.source.contains(
            "float4 v_0_1 = tex0[0x00000001u].sample(samp0[0x00000001u], v_0_0, level(as_type<float>(0x3F800000u)));"
        ));
        assert_eq!(artifact.bindings.texture_count, 2);
        assert_eq!(artifact.bindings.sampler_count, 2);
        assert_eq!(artifact.bindings.resources.len(), 1);
        assert_eq!(
            artifact.bindings.resources[0].kind,
            MslResourceKind::SampledImage
        );
        assert_eq!(artifact.bindings.resources[0].binding, 0);
        assert_eq!(
            artifact.bindings.resources[0].count,
            std::num::NonZeroU32::new(2)
        );
    }

    #[test]
    fn emits_fragment_implicit_lod_without_level_argument() {
        let artifact = emit_msl(
            &sampled_texture_program(1, false),
            &Profile::default(),
            &RuntimeInfo::default(),
        )
        .unwrap();

        assert!(artifact
            .source
            .source
            .contains("float4 v_0_1 = tex0.sample(samp0, v_0_0);"));
    }

    #[test]
    fn emits_array_fetch_offset_and_dimension_query() {
        let artifact = emit_msl(
            &array_2d_fetch_program(false, true),
            &Profile::default(),
            &RuntimeInfo::default(),
        )
        .unwrap();
        let source = &artifact.source.source;

        assert!(source.contains("texture2d_array<float> tex0 [[texture(0)]]"));
        assert!(source.contains("+ uint3((v_0_1).xy, 0u)"));
        assert!(source.contains(".read("));
        assert!(source.contains(".get_width(0x00000003u)"));
        assert!(source.contains(".get_height(0x00000003u)"));
        assert!(source.contains(".get_array_size()"));
        assert!(source.contains(".get_num_mip_levels()"));
    }

    #[test]
    fn emits_multisample_array_fetch_without_lod() {
        let artifact = emit_msl(
            &array_2d_fetch_program(true, false),
            &Profile::default(),
            &RuntimeInfo::default(),
        )
        .unwrap();
        let source = &artifact.source.source;

        assert!(source.contains("texture2d_ms_array<float> tex0 [[texture(0)]]"));
        assert!(source.contains(".read((v_0_0).xy, (v_0_0).z, 0x00000002u)"));
        assert!(source
            .contains("uint4(tex0.get_width(), tex0.get_height(), tex0.get_array_size(), 0u)"));
        assert!(!source.contains("get_num_mip_levels"));
    }

    #[test]
    fn emits_capability_gated_texture_lod_queries() {
        let program = query_lod_program(TextureType::ColorArray2D);
        assert_eq!(
            emit_msl(&program, &Profile::default(), &RuntimeInfo::default()),
            Err(MslError::UnsupportedProgramFeature(
                "texture LOD query on the selected Metal device"
            ))
        );

        let artifact = emit_msl_with_options(
            &program,
            &Profile::default(),
            &RuntimeInfo::default(),
            &MslOptions {
                supports_query_texture_lod: true,
                ..MslOptions::default()
            },
        )
        .unwrap();
        let source = &artifact.source.source;
        assert!(source.contains("tex0.calculate_clamped_lod(samp0, (v_0_0).xy)"));
        assert!(source.contains("tex0.calculate_unclamped_lod(samp0, (v_0_0).xy)"));
        assert!(source.contains(", 0.0f, 0.0f)"));

        let one_dimensional = query_lod_program(TextureType::Color1D);
        assert_eq!(
            emit_msl_with_options(
                &one_dimensional,
                &Profile::default(),
                &RuntimeInfo::default(),
                &MslOptions {
                    supports_query_texture_lod: true,
                    ..MslOptions::default()
                },
            ),
            Err(MslError::UnsupportedProgramFeature(
                "texture LOD query on a Metal 1D texture"
            ))
        );
    }

    #[test]
    fn emits_texture_gradients_with_upstream_derivative_packing() {
        let two_dimensional = emit_msl(
            &gradient_program(TextureType::Color2D, 2, true, true),
            &Profile::default(),
            &RuntimeInfo::default(),
        )
        .unwrap();
        let source = &two_dimensional.source.source;
        assert!(source
            .contains("gradient2d(float2((v_0_1).x, (v_0_1).z), float2((v_0_1).y, (v_0_1).w))"));
        assert!(source.contains("int2(-1, 2)"));
        assert!(source.contains("min_lod_clamp(as_type<float>(0x3F000000u))"));

        let cube = emit_msl(
            &gradient_program(TextureType::ColorCube, 3, false, false),
            &Profile::default(),
            &RuntimeInfo::default(),
        )
        .unwrap();
        assert!(cube.source.source.contains("gradientcube(float3("));
    }

    #[test]
    fn rejects_unported_sample_operands_instead_of_dropping_them() {
        let mut program = sampled_texture_program(1, true);
        program.blocks[0].inst_mut(1).args[3] = Value::ImmU32(1);

        assert_eq!(
            emit_msl(&program, &Profile::default(), &RuntimeInfo::default()),
            Err(MslError::UnsupportedProgramFeature("texture sample offset"))
        );

        program.blocks[0].inst_mut(1).args[3] = Value::Void;
        program.blocks[0].inst_mut(1).flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::Color2D as u8,
            has_lod_clamp: true,
            ..Default::default()
        }
        .to_u32();
        assert_eq!(
            emit_msl(&program, &Profile::default(), &RuntimeInfo::default()),
            Err(MslError::UnsupportedProgramFeature("texture LOD clamp"))
        );
    }

    #[test]
    fn rejects_unported_ir_instead_of_emitting_a_fallback() {
        let mut program = empty_program(Stage::Fragment);
        program.blocks[0].append_new_inst(Opcode::UndefU32, vec![]);

        assert_eq!(
            emit_msl(&program, &Profile::default(), &RuntimeInfo::default()),
            Err(MslError::UnsupportedOpcode {
                block: 0,
                inst: 0,
                opcode: Opcode::UndefU32,
            })
        );
    }

    #[test]
    fn gates_native_int64_on_the_selected_metal_profile() {
        let mut program = empty_program(Stage::Fragment);
        program.info.uses_int64 = true;
        program.blocks[0].append_new_inst(Opcode::IAdd64, vec![Value::ImmU64(1), Value::ImmU64(2)]);

        assert_eq!(
            emit_msl(&program, &Profile::default(), &RuntimeInfo::default()),
            Err(MslError::UnsupportedProgramFeature(
                "64-bit integers on the selected Metal device"
            ))
        );

        let profile = Profile {
            support_int64: true,
            ..Profile::default()
        };
        let artifact = emit_msl(&program, &profile, &RuntimeInfo::default()).unwrap();
        assert!(artifact
            .source
            .source
            .contains("ulong v_0_0 = (0x0000000000000001ul) + (0x0000000000000002ul);"));
    }

    #[test]
    fn emits_native_half_and_int64_scalar_families() {
        let mut program = empty_program(Stage::VertexB);
        program.info.uses_fp16 = true;
        program.info.uses_fp16_denorms_preserve = true;
        program.info.uses_int64 = true;
        let block = &mut program.blocks[0];
        let add64 = block.append_new_inst(
            Opcode::IAdd64,
            vec![Value::ImmU64(0xFEDC_BA98_7654_3210), Value::ImmU64(1)],
        );
        block.append_new_inst(
            Opcode::ShiftRightArithmetic64,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: add64,
                }),
                Value::ImmU32(4),
            ],
        );
        block.append_new_inst(Opcode::IAbs64, vec![Value::ImmU64(u64::MAX)]);
        let add16 = block.append_new_inst(
            Opcode::FPAdd16,
            vec![Value::ImmF16(0x3C00), Value::ImmF16(0x4000)],
        );
        block.inst_mut(add16).flags = crate::ir::types::FpControl {
            no_contraction: true,
            ..Default::default()
        }
        .to_u32();
        block.append_new_inst(
            Opcode::FPFma16,
            vec![
                Value::ImmF16(0x3C00),
                Value::ImmF16(0x4000),
                Value::ImmF16(0x4200),
            ],
        );
        block.append_new_inst(
            Opcode::FPUnordEqual16,
            vec![Value::ImmF16(0x7E00), Value::ImmF16(0x3C00)],
        );
        block.append_new_inst(Opcode::ConvertF16S8, vec![Value::ImmU32(0xFF)]);
        block.append_new_inst(Opcode::ConvertS16F16, vec![Value::ImmF16(0xBC00)]);
        block.append_new_inst(Opcode::ConvertU16F16, vec![Value::ImmF16(0x3C00)]);
        block.append_new_inst(Opcode::ConvertF16F32, vec![Value::ImmF32(f32::NAN)]);
        block.append_new_inst(Opcode::ConvertF32U64, vec![Value::ImmU64(7)]);

        let profile = Profile {
            support_int64: true,
            ..Profile::default()
        };
        let artifact = emit_msl(&program, &profile, &RuntimeInfo::default()).unwrap();
        let source = &artifact.source.source;
        assert!(source.contains("ulong v_0_0 = (0xFEDCBA9876543210ul) +"));
        assert!(source.contains("as_type<ulong>(as_type<long>(v_0_0) >>"));
        assert!(source.contains("as_type<ulong>(abs(as_type<long>(0xFFFFFFFFFFFFFFFFul)))"));
        assert!(source.contains(
            "half v_0_3 = spvFAdd(as_type<half>(ushort(0x3C00u)), as_type<half>(ushort(0x4000u)));"
        ));
        assert!(source.contains("half v_0_4 = fma("));
        assert!(source.contains("isnan(as_type<half>(ushort(0x7E00u)))"));
        assert!(source.contains("half v_0_6 = half((as_type<int>"));
        assert!(source
            .contains("uint v_0_7 = as_type<uint>(int(short(as_type<half>(ushort(0xBC00u)))));"));
        assert!(source.contains("uint v_0_8 = uint(ushort(as_type<half>(ushort(0x3C00u))));"));
        assert!(source.contains(
            "half v_0_9 = isnan(half(as_type<float>(0x7FC00000u))) ? as_type<half>(ushort(0u)) : half(as_type<float>(0x7FC00000u));"
        ));
        assert!(source.contains("float v_0_10 = float(0x0000000000000007ul);"));
    }

    #[test]
    fn rejects_unmerged_vertex_a() {
        assert_eq!(
            emit_msl(
                &empty_program(Stage::VertexA),
                &Profile::default(),
                &RuntimeInfo::default()
            ),
            Err(MslError::UnmergedVertexA)
        );
    }

    #[test]
    fn rejects_descriptor_aliasing_storage_resources() {
        let mut program = empty_program(Stage::Fragment);
        program.info.storage_buffers_descriptors.push(
            crate::shader_info::StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 1,
                is_written: false,
            },
        );

        let profile = Profile {
            support_descriptor_aliasing: true,
            ..Profile::default()
        };
        assert_eq!(
            emit_msl(&program, &profile, &RuntimeInfo::default()),
            Err(MslError::UnsupportedProgramFeature(
                "descriptor-aliasing storage buffers"
            ))
        );
    }

    #[test]
    fn emits_non_aliasing_constant_buffer_loads_and_direct_binding_abi() {
        let mut program = empty_program(Stage::Fragment);
        program
            .info
            .constant_buffer_descriptors
            .push(crate::shader_info::ConstantBufferDescriptor { index: 3, count: 1 });
        program.info.constant_buffer_mask = 1 << 3;
        program.info.constant_buffer_used_sizes[3] = 64;
        program.info.uses_int8 = true;
        program.info.uses_int16 = true;
        program.info.used_constant_buffer_types = crate::ir::Type::U8 as u32
            | crate::ir::Type::U16 as u32
            | crate::ir::Type::U32 as u32
            | crate::ir::Type::F32 as u32
            | crate::ir::Type::U32x2 as u32;
        let block = &mut program.blocks[0];
        block.append_new_inst(Opcode::GetCbufU8, vec![Value::ImmU32(3), Value::ImmU32(5)]);
        block.append_new_inst(Opcode::GetCbufS16, vec![Value::ImmU32(3), Value::ImmU32(6)]);
        block.append_new_inst(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(3), Value::ImmU32(20)],
        );
        block.append_new_inst(
            Opcode::GetCbufF32,
            vec![Value::ImmU32(3), Value::ImmU32(24)],
        );
        block.append_new_inst(
            Opcode::GetCbufU32x2,
            vec![Value::ImmU32(3), Value::ImmU32(8)],
        );

        let profile = Profile {
            unified_descriptor_binding: true,
            ..Profile::default()
        };
        let mut bindings = Bindings {
            unified: 7,
            ..Bindings::default()
        };
        let artifact =
            emit_msl_with_bindings(&program, &profile, &RuntimeInfo::default(), &mut bindings)
                .unwrap();
        let source = &artifact.source.source;
        assert!(source.contains("fragment void main0(constant uint4* c3 [[buffer(0)]])"));
        assert!(source.contains("extract_bits(c3[0u][1u], 8u, 8u)"));
        assert!(source.contains("as_type<uint>(extract_bits(as_type<int>(c3[0u][1u]), 16u, 16u))"));
        assert!(source.contains("uint v_0_2 = c3[1u][1u];"));
        assert!(source.contains("float v_0_3 = as_type<float>(c3[1u][2u]);"));
        assert!(source.contains("uint2 v_0_4 = uint2(c3[0u][2u], c3[0u][3u]);"));
        assert_eq!(bindings.unified, 8);
        assert_eq!(artifact.bindings.buffer_count, 1);
        assert_eq!(artifact.bindings.resources.len(), 1);
        assert_eq!(artifact.bindings.resources[0].descriptor_set, 0);
        assert_eq!(artifact.bindings.resources[0].binding, 7);
        assert_eq!(
            artifact.bindings.resources[0].kind,
            MslResourceKind::UniformBuffer
        );
        assert_eq!(artifact.bindings.resources[0].buffer_index, 0);
        assert_eq!(artifact.bindings.resources[0].count, None);
    }

    #[test]
    fn dynamic_constant_buffer_offsets_follow_the_upstream_uint4_path() {
        let mut program = empty_program(Stage::Compute);
        program
            .info
            .constant_buffer_descriptors
            .push(crate::shader_info::ConstantBufferDescriptor { index: 2, count: 1 });
        let offset = program.blocks[0]
            .append_new_inst(Opcode::IAdd32, vec![Value::ImmU32(16), Value::ImmU32(4)]);
        program.blocks[0].append_new_inst(
            Opcode::GetCbufU32,
            vec![
                Value::ImmU32(2),
                Value::Inst(InstRef {
                    block: 0,
                    inst: offset,
                }),
            ],
        );

        let artifact = emit_msl(&program, &Profile::default(), &RuntimeInfo::default()).unwrap();
        assert!(artifact
            .source
            .source
            .contains("uint v_0_1 = c2[((v_0_0) >> 4u)][(((v_0_0) >> 2u) & 3u)];"));
    }

    #[test]
    fn emits_non_aliasing_storage_buffer_memory_and_direct_binding_abi() {
        let mut program = empty_program(Stage::Compute);
        program.info.storage_buffers_descriptors.push(
            crate::shader_info::StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 2,
                is_written: true,
            },
        );
        program.info.uses_int8 = true;
        program.info.uses_int16 = true;
        program.info.used_storage_buffer_types = crate::ir::Type::U8 as u32
            | crate::ir::Type::U16 as u32
            | crate::ir::Type::U32 as u32
            | crate::ir::Type::U32x2 as u32
            | crate::ir::Type::U32x4 as u32;
        let block = &mut program.blocks[0];
        block.append_new_inst(
            Opcode::LoadStorageU8,
            vec![Value::ImmU32(0), Value::ImmU32(1)],
        );
        block.append_new_inst(
            Opcode::LoadStorageS16,
            vec![Value::ImmU32(1), Value::ImmU32(2)],
        );
        block.append_new_inst(
            Opcode::LoadStorage32,
            vec![Value::ImmU32(0), Value::ImmU32(4)],
        );
        let load64 = block.append_new_inst(
            Opcode::LoadStorage64,
            vec![Value::ImmU32(0), Value::ImmU32(8)],
        );
        let load128 = block.append_new_inst(
            Opcode::LoadStorage128,
            vec![Value::ImmU32(0), Value::ImmU32(16)],
        );
        block.append_new_inst(
            Opcode::WriteStorageU8,
            vec![Value::ImmU32(0), Value::ImmU32(3), Value::ImmU32(0xAB)],
        );
        block.append_new_inst(
            Opcode::WriteStorageU16,
            vec![Value::ImmU32(0), Value::ImmU32(6), Value::ImmU32(0xCDEF)],
        );
        block.append_new_inst(
            Opcode::WriteStorage32,
            vec![Value::ImmU32(0), Value::ImmU32(4), Value::ImmU32(7)],
        );
        block.append_new_inst(
            Opcode::WriteStorage64,
            vec![
                Value::ImmU32(0),
                Value::ImmU32(8),
                Value::Inst(InstRef {
                    block: 0,
                    inst: load64,
                }),
            ],
        );
        block.append_new_inst(
            Opcode::WriteStorage128,
            vec![
                Value::ImmU32(0),
                Value::ImmU32(16),
                Value::Inst(InstRef {
                    block: 0,
                    inst: load128,
                }),
            ],
        );

        let profile = Profile {
            unified_descriptor_binding: true,
            ..Profile::default()
        };
        let mut bindings = Bindings {
            unified: 5,
            ..Bindings::default()
        };
        let artifact =
            emit_msl_with_bindings(&program, &profile, &RuntimeInfo::default(), &mut bindings)
                .unwrap();
        let source = &artifact.source.source;
        assert!(source.contains("kernel void main0(device uint* ssbo0 [[buffer(0)]])"));
        assert!(source.contains("extract_bits(ssbo0[0u], 8u, 8u)"));
        assert!(source.contains("as_type<uint>(extract_bits(as_type<int>(ssbo0[0u]), 16u, 16u))"));
        assert!(source.contains("uint v_0_2 = ssbo0[1u];"));
        assert!(source.contains("uint2 v_0_3 = uint2(ssbo0[2u], ssbo0[3u]);"));
        assert!(source.contains("uint4 v_0_4 = uint4(ssbo0[4u], ssbo0[5u], ssbo0[6u], ssbo0[7u]);"));
        assert!(source.contains("spvWriteStorageBits(&ssbo0[0u], 0x000000ABu, 24u, 8u);"));
        assert!(source.contains("spvWriteStorageBits(&ssbo0[1u], 0x0000CDEFu, 16u, 16u);"));
        assert!(source.contains("ssbo0[1u] = 0x00000007u;"));
        assert!(source.contains("ssbo0[2u] = v_0_3.x;"));
        assert!(source.contains("ssbo0[7u] = v_0_4.w;"));
        assert!(source.contains("atomic_compare_exchange_weak_explicit"));
        assert_eq!(bindings.unified, 7);
        assert_eq!(artifact.bindings.buffer_count, 1);
        assert_eq!(artifact.bindings.resources.len(), 1);
        assert_eq!(artifact.bindings.resources[0].binding, 5);
        assert_eq!(
            artifact.bindings.resources[0].kind,
            MslResourceKind::StorageBuffer
        );
        assert_eq!(artifact.bindings.resources[0].buffer_index, 0);
        assert_eq!(artifact.bindings.resources[0].count, None);
    }

    #[test]
    fn emits_shared_memory_with_upstream_word_layout_and_subword_cas() {
        let mut program = empty_program(Stage::Compute);
        program.shared_memory_size = 64;
        program.info.uses_int8 = true;
        program.info.uses_int16 = true;
        let block = &mut program.blocks[0];
        block.append_new_inst(Opcode::LoadSharedU8, vec![Value::ImmU32(1)]);
        block.append_new_inst(Opcode::LoadSharedS16, vec![Value::ImmU32(2)]);
        block.append_new_inst(Opcode::LoadSharedU32, vec![Value::ImmU32(4)]);
        let load64 = block.append_new_inst(Opcode::LoadSharedU64, vec![Value::ImmU32(8)]);
        let load128 = block.append_new_inst(Opcode::LoadSharedU128, vec![Value::ImmU32(16)]);
        block.append_new_inst(
            Opcode::WriteSharedU8,
            vec![Value::ImmU32(3), Value::ImmU32(0xAB)],
        );
        block.append_new_inst(
            Opcode::WriteSharedU16,
            vec![Value::ImmU32(6), Value::ImmU32(0xCDEF)],
        );
        block.append_new_inst(
            Opcode::WriteSharedU32,
            vec![Value::ImmU32(4), Value::ImmU32(7)],
        );
        block.append_new_inst(
            Opcode::WriteSharedU64,
            vec![
                Value::ImmU32(8),
                Value::Inst(InstRef {
                    block: 0,
                    inst: load64,
                }),
            ],
        );
        block.append_new_inst(
            Opcode::WriteSharedU128,
            vec![
                Value::ImmU32(16),
                Value::Inst(InstRef {
                    block: 0,
                    inst: load128,
                }),
            ],
        );
        block.append_new_inst(Opcode::Barrier, vec![]);

        let artifact = emit_msl(&program, &Profile::default(), &RuntimeInfo::default()).unwrap();
        let source = &artifact.source.source;
        assert!(source.contains("kernel void main0() {\n    threadgroup uint smem[16];"));
        assert!(source.contains(
            "extract_bits(smem[((0x00000001u) >> 2u)], (((0x00000001u) << 3u) & 24u), 8u)"
        ));
        assert!(source.contains("as_type<uint>(extract_bits(as_type<int>(smem[((0x00000002u) >> 2u)]), (((0x00000002u) << 3u) & 16u), 16u))"));
        assert!(source.contains("uint v_0_2 = smem[((0x00000004u) >> 2u)];"));
        assert!(source.contains("uint2 v_0_3 = uint2(smem[((0x00000008u) >> 2u)]"));
        assert!(source.contains("uint4 v_0_4 = uint4(smem[((0x00000010u) >> 2u)]"));
        assert!(source.contains("spvWriteSharedBits(&smem[((0x00000003u) >> 2u)]"));
        assert!(source.contains("threadgroup atomic_uint* atomic_pointer"));
        assert!(source.contains("smem[((0x00000004u) >> 2u)] = 0x00000007u;"));
        assert!(source.contains("threadgroup_barrier(mem_flags::mem_threadgroup);"));
    }

    #[test]
    fn emits_shared_and_storage_u32_atomics_with_upstream_cas_semantics() {
        let mut program = empty_program(Stage::Compute);
        program.shared_memory_size = 64;
        program.info.uses_shared_increment = true;
        program.info.uses_shared_decrement = true;
        program.info.uses_atomic_s32_min = true;
        program.info.uses_atomic_s32_max = true;
        program.info.storage_buffers_descriptors.push(
            crate::shader_info::StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 1,
                is_written: true,
            },
        );
        let block = &mut program.blocks[0];
        for opcode in [
            Opcode::SharedAtomicIAdd32,
            Opcode::SharedAtomicSMin32,
            Opcode::SharedAtomicUMin32,
            Opcode::SharedAtomicSMax32,
            Opcode::SharedAtomicUMax32,
            Opcode::SharedAtomicInc32,
            Opcode::SharedAtomicDec32,
            Opcode::SharedAtomicAnd32,
            Opcode::SharedAtomicOr32,
            Opcode::SharedAtomicXor32,
            Opcode::SharedAtomicExchange32,
        ] {
            block.append_new_inst(opcode, vec![Value::ImmU32(4), Value::ImmU32(7)]);
        }
        for opcode in [
            Opcode::StorageAtomicIAdd32,
            Opcode::StorageAtomicSMin32,
            Opcode::StorageAtomicUMin32,
            Opcode::StorageAtomicSMax32,
            Opcode::StorageAtomicUMax32,
            Opcode::StorageAtomicInc32,
            Opcode::StorageAtomicDec32,
            Opcode::StorageAtomicAnd32,
            Opcode::StorageAtomicOr32,
            Opcode::StorageAtomicXor32,
            Opcode::StorageAtomicExchange32,
        ] {
            block.append_new_inst(
                opcode,
                vec![Value::ImmU32(0), Value::ImmU32(8), Value::ImmU32(9)],
            );
        }

        let artifact = emit_msl(&program, &Profile::default(), &RuntimeInfo::default()).unwrap();
        let source = &artifact.source.source;
        assert!(
            source.contains("atomic_fetch_add_explicit(reinterpret_cast<threadgroup atomic_uint*>")
        );
        assert!(source.contains("reinterpret_cast<threadgroup atomic_int*>"));
        assert!(source.contains("spvAtomicInc(reinterpret_cast<threadgroup atomic_uint*>"));
        assert!(source.contains("spvAtomicDec(reinterpret_cast<threadgroup atomic_uint*>"));
        assert!(source.contains(
            "atomic_exchange_explicit(reinterpret_cast<device atomic_uint*>(&ssbo0[2u])"
        ));
        assert!(source.contains("reinterpret_cast<device atomic_int*>"));
        assert!(source.contains("uint desired = expected >= limit ? 0u : expected + 1u;"));
        assert!(source.contains(
            "uint desired = expected == 0u || expected > limit ? limit : expected - 1u;"
        ));
    }

    #[test]
    fn dynamic_storage_offsets_follow_the_upstream_word_index_path() {
        let mut program = empty_program(Stage::Compute);
        program.info.storage_buffers_descriptors.push(
            crate::shader_info::StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 1,
                is_written: true,
            },
        );
        program.info.used_storage_buffer_types = crate::ir::Type::U32 as u32;
        let offset = program.blocks[0]
            .append_new_inst(Opcode::IAdd32, vec![Value::ImmU32(8), Value::ImmU32(4)]);
        program.blocks[0].append_new_inst(
            Opcode::LoadStorage32,
            vec![
                Value::ImmU32(0),
                Value::Inst(InstRef {
                    block: 0,
                    inst: offset,
                }),
            ],
        );
        program.blocks[0].append_new_inst(
            Opcode::WriteStorage32,
            vec![
                Value::ImmU32(0),
                Value::Inst(InstRef {
                    block: 0,
                    inst: offset,
                }),
                Value::ImmU32(7),
            ],
        );

        let artifact = emit_msl(&program, &Profile::default(), &RuntimeInfo::default()).unwrap();
        assert!(artifact
            .source
            .source
            .contains("uint v_0_1 = ssbo0[((v_0_0) >> 2u)];"));
        assert!(artifact
            .source
            .source
            .contains("ssbo0[((v_0_0) >> 2u)] = 0x00000007u;"));
    }

    #[test]
    fn shared_binding_counters_preserve_graphics_stage_descriptor_order() {
        let mut vertex = empty_program(Stage::VertexB);
        vertex
            .info
            .constant_buffer_descriptors
            .push(crate::shader_info::ConstantBufferDescriptor { index: 0, count: 1 });
        let mut fragment = empty_program(Stage::Fragment);
        fragment
            .info
            .constant_buffer_descriptors
            .push(crate::shader_info::ConstantBufferDescriptor { index: 1, count: 1 });
        let profile = Profile {
            unified_descriptor_binding: true,
            ..Profile::default()
        };
        let mut bindings = Bindings::default();

        let vertex =
            emit_msl_with_bindings(&vertex, &profile, &RuntimeInfo::default(), &mut bindings)
                .unwrap();
        let fragment =
            emit_msl_with_bindings(&fragment, &profile, &RuntimeInfo::default(), &mut bindings)
                .unwrap();

        assert_eq!(vertex.bindings.resources[0].binding, 0);
        assert_eq!(fragment.bindings.resources[0].binding, 1);
        assert_eq!(vertex.bindings.resources[0].buffer_index, 0);
        assert_eq!(fragment.bindings.resources[0].buffer_index, 0);
        assert_eq!(bindings.unified, 2);
    }

    #[test]
    fn rejects_constant_buffer_descriptor_indexing_without_a_silent_alias() {
        let mut program = empty_program(Stage::Fragment);
        program
            .info
            .constant_buffer_descriptors
            .push(crate::shader_info::ConstantBufferDescriptor { index: 0, count: 2 });

        assert_eq!(
            emit_msl(&program, &Profile::default(), &RuntimeInfo::default()),
            Err(MslError::UnsupportedProgramFeature(
                "constant buffer descriptor indexing"
            ))
        );
    }

    #[test]
    fn rejects_unported_stage_interfaces_even_without_instructions() {
        let mut program = empty_program(Stage::Fragment);
        program.info.loads.mask[0] = 1u64 << 32;

        assert_eq!(
            emit_msl(&program, &Profile::default(), &RuntimeInfo::default()),
            Err(MslError::UnsupportedProgramFeature(
                "stage inputs or outputs"
            ))
        );
    }

    #[test]
    fn emits_typed_ssa_integer_and_float_expressions() {
        let mut program = empty_program(Stage::VertexB);
        let integer = program.blocks[0]
            .append_new_inst(Opcode::IAdd32, vec![Value::ImmU32(1), Value::ImmU32(2)]);
        let float = program.blocks[0].append_new_inst(
            Opcode::FPAdd32,
            vec![Value::ImmF32(-0.0), Value::ImmF32(2.0)],
        );
        program.blocks[0].inst_mut(float).flags = crate::ir::types::FpControl {
            no_contraction: true,
            ..Default::default()
        }
        .to_u32();

        let artifact = emit_msl(&program, &Profile::default(), &RuntimeInfo::default()).unwrap();
        assert!(artifact
            .source
            .source
            .contains("uint v_0_0 = (0x00000001u) + (0x00000002u);"));
        assert!(artifact.source.source.contains(
            "float v_0_1 = spvFAdd(as_type<float>(0x80000000u), as_type<float>(0x40000000u));"
        ));
        assert!(artifact
            .source
            .source
            .contains("[[clang::optnone]] T spvFAdd"));
        assert_eq!(integer, 0);
    }

    #[test]
    fn emits_iadd_flags_before_visiting_associated_pseudos() {
        let mut program = empty_program(Stage::VertexB);
        {
            let mut emitter = Emitter::new(&mut program, 0);
            let add = emitter.iadd_32(Value::ImmU32(u32::MAX), Value::ImmU32(1));
            emitter.get_zero_from_op(add);
            emitter.get_sign_from_op(add);
            emitter.get_carry_from_op(add);
            emitter.get_overflow_from_op(add);
        }

        let artifact = emit_msl(&program, &Profile::default(), &RuntimeInfo::default()).unwrap();
        let source = &artifact.source.source;
        assert!(source.contains("uint v_0_0 = (0xFFFFFFFFu) + (0x00000001u);"));
        assert!(source.contains("bool v_0_1 = (v_0_0) == 0u;"));
        assert!(source.contains("bool v_0_2 = as_type<int>(v_0_0) < 0;"));
        assert!(source.contains("bool v_0_3 = (v_0_0) < (0xFFFFFFFFu);"));
        assert!(source.contains(
            "bool v_0_4 = (as_type<int>(0xFFFFFFFFu) >= 0) ? (as_type<int>(0x00000001u) > as_type<int>(0x7FFFFFFFu - (0xFFFFFFFFu)))"
        ));
    }

    #[test]
    fn rejects_a_pseudo_without_a_parent_definition() {
        let mut program = empty_program(Stage::VertexB);
        program.blocks[0].append_new_inst(Opcode::GetZeroFromOp, vec![Value::ImmU32(0)]);

        assert_eq!(
            emit_msl(&program, &Profile::default(), &RuntimeInfo::default()),
            Err(MslError::UnsupportedOpcode {
                block: 0,
                inst: 0,
                opcode: Opcode::GetZeroFromOp,
            })
        );
    }

    #[test]
    fn preserves_ordered_and_unordered_nan_comparison_semantics() {
        let mut program = empty_program(Stage::Fragment);
        program.blocks[0].append_new_inst(
            Opcode::FPOrdNotEqual32,
            vec![Value::ImmF32(f32::NAN), Value::ImmF32(1.0)],
        );
        program.blocks[0].append_new_inst(
            Opcode::FPUnordEqual32,
            vec![Value::ImmF32(f32::NAN), Value::ImmF32(1.0)],
        );

        let artifact = emit_msl(&program, &Profile::default(), &RuntimeInfo::default()).unwrap();
        let source = &artifact.source.source;
        assert!(source.contains(
            "!isnan(as_type<float>(0x7FC00000u)) && !isnan(as_type<float>(0x3F800000u))"
        ));
        assert!(source
            .contains("isnan(as_type<float>(0x7FC00000u)) || isnan(as_type<float>(0x3F800000u))"));
    }

    #[test]
    fn emits_signed_comparisons_bitcasts_selects_and_scalar_conversions() {
        let mut program = empty_program(Stage::VertexB);
        let signed_less = program.blocks[0].append_new_inst(
            Opcode::SLessThan,
            vec![Value::ImmU32(0xFFFF_FFFF), Value::ImmU32(1)],
        );
        let selected = program.blocks[0].append_new_inst(
            Opcode::SelectU32,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: signed_less,
                }),
                Value::ImmU32(7),
                Value::ImmU32(9),
            ],
        );
        program.blocks[0].append_new_inst(
            Opcode::BitCastF32U32,
            vec![Value::Inst(InstRef {
                block: 0,
                inst: selected,
            })],
        );
        program.blocks[0].append_new_inst(Opcode::ConvertS32F32, vec![Value::ImmF32(-2.0)]);
        program.blocks[0].append_new_inst(Opcode::ConvertF32S32, vec![Value::ImmU32(0xFFFF_FFFE)]);

        let artifact = emit_msl(&program, &Profile::default(), &RuntimeInfo::default()).unwrap();
        let source = &artifact.source.source;
        assert!(
            source.contains("bool v_0_0 = as_type<int>(0xFFFFFFFFu) < as_type<int>(0x00000001u);")
        );
        assert!(source.contains("uint v_0_1 = (v_0_0) ? (0x00000007u) : (0x00000009u);"));
        assert!(source.contains("float v_0_2 = as_type<float>(v_0_1);"));
        assert!(source.contains("uint v_0_3 = as_type<uint>(int(as_type<float>(0xC0000000u)));"));
        assert!(source.contains("float v_0_4 = float(as_type<int>(0xFFFFFFFEu));"));
    }

    #[test]
    fn emits_bitfield_operations_and_maxwell_msb_sentinels() {
        let mut program = empty_program(Stage::VertexB);
        {
            let mut emitter = Emitter::new(&mut program, 0);
            let extract = emitter.bit_field_s_extract(
                Value::ImmU32(0x8000_0000),
                Value::ImmU32(8),
                Value::ImmU32(16),
            );
            emitter.get_zero_from_op(extract);
            emitter.get_sign_from_op(extract);
            emitter.bit_field_insert(
                Value::ImmU32(0xFFFF_0000),
                Value::ImmU32(0x1234_5678),
                Value::ImmU32(4),
                Value::ImmU32(8),
            );
            emitter.bit_reverse_32(Value::ImmU32(1));
            emitter.bit_count_32(Value::ImmU32(0xF0F0_0000));
            emitter.find_s_msb_32(Value::ImmU32(u32::MAX));
            emitter.find_u_msb_32(Value::ImmU32(0));
        }

        let artifact = emit_msl(&program, &Profile::default(), &RuntimeInfo::default()).unwrap();
        let source = &artifact.source.source;
        assert!(source.contains(
            "as_type<uint>(extract_bits(as_type<int>(0x80000000u), 0x00000008u, 0x00000010u))"
        ));
        assert!(source.contains("bool v_0_1 = (v_0_0) == 0u;"));
        assert!(source.contains("bool v_0_2 = as_type<int>(v_0_0) < 0;"));
        assert!(source.contains("insert_bits(0xFFFF0000u, 0x12345678u, 0x00000004u, 0x00000008u)"));
        assert!(source.contains("reverse_bits(0x00000001u)"));
        assert!(source.contains("popcount(0xF0F00000u)"));
        assert!(source.contains(
            "31u - clz((as_type<int>(0xFFFFFFFFu) < 0 ? ~(0xFFFFFFFFu) : (0xFFFFFFFFu)))"
        ));
        assert!(source.contains("31u - clz(0x00000000u)"));
    }

    #[test]
    fn emits_vertex_position_and_fragment_color_outputs() {
        let mut vertex = empty_program(Stage::VertexB);
        vertex.info.stores.set(28, true);
        vertex.blocks[0].append_new_inst(
            Opcode::SetAttribute,
            vec![
                Value::Attribute(crate::ir::Attribute::POSITION_X),
                Value::ImmF32(1.0),
                Value::ImmU32(0),
            ],
        );
        let vertex = emit_msl(&vertex, &Profile::default(), &RuntimeInfo::default()).unwrap();
        assert!(vertex
            .source
            .source
            .contains("output.position.x = as_type<float>(0x3F800000u);"));

        let mut fragment = empty_program(Stage::Fragment);
        fragment.info.stores_frag_color[0] = true;
        fragment.blocks[0].append_new_inst(
            Opcode::SetFragColor,
            vec![Value::ImmU32(0), Value::ImmU32(2), Value::ImmF32(0.5)],
        );
        let fragment = emit_msl(&fragment, &Profile::default(), &RuntimeInfo::default()).unwrap();
        assert!(fragment
            .source
            .source
            .contains("float4 color0 [[color(0)]];"));
        assert!(fragment
            .source
            .source
            .contains("output.color0.z = as_type<float>(0x3F000000u);"));
    }

    #[test]
    fn emits_storage_image_access_and_binding_abi() {
        let mut profile = Profile::default();
        profile.support_typeless_image_loads = true;
        let options = MslOptions {
            supports_read_write_textures: true,
            ..MslOptions::default()
        };

        let integer = storage_image_program(TextureType::ColorArray2D, 3, true, true, true);
        let artifact =
            emit_msl_with_options(&integer, &profile, &RuntimeInfo::default(), &options).unwrap();
        assert_eq!(artifact.bindings.texture_count, 3);
        assert_eq!(artifact.bindings.resources.len(), 1);
        assert_eq!(
            artifact.bindings.resources[0].kind,
            MslResourceKind::StorageImage
        );
        assert_eq!(artifact.bindings.resources[0].binding, 0);
        assert_eq!(artifact.bindings.resources[0].texture_index, 0);
        assert_eq!(artifact.bindings.resources[0].count.unwrap().get(), 3);
        assert!(artifact
            .source
            .source
            .contains("array<texture2d_array<uint, access::read_write>, 3> img0 [[texture(0)]]"));
        assert!(artifact.source.source.contains("img0[0x00000002u].read("));
        assert!(artifact.source.source.contains("img0[0x00000002u].write("));

        let float = storage_image_program(TextureType::Color3D, 1, false, true, true);
        let artifact =
            emit_msl_with_options(&float, &profile, &RuntimeInfo::default(), &options).unwrap();
        assert!(artifact
            .source
            .source
            .contains("texture3d<float, access::read_write> img0 [[texture(0)]]"));
        assert!(artifact.source.source.contains("as_type<uint4>(img0.read("));
        assert!(artifact
            .source
            .source
            .contains("img0.write(as_type<float4>("));
    }

    #[test]
    fn storage_image_access_qualifiers_follow_descriptor_usage() {
        let read = storage_image_program(TextureType::Color2D, 1, true, true, false);
        let read = emit_msl(&read, &Profile::default(), &RuntimeInfo::default()).unwrap();
        assert!(read
            .source
            .source
            .contains("texture2d<uint, access::read> img0"));

        let write = storage_image_program(TextureType::Color2D, 1, true, false, true);
        let write = emit_msl(&write, &Profile::default(), &RuntimeInfo::default()).unwrap();
        assert!(write
            .source
            .source
            .contains("texture2d<uint, access::write> img0"));

        let read_write = storage_image_program(TextureType::Color2D, 1, true, true, true);
        assert_eq!(
            emit_msl(&read_write, &Profile::default(), &RuntimeInfo::default()),
            Err(MslError::UnsupportedProgramFeature(
                "read/write storage image on this Metal device"
            ))
        );
    }

    #[test]
    fn unsupported_typeless_image_read_matches_upstream_zero() {
        let program = storage_image_program(TextureType::Color2D, 1, false, true, false);
        let artifact = emit_msl(&program, &Profile::default(), &RuntimeInfo::default()).unwrap();
        assert!(artifact.source.source.contains("= uint4(0u);"));
        assert!(!artifact.source.source.contains(".read("));
    }

    #[test]
    fn emits_native_texture_atomics_and_signed_reinterpretation() {
        let program = storage_image_atomic_program(
            Value::ImmU32(0),
            &[
                Opcode::ImageAtomicIAdd32,
                Opcode::ImageAtomicSMin32,
                Opcode::ImageAtomicUMin32,
                Opcode::ImageAtomicSMax32,
                Opcode::ImageAtomicUMax32,
                Opcode::ImageAtomicAnd32,
                Opcode::ImageAtomicOr32,
                Opcode::ImageAtomicXor32,
                Opcode::ImageAtomicExchange32,
            ],
        );
        let options = MslOptions {
            language_version: MslVersion::V3_1,
            supports_read_write_textures: true,
            supports_texture_atomics: true,
            ..MslOptions::default()
        };
        let artifact = emit_msl_with_options(
            &program,
            &Profile::default(),
            &RuntimeInfo::default(),
            &options,
        )
        .unwrap();
        let source = &artifact.source.source;

        for method in [
            "atomic_fetch_add",
            "atomic_fetch_min",
            "atomic_fetch_max",
            "atomic_fetch_and",
            "atomic_fetch_or",
            "atomic_fetch_xor",
            "atomic_exchange",
        ] {
            assert!(source.contains(method), "missing {method} in:\n{source}");
        }
        assert!(source.contains("texture2d<uint, access::read_write> img0"));
        assert!(source.contains("spvTextureCast<texture2d<int, access::read_write>>(img0)"));
        assert!(source.contains("as_type<int>(0x80000001u)"));
        assert!(source.contains("as_type<uint>(spvTextureCast"));
        assert!(source.contains("reinterpret_cast<thread const T&>(image)"));
        assert_eq!(source.matches(").x").count(), 9);
    }

    #[test]
    fn texture_atomics_require_a_capable_device_and_zero_descriptor_index() {
        let program = storage_image_atomic_program(Value::ImmU32(0), &[Opcode::ImageAtomicIAdd32]);
        let msl_2_3 = MslOptions {
            supports_read_write_textures: true,
            supports_texture_atomics: true,
            ..MslOptions::default()
        };
        assert_eq!(
            emit_msl_with_options(
                &program,
                &Profile::default(),
                &RuntimeInfo::default(),
                &msl_2_3,
            ),
            Err(MslError::UnsupportedProgramFeature(
                "texture atomics on this Metal device"
            ))
        );

        let missing_device_capability = MslOptions {
            language_version: MslVersion::V3_1,
            supports_read_write_textures: true,
            supports_texture_atomics: false,
            ..MslOptions::default()
        };
        assert_eq!(
            emit_msl_with_options(
                &program,
                &Profile::default(),
                &RuntimeInfo::default(),
                &missing_device_capability,
            ),
            Err(MslError::UnsupportedProgramFeature(
                "texture atomics on this Metal device"
            ))
        );

        let options = MslOptions {
            language_version: MslVersion::V3_1,
            supports_read_write_textures: true,
            supports_texture_atomics: true,
            ..MslOptions::default()
        };
        let indexed = storage_image_atomic_program(Value::ImmU32(1), &[Opcode::ImageAtomicIAdd32]);
        assert_eq!(
            emit_msl_with_options(
                &indexed,
                &Profile::default(),
                &RuntimeInfo::default(),
                &options,
            ),
            Err(MslError::UnsupportedProgramFeature(
                "storage-image atomic descriptor indexing"
            ))
        );
    }

    #[test]
    fn texture_atomic_inc_and_dec_match_upstream_unimplemented_state() {
        let options = MslOptions {
            language_version: MslVersion::V3_1,
            supports_read_write_textures: true,
            supports_texture_atomics: true,
            ..MslOptions::default()
        };
        for opcode in [Opcode::ImageAtomicInc32, Opcode::ImageAtomicDec32] {
            let program = storage_image_atomic_program(Value::ImmU32(0), &[opcode]);
            assert!(matches!(
                emit_msl_with_options(
                    &program,
                    &Profile::default(),
                    &RuntimeInfo::default(),
                    &options,
                ),
                Err(MslError::UnsupportedOpcode { opcode: found, .. }) if found == opcode
            ));
        }
    }
}

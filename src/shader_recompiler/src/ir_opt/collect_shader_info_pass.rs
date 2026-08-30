// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Collect shader info pass — scan IR to determine resource usage.
//!
//! Matches upstream `collect_shader_info_pass.cpp`.
//!
//! Scans all instructions to determine which constant buffers, textures,
//! generic attributes, and storage buffers are used. Populates `Info`
//! via `VaryingState` for loads/stores (upstream-faithful pattern).

use crate::ir::opcodes::Opcode;
use crate::ir::program::{CbufDescriptor, Program, TexDescriptor};
use crate::ir::types::{FmzMode, FpControl, TextureInstInfo, Type};
use crate::ir::value::{Attribute, Value};
use crate::program_header::{PixelImap, ProgramHeader};
use crate::shader_info::{ImageFormat, Info, TextureType};

const NVN_DESCRIPTOR_SIZE: u32 = 0x10;
const NUM_NVN_BUFFERS: u32 = 16;

fn nvn_buffer_base(stage: crate::ir::types::ShaderStage) -> u32 {
    use crate::ir::types::ShaderStage;

    match stage {
        ShaderStage::VertexA | ShaderStage::VertexB => 0x110,
        ShaderStage::TessellationControl => 0x210,
        ShaderStage::TessellationEval | ShaderStage::Compute => 0x310,
        ShaderStage::Geometry => 0x410,
        ShaderStage::Fragment => 0x510,
    }
}

/// Port of upstream `CheckCBufNVN`.
fn check_cbuf_nvn(info: &mut Info, inst: &crate::ir::instruction::Inst) {
    let Some(cbuf_index) = inst.args.first() else {
        return;
    };
    if !cbuf_index.is_immediate() {
        info.nvn_buffer_used = u16::MAX;
        return;
    }
    if cbuf_index.imm_u32() != 0 {
        return;
    }
    let Some(cbuf_offset) = inst.args.get(1) else {
        return;
    };
    if !cbuf_offset.is_immediate() {
        info.nvn_buffer_used = u16::MAX;
        return;
    }
    let offset = cbuf_offset.imm_u32();
    let upper_limit = info.nvn_buffer_base + NVN_DESCRIPTOR_SIZE * NUM_NVN_BUFFERS;
    if offset >= info.nvn_buffer_base && offset < upper_limit {
        let nvn_index = (offset - info.nvn_buffer_base) / NVN_DESCRIPTOR_SIZE;
        info.nvn_buffer_used |= 1u16 << nvn_index;
    }
}

fn visit_usages(info: &mut Info, opcode: Opcode) {
    if matches!(
        opcode,
        Opcode::CompositeConstructF16x2
            | Opcode::CompositeConstructF16x3
            | Opcode::CompositeConstructF16x4
            | Opcode::CompositeExtractF16x2
            | Opcode::CompositeExtractF16x3
            | Opcode::CompositeExtractF16x4
            | Opcode::CompositeInsertF16x2
            | Opcode::CompositeInsertF16x3
            | Opcode::CompositeInsertF16x4
            | Opcode::SelectF16
            | Opcode::BitCastU16F16
            | Opcode::BitCastF16U16
            | Opcode::PackFloat2x16
            | Opcode::UnpackFloat2x16
            | Opcode::ConvertS16F16
            | Opcode::ConvertS32F16
            | Opcode::ConvertS64F16
            | Opcode::ConvertU16F16
            | Opcode::ConvertU32F16
            | Opcode::ConvertU64F16
            | Opcode::ConvertF16S8
            | Opcode::ConvertF16S16
            | Opcode::ConvertF16S32
            | Opcode::ConvertF16S64
            | Opcode::ConvertF16U8
            | Opcode::ConvertF16U16
            | Opcode::ConvertF16U32
            | Opcode::ConvertF16U64
            | Opcode::ConvertF16F32
            | Opcode::ConvertF32F16
            | Opcode::FPAbs16
            | Opcode::FPAdd16
            | Opcode::FPCeil16
            | Opcode::FPFloor16
            | Opcode::FPFma16
            | Opcode::FPMul16
            | Opcode::FPNeg16
            | Opcode::FPRoundEven16
            | Opcode::FPSaturate16
            | Opcode::FPClamp16
            | Opcode::FPTrunc16
            | Opcode::FPOrdEqual16
            | Opcode::FPUnordEqual16
            | Opcode::FPOrdNotEqual16
            | Opcode::FPUnordNotEqual16
            | Opcode::FPOrdLessThan16
            | Opcode::FPUnordLessThan16
            | Opcode::FPOrdGreaterThan16
            | Opcode::FPUnordGreaterThan16
            | Opcode::FPOrdLessThanEqual16
            | Opcode::FPUnordLessThanEqual16
            | Opcode::FPOrdGreaterThanEqual16
            | Opcode::FPUnordGreaterThanEqual16
            | Opcode::FPIsNan16
            | Opcode::GlobalAtomicAddF16x2
            | Opcode::GlobalAtomicMinF16x2
            | Opcode::GlobalAtomicMaxF16x2
            | Opcode::StorageAtomicAddF16x2
            | Opcode::StorageAtomicMinF16x2
            | Opcode::StorageAtomicMaxF16x2
    ) {
        info.uses_fp16 = true;
    }

    if matches!(
        opcode,
        Opcode::CompositeConstructF64x2
            | Opcode::CompositeConstructF64x3
            | Opcode::CompositeConstructF64x4
            | Opcode::CompositeExtractF64x2
            | Opcode::CompositeExtractF64x3
            | Opcode::CompositeExtractF64x4
            | Opcode::CompositeInsertF64x2
            | Opcode::CompositeInsertF64x3
            | Opcode::CompositeInsertF64x4
            | Opcode::SelectF64
            | Opcode::BitCastU64F64
            | Opcode::BitCastF64U64
            | Opcode::PackDouble2x32
            | Opcode::UnpackDouble2x32
            | Opcode::FPAbs64
            | Opcode::FPAdd64
            | Opcode::FPCeil64
            | Opcode::FPFloor64
            | Opcode::FPFma64
            | Opcode::FPMax64
            | Opcode::FPMin64
            | Opcode::FPMul64
            | Opcode::FPNeg64
            | Opcode::FPRecip64
            | Opcode::FPRecipSqrt64
            | Opcode::FPRoundEven64
            | Opcode::FPSaturate64
            | Opcode::FPClamp64
            | Opcode::FPTrunc64
            | Opcode::FPOrdEqual64
            | Opcode::FPUnordEqual64
            | Opcode::FPOrdNotEqual64
            | Opcode::FPUnordNotEqual64
            | Opcode::FPOrdLessThan64
            | Opcode::FPUnordLessThan64
            | Opcode::FPOrdGreaterThan64
            | Opcode::FPUnordGreaterThan64
            | Opcode::FPOrdLessThanEqual64
            | Opcode::FPUnordLessThanEqual64
            | Opcode::FPOrdGreaterThanEqual64
            | Opcode::FPUnordGreaterThanEqual64
            | Opcode::FPIsNan64
            | Opcode::ConvertS16F64
            | Opcode::ConvertS32F64
            | Opcode::ConvertS64F64
            | Opcode::ConvertU16F64
            | Opcode::ConvertU32F64
            | Opcode::ConvertU64F64
            | Opcode::ConvertF32F64
            | Opcode::ConvertF64F32
            | Opcode::ConvertF64S8
            | Opcode::ConvertF64S16
            | Opcode::ConvertF64S32
            | Opcode::ConvertF64S64
            | Opcode::ConvertF64U8
            | Opcode::ConvertF64U16
            | Opcode::ConvertF64U32
            | Opcode::ConvertF64U64
    ) {
        info.uses_fp64 = true;
    }

    if matches!(
        opcode,
        Opcode::GetCbufU8
            | Opcode::GetCbufS8
            | Opcode::UndefU8
            | Opcode::LoadGlobalU8
            | Opcode::LoadGlobalS8
            | Opcode::WriteGlobalU8
            | Opcode::WriteGlobalS8
            | Opcode::LoadStorageU8
            | Opcode::LoadStorageS8
            | Opcode::WriteStorageU8
            | Opcode::WriteStorageS8
            | Opcode::LoadSharedU8
            | Opcode::LoadSharedS8
            | Opcode::WriteSharedU8
            | Opcode::SelectU8
            | Opcode::ConvertF16S8
            | Opcode::ConvertF16U8
            | Opcode::ConvertF32S8
            | Opcode::ConvertF32U8
            | Opcode::ConvertF64S8
            | Opcode::ConvertF64U8
    ) {
        info.uses_int8 = true;
    }

    if matches!(
        opcode,
        Opcode::GetCbufU16
            | Opcode::GetCbufS16
            | Opcode::UndefU16
            | Opcode::LoadGlobalU16
            | Opcode::LoadGlobalS16
            | Opcode::WriteGlobalU16
            | Opcode::WriteGlobalS16
            | Opcode::LoadStorageU16
            | Opcode::LoadStorageS16
            | Opcode::WriteStorageU16
            | Opcode::WriteStorageS16
            | Opcode::LoadSharedU16
            | Opcode::LoadSharedS16
            | Opcode::WriteSharedU16
            | Opcode::SelectU16
            | Opcode::BitCastU16F16
            | Opcode::BitCastF16U16
            | Opcode::ConvertS16F16
            | Opcode::ConvertS16F32
            | Opcode::ConvertS16F64
            | Opcode::ConvertU16F16
            | Opcode::ConvertU16F32
            | Opcode::ConvertU16F64
            | Opcode::ConvertF16S16
            | Opcode::ConvertF16U16
            | Opcode::ConvertF32S16
            | Opcode::ConvertF32U16
            | Opcode::ConvertF64S16
            | Opcode::ConvertF64U16
    ) {
        info.uses_int16 = true;
    }

    if matches!(
        opcode,
        Opcode::UndefU64
            | Opcode::LoadGlobalU8
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
            | Opcode::SelectU64
            | Opcode::BitCastU64F64
            | Opcode::BitCastF64U64
            | Opcode::PackUint2x32
            | Opcode::UnpackUint2x32
            | Opcode::IAdd64
            | Opcode::ISub64
            | Opcode::INeg64
            | Opcode::ShiftLeftLogical64
            | Opcode::ShiftRightLogical64
            | Opcode::ShiftRightArithmetic64
            | Opcode::ConvertS64F16
            | Opcode::ConvertS64F32
            | Opcode::ConvertS64F64
            | Opcode::ConvertU64F16
            | Opcode::ConvertU64F32
            | Opcode::ConvertU64F64
            | Opcode::ConvertU64U32
            | Opcode::ConvertU32U64
            | Opcode::ConvertF16U64
            | Opcode::ConvertF32U64
            | Opcode::ConvertF64U64
            | Opcode::SharedAtomicExchange64
            | Opcode::GlobalAtomicIAdd64
            | Opcode::GlobalAtomicSMin64
            | Opcode::GlobalAtomicUMin64
            | Opcode::GlobalAtomicSMax64
            | Opcode::GlobalAtomicUMax64
            | Opcode::GlobalAtomicAnd64
            | Opcode::GlobalAtomicOr64
            | Opcode::GlobalAtomicXor64
            | Opcode::GlobalAtomicExchange64
            | Opcode::StorageAtomicIAdd64
            | Opcode::StorageAtomicSMin64
            | Opcode::StorageAtomicUMin64
            | Opcode::StorageAtomicSMax64
            | Opcode::StorageAtomicUMax64
            | Opcode::StorageAtomicAnd64
            | Opcode::StorageAtomicOr64
            | Opcode::StorageAtomicXor64
            | Opcode::StorageAtomicExchange64
    ) {
        info.uses_int64 = true;
    }
}

/// Port of upstream `VisitFpModifiers`.
fn visit_fp_modifiers(info: &mut Info, opcode: Opcode, flags: u32) {
    let control = FpControl::from_u32(flags);
    if matches!(
        opcode,
        Opcode::FPAdd16
            | Opcode::FPFma16
            | Opcode::FPMul16
            | Opcode::FPRoundEven16
            | Opcode::FPFloor16
            | Opcode::FPCeil16
            | Opcode::FPTrunc16
    ) {
        match control.fmz_mode {
            FmzMode::DontCare => {}
            FmzMode::FTZ | FmzMode::FMZ => info.uses_fp16_denorms_flush = true,
            FmzMode::None => info.uses_fp16_denorms_preserve = true,
        }
        return;
    }

    if matches!(
        opcode,
        Opcode::FPAdd32
            | Opcode::FPFma32
            | Opcode::FPMul32
            | Opcode::FPRoundEven32
            | Opcode::FPFloor32
            | Opcode::FPCeil32
            | Opcode::FPTrunc32
            | Opcode::FPOrdEqual32
            | Opcode::FPUnordEqual32
            | Opcode::FPOrdNotEqual32
            | Opcode::FPUnordNotEqual32
            | Opcode::FPOrdLessThan32
            | Opcode::FPUnordLessThan32
            | Opcode::FPOrdGreaterThan32
            | Opcode::FPUnordGreaterThan32
            | Opcode::FPOrdLessThanEqual32
            | Opcode::FPUnordLessThanEqual32
            | Opcode::FPOrdGreaterThanEqual32
            | Opcode::FPUnordGreaterThanEqual32
            | Opcode::ConvertF16F32
            | Opcode::ConvertF64F32
    ) {
        match control.fmz_mode {
            FmzMode::DontCare => {}
            FmzMode::FTZ | FmzMode::FMZ => info.uses_fp32_denorms_flush = true,
            FmzMode::None => info.uses_fp32_denorms_preserve = true,
        }
    }
}

fn cbuf_type_bit(opcode: Opcode) -> u32 {
    match opcode {
        Opcode::GetCbufU8 | Opcode::GetCbufS8 => Type::U8 as u32,
        Opcode::GetCbufU16 | Opcode::GetCbufS16 => Type::U16 as u32,
        Opcode::GetCbufU32 => Type::U32 as u32,
        Opcode::GetCbufF32 => Type::F32 as u32,
        Opcode::GetCbufU32x2 => Type::U32x2 as u32,
        _ => 0,
    }
}

fn cbuf_element_size(opcode: Opcode) -> u32 {
    match opcode {
        Opcode::GetCbufU8 | Opcode::GetCbufS8 => 1,
        Opcode::GetCbufU16 | Opcode::GetCbufS16 => 2,
        Opcode::GetCbufU32 | Opcode::GetCbufF32 => 4,
        Opcode::GetCbufU32x2 => 8,
        _ => 0,
    }
}

/// Collect shader resource usage information.
pub fn collect_shader_info_pass(program: &mut Program) {
    let mut uses_local_memory = false;

    program.info.nvn_buffer_base = nvn_buffer_base(program.stage);

    // Rebuild cbuf usage from the optimized IR. Frontend translation can
    // conservatively register cbufs before DCE removes the actual load; keeping
    // those stale mask bits makes OpenGL bind zero-sized UBO ranges.
    program.info.constant_buffer_mask = 0;
    program.info.constant_buffer_used_sizes = [0; crate::shader_info::Info::MAX_CBUFS];
    program.info.constant_buffer_descriptors.clear();
    program.info.used_constant_buffer_types = 0;

    let mut cbuf_set = std::collections::BTreeSet::<u32>::new();
    let mut tex_set = std::collections::BTreeSet::<u32>::new();

    for block in &program.blocks {
        for inst in block.iter() {
            visit_usages(&mut program.info, inst.opcode);
            visit_fp_modifiers(&mut program.info, inst.opcode, inst.flags);
            if matches!(
                inst.opcode,
                Opcode::SharedAtomicSMin32 | Opcode::StorageAtomicSMin32
            ) {
                program.info.uses_atomic_s32_min = true;
            }
            if matches!(
                inst.opcode,
                Opcode::SharedAtomicSMax32 | Opcode::StorageAtomicSMax32
            ) {
                program.info.uses_atomic_s32_max = true;
            }
            match inst.opcode {
                Opcode::GlobalAtomicInc32 | Opcode::StorageAtomicInc32 => {
                    program.info.used_storage_buffer_types |= Type::U32 as u32;
                    program.info.uses_global_increment = true;
                }
                Opcode::GlobalAtomicDec32 | Opcode::StorageAtomicDec32 => {
                    program.info.used_storage_buffer_types |= Type::U32 as u32;
                    program.info.uses_global_decrement = true;
                }
                Opcode::GlobalAtomicAddF32 | Opcode::StorageAtomicAddF32 => {
                    program.info.used_storage_buffer_types |= Type::U32 as u32;
                    program.info.uses_atomic_f32_add = true;
                }
                Opcode::GlobalAtomicAddF16x2 | Opcode::StorageAtomicAddF16x2 => {
                    program.info.used_storage_buffer_types |= Type::U32 as u32;
                    program.info.uses_atomic_f16x2_add = true;
                }
                Opcode::GlobalAtomicAddF32x2 | Opcode::StorageAtomicAddF32x2 => {
                    program.info.used_storage_buffer_types |= Type::U32 as u32;
                    program.info.uses_atomic_f32x2_add = true;
                }
                Opcode::GlobalAtomicMinF16x2 | Opcode::StorageAtomicMinF16x2 => {
                    program.info.used_storage_buffer_types |= Type::U32 as u32;
                    program.info.uses_atomic_f16x2_min = true;
                }
                Opcode::GlobalAtomicMinF32x2 | Opcode::StorageAtomicMinF32x2 => {
                    program.info.used_storage_buffer_types |= Type::U32 as u32;
                    program.info.uses_atomic_f32x2_min = true;
                }
                Opcode::GlobalAtomicMaxF16x2 | Opcode::StorageAtomicMaxF16x2 => {
                    program.info.used_storage_buffer_types |= Type::U32 as u32;
                    program.info.uses_atomic_f16x2_max = true;
                }
                Opcode::GlobalAtomicMaxF32x2 | Opcode::StorageAtomicMaxF32x2 => {
                    program.info.used_storage_buffer_types |= Type::U32 as u32;
                    program.info.uses_atomic_f32x2_max = true;
                }
                Opcode::GlobalAtomicIAdd64
                | Opcode::GlobalAtomicSMin64
                | Opcode::GlobalAtomicUMin64
                | Opcode::GlobalAtomicSMax64
                | Opcode::GlobalAtomicUMax64
                | Opcode::GlobalAtomicAnd64
                | Opcode::GlobalAtomicOr64
                | Opcode::GlobalAtomicXor64
                | Opcode::GlobalAtomicExchange64
                | Opcode::StorageAtomicIAdd64
                | Opcode::StorageAtomicSMin64
                | Opcode::StorageAtomicUMin64
                | Opcode::StorageAtomicSMax64
                | Opcode::StorageAtomicUMax64
                | Opcode::StorageAtomicAnd64
                | Opcode::StorageAtomicOr64
                | Opcode::StorageAtomicXor64
                | Opcode::StorageAtomicExchange64 => {
                    program.info.used_storage_buffer_types |=
                        (Type::U64 as u32) | (Type::U32x2 as u32);
                    program.info.uses_int64 = true;
                    program.info.uses_int64_bit_atomics = true;
                }
                _ => {}
            }
            match inst.opcode {
                // Constant buffer access
                Opcode::GetCbufU32
                | Opcode::GetCbufF32
                | Opcode::GetCbufU8
                | Opcode::GetCbufS8
                | Opcode::GetCbufU16
                | Opcode::GetCbufS16
                | Opcode::GetCbufU32x2 => {
                    check_cbuf_nvn(&mut program.info, inst);
                    if let Some(&Value::ImmU32(idx)) = inst.args.first() {
                        cbuf_set.insert(idx);
                        program.info.constant_buffer_mask |= 1u32 << idx;
                        let element_size = cbuf_element_size(inst.opcode);
                        let size = &mut program.info.constant_buffer_used_sizes[idx as usize];
                        if let Some(Value::ImmU32(offset)) = inst.args.get(1) {
                            *size = (*size).max(offset + element_size).div_ceil(16) * 16;
                        } else {
                            *size = 0x10000;
                        }
                        program.info.used_constant_buffer_types |= cbuf_type_bit(inst.opcode);
                    }
                }

                // Attribute loads → VaryingState
                Opcode::GetAttribute | Opcode::GetAttributeU32 => {
                    if let Some(Value::Attribute(attr)) = inst.args.first() {
                        program.info.loads.set(attr.0 as usize, true);
                    }
                }

                // Attribute stores → VaryingState
                Opcode::SetAttribute => {
                    if let Some(Value::Attribute(attr)) = inst.args.first() {
                        program.info.stores.set(attr.0 as usize, true);
                    }
                }
                Opcode::GetAttributeIndexed => {
                    program.info.loads_indexed_attributes = true;
                }
                Opcode::SetAttributeIndexed => {
                    program.info.stores_indexed_attributes = true;
                }
                Opcode::GetPatch => {
                    if let Some(Value::Patch(patch)) = inst.args.first() {
                        if patch.is_generic() {
                            program.info.uses_patches[patch.generic_index() as usize] = true;
                        }
                    }
                }
                Opcode::SetPatch => {
                    if let Some(Value::Patch(patch)) = inst.args.first() {
                        if patch.is_generic() {
                            program.info.uses_patches[patch.generic_index() as usize] = true;
                        } else {
                            match *patch {
                                crate::ir::value::Patch::TESS_LOD_LEFT
                                | crate::ir::value::Patch::TESS_LOD_TOP
                                | crate::ir::value::Patch::TESS_LOD_RIGHT
                                | crate::ir::value::Patch::TESS_LOD_BOTTOM => {
                                    program.info.stores_tess_level_outer = true;
                                }
                                crate::ir::value::Patch::TESS_LOD_INTERIOR_U
                                | crate::ir::value::Patch::TESS_LOD_INTERIOR_V => {
                                    program.info.stores_tess_level_inner = true;
                                }
                                _ => {}
                            }
                        }
                    }
                }

                // Fragment color output
                Opcode::SetFragColor => {
                    if let Some(Value::ImmU32(render_target)) = inst.args.first() {
                        if (*render_target as usize) < program.info.stores_frag_color.len() {
                            program.info.stores_frag_color[*render_target as usize] = true;
                        }
                    }
                }
                Opcode::SetSampleMask => {
                    program.info.stores_sample_mask = true;
                }
                Opcode::SetFragDepth => {
                    program.info.stores_frag_depth = true;
                }
                Opcode::WorkgroupId => {
                    program.info.uses_workgroup_id = true;
                }
                Opcode::LocalInvocationId => {
                    program.info.uses_local_invocation_id = true;
                }
                Opcode::InvocationId => {
                    program.info.uses_invocation_id = true;
                }
                Opcode::InvocationInfo => {
                    program.info.uses_invocation_info = true;
                }
                Opcode::SampleId => {
                    program.info.uses_sample_id = true;
                }
                Opcode::IsHelperInvocation => {
                    program.info.uses_is_helper_invocation = true;
                }
                Opcode::ResolutionDownFactor | Opcode::IsTextureScaled | Opcode::IsImageScaled => {
                    program.info.uses_rescaling_uniform = true;
                }
                Opcode::RenderArea => {
                    program.info.uses_render_area = true;
                }
                Opcode::DemoteToHelperInvocation => {
                    program.info.uses_demote_to_helper_invocation = true;
                }
                Opcode::LaneId => {
                    program.info.uses_subgroup_invocation_id = true;
                }
                Opcode::ShuffleIndex
                | Opcode::ShuffleUp
                | Opcode::ShuffleDown
                | Opcode::ShuffleButterfly => {
                    program.info.uses_subgroup_shuffles = true;
                }
                Opcode::SubgroupEqMask
                | Opcode::SubgroupLtMask
                | Opcode::SubgroupLeMask
                | Opcode::SubgroupGtMask
                | Opcode::SubgroupGeMask => {
                    program.info.uses_subgroup_mask = true;
                }
                Opcode::VoteAll | Opcode::VoteAny | Opcode::VoteEqual | Opcode::SubgroupBallot => {
                    program.info.uses_subgroup_vote = true;
                }
                Opcode::FSwizzleAdd => {
                    program.info.uses_fswzadd = true;
                }
                Opcode::DPdxFine | Opcode::DPdyFine | Opcode::DPdxCoarse | Opcode::DPdyCoarse => {
                    program.info.uses_derivatives = true;
                }

                // Texture access
                Opcode::BindlessImageSampleImplicitLod
                | Opcode::BindlessImageSampleExplicitLod
                | Opcode::BindlessImageSampleDrefImplicitLod
                | Opcode::BindlessImageSampleDrefExplicitLod
                | Opcode::BindlessImageGather
                | Opcode::BindlessImageGatherDref
                | Opcode::BindlessImageFetch
                | Opcode::BindlessImageQueryDimensions
                | Opcode::BindlessImageQueryLod
                | Opcode::BindlessImageGradient
                | Opcode::BoundImageSampleImplicitLod
                | Opcode::BoundImageSampleExplicitLod
                | Opcode::BoundImageSampleDrefImplicitLod
                | Opcode::BoundImageSampleDrefExplicitLod
                | Opcode::BoundImageGather
                | Opcode::BoundImageGatherDref
                | Opcode::BoundImageFetch
                | Opcode::BoundImageQueryDimensions
                | Opcode::BoundImageQueryLod
                | Opcode::BoundImageGradient
                | Opcode::ImageFetch
                | Opcode::ImageQueryDimensions
                | Opcode::ImageGradient
                | Opcode::ImageGather
                | Opcode::ImageGatherDref => {
                    if let Some(&Value::ImmU32(idx)) = inst.args.first() {
                        tex_set.insert(idx);
                    }
                    let flags = TextureInstInfo::from_u32(inst.flags);
                    let ty = TextureType::from_u8(flags.texture_type);
                    program.info.uses_sampled_1d |=
                        ty == TextureType::Color1D || ty == TextureType::ColorArray1D;
                    program.info.uses_sparse_residency |= inst
                        .get_associated_pseudo(Opcode::GetSparseFromOp)
                        .is_some();
                }
                Opcode::ImageSampleImplicitLod
                | Opcode::ImageSampleExplicitLod
                | Opcode::ImageSampleDrefImplicitLod
                | Opcode::ImageSampleDrefExplicitLod
                | Opcode::ImageQueryLod => {
                    if let Some(&Value::ImmU32(idx)) = inst.args.first() {
                        tex_set.insert(idx);
                    }
                    let flags = TextureInstInfo::from_u32(inst.flags);
                    let ty = TextureType::from_u8(flags.texture_type);
                    program.info.uses_sampled_1d |=
                        ty == TextureType::Color1D || ty == TextureType::ColorArray1D;
                    program.info.uses_shadow_lod |= flags.is_depth;
                    program.info.uses_sparse_residency |= inst
                        .get_associated_pseudo(Opcode::GetSparseFromOp)
                        .is_some();
                }
                Opcode::ImageRead => {
                    let flags = TextureInstInfo::from_u32(inst.flags);
                    let ty = TextureType::from_u8(flags.texture_type);
                    program.info.uses_typeless_image_reads |=
                        ImageFormat::from_u8(flags.image_format) == ImageFormat::Typeless;
                    program.info.uses_image_1d |=
                        ty == TextureType::Color1D || ty == TextureType::ColorArray1D;
                    program.info.uses_sparse_residency |= inst
                        .get_associated_pseudo(Opcode::GetSparseFromOp)
                        .is_some();
                }
                Opcode::ImageWrite => {
                    let flags = TextureInstInfo::from_u32(inst.flags);
                    let ty = TextureType::from_u8(flags.texture_type);
                    program.info.uses_typeless_image_writes |=
                        ImageFormat::from_u8(flags.image_format) == ImageFormat::Typeless;
                    program.info.uses_image_buffers |= ty == TextureType::Buffer;
                    program.info.uses_image_1d |=
                        ty == TextureType::Color1D || ty == TextureType::ColorArray1D;
                }

                // Local memory
                Opcode::LoadLocal | Opcode::WriteLocal => {
                    uses_local_memory = true;
                }

                Opcode::LoadStorageU8
                | Opcode::LoadStorageS8
                | Opcode::WriteStorageU8
                | Opcode::WriteStorageS8 => {
                    program.info.used_storage_buffer_types |= Type::U8 as u32;
                }
                Opcode::LoadStorageU16
                | Opcode::LoadStorageS16
                | Opcode::WriteStorageU16
                | Opcode::WriteStorageS16 => {
                    program.info.used_storage_buffer_types |= Type::U16 as u32;
                }
                Opcode::LoadStorage32
                | Opcode::WriteStorage32
                | Opcode::StorageAtomicIAdd32
                | Opcode::StorageAtomicSMin32
                | Opcode::StorageAtomicUMin32
                | Opcode::StorageAtomicSMax32
                | Opcode::StorageAtomicUMax32
                | Opcode::StorageAtomicAnd32
                | Opcode::StorageAtomicOr32
                | Opcode::StorageAtomicXor32
                | Opcode::StorageAtomicExchange32 => {
                    program.info.used_storage_buffer_types |= Type::U32 as u32;
                }
                Opcode::LoadStorage64
                | Opcode::WriteStorage64
                | Opcode::StorageAtomicIAdd32x2
                | Opcode::StorageAtomicSMin32x2
                | Opcode::StorageAtomicUMin32x2
                | Opcode::StorageAtomicSMax32x2
                | Opcode::StorageAtomicUMax32x2
                | Opcode::StorageAtomicAnd32x2
                | Opcode::StorageAtomicOr32x2
                | Opcode::StorageAtomicXor32x2
                | Opcode::StorageAtomicExchange32x2 => {
                    program.info.used_storage_buffer_types |= Type::U32x2 as u32;
                }
                Opcode::LoadStorage128 | Opcode::WriteStorage128 => {
                    program.info.used_storage_buffer_types |= Type::U32x4 as u32;
                }

                // Global memory
                Opcode::LoadGlobalU8
                | Opcode::LoadGlobalS8
                | Opcode::LoadGlobalU16
                | Opcode::LoadGlobalS16
                | Opcode::LoadGlobal32
                | Opcode::LoadGlobal64
                | Opcode::LoadGlobal128 => {
                    program.info.uses_int64 = true;
                    program.info.uses_global_memory = true;
                    program.info.used_constant_buffer_types |=
                        (Type::U32 as u32) | (Type::U32x2 as u32);
                    program.info.used_storage_buffer_types |=
                        (Type::U32 as u32) | (Type::U32x2 as u32) | (Type::U32x4 as u32);
                }
                Opcode::WriteGlobalU8
                | Opcode::WriteGlobalS8
                | Opcode::WriteGlobalU16
                | Opcode::WriteGlobalS16
                | Opcode::WriteGlobal32
                | Opcode::WriteGlobal64
                | Opcode::WriteGlobal128
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
                | Opcode::GlobalAtomicMaxF32x2 => {
                    program.info.stores_global_memory = true;
                    program.info.uses_int64 = true;
                    program.info.uses_global_memory = true;
                    program.info.used_constant_buffer_types |=
                        (Type::U32 as u32) | (Type::U32x2 as u32);
                    program.info.used_storage_buffer_types |=
                        (Type::U32 as u32) | (Type::U32x2 as u32) | (Type::U32x4 as u32);
                }
                Opcode::SharedAtomicInc32 => {
                    program.info.uses_shared_increment = true;
                }
                Opcode::SharedAtomicDec32 => {
                    program.info.uses_shared_decrement = true;
                }
                Opcode::SharedAtomicExchange64 => {
                    program.info.uses_int64 = true;
                    program.info.uses_int64_bit_atomics = true;
                }
                _ => {}
            }
        }
    }

    program.info.constant_buffer_descriptors = cbuf_set
        .into_iter()
        .map(|index| CbufDescriptor { index, count: 1 })
        .collect();

    if program.info.texture_descriptors.is_empty() {
        program.info.texture_descriptors = tex_set
            .into_iter()
            .map(|index| TexDescriptor {
                cbuf_index: index,
                texture_type: crate::shader_info::TextureType::Color2D,
                is_depth: false,
                is_multisample: false,
                is_integer: false,
                has_secondary: false,
                cbuf_offset: 0,
                shift_left: 0,
                secondary_cbuf_index: 0,
                secondary_cbuf_offset: 0,
                secondary_shift_left: 0,
                count: 1,
                size_shift: 0,
            })
            .collect();
    }

    if uses_local_memory {
        program.info.uses_local_memory = true;
        if program.local_memory_size == 0 {
            program.local_memory_size = 0x1000;
        }
    }
}

/// Header-aware variant of upstream `CollectShaderInfoPass`.
///
/// Upstream calls `GatherInfoFromHeader(env, info)` after scanning IR. The
/// Rust port keeps the env-less pass for tests and legacy call sites, while
/// shader-cache paths that own the SPH call this variant.
pub fn collect_shader_info_pass_with_sph(program: &mut Program, sph: &ProgramHeader) {
    collect_shader_info_pass(program);
    gather_info_from_header(program, sph);
}

fn gather_info_from_header(program: &mut Program, sph: &ProgramHeader) {
    use crate::ir::types::ShaderStage;

    if program.stage == ShaderStage::Compute {
        return;
    }

    if program.stage == ShaderStage::Fragment {
        if !program.info.loads_indexed_attributes {
            return;
        }
        for index in 0..32 {
            let mask = sph.ps_generic_input_map(index);
            for (element, imap) in mask.iter().enumerate() {
                program.info.loads.set(
                    Attribute::generic(index, element as u32).0 as usize,
                    *imap != PixelImap::Unused,
                );
            }
        }
        return;
    }

    if program.info.loads_indexed_attributes {
        for index in 0..32 {
            let mask = sph.vtg_input_generic(index as usize);
            for (element, used) in mask.iter().enumerate() {
                program
                    .info
                    .loads
                    .set(Attribute::generic(index, element as u32).0 as usize, *used);
            }
        }
        set_clip_distances(&mut program.info.loads, sph.vtg_imap_systemc());
        set_systemb_attributes(&mut program.info.loads, sph.vtg_imap_systemb());
        set_systemc_attributes(&mut program.info.loads, sph.vtg_imap_systemc());
    }

    if program.info.stores_indexed_attributes {
        for index in 0..32 {
            let mask = sph.vtg_output_generic(index as usize);
            for (element, used) in mask.iter().enumerate() {
                program
                    .info
                    .stores
                    .set(Attribute::generic(index, element as u32).0 as usize, *used);
            }
        }
        let clip_mask = sph.vtg_omap_systemc();
        set_clip_distances(&mut program.info.stores, clip_mask);
        for index in 0..8 {
            if ((clip_mask >> index) & 1) != 0 {
                program.info.used_clip_distances = index + 1;
            }
        }
        set_systemb_attributes(&mut program.info.stores, sph.vtg_omap_systemb());
        set_systemc_attributes(&mut program.info.stores, sph.vtg_omap_systemc());
    }
}

fn set_clip_distances(state: &mut crate::varying_state::VaryingState, mask: u16) {
    for index in 0..8 {
        state.set(
            (Attribute::CLIP_DISTANCE_0.0 + index) as usize,
            ((mask >> index) & 1) != 0,
        );
    }
}

fn set_systemb_attributes(state: &mut crate::varying_state::VaryingState, mask: u8) {
    const ATTRIBUTES: [Attribute; 8] = [
        Attribute::PRIMITIVE_ID,
        Attribute::LAYER,
        Attribute::VIEWPORT_INDEX,
        Attribute::POINT_SIZE,
        Attribute::POSITION_X,
        Attribute::POSITION_Y,
        Attribute::POSITION_Z,
        Attribute::POSITION_W,
    ];
    for (index, attribute) in ATTRIBUTES.iter().enumerate() {
        state.set(attribute.0 as usize, ((mask >> index) & 1) != 0);
    }
}

fn set_systemc_attributes(state: &mut crate::varying_state::VaryingState, mask: u16) {
    const FOG_COORDINATE: Attribute = Attribute(186);
    const ATTRIBUTES: [(Attribute, u16); 7] = [
        (Attribute::POINT_SPRITE_S, 1 << 8),
        (Attribute::POINT_SPRITE_T, 1 << 9),
        (FOG_COORDINATE, 1 << 10),
        (Attribute::TESSELLATION_EVALUATION_POINT_U, 1 << 12),
        (Attribute::TESSELLATION_EVALUATION_POINT_V, 1 << 13),
        (Attribute::INSTANCE_ID, 1 << 14),
        (Attribute::VERTEX_ID, 1 << 15),
    ];
    for (attribute, bit) in ATTRIBUTES {
        state.set(attribute.0 as usize, (mask & bit) != 0);
    }
}

#[cfg(test)]
mod tests {
    use super::{collect_shader_info_pass, collect_shader_info_pass_with_sph};
    use crate::ir::basic_block::Block;
    use crate::ir::instruction::Inst;
    use crate::ir::opcodes::Opcode;
    use crate::ir::program::Program;
    use crate::ir::types::{FmzMode, FpControl, ShaderStage, TextureInstInfo};
    use crate::ir::value::{Attribute, Value};
    use crate::program_header::ProgramHeader;
    use crate::shader_info::{ImageFormat, TextureType};

    #[test]
    fn collect_info_marks_scalar_width_usages_like_upstream() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        for (opcode, argument) in [
            (Opcode::UnpackFloat2x16, Value::ImmU32(0)),
            (Opcode::ConvertF64S8, Value::ImmU32(0)),
            (Opcode::ConvertF32U16, Value::ImmU32(0)),
            (Opcode::PackUint2x32, Value::ImmU32(0)),
        ] {
            program
                .block_mut(0)
                .append_inst(Inst::new(opcode, vec![argument]));
        }

        collect_shader_info_pass(&mut program);

        assert!(program.info.uses_fp16);
        assert!(program.info.uses_fp64);
        assert!(program.info.uses_int8);
        assert!(program.info.uses_int16);
        assert!(program.info.uses_int64);
    }

    #[test]
    fn collect_info_records_fp_denorm_modes_like_upstream() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        for (opcode, fmz_mode) in [
            (Opcode::FPAdd16, FmzMode::FTZ),
            (Opcode::FPFma16, FmzMode::None),
            (Opcode::FPMul32, FmzMode::FMZ),
            (Opcode::FPOrdEqual32, FmzMode::None),
        ] {
            let flags = FpControl {
                fmz_mode,
                ..Default::default()
            };
            program
                .block_mut(0)
                .append_inst(Inst::with_flags(opcode, Vec::new(), flags.to_u32()));
        }

        collect_shader_info_pass(&mut program);

        assert!(program.info.uses_fp16_denorms_flush);
        assert!(program.info.uses_fp16_denorms_preserve);
        assert!(program.info.uses_fp32_denorms_flush);
        assert!(program.info.uses_fp32_denorms_preserve);
    }

    #[test]
    fn collect_info_records_demote_usage_like_upstream() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        program
            .block_mut(0)
            .append_inst(Inst::new(Opcode::DemoteToHelperInvocation, vec![]));

        collect_shader_info_pass(&mut program);

        assert!(program.info.uses_demote_to_helper_invocation);
    }

    #[test]
    fn collect_info_records_all_subgroup_interface_users() {
        let mut program = Program::new(ShaderStage::Compute);
        program.blocks.push(Block::new());
        for opcode in [
            Opcode::LaneId,
            Opcode::ShuffleIndex,
            Opcode::SubgroupEqMask,
            Opcode::VoteAll,
            Opcode::FSwizzleAdd,
        ] {
            program
                .block_mut(0)
                .append_inst(Inst::new(opcode, Vec::new()));
        }

        collect_shader_info_pass(&mut program);

        assert!(program.info.uses_subgroup_invocation_id);
        assert!(program.info.uses_subgroup_shuffles);
        assert!(program.info.uses_subgroup_mask);
        assert!(program.info.uses_subgroup_vote);
        assert!(program.info.uses_fswzadd);
    }

    #[test]
    fn collect_info_tracks_stage_specific_nvn_driver_cbuf_descriptors() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        for offset in [0x510, 0x56f] {
            program.block_mut(0).append_inst(Inst::new(
                Opcode::GetCbufU32,
                vec![Value::ImmU32(0), Value::ImmU32(offset)],
            ));
        }
        program.block_mut(0).append_inst(Inst::new(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(1), Value::ImmU32(0x510)],
        ));

        collect_shader_info_pass(&mut program);

        assert_eq!(program.info.nvn_buffer_base, 0x510);
        assert_eq!(program.info.nvn_buffer_used, (1 << 0) | (1 << 5));
    }

    #[test]
    fn collect_info_marks_every_nvn_buffer_for_indirect_cbuf_indexing() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        program.block_mut(0).append_inst(Inst::new(
            Opcode::GetCbufU32,
            vec![
                Value::Inst(crate::ir::value::InstRef { block: 0, inst: 0 }),
                Value::ImmU32(0x110),
            ],
        ));

        collect_shader_info_pass(&mut program);

        assert_eq!(program.info.nvn_buffer_base, 0x110);
        assert_eq!(program.info.nvn_buffer_used, u16::MAX);
    }

    #[test]
    fn collect_info_header_sets_fragment_indexed_generic_loads() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        program.block_mut(0).append_inst(Inst::new(
            Opcode::GetAttributeIndexed,
            vec![Value::ImmU32(0), Value::ImmU32(0)],
        ));

        let mut sph = ProgramHeader::default();
        sph.raw[6] = 0b11_10_01_00;

        collect_shader_info_pass_with_sph(&mut program, &sph);

        assert!(!program.info.loads.get(Attribute::generic(0, 0).0 as usize));
        assert!(program.info.loads.get(Attribute::generic(0, 1).0 as usize));
        assert!(program.info.loads.get(Attribute::generic(0, 2).0 as usize));
        assert!(program.info.loads.get(Attribute::generic(0, 3).0 as usize));
    }

    #[test]
    fn collect_info_sets_signed_atomic_helper_flags() {
        let mut program = Program::new(ShaderStage::Compute);
        program.blocks.push(Block::new());
        program.block_mut(0).append_inst(Inst::new(
            Opcode::StorageAtomicSMin32,
            vec![Value::ImmU32(0), Value::ImmU32(16), Value::ImmU32(7)],
        ));
        program.block_mut(0).append_inst(Inst::new(
            Opcode::SharedAtomicSMax32,
            vec![Value::ImmU32(0), Value::ImmU32(7)],
        ));

        super::collect_shader_info_pass(&mut program);

        assert!(program.info.uses_atomic_s32_min);
        assert!(program.info.uses_atomic_s32_max);
    }

    #[test]
    fn collect_info_sets_shared_inc_dec_helper_flags() {
        let mut program = Program::new(ShaderStage::Compute);
        program.blocks.push(Block::new());
        program.block_mut(0).append_inst(Inst::new(
            Opcode::SharedAtomicInc32,
            vec![Value::ImmU32(0), Value::ImmU32(7)],
        ));
        program.block_mut(0).append_inst(Inst::new(
            Opcode::SharedAtomicDec32,
            vec![Value::ImmU32(4), Value::ImmU32(7)],
        ));

        super::collect_shader_info_pass(&mut program);

        assert!(program.info.uses_shared_increment);
        assert!(program.info.uses_shared_decrement);
    }

    #[test]
    fn collect_info_marks_depth_texture_samples_as_shadow_lod_users() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::ColorArray2D as u8,
            is_depth: true,
            ..Default::default()
        };
        program.block_mut(0).append_inst(Inst::with_flags(
            Opcode::ImageSampleDrefExplicitLod,
            vec![
                Value::ImmU32(3),
                Value::ImmU32(0),
                Value::ImmU32(0),
                Value::ImmU32(0),
            ],
            flags.to_u32(),
        ));

        super::collect_shader_info_pass(&mut program);

        assert!(program.info.uses_shadow_lod);
        assert_eq!(program.info.texture_descriptors.len(), 1);
        assert_eq!(program.info.texture_descriptors[0].cbuf_index, 3);
    }

    #[test]
    fn collect_info_records_typeless_image_access_capabilities() {
        let mut program = Program::new(ShaderStage::Compute);
        program.blocks.push(Block::new());
        let read = TextureInstInfo {
            texture_type: TextureType::Color1D as u8,
            image_format: ImageFormat::Typeless as u8,
            ..Default::default()
        };
        let write = TextureInstInfo {
            texture_type: TextureType::Buffer as u8,
            image_format: ImageFormat::Typeless as u8,
            ..Default::default()
        };
        program.block_mut(0).append_inst(Inst::with_flags(
            Opcode::ImageRead,
            vec![],
            read.to_u32(),
        ));
        program.block_mut(0).append_inst(Inst::with_flags(
            Opcode::ImageWrite,
            vec![],
            write.to_u32(),
        ));

        collect_shader_info_pass(&mut program);

        assert!(program.info.uses_typeless_image_reads);
        assert!(program.info.uses_typeless_image_writes);
        assert!(program.info.uses_image_1d);
        assert!(program.info.uses_image_buffers);
    }

    #[test]
    fn collect_info_header_sets_vtg_indexed_generic_loads_and_stores() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        program.block_mut(0).append_inst(Inst::new(
            Opcode::GetAttributeIndexed,
            vec![Value::ImmU32(0), Value::ImmU32(0)],
        ));
        program.block_mut(0).append_inst(Inst::new(
            Opcode::SetAttributeIndexed,
            vec![Value::ImmU32(0), Value::ImmU32(0)],
        ));

        let mut sph = ProgramHeader::default();
        sph.raw[5] = 0b0000_0010 << 24;
        sph.raw[6] = 0b1010;
        sph.raw[10] = (0b0000_0011 | (1 << 14)) << 16;
        sph.raw[13] = (0b0000_1000 << 8) | (0b0101 << 16);
        sph.raw[18] = 0b1000_0000 | (1 << 9) | (1 << 15);

        collect_shader_info_pass_with_sph(&mut program, &sph);

        assert!(!program.info.loads.get(Attribute::PRIMITIVE_ID.0 as usize));
        assert!(program.info.loads.get(Attribute::LAYER.0 as usize));
        assert!(program
            .info
            .loads
            .get(Attribute::CLIP_DISTANCE_0.0 as usize));
        assert!(program
            .info
            .loads
            .get(Attribute::CLIP_DISTANCE_0.0 as usize + 1));
        assert!(program.info.loads.get(Attribute::INSTANCE_ID.0 as usize));

        assert!(!program.info.loads.get(Attribute::generic(0, 0).0 as usize));
        assert!(program.info.loads.get(Attribute::generic(0, 1).0 as usize));
        assert!(!program.info.loads.get(Attribute::generic(0, 2).0 as usize));
        assert!(program.info.loads.get(Attribute::generic(0, 3).0 as usize));

        assert!(program.info.stores.get(Attribute::POINT_SIZE.0 as usize));
        assert!(program
            .info
            .stores
            .get(Attribute::CLIP_DISTANCE_0.0 as usize + 7));
        assert!(program
            .info
            .stores
            .get(Attribute::POINT_SPRITE_T.0 as usize));
        assert!(program.info.stores.get(Attribute::VERTEX_ID.0 as usize));
        assert_eq!(program.info.used_clip_distances, 8);

        assert!(program.info.stores.get(Attribute::generic(0, 0).0 as usize));
        assert!(!program.info.stores.get(Attribute::generic(0, 1).0 as usize));
        assert!(program.info.stores.get(Attribute::generic(0, 2).0 as usize));
        assert!(!program.info.stores.get(Attribute::generic(0, 3).0 as usize));
    }
}

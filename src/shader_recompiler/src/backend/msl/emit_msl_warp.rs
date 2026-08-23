// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Warp and derivative instruction emission.
//!
//! This is the native-MSL counterpart of Eden's
//! `backend/spirv/emit_spirv_warp.cpp`.

use crate::ir::instruction::Inst;
use crate::ir::opcodes::Opcode;
use crate::ir::types::Type;
use crate::ir::value::{InstRef, Value};

use super::msl_emit_context::MslEmitContext;
use super::MslError;

fn raw_lane_id(context: &MslEmitContext) -> &'static str {
    context.subgroup_lane_id_expression()
}

fn lane_id(context: &MslEmitContext) -> String {
    let lane = raw_lane_id(context);
    if context.warp_size_potentially_larger_than_guest() {
        format!("({lane} & 31u)")
    } else {
        lane.to_owned()
    }
}

fn ballot_word(context: &MslEmitContext, predicate: &str) -> String {
    let ballot = format!("as_type<uint2>((simd_vote::vote_t)simd_ballot({predicate}))");
    if context.fixed_subgroup_size() > 32 {
        format!("({ballot})[{} >> 5u]", raw_lane_id(context))
    } else {
        format!("({ballot}).x")
    }
}

fn define_partition_ballots(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    predicate: &str,
) -> (String, String) {
    let active_name = format!("warp_active_{}_{}", inst_ref.block, inst_ref.inst);
    let ballot_name = format!("warp_ballot_{}_{}", inst_ref.block, inst_ref.inst);
    let active = ballot_word(context, "true");
    let ballot = ballot_word(context, predicate);
    context.push_statement(format!("uint {active_name} = {active};"));
    context.push_statement(format!("uint {ballot_name} = {ballot};"));
    (active_name, ballot_name)
}

pub fn emit_lane_id(context: &mut MslEmitContext, inst_ref: InstRef) -> Result<(), MslError> {
    context.define(inst_ref, Type::U32, lane_id(context), false)
}

pub fn emit_vote_all(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let predicate = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let expression = if !context.supports_subgroups() {
        predicate
    } else if !context.warp_size_potentially_larger_than_guest() {
        format!("simd_all({predicate})")
    } else {
        let (active, ballot) = define_partition_ballots(context, inst_ref, &predicate);
        format!("(({ballot}) & ({active})) == ({active})")
    };
    context.define(inst_ref, Type::U1, expression, false)
}

pub fn emit_vote_any(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let predicate = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let expression = if !context.supports_subgroups() {
        predicate
    } else if !context.warp_size_potentially_larger_than_guest() {
        format!("simd_any({predicate})")
    } else {
        let (active, ballot) = define_partition_ballots(context, inst_ref, &predicate);
        format!("(({ballot}) & ({active})) != 0u")
    };
    context.define(inst_ref, Type::U1, expression, false)
}

pub fn emit_vote_equal(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let predicate = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let expression = if !context.supports_subgroups() {
        "true".to_owned()
    } else if !context.warp_size_potentially_larger_than_guest() {
        format!("simd_all({predicate}) || !simd_any({predicate})")
    } else {
        let (active, ballot) = define_partition_ballots(context, inst_ref, &predicate);
        format!("((({ballot}) ^ ({active})) == 0u) || ((({ballot}) ^ ({active})) == ({active}))")
    };
    context.define(inst_ref, Type::U1, expression, false)
}

pub fn emit_subgroup_ballot(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let predicate = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let expression = if context.supports_subgroups() {
        ballot_word(context, &predicate)
    } else {
        format!("({predicate}) ? 1u : 0u")
    };
    context.define(inst_ref, Type::U32, expression, false)
}

pub fn emit_subgroup_mask(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    opcode: Opcode,
) -> Result<(), MslError> {
    let expression = if !context.supports_subgroups() {
        match opcode {
            Opcode::SubgroupEqMask | Opcode::SubgroupLeMask | Opcode::SubgroupGeMask => "1u",
            Opcode::SubgroupLtMask | Opcode::SubgroupGtMask => "0u",
            _ => unreachable!("non-mask opcode passed to subgroup mask emitter"),
        }
        .to_owned()
    } else {
        let lane = lane_id(context);
        match opcode {
            Opcode::SubgroupEqMask => format!("1u << ({lane})"),
            Opcode::SubgroupLtMask => format!("({lane}) == 0u ? 0u : ((1u << ({lane})) - 1u)"),
            Opcode::SubgroupLeMask => {
                format!("({lane}) == 31u ? 0xFFFFFFFFu : ((1u << (({lane}) + 1u)) - 1u)")
            }
            Opcode::SubgroupGtMask => {
                format!("({lane}) == 31u ? 0u : ~((1u << (({lane}) + 1u)) - 1u)")
            }
            Opcode::SubgroupGeMask => {
                format!("({lane}) == 0u ? 0xFFFFFFFFu : ~((1u << ({lane})) - 1u)")
            }
            _ => unreachable!("non-mask opcode passed to subgroup mask emitter"),
        }
    };
    context.define(inst_ref, Type::U32, expression, false)
}

fn set_in_bounds_flag(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    expression: &str,
) -> Result<String, MslError> {
    let Some(in_bounds) = inst.get_associated_pseudo(Opcode::GetInBoundsFromOp) else {
        return Ok(expression.to_owned());
    };
    context.define(in_bounds, Type::U1, expression.to_owned(), false)?;
    context.value_expression(&Value::Inst(in_bounds), inst_ref, 0)
}

fn add_partition_base(context: &MslEmitContext, source_lane: String) -> String {
    if context.warp_size_potentially_larger_than_guest() {
        let lane = raw_lane_id(context);
        format!("({source_lane}) + ((({lane}) >> 5u) << 5u)")
    } else {
        source_lane
    }
}

pub fn emit_shuffle(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let index = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let clamp = context.value_expression(inst.arg(2), inst_ref, 2)?;
    let segmentation_mask = context.value_expression(inst.arg(3), inst_ref, 3)?;
    let thread_id = lane_id(context);
    let not_seg_mask = format!("~({segmentation_mask})");
    let min_thread_id = format!("({thread_id}) & ({segmentation_mask})");
    let max_thread_id = format!("({min_thread_id}) | (({clamp}) & ({not_seg_mask}))");
    let source_lane = match inst.opcode {
        Opcode::ShuffleIndex => {
            format!("(({index}) & ({not_seg_mask})) | ({min_thread_id})")
        }
        Opcode::ShuffleUp => format!("({thread_id}) - ({index})"),
        Opcode::ShuffleDown => format!("({thread_id}) + ({index})"),
        Opcode::ShuffleButterfly => format!("({thread_id}) ^ ({index})"),
        _ => unreachable!("non-shuffle opcode passed to shuffle emitter"),
    };
    let in_bounds = match inst.opcode {
        Opcode::ShuffleUp => {
            format!("as_type<int>({source_lane}) >= as_type<int>({max_thread_id})")
        }
        Opcode::ShuffleIndex | Opcode::ShuffleDown | Opcode::ShuffleButterfly => {
            format!("as_type<int>({source_lane}) <= as_type<int>({max_thread_id})")
        }
        _ => unreachable!(),
    };
    let in_bounds = set_in_bounds_flag(context, inst_ref, inst, &in_bounds)?;
    let expression = if context.supports_subgroups() {
        let source_lane = add_partition_base(context, source_lane);
        format!("({in_bounds}) ? simd_shuffle({value}, {source_lane}) : ({value})")
    } else {
        value
    };
    context.define(inst_ref, Type::U32, expression, false)
}

pub fn emit_fswizzle_add(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let op_a = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let op_b = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let swizzle = context.value_expression(inst.arg(2), inst_ref, 2)?;
    let lane = raw_lane_id(context);
    let mask = format!("fswizzle_mask_{}_{}", inst_ref.block, inst_ref.inst);
    context.push_statement(format!(
        "uint {mask} = (({swizzle}) >> ((({lane}) & 3u) << 1u)) & 3u;"
    ));
    let modifier_a = format!("float4(-1.0f, 1.0f, -1.0f, 0.0f)[{mask}]");
    let modifier_b = format!("float4(-1.0f, -1.0f, 1.0f, -1.0f)[{mask}]");
    context.define(
        inst_ref,
        Type::F32,
        format!("(({op_a}) * ({modifier_a})) + (({op_b}) * ({modifier_b}))"),
        false,
    )
}

fn emit_derivative(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    function: &str,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(inst_ref, Type::F32, format!("{function}({value})"), false)
}

/// Metal has one derivative quality and SPIRV-Cross maps both SPIR-V
/// `DPdxFine` and `DPdxCoarse` to `dfdx`.
pub fn emit_dpdx(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    emit_derivative(context, inst_ref, inst, "dfdx")
}

/// Metal has one derivative quality and SPIRV-Cross maps both SPIR-V
/// `DPdyFine` and `DPdyCoarse` to `dfdy`.
pub fn emit_dpdy(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    emit_derivative(context, inst_ref, inst, "dfdy")
}

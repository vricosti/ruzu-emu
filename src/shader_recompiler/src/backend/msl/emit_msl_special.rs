// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Entry-point prologue and epilogue emission.
//!
//! This owns the native-MSL counterpart of Eden's
//! `backend/spirv/emit_spirv_special.cpp`.

use crate::ir;
use crate::ir::value::Attribute;
use crate::runtime_info::CompareFunction;
use crate::stage::Stage;

use super::msl_emit_context::MslEmitContext;

fn float_bits(value: f32) -> String {
    format!("as_type<float>(0x{:08X}u)", value.to_bits())
}

fn set_fixed_pipeline_point_size(context: &mut MslEmitContext) {
    let Some(point_size) = context.fixed_state_point_size() else {
        return;
    };
    if context.emits_point_size() {
        context.emit_statement(&format!("output.point_size = {};", float_bits(point_size)));
    }
}

/// Emit Eden's `EmitPrologue` behavior into the direct MSL entry point.
pub fn emit_prologue(context: &mut MslEmitContext, program: &ir::Program) {
    if context.stage() == Stage::Fragment && context.dual_source_blend() {
        for index in 0..2 {
            if context.emits_frag_color(index) {
                context.emit_statement(&format!(
                    "output.color{index} = float4(0.0f, 0.0f, 0.0f, 1.0f);"
                ));
            }
        }
    }

    if context.stage() == Stage::VertexB {
        context.emit_statement("output.position = float4(0.0f, 0.0f, 0.0f, 1.0f);");
        for index in 0..32 {
            if program.info.stores.generic_any(index) {
                context.emit_statement(&format!(
                    "output.out_attr{index} = float4(0.0f, 0.0f, 0.0f, 1.0f);"
                ));
            }
        }
        for index in 0..context.clip_distance_count() {
            let attribute = Attribute::CLIP_DISTANCE_0.0 as usize + index as usize;
            if !program.info.stores.get(attribute) {
                context.emit_statement(&format!("output.clip_distance[{index}] = 0.0f;"));
            }
        }
        set_fixed_pipeline_point_size(context);
    }
}

fn alpha_test(context: &mut MslEmitContext) {
    let Some((comparison, reference)) = context.alpha_test() else {
        return;
    };
    if comparison == CompareFunction::Always || !context.emits_frag_color(0) {
        return;
    }

    let alpha = "output.color0.w";
    let reference = float_bits(reference);
    let condition = match comparison {
        CompareFunction::Never => "false".to_owned(),
        CompareFunction::Less => format!("{alpha} < {reference}"),
        CompareFunction::Equal => format!("{alpha} == {reference}"),
        CompareFunction::LessThanEqual => format!("{alpha} <= {reference}"),
        CompareFunction::Greater => format!("{alpha} > {reference}"),
        CompareFunction::NotEqual => {
            format!("!isnan({alpha}) && !isnan({reference}) && {alpha} != {reference}")
        }
        CompareFunction::GreaterThanEqual => format!("{alpha} >= {reference}"),
        CompareFunction::Always => unreachable!(),
    };
    context.emit_statement(&format!("if (!({condition})) discard_fragment();"));
}

/// Emit Eden's `EmitEpilogue` behavior into the direct MSL entry point.
pub fn emit_epilogue(context: &mut MslEmitContext) {
    if context.stage() == Stage::VertexB && context.converts_depth_mode() {
        context
            .emit_statement("output.position.z = (output.position.z + output.position.w) * 0.5f;");
    }
    if context.stage() == Stage::Fragment {
        alpha_test(context);
    }
}

// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Warp and derivative instruction emission.
//!
//! This is the native-MSL counterpart of Eden's
//! `backend/spirv/emit_spirv_warp.cpp`.

use crate::ir::instruction::Inst;
use crate::ir::types::Type;
use crate::ir::value::InstRef;

use super::msl_emit_context::MslEmitContext;
use super::MslError;

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

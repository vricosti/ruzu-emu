//! Port of Eden Dynarmic `backend/x64/emit_x64.cpp`.

use crate::backend::x64::emit_context::EmitContext;
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::ir::inst::Inst;
use crate::ir::value::InstRef;

/// Register the upper result already emitted by its multi-result producer.
pub fn emit_get_upper_from_op(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    ra.register_pseudo_operation(inst_ref, &inst.args, inst.num_args());
}

/// Register the lower result already emitted by its multi-result producer.
pub fn emit_get_lower_from_op(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    ra.register_pseudo_operation(inst_ref, &inst.args, inst.num_args());
}

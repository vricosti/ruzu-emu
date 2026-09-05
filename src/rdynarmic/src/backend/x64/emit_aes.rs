use crate::backend::x64::emit_context::EmitContext;
use crate::backend::x64::emit_vector_helpers::emit_one_arg_fallback;
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::common::crypto::aes;
use crate::ir::inst::Inst;
use crate::ir::value::InstRef;

pub fn emit_aes_decrypt_single_round(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    if !ctx.has_host_feature(HostFeature::AES) {
        emit_one_arg_fallback(
            ra,
            inst_ref,
            inst,
            aes::decrypt_single_round as *const () as usize,
        );
        return;
    }
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let data = ra.use_scratch_xmm(&mut args[0]);
    let zero = ra.scratch_xmm();
    ra.asm.xorps(zero, zero).unwrap();
    ra.asm.aesdeclast(data, zero).unwrap();
    ra.release(zero);
    ra.define_value(inst_ref, data);
}

pub fn emit_aes_encrypt_single_round(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    if !ctx.has_host_feature(HostFeature::AES) {
        emit_one_arg_fallback(
            ra,
            inst_ref,
            inst,
            aes::encrypt_single_round as *const () as usize,
        );
        return;
    }
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let data = ra.use_scratch_xmm(&mut args[0]);
    let zero = ra.scratch_xmm();
    ra.asm.xorps(zero, zero).unwrap();
    ra.asm.aesenclast(data, zero).unwrap();
    ra.release(zero);
    ra.define_value(inst_ref, data);
}

pub fn emit_aes_inverse_mix_columns(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    if !ctx.has_host_feature(HostFeature::AES) {
        emit_one_arg_fallback(
            ra,
            inst_ref,
            inst,
            aes::inverse_mix_columns as *const () as usize,
        );
        return;
    }
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let data = ra.use_scratch_xmm(&mut args[0]);
    ra.asm.aesimc(data, data).unwrap();
    ra.define_value(inst_ref, data);
}

pub fn emit_aes_mix_columns(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    if !ctx.has_host_feature(HostFeature::AES) {
        emit_one_arg_fallback(ra, inst_ref, inst, aes::mix_columns as *const () as usize);
        return;
    }
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let data = ra.use_scratch_xmm(&mut args[0]);
    let zero = ra.scratch_xmm();
    ra.asm.xorps(zero, zero).unwrap();
    ra.asm.aesdeclast(data, zero).unwrap();
    ra.asm.aesenc(data, zero).unwrap();
    ra.release(zero);
    ra.define_value(inst_ref, data);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emitter_signatures_match_the_aes_owner() {
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_aes_decrypt_single_round;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_aes_encrypt_single_round;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_aes_inverse_mix_columns;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_aes_mix_columns;
    }
}

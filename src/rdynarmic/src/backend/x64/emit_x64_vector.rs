//! Port of Eden Dynarmic `backend/x64/emit_x64_vector.cpp`.

use crate::backend::x64::abi;
use crate::backend::x64::constants::cmp_int;
use crate::backend::x64::emit_context::EmitContext;
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::ir::inst::Inst;
use crate::ir::opcode::Opcode;
use crate::ir::value::InstRef;
use crate::ir::value::Value;
use rxbyak::{qword_ptr, xmmword_ptr, RegExp, K1, K2, RSP, XMM0};

pub fn emit_vector_broadcast_element_lower8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let index = args[1].get_immediate_u8();
    assert!(index < 16);
    if index > 0 {
        ra.asm.psrldq(result, index).unwrap();
    }
    if ctx.has_host_feature(HostFeature::AVX2) {
        ra.asm.vpbroadcastb(result, result).unwrap();
        ra.asm.vmovq(result, result).unwrap();
    } else if ctx.has_host_feature(HostFeature::SSSE3) {
        let temporary = ra.scratch_xmm();
        ra.asm.pxor(temporary, temporary).unwrap();
        ra.asm.pshufb(result, temporary).unwrap();
        ra.asm.movq(result, result).unwrap();
        ra.release(temporary);
    } else {
        ra.asm.punpcklbw(result, result).unwrap();
        ra.asm.pshuflw(result, result, 0).unwrap();
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_broadcast_element_lower16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let index = args[1].get_immediate_u8();
    assert!(index < 8);
    if index > 0 {
        ra.asm.psrldq(result, index * 2).unwrap();
    }
    ra.asm.pshuflw(result, result, 0).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_broadcast_element_lower32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let index = args[1].get_immediate_u8();
    assert!(index < 4);
    if index > 0 {
        ra.asm.psrldq(result, index * 4).unwrap();
    }
    ra.asm.pshuflw(result, result, 0b01_00_01_00).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_broadcast_element8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let index = args[1].get_immediate_u8();
    assert!(index < 16);
    if index > 0 {
        ra.asm.psrldq(result, index).unwrap();
    }
    if ctx.has_host_feature(HostFeature::AVX2) {
        ra.asm.vpbroadcastb(result, result).unwrap();
    } else if ctx.has_host_feature(HostFeature::SSSE3) {
        let temporary = ra.scratch_xmm();
        ra.asm.pxor(temporary, temporary).unwrap();
        ra.asm.pshufb(result, temporary).unwrap();
        ra.release(temporary);
    } else {
        ra.asm.punpcklbw(result, result).unwrap();
        ra.asm.pshuflw(result, result, 0).unwrap();
        ra.asm.punpcklqdq(result, result).unwrap();
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_broadcast_element16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let index = args[1].get_immediate_u8();
    assert!(index < 8);
    if index == 0 && ctx.has_host_feature(HostFeature::AVX2) {
        ra.asm.vpbroadcastw(result, result).unwrap();
    } else if index < 4 {
        ra.asm.pshuflw(result, result, index * 0x55).unwrap();
        ra.asm.punpcklqdq(result, result).unwrap();
    } else {
        ra.asm.pshufhw(result, result, (index - 4) * 0x55).unwrap();
        ra.asm.punpckhqdq(result, result).unwrap();
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_broadcast_element32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let index = args[1].get_immediate_u8();
    assert!(index < 4);
    ra.asm.pshufd(result, result, index * 0x55).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_broadcast_element64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let index = args[1].get_immediate_u8();
    assert!(index < 2);
    if ctx.has_host_feature(HostFeature::AVX) {
        ra.asm.vpermilpd_imm(result, result, index * 3).unwrap();
    } else if index == 0 {
        ra.asm.punpcklqdq(result, result).unwrap();
    } else {
        ra.asm.punpckhqdq(result, result).unwrap();
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_reduce_add8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let data = ra.use_scratch_xmm(&mut args[0]);

    ra.asm.pshufd(XMM0, data, 0b01_00_11_10).unwrap();
    ra.asm.paddb(data, XMM0).unwrap();
    ra.asm.pxor(XMM0, XMM0).unwrap();
    ra.asm.psadbw(data, XMM0).unwrap();
    ra.asm.pslldq(data, 15).unwrap();
    ra.asm.psrldq(data, 15).unwrap();

    ra.define_value(inst_ref, data);
}

pub fn emit_vector_reduce_add16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let data = ra.use_scratch_xmm(&mut args[0]);

    if ctx.has_host_feature(HostFeature::SSSE3) {
        ra.asm.pxor(XMM0, XMM0).unwrap();
        ra.asm.phaddw(data, XMM0).unwrap();
        ra.asm.phaddw(data, XMM0).unwrap();
        ra.asm.phaddw(data, XMM0).unwrap();
    } else {
        ra.asm.pshufd(XMM0, data, 0b00_01_10_11).unwrap();
        ra.asm.paddw(data, XMM0).unwrap();

        let constant = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x0001_0001_0001_0001, 0x0001_0001_0001_0001);
        ra.asm.movdqa(XMM0, xmmword_ptr(constant)).unwrap();
        ra.asm.pmaddwd(data, XMM0).unwrap();

        ra.asm.pshufd(XMM0, data, 0b10_11_00_01).unwrap();
        ra.asm.paddd(data, XMM0).unwrap();
        ra.asm.pslldq(data, 14).unwrap();
        ra.asm.psrldq(data, 14).unwrap();
    }

    ra.define_value(inst_ref, data);
}

pub fn emit_vector_reduce_add32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let data = ra.use_scratch_xmm(&mut args[0]);

    ra.asm.pshufd(XMM0, data, 0b00_01_10_11).unwrap();
    ra.asm.paddd(data, XMM0).unwrap();
    if ctx.has_host_feature(HostFeature::SSSE3) {
        ra.asm.phaddd(data, data).unwrap();
    } else {
        ra.asm.pshufd(XMM0, data, 0b10_11_00_01).unwrap();
        ra.asm.paddd(data, XMM0).unwrap();
    }
    ra.asm.psrldq(data, 12).unwrap();

    ra.define_value(inst_ref, data);
}

pub fn emit_vector_reduce_add64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let data = ra.use_scratch_xmm(&mut args[0]);

    ra.asm.pshufd(XMM0, data, 0b01_00_11_10).unwrap();
    ra.asm.paddq(data, XMM0).unwrap();
    ra.asm.movq(data, data).unwrap();

    ra.define_value(inst_ref, data);
}

pub fn emit_vector_signed_multiply16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let block = ctx.block.expect("IR block required for pseudo-operations");
    let upper_inst = block.get_associated_pseudo_operation(inst_ref, Opcode::GetUpperFromOp);
    let lower_inst = block.get_associated_pseudo_operation(inst_ref, Opcode::GetLowerFromOp);

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let x = ra.use_xmm(&mut args[0]);
    let y = ra.use_xmm(&mut args[1]);

    if let Some(upper_inst) = upper_inst {
        let result = ra.scratch_xmm();
        if ctx.has_host_feature(HostFeature::AVX) {
            ra.asm.vpmulhw(result, x, y).unwrap();
        } else {
            ra.asm.movdqa(result, x).unwrap();
            ra.asm.pmulhw(result, y).unwrap();
        }
        ra.define_value(upper_inst, result);
    }

    if let Some(lower_inst) = lower_inst {
        let result = ra.scratch_xmm();
        if ctx.has_host_feature(HostFeature::AVX) {
            ra.asm.vpmullw(result, x, y).unwrap();
        } else {
            ra.asm.movdqa(result, x).unwrap();
            ra.asm.pmullw(result, y).unwrap();
        }
        ra.define_value(lower_inst, result);
    }
}

pub fn emit_vector_signed_multiply32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let block = ctx.block.expect("IR block required for pseudo-operations");
    let upper_inst = block.get_associated_pseudo_operation(inst_ref, Opcode::GetUpperFromOp);
    let lower_inst = block.get_associated_pseudo_operation(inst_ref, Opcode::GetLowerFromOp);

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());

    if lower_inst.is_some() && upper_inst.is_none() && ctx.has_host_feature(HostFeature::AVX) {
        let x = ra.use_xmm(&mut args[0]);
        let y = ra.use_xmm(&mut args[1]);
        let result = ra.scratch_xmm();
        ra.asm.vpmulld(result, x, y).unwrap();
        ra.define_value(lower_inst.unwrap(), result);
        return;
    }

    if ctx.has_host_feature(HostFeature::AVX) {
        let x = ra.use_scratch_xmm(&mut args[0]);
        let y = ra.use_scratch_xmm(&mut args[1]);

        if let Some(lower_inst) = lower_inst {
            let lower_result = ra.scratch_xmm();
            ra.asm.vpmulld(lower_result, x, y).unwrap();
            ra.define_value(lower_inst, lower_result);
        }

        let result = ra.scratch_xmm();
        ra.asm.vpmuldq(result, x, y).unwrap();
        ra.asm.vpsrlq_imm(x, x, 32).unwrap();
        ra.asm.vpsrlq_imm(y, y, 32).unwrap();
        ra.asm.vpmuldq(x, x, y).unwrap();
        ra.asm.shufps(result, x, 0b1101_1101).unwrap();
        ra.define_value(upper_inst.unwrap(), result);
        return;
    }

    let x = ra.use_scratch_xmm(&mut args[0]);
    let y = ra.use_scratch_xmm(&mut args[1]);
    let tmp = ra.scratch_xmm();
    let sign_correction = ra.scratch_xmm();
    let upper_result = ra.scratch_xmm();
    let lower_result = ra.scratch_xmm();

    ra.asm.movdqa(tmp, x).unwrap();
    ra.asm.movdqa(sign_correction, y).unwrap();
    ra.asm.psrad_imm(tmp, 31).unwrap();
    ra.asm.psrad_imm(sign_correction, 31).unwrap();
    ra.asm.pand(tmp, y).unwrap();
    ra.asm.pand(sign_correction, x).unwrap();
    ra.asm.paddd(sign_correction, tmp).unwrap();
    let sign_mask = ra
        .constant_pool
        .as_mut()
        .expect("constant pool required")
        .get_constant(0x7fff_ffff_7fff_ffff, 0x7fff_ffff_7fff_ffff);
    ra.asm
        .pand(sign_correction, xmmword_ptr(sign_mask))
        .unwrap();

    ra.asm.movdqa(tmp, x).unwrap();
    ra.asm.pmuludq(tmp, y).unwrap();
    ra.asm.psrlq_imm(x, 32).unwrap();
    ra.asm.psrlq_imm(y, 32).unwrap();
    ra.asm.pmuludq(x, y).unwrap();

    ra.asm.pcmpeqw(upper_result, upper_result).unwrap();
    ra.asm.pcmpeqw(lower_result, lower_result).unwrap();
    ra.asm.psllq_imm(upper_result, 32).unwrap();
    ra.asm.psrlq_imm(lower_result, 32).unwrap();
    ra.asm.pand(upper_result, x).unwrap();
    ra.asm.pand(lower_result, tmp).unwrap();
    ra.asm.psrlq_imm(tmp, 32).unwrap();
    ra.asm.psllq_imm(x, 32).unwrap();
    ra.asm.por(upper_result, tmp).unwrap();
    ra.asm.por(lower_result, x).unwrap();
    ra.asm.psubd(upper_result, sign_correction).unwrap();

    if let Some(upper_inst) = upper_inst {
        ra.define_value(upper_inst, upper_result);
    }
    if let Some(lower_inst) = lower_inst {
        ra.define_value(lower_inst, lower_result);
    }
}

pub fn emit_vector_unsigned_multiply16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let block = ctx.block.expect("IR block required for pseudo-operations");
    let upper_inst = block.get_associated_pseudo_operation(inst_ref, Opcode::GetUpperFromOp);
    let lower_inst = block.get_associated_pseudo_operation(inst_ref, Opcode::GetLowerFromOp);

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let x = ra.use_xmm(&mut args[0]);
    let y = ra.use_xmm(&mut args[1]);

    if let Some(upper_inst) = upper_inst {
        let result = ra.scratch_xmm();
        if ctx.has_host_feature(HostFeature::AVX) {
            ra.asm.vpmulhuw(result, x, y).unwrap();
        } else {
            ra.asm.movdqa(result, x).unwrap();
            ra.asm.pmulhuw(result, y).unwrap();
        }
        ra.define_value(upper_inst, result);
    }

    if let Some(lower_inst) = lower_inst {
        let result = ra.scratch_xmm();
        if ctx.has_host_feature(HostFeature::AVX) {
            ra.asm.vpmullw(result, x, y).unwrap();
        } else {
            ra.asm.movdqa(result, x).unwrap();
            ra.asm.pmullw(result, y).unwrap();
        }
        ra.define_value(lower_inst, result);
    }
}

pub fn emit_vector_unsigned_multiply32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let block = ctx.block.expect("IR block required for pseudo-operations");
    let upper_inst = block.get_associated_pseudo_operation(inst_ref, Opcode::GetUpperFromOp);
    let lower_inst = block.get_associated_pseudo_operation(inst_ref, Opcode::GetLowerFromOp);

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());

    if lower_inst.is_some() && upper_inst.is_none() && ctx.has_host_feature(HostFeature::AVX) {
        let x = ra.use_xmm(&mut args[0]);
        let y = ra.use_xmm(&mut args[1]);
        let result = ra.scratch_xmm();
        ra.asm.vpmulld(result, x, y).unwrap();
        ra.define_value(lower_inst.unwrap(), result);
    } else if ctx.has_host_feature(HostFeature::AVX) {
        let x = ra.use_scratch_xmm(&mut args[0]);
        let y = ra.use_scratch_xmm(&mut args[1]);

        if let Some(lower_inst) = lower_inst {
            let lower_result = ra.scratch_xmm();
            ra.asm.vpmulld(lower_result, x, y).unwrap();
            ra.define_value(lower_inst, lower_result);
        }

        let result = ra.scratch_xmm();
        ra.asm.vpmuludq(result, x, y).unwrap();
        ra.asm.vpsrlq_imm(x, x, 32).unwrap();
        ra.asm.vpsrlq_imm(y, y, 32).unwrap();
        ra.asm.vpmuludq(x, x, y).unwrap();
        ra.asm.shufps(result, x, 0b1101_1101).unwrap();
        ra.define_value(upper_inst.unwrap(), result);
    } else {
        let x = ra.use_scratch_xmm(&mut args[0]);
        let y = ra.use_scratch_xmm(&mut args[1]);
        let tmp = ra.scratch_xmm();
        let upper_result = upper_inst.map(|_| ra.scratch_xmm());
        let lower_result = lower_inst.map(|_| ra.scratch_xmm());

        ra.asm.movdqa(tmp, x).unwrap();
        ra.asm.pmuludq(tmp, y).unwrap();
        ra.asm.psrlq_imm(x, 32).unwrap();
        ra.asm.psrlq_imm(y, 32).unwrap();
        ra.asm.pmuludq(x, y).unwrap();

        if let Some(upper_result) = upper_result {
            ra.asm.pcmpeqw(upper_result, upper_result).unwrap();
        }
        if let Some(lower_result) = lower_result {
            ra.asm.pcmpeqw(lower_result, lower_result).unwrap();
        }
        if let Some(upper_result) = upper_result {
            ra.asm.psllq_imm(upper_result, 32).unwrap();
        }
        if let Some(lower_result) = lower_result {
            ra.asm.psrlq_imm(lower_result, 32).unwrap();
        }
        if let Some(upper_result) = upper_result {
            ra.asm.pand(upper_result, x).unwrap();
        }
        if let Some(lower_result) = lower_result {
            ra.asm.pand(lower_result, tmp).unwrap();
        }
        if upper_inst.is_some() {
            ra.asm.psrlq_imm(tmp, 32).unwrap();
        }
        if lower_inst.is_some() {
            ra.asm.psllq_imm(x, 32).unwrap();
        }
        if let Some(upper_result) = upper_result {
            ra.asm.por(upper_result, tmp).unwrap();
        }
        if let Some(lower_result) = lower_result {
            ra.asm.por(lower_result, x).unwrap();
        }
        if let (Some(upper_inst), Some(upper_result)) = (upper_inst, upper_result) {
            ra.define_value(upper_inst, upper_result);
        }
        if let (Some(lower_inst), Some(lower_result)) = (lower_inst, lower_result) {
            ra.define_value(lower_inst, lower_result);
        }
    }
}

// ---------------------------------------------------------------------------
// VectorTable / VectorTableLookup64 / VectorTableLookup128
// Port of upstream EmitVectorTable, EmitVectorTableLookup64 and
// EmitVectorTableLookup128 (emit_x64_vector.cpp).
// ---------------------------------------------------------------------------

pub fn emit_vector_table(_ctx: &EmitContext, _ra: &mut RegAlloc, _inst_ref: InstRef, inst: &Inst) {
    // Do nothing. We *want* to hold on to the refcount for our arguments, so VectorTableLookup can use our arguments.
    assert_eq!(inst.use_count, 1, "Table cannot be used multiple times");
}

fn vector_table_inst<'a>(ctx: &'a EmitContext<'a>, inst: &Inst) -> &'a Inst {
    let Value::Inst(table_ref) = inst.args[1] else {
        panic!("VectorTableLookup arg1 must be a VectorTable instruction");
    };
    let table = ctx
        .block
        .expect("EmitContext::block is required for VectorTableLookup")
        .get(table_ref);
    assert_eq!(table.opcode, Opcode::VectorTable);
    table
}

/// Upstream `code.Const(xword, lower, upper)`.
fn xword_const(ra: &mut RegAlloc, lower: u64, upper: u64) -> rxbyak::Address {
    let address = ra
        .constant_pool
        .as_mut()
        .expect("constant pool required for vector constants")
        .get_constant(lower, upper);
    xmmword_ptr(address)
}

/// `mcl::bit::replicate_element<u8, u64>`.
const fn replicate_u8(value: u8) -> u64 {
    (value as u64) * 0x0101_0101_0101_0101
}

/// Host fallback of upstream (no SSE4.1): `CallLambda` over `HalfVectorArray<u8>`
/// / `VectorArray<u8>` tables.
/// `ELEMENTS` is 8 (`HalfVectorArray<u8>`) or 16 (`VectorArray<u8>`); the
/// tables are stored in 16-byte slots.
extern "C" fn vector_table_lookup_fallback<const ELEMENTS: usize>(
    table: *const [u8; 16],
    result: *mut [u8; 16],
    indicies: *const [u8; 16],
    table_size: usize,
) {
    unsafe {
        let result = &mut *result;
        let indicies = &*indicies;
        for i in 0..ELEMENTS {
            let index = indicies[i] as usize / ELEMENTS;
            let elem = indicies[i] as usize % ELEMENTS;
            if index < table_size {
                result[i] = (*table.add(index))[elem];
            }
        }
    }
}

fn emit_vector_table_lookup_fallback(
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    args: &mut crate::backend::x64::reg_alloc::ArgumentInfo,
    table: &mut crate::backend::x64::reg_alloc::ArgumentInfo,
    table_size: usize,
    elements: usize,
) {
    // Every table entry is stored in a 16-byte slot; 64-bit tables only use
    // the low half (upstream packs them in 8-byte slots, same values read).
    let stack_space = (table_size + 2) * 16;
    ra.alloc_stack_space(stack_space + abi::ABI_SHADOW_SPACE);
    let base = RegExp::from(RSP) + abi::ABI_SHADOW_SPACE as i32;
    for i in 0..table_size {
        let table_value = ra.use_xmm(&mut table[i]);
        ra.asm
            .movaps(xmmword_ptr(base + (i * 16) as i32), table_value)
            .unwrap();
        ra.release(table_value);
    }
    let defaults = ra.use_xmm(&mut args[0]);
    let indicies = ra.use_xmm(&mut args[2]);
    let result = ra.scratch_xmm();
    ra.end_of_alloc_scope();
    ra.host_call(None, &mut [None, None, None, None]);
    let param = |i: usize| abi::ABI_PARAMS[i].to_reg64();
    ra.asm.lea(param(0), qword_ptr(base)).unwrap();
    ra.asm
        .lea(param(1), qword_ptr(base + (table_size * 16) as i32))
        .unwrap();
    ra.asm
        .lea(param(2), qword_ptr(base + ((table_size + 1) * 16) as i32))
        .unwrap();
    ra.asm
        .mov(param(3).cvt32().unwrap(), table_size as i32)
        .unwrap();
    ra.asm
        .movaps(xmmword_ptr(base + (table_size * 16) as i32), defaults)
        .unwrap();
    ra.asm
        .movaps(xmmword_ptr(base + ((table_size + 1) * 16) as i32), indicies)
        .unwrap();
    let function = match elements {
        8 => vector_table_lookup_fallback::<8> as *const () as usize,
        16 => vector_table_lookup_fallback::<16> as *const () as usize,
        _ => unreachable!(),
    };
    ra.asm.mov(rxbyak::RAX, function as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();
    ra.asm
        .movaps(result, xmmword_ptr(base + (table_size * 16) as i32))
        .unwrap();
    ra.release_stack_space(stack_space + abi::ABI_SHADOW_SPACE);
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_table_lookup64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let table_inst = vector_table_inst(ctx, inst);

    // The VectorTable argument is read through `table`, not allocated here.
    let lookup_args = [inst.args[0], Value::Void, inst.args[2]];
    let mut args = ra.get_argument_info(inst_ref, &lookup_args, lookup_args.len());
    let mut table = ra.get_argument_info(inst_ref, &table_inst.args, table_inst.num_args());

    let table_size = table
        .iter()
        .take(table_inst.num_args())
        .filter(|elem| !elem.is_void())
        .count();
    let is_defaults_zero = inst.args[0].is_zero();

    if ctx.has_host_feature(
        HostFeature::AVX512_ORTHO | HostFeature::AVX512BW | HostFeature::AVX512VBMI,
    ) {
        let indicies = if table_size <= 2 {
            ra.use_xmm(&mut args[2])
        } else {
            ra.use_scratch_xmm(&mut args[2])
        };

        let index_count = replicate_u8((table_size * 8) as u8);
        let index_count = xword_const(ra, index_count, 0);
        ra.asm
            .vpcmpub(K1, indicies, index_count, cmp_int::LESS_THAN)
            .unwrap();

        match table_size {
            1 => {
                let xmm_table0 = ra.use_xmm(&mut table[0]);
                if is_defaults_zero {
                    let result = ra.scratch_xmm();
                    ra.asm
                        .vpermb(result.k(1).z(), indicies, xmm_table0)
                        .unwrap();
                    ra.define_value(inst_ref, result);
                } else {
                    let result = ra.use_scratch_xmm(&mut args[0]);
                    ra.asm.vpermb(result.k(1), indicies, xmm_table0).unwrap();
                    ra.define_value(inst_ref, result);
                }
            }
            2 => {
                let xmm_table0_lower = ra.use_xmm(&mut table[0]);
                let xmm_table0_upper = ra.use_xmm(&mut table[1]);
                ra.asm
                    .vpunpcklqdq(XMM0, xmm_table0_lower, xmm_table0_upper)
                    .unwrap();
                if is_defaults_zero {
                    let result = ra.scratch_xmm();
                    ra.asm.vpermb(result.k(1).z(), indicies, XMM0).unwrap();
                    ra.define_value(inst_ref, result);
                } else {
                    let result = ra.use_scratch_xmm(&mut args[0]);
                    ra.asm.vpermb(result.k(1), indicies, XMM0).unwrap();
                    ra.define_value(inst_ref, result);
                }
            }
            3 => {
                let xmm_table0_lower = ra.use_xmm(&mut table[0]);
                let xmm_table0_upper = ra.use_xmm(&mut table[1]);
                let xmm_table1 = ra.use_xmm(&mut table[2]);
                ra.asm
                    .vpunpcklqdq(XMM0, xmm_table0_lower, xmm_table0_upper)
                    .unwrap();
                if is_defaults_zero {
                    ra.asm
                        .vpermi2b(indicies.k(1).z(), XMM0, xmm_table1)
                        .unwrap();
                    ra.define_value(inst_ref, indicies);
                } else {
                    let result = ra.use_scratch_xmm(&mut args[0]);
                    ra.asm.vpermi2b(indicies, XMM0, xmm_table1).unwrap();
                    ra.asm.vmovdqu8(result.k(1), indicies).unwrap();
                    ra.define_value(inst_ref, result);
                }
            }
            4 => {
                let xmm_table0_lower = ra.use_xmm(&mut table[0]);
                let xmm_table0_upper = ra.use_xmm(&mut table[1]);
                let xmm_table1 = ra.use_scratch_xmm(&mut table[2]);
                let xmm_table1_upper = ra.use_xmm(&mut table[3]);
                ra.asm
                    .vpunpcklqdq(XMM0, xmm_table0_lower, xmm_table0_upper)
                    .unwrap();
                ra.asm
                    .vpunpcklqdq(xmm_table1, xmm_table1, xmm_table1_upper)
                    .unwrap();
                if is_defaults_zero {
                    ra.asm
                        .vpermi2b(indicies.k(1).z(), XMM0, xmm_table1)
                        .unwrap();
                    ra.define_value(inst_ref, indicies);
                } else {
                    let result = ra.use_scratch_xmm(&mut args[0]);
                    ra.asm.vpermi2b(indicies, XMM0, xmm_table1).unwrap();
                    ra.asm.vmovdqu8(result.k(1), indicies).unwrap();
                    ra.define_value(inst_ref, result);
                }
            }
            _ => unreachable!(),
        }
        return;
    }

    const SAT_CONST: [u64; 5] = [
        0,
        0x7878_7878_7878_7878,
        0x7070_7070_7070_7070,
        0x6868_6868_6868_6868,
        0x6060_6060_6060_6060,
    ];

    if ctx.has_host_feature(HostFeature::SSSE3) && is_defaults_zero && table_size == 1 {
        let indicies = ra.use_scratch_xmm(&mut args[2]);
        let xmm_table0 = ra.use_xmm(&mut table[0]);
        let result = ra.scratch_xmm();

        ra.asm.xorps(result, result).unwrap();
        ra.asm.movsd(result, xmm_table0).unwrap();
        let sat = xword_const(ra, 0x7070_7070_7070_7070, 0xFFFF_FFFF_FFFF_FFFF);
        ra.asm.paddusb(indicies, sat).unwrap();
        ra.asm.pshufb(result, indicies).unwrap();

        ra.define_value(inst_ref, result);
        return;
    }

    if ctx.has_host_feature(HostFeature::SSSE3) && is_defaults_zero && table_size == 2 {
        let indicies = ra.use_scratch_xmm(&mut args[2]);
        let xmm_table0 = ra.use_scratch_xmm(&mut table[0]);
        let xmm_table0_upper = ra.use_xmm(&mut table[1]);

        ra.asm.punpcklqdq(xmm_table0, xmm_table0_upper).unwrap();
        let sat = xword_const(ra, 0x7070_7070_7070_7070, 0xFFFF_FFFF_FFFF_FFFF);
        ra.asm.paddusb(indicies, sat).unwrap();
        ra.asm.pshufb(xmm_table0, indicies).unwrap();

        ra.define_value(inst_ref, xmm_table0);
        return;
    }

    if ctx.has_host_feature(HostFeature::SSE41) && table_size <= 2 {
        let indicies = ra.use_xmm(&mut args[2]);
        let defaults = ra.use_xmm(&mut args[0]);
        let xmm_table0 = ra.use_scratch_xmm(&mut table[0]);

        if table_size == 2 {
            let xmm_table0_upper = ra.use_xmm(&mut table[1]);
            ra.asm.punpcklqdq(xmm_table0, xmm_table0_upper).unwrap();
            ra.release(xmm_table0_upper);
        }

        let sat = xword_const(ra, SAT_CONST[table_size], 0xFFFF_FFFF_FFFF_FFFF);
        if ctx.has_host_feature(HostFeature::AVX) {
            ra.asm.vpaddusb(XMM0, indicies, sat).unwrap();
        } else {
            ra.asm.movaps(XMM0, indicies).unwrap();
            ra.asm.paddusb(XMM0, sat).unwrap();
        }
        ra.asm.pshufb(xmm_table0, indicies).unwrap();
        ra.asm.pblendvb(xmm_table0, defaults).unwrap();

        ra.define_value(inst_ref, xmm_table0);
        return;
    }

    if ctx.has_host_feature(HostFeature::SSE41) && is_defaults_zero {
        let indicies = ra.use_scratch_xmm(&mut args[2]);
        let xmm_table0 = ra.use_scratch_xmm(&mut table[0]);
        let xmm_table1 = ra.use_scratch_xmm(&mut table[2]);

        {
            let xmm_table0_upper = ra.use_xmm(&mut table[1]);
            ra.asm.punpcklqdq(xmm_table0, xmm_table0_upper).unwrap();
            ra.release(xmm_table0_upper);
        }
        if table_size == 3 {
            ra.asm.pxor(XMM0, XMM0).unwrap();
            ra.asm.punpcklqdq(xmm_table1, XMM0).unwrap();
        } else {
            assert_eq!(table_size, 4);
            let xmm_table1_upper = ra.use_xmm(&mut table[3]);
            ra.asm.punpcklqdq(xmm_table1, xmm_table1_upper).unwrap();
            ra.release(xmm_table1_upper);
        }

        let sat70 = xword_const(ra, 0x7070_7070_7070_7070, 0xFFFF_FFFF_FFFF_FFFF);
        if ctx.has_host_feature(HostFeature::AVX) {
            ra.asm.vpaddusb(XMM0, indicies, sat70).unwrap();
        } else {
            ra.asm.movaps(XMM0, indicies).unwrap();
            ra.asm.paddusb(XMM0, sat70).unwrap();
        }
        let sat60 = xword_const(ra, 0x6060_6060_6060_6060, 0xFFFF_FFFF_FFFF_FFFF);
        ra.asm.paddusb(indicies, sat60).unwrap();
        ra.asm.pshufb(xmm_table0, XMM0).unwrap();
        ra.asm.pshufb(xmm_table1, indicies).unwrap();
        ra.asm.pblendvb(xmm_table0, xmm_table1).unwrap();

        ra.define_value(inst_ref, xmm_table0);
        return;
    }

    if ctx.has_host_feature(HostFeature::SSE41) {
        let indicies = ra.use_scratch_xmm(&mut args[2]);
        let defaults = ra.use_xmm(&mut args[0]);
        let xmm_table0 = ra.use_scratch_xmm(&mut table[0]);
        let xmm_table1 = ra.use_scratch_xmm(&mut table[2]);

        {
            let xmm_table0_upper = ra.use_xmm(&mut table[1]);
            ra.asm.punpcklqdq(xmm_table0, xmm_table0_upper).unwrap();
            ra.release(xmm_table0_upper);
        }
        if table_size == 4 {
            let xmm_table1_upper = ra.use_xmm(&mut table[3]);
            ra.asm.punpcklqdq(xmm_table1, xmm_table1_upper).unwrap();
            ra.release(xmm_table1_upper);
        }

        let sat70 = xword_const(ra, 0x7070_7070_7070_7070, 0xFFFF_FFFF_FFFF_FFFF);
        if ctx.has_host_feature(HostFeature::AVX) {
            ra.asm.vpaddusb(XMM0, indicies, sat70).unwrap();
        } else {
            ra.asm.movaps(XMM0, indicies).unwrap();
            ra.asm.paddusb(XMM0, sat70).unwrap();
        }
        ra.asm.pshufb(xmm_table0, indicies).unwrap();
        ra.asm.pshufb(xmm_table1, indicies).unwrap();
        ra.asm.pblendvb(xmm_table0, xmm_table1).unwrap();
        let sat = xword_const(ra, SAT_CONST[table_size], 0xFFFF_FFFF_FFFF_FFFF);
        if ctx.has_host_feature(HostFeature::AVX) {
            ra.asm.vpaddusb(XMM0, indicies, sat).unwrap();
        } else {
            ra.asm.movaps(XMM0, indicies).unwrap();
            ra.asm.paddusb(XMM0, sat).unwrap();
        }
        ra.asm.pblendvb(xmm_table0, defaults).unwrap();

        ra.define_value(inst_ref, xmm_table0);
        return;
    }

    emit_vector_table_lookup_fallback(ra, inst_ref, &mut args, &mut table, table_size, 8);
}

pub fn emit_vector_table_lookup128(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let table_inst = vector_table_inst(ctx, inst);

    // The VectorTable argument is read through `table`, not allocated here.
    let lookup_args = [inst.args[0], Value::Void, inst.args[2]];
    let mut args = ra.get_argument_info(inst_ref, &lookup_args, lookup_args.len());
    let mut table = ra.get_argument_info(inst_ref, &table_inst.args, table_inst.num_args());

    let table_size = table
        .iter()
        .take(table_inst.num_args())
        .filter(|elem| !elem.is_void())
        .count();
    let is_defaults_zero = match inst.args[0] {
        Value::Inst(r) => {
            ctx.block
                .expect("EmitContext::block is required for VectorTableLookup")
                .get(r)
                .opcode
                == Opcode::ZeroVector
        }
        _ => false,
    };

    let vbmi = HostFeature::AVX512_ORTHO | HostFeature::AVX512BW | HostFeature::AVX512VBMI;
    if ctx.has_host_feature(vbmi) && table_size == 4 {
        let indicies = ra.use_scratch_xmm(&mut args[2]);

        let c32 = xword_const(ra, replicate_u8(2 * 16), replicate_u8(2 * 16));
        ra.asm
            .vpcmpub(K1, indicies, c32, cmp_int::LESS_THAN)
            .unwrap();
        let c64 = xword_const(ra, replicate_u8(4 * 16), replicate_u8(4 * 16));
        ra.asm
            .vpcmpub(K2, indicies, c64, cmp_int::LESS_THAN)
            .unwrap();

        // Handle vector-table 0,1
        let xmm_table0 = ra.use_xmm(&mut table[0]);
        let xmm_table1 = ra.use_xmm(&mut table[1]);

        ra.asm
            .vpermi2b(indicies.k(1), xmm_table0, xmm_table1)
            .unwrap();

        ra.release(xmm_table0);
        ra.release(xmm_table1);

        // Handle vector-table 2,3
        let xmm_table2 = ra.use_xmm(&mut table[2]);
        let xmm_table3 = ra.use_xmm(&mut table[3]);

        ra.asm.kandnw(K1, K1, K2).unwrap();
        ra.asm
            .vpermi2b(indicies.k(1), xmm_table2, xmm_table3)
            .unwrap();

        if is_defaults_zero {
            ra.asm.vmovdqu8(indicies.k(2).z(), indicies).unwrap();
            ra.define_value(inst_ref, indicies);
        } else {
            let defaults = ra.use_scratch_xmm(&mut args[0]);
            ra.asm.vmovdqu8(defaults.k(2), indicies).unwrap();
            ra.define_value(inst_ref, defaults);
        }
    } else if ctx.has_host_feature(vbmi) && table_size == 3 {
        let indicies = ra.use_scratch_xmm(&mut args[2]);

        let c32 = xword_const(ra, replicate_u8(2 * 16), replicate_u8(2 * 16));
        ra.asm
            .vpcmpub(K1, indicies, c32, cmp_int::LESS_THAN)
            .unwrap();
        let c48 = xword_const(ra, replicate_u8(3 * 16), replicate_u8(3 * 16));
        ra.asm
            .vpcmpub(K2, indicies, c48, cmp_int::LESS_THAN)
            .unwrap();

        // Handle vector-table 0,1
        let xmm_table0 = ra.use_xmm(&mut table[0]);
        let xmm_table1 = ra.use_xmm(&mut table[1]);

        ra.asm
            .vpermi2b(indicies.k(1), xmm_table0, xmm_table1)
            .unwrap();

        ra.release(xmm_table0);
        ra.release(xmm_table1);

        // Handle vector-table 2
        let xmm_table2 = ra.use_xmm(&mut table[2]);

        ra.asm.kandnw(K1, K1, K2).unwrap();
        ra.asm.vpermb(indicies.k(1), indicies, xmm_table2).unwrap();

        if is_defaults_zero {
            ra.asm.vmovdqu8(indicies.k(2).z(), indicies).unwrap();
            ra.define_value(inst_ref, indicies);
        } else {
            let defaults = ra.use_scratch_xmm(&mut args[0]);
            ra.asm.vmovdqu8(defaults.k(2), indicies).unwrap();
            ra.define_value(inst_ref, defaults);
        }
    } else if ctx.has_host_feature(vbmi) && table_size == 2 {
        let indicies = ra.use_scratch_xmm(&mut args[2]);
        let xmm_table0 = ra.use_xmm(&mut table[0]);
        let xmm_table1 = ra.use_xmm(&mut table[1]);

        let c32 = xword_const(ra, replicate_u8(2 * 16), replicate_u8(2 * 16));
        ra.asm
            .vpcmpub(K1, indicies, c32, cmp_int::LESS_THAN)
            .unwrap();

        if is_defaults_zero {
            ra.asm
                .vpermi2b(indicies.k(1).z(), xmm_table0, xmm_table1)
                .unwrap();
            ra.define_value(inst_ref, indicies);
        } else {
            let result = ra.use_scratch_xmm(&mut args[0]);
            ra.asm.vpermi2b(indicies, xmm_table0, xmm_table1).unwrap();
            ra.asm.vmovdqu8(result.k(1), indicies).unwrap();
            ra.define_value(inst_ref, result);
        }
    } else if ctx.has_host_feature(vbmi) && table_size == 1 {
        let indicies = ra.use_xmm(&mut args[2]);
        let xmm_table0 = ra.use_xmm(&mut table[0]);
        let c16 = xword_const(ra, replicate_u8(16), replicate_u8(16));
        ra.asm
            .vpcmpub(K1, indicies, c16, cmp_int::LESS_THAN)
            .unwrap();
        if is_defaults_zero {
            let result = ra.scratch_xmm();
            ra.asm
                .vpermb(result.k(1).z(), indicies, xmm_table0)
                .unwrap();
            ra.define_value(inst_ref, result);
        } else {
            let result = ra.use_scratch_xmm(&mut args[0]);
            ra.asm.vpermb(result.k(1), indicies, xmm_table0).unwrap();
            ra.define_value(inst_ref, result);
        }
    } else if ctx.has_host_feature(HostFeature::SSSE3) && is_defaults_zero && table_size == 1 {
        let indicies = ra.use_scratch_xmm(&mut args[2]);
        let xmm_table0 = ra.use_scratch_xmm(&mut table[0]);

        let sat = xword_const(ra, 0x7070_7070_7070_7070, 0x7070_7070_7070_7070);
        ra.asm.paddusb(indicies, sat).unwrap();
        ra.asm.pshufb(xmm_table0, indicies).unwrap();

        ra.define_value(inst_ref, xmm_table0);
    } else if ctx.has_host_feature(HostFeature::SSE41) && table_size == 1 {
        let indicies = ra.use_xmm(&mut args[2]);
        let defaults = ra.use_xmm(&mut args[0]);
        let xmm_table0 = ra.use_scratch_xmm(&mut table[0]);

        let sat = xword_const(ra, 0x7070_7070_7070_7070, 0x7070_7070_7070_7070);
        if ctx.has_host_feature(HostFeature::AVX) {
            ra.asm.vpaddusb(XMM0, indicies, sat).unwrap();
        } else {
            ra.asm.movaps(XMM0, indicies).unwrap();
            ra.asm.paddusb(XMM0, sat).unwrap();
        }
        ra.asm.pshufb(xmm_table0, indicies).unwrap();
        ra.asm.pblendvb(xmm_table0, defaults).unwrap();

        ra.define_value(inst_ref, xmm_table0);
    } else if ctx.has_host_feature(HostFeature::SSE41) && is_defaults_zero && table_size == 2 {
        let indicies = ra.use_scratch_xmm(&mut args[2]);
        let xmm_table0 = ra.use_scratch_xmm(&mut table[0]);
        let xmm_table1 = ra.use_scratch_xmm(&mut table[1]);

        let sat70 = xword_const(ra, 0x7070_7070_7070_7070, 0x7070_7070_7070_7070);
        if ctx.has_host_feature(HostFeature::AVX) {
            ra.asm.vpaddusb(XMM0, indicies, sat70).unwrap();
        } else {
            ra.asm.movaps(XMM0, indicies).unwrap();
            ra.asm.paddusb(XMM0, sat70).unwrap();
        }
        let sat60 = xword_const(ra, 0x6060_6060_6060_6060, 0x6060_6060_6060_6060);
        ra.asm.paddusb(indicies, sat60).unwrap();
        ra.asm.pshufb(xmm_table0, XMM0).unwrap();
        ra.asm.pshufb(xmm_table1, indicies).unwrap();
        ra.asm.pblendvb(xmm_table0, xmm_table1).unwrap();

        ra.define_value(inst_ref, xmm_table0);
    } else if ctx.has_host_feature(HostFeature::AVX512_ORTHO | HostFeature::AVX512BW) {
        let indicies = ra.use_xmm(&mut args[2]);
        let result = ra.use_scratch_xmm(&mut args[0]);
        let masked = ra.scratch_xmm();

        // Upstream uses a broadcast operand (`xword_b`); the full 128-bit
        // constant below holds the same value in every lane.
        let high_nibble = xword_const(ra, 0xF0F0_F0F0_F0F0_F0F0, 0xF0F0_F0F0_F0F0_F0F0);
        ra.asm.vpandd(masked, indicies, high_nibble).unwrap();

        for i in 0..table_size {
            let xmm_table = ra.use_scratch_xmm(&mut table[i]);
            let table_index = replicate_u8((i * 16) as u8);
            let index_vector = xword_const(ra, table_index, table_index);

            // Upstream: `vpcmpeqb(k1, masked, Const(...))`. rxbyak only
            // generates the VEX `vpcmpeqb xmm, xmm, xmm/m`; the EVEX
            // opmask-destination form is emitted as the equivalent
            // `vpcmpub k1, masked, Const(...), EQUAL` (same predicate).
            ra.asm
                .vpcmpub(K1, masked, index_vector, cmp_int::EQUAL)
                .unwrap();

            if table_index == 0 && is_defaults_zero {
                ra.asm
                    .vpshufb(result.k(1).z(), xmm_table, indicies)
                    .unwrap();
            } else {
                ra.asm.vpshufb(result.k(1), xmm_table, indicies).unwrap();
            }

            ra.release(xmm_table);
        }

        ra.define_value(inst_ref, result);
    } else if ctx.has_host_feature(HostFeature::SSE41) {
        let indicies = ra.use_xmm(&mut args[2]);
        let result = ra.use_scratch_xmm(&mut args[0]);
        let masked = ra.scratch_xmm();

        let high_nibble = xword_const(ra, 0xF0F0_F0F0_F0F0_F0F0, 0xF0F0_F0F0_F0F0_F0F0);
        ra.asm.movaps(masked, high_nibble).unwrap();
        ra.asm.pand(masked, indicies).unwrap();

        for i in 0..table_size {
            let xmm_table = ra.use_scratch_xmm(&mut table[i]);

            let table_index = replicate_u8((i * 16) as u8);

            if table_index == 0 {
                ra.asm.pxor(XMM0, XMM0).unwrap();
                ra.asm.pcmpeqb(XMM0, masked).unwrap();
            } else if ctx.has_host_feature(HostFeature::AVX) {
                let index_vector = xword_const(ra, table_index, table_index);
                ra.asm.vpcmpeqb(XMM0, masked, index_vector).unwrap();
            } else {
                let index_vector = xword_const(ra, table_index, table_index);
                ra.asm.movaps(XMM0, index_vector).unwrap();
                ra.asm.pcmpeqb(XMM0, masked).unwrap();
            }
            ra.asm.pshufb(xmm_table, indicies).unwrap();
            ra.asm.pblendvb(result, xmm_table).unwrap();

            ra.release(xmm_table);
        }

        ra.define_value(inst_ref, result);
    } else {
        emit_vector_table_lookup_fallback(ra, inst_ref, &mut args, &mut table, table_size, 16);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::x64::callback::Callback;
    use crate::backend::x64::constant_pool::ConstantPool;
    use crate::backend::x64::emit_context::{EmitCallbacks, EmitConfig};
    use crate::ir::block::Block;
    use crate::ir::location::LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::value::Value;
    use rxbyak::CodeAssembler;

    struct NoopCallback;

    impl Callback for NoopCallback {
        fn emit_call(
            &self,
            _code: &mut CodeAssembler,
            _setup: &dyn Fn(&mut CodeAssembler, &[rxbyak::Reg]) -> rxbyak::Result<()>,
        ) -> rxbyak::Result<()> {
            unreachable!("callback emission is not used in this unit test");
        }

        fn emit_call_with_return_pointer(
            &self,
            _code: &mut CodeAssembler,
            _setup: &dyn Fn(&mut CodeAssembler, rxbyak::Reg, &[rxbyak::Reg]) -> rxbyak::Result<()>,
        ) -> rxbyak::Result<()> {
            unreachable!("callback emission is not used in this unit test");
        }
    }

    fn dummy_emit_config() -> EmitConfig {
        fn cb() -> Box<dyn Callback> {
            Box::new(NoopCallback)
        }

        EmitConfig {
            coprocessors: crate::interface::a32::config::empty_coprocessors(),
            callbacks: EmitCallbacks {
                memory_read_8: cb(),
                memory_read_16: cb(),
                memory_read_32: cb(),
                memory_read_64: cb(),
                memory_read_128: cb(),
                memory_write_8: cb(),
                memory_write_16: cb(),
                memory_write_32: cb(),
                memory_write_64: cb(),
                memory_write_128: cb(),
                call_supervisor: cb(),
                exception_raised: cb(),
                data_cache_operation: cb(),
                instruction_cache_operation: cb(),
                instruction_synchronization_barrier: cb(),
                add_ticks: cb(),
                get_ticks_remaining: cb(),
                exclusive_clear: cb(),
                exclusive_read_8: cb(),
                exclusive_read_16: cb(),
                exclusive_read_32: cb(),
                exclusive_read_64: cb(),
                exclusive_read_128: cb(),
                get_cntpct: cb(),
                exclusive_write_8: cb(),
                exclusive_write_16: cb(),
                exclusive_write_32: cb(),
                exclusive_write_64: cb(),
                exclusive_write_128: cb(),
            },
            raw_exclusive_write_callbacks: None,
            enable_cycle_counting: false,
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
            global_monitor: None,
            tpidrro_el0: None,
            tpidr_el0: None,
            cntfrq_el0: 600_000_000,
            ctr_el0: 0x8444_c004,
            dczid_el0: 4,
            hook_data_cache_operations: false,
            hook_isb: false,
        }
    }

    fn emit_with_features(opcode: Opcode, index: u8, features: HostFeature) -> Vec<u8> {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let mut constant_pool = ConstantPool::new(1024);
        constant_pool.set_pool_base(unsafe { asm.top().add(3072) as *mut u8 });
        let mut ra = RegAlloc::new_default(&mut asm, vec![(1, 128), (0, 128)]);
        ra.constant_pool = Some(&mut constant_pool);
        let source = ra.scratch_xmm();
        ra.define_value(InstRef(0), source);
        ra.end_of_alloc_scope();

        let config = dummy_emit_config();
        let mut ctx = EmitContext::new(LocationDescriptor::new(0), &config);
        ctx.host_features = features;
        let inst = if matches!(
            opcode,
            Opcode::VectorReduceAdd8
                | Opcode::VectorReduceAdd16
                | Opcode::VectorReduceAdd32
                | Opcode::VectorReduceAdd64
        ) {
            Inst::new(opcode, &[Value::Inst(InstRef(0))])
        } else {
            Inst::new(opcode, &[Value::Inst(InstRef(0)), Value::ImmU8(index)])
        };
        match opcode {
            Opcode::VectorBroadcastElementLower8 => {
                emit_vector_broadcast_element_lower8(&ctx, &mut ra, InstRef(1), &inst)
            }
            Opcode::VectorBroadcastElementLower16 => {
                emit_vector_broadcast_element_lower16(&ctx, &mut ra, InstRef(1), &inst)
            }
            Opcode::VectorBroadcastElementLower32 => {
                emit_vector_broadcast_element_lower32(&ctx, &mut ra, InstRef(1), &inst)
            }
            Opcode::VectorBroadcastElement8 => {
                emit_vector_broadcast_element8(&ctx, &mut ra, InstRef(1), &inst)
            }
            Opcode::VectorBroadcastElement16 => {
                emit_vector_broadcast_element16(&ctx, &mut ra, InstRef(1), &inst)
            }
            Opcode::VectorBroadcastElement32 => {
                emit_vector_broadcast_element32(&ctx, &mut ra, InstRef(1), &inst)
            }
            Opcode::VectorBroadcastElement64 => {
                emit_vector_broadcast_element64(&ctx, &mut ra, InstRef(1), &inst)
            }
            Opcode::VectorReduceAdd8 => emit_vector_reduce_add8(&ctx, &mut ra, InstRef(1), &inst),
            Opcode::VectorReduceAdd16 => emit_vector_reduce_add16(&ctx, &mut ra, InstRef(1), &inst),
            Opcode::VectorReduceAdd32 => emit_vector_reduce_add32(&ctx, &mut ra, InstRef(1), &inst),
            Opcode::VectorReduceAdd64 => emit_vector_reduce_add64(&ctx, &mut ra, InstRef(1), &inst),
            _ => unreachable!("test helper only covers selected broadcast-element opcodes"),
        }
        ra.end_of_alloc_scope();
        ra.asm.code().to_vec()
    }

    fn emit_multiply_with_features(
        opcode: Opcode,
        upper: bool,
        lower: bool,
        features: HostFeature,
    ) -> Vec<u8> {
        let mut block = Block::new(LocationDescriptor::new(0));
        let lhs = block.append(Opcode::ZeroVector, &[]);
        let rhs = block.append(Opcode::ZeroVector, &[]);
        let multiply = block.append(opcode, &[Value::Inst(lhs), Value::Inst(rhs)]);
        if upper {
            block.append(Opcode::GetUpperFromOp, &[Value::Inst(multiply)]);
        }
        if lower {
            block.append(Opcode::GetLowerFromOp, &[Value::Inst(multiply)]);
        }
        block.rebuild_pseudo_op_links();

        let mut asm = CodeAssembler::new(4096).unwrap();
        let mut constant_pool = ConstantPool::new(1024);
        constant_pool.set_pool_base(unsafe { asm.top().add(3072) as *mut u8 });
        let mut inst_info = vec![(1, 128), (1, 128), ((upper as u32) + (lower as u32), 0)];
        if upper {
            inst_info.push((0, 128));
        }
        if lower {
            inst_info.push((0, 128));
        }
        let mut ra = RegAlloc::new_default(&mut asm, inst_info);
        ra.constant_pool = Some(&mut constant_pool);

        let lhs_reg = ra.scratch_xmm();
        ra.define_value(lhs, lhs_reg);
        ra.end_of_alloc_scope();
        let rhs_reg = ra.scratch_xmm();
        ra.define_value(rhs, rhs_reg);
        ra.end_of_alloc_scope();

        let config = dummy_emit_config();
        let mut ctx = EmitContext::new(LocationDescriptor::new(0), &config);
        ctx.host_features = features;
        ctx.block = Some(&block);
        let inst = block.get(multiply);
        match opcode {
            Opcode::VectorSignedMultiply16 => {
                emit_vector_signed_multiply16(&ctx, &mut ra, multiply, inst)
            }
            Opcode::VectorSignedMultiply32 => {
                emit_vector_signed_multiply32(&ctx, &mut ra, multiply, inst)
            }
            Opcode::VectorUnsignedMultiply16 => {
                emit_vector_unsigned_multiply16(&ctx, &mut ra, multiply, inst)
            }
            Opcode::VectorUnsignedMultiply32 => {
                emit_vector_unsigned_multiply32(&ctx, &mut ra, multiply, inst)
            }
            _ => unreachable!("test helper only covers multi-result multiply opcodes"),
        }
        ra.end_of_alloc_scope();
        ra.asm.code().to_vec()
    }

    fn has_legacy_opcode(code: &[u8], opcode: u8) -> bool {
        code.windows(3).any(|bytes| bytes == [0x66, 0x0f, opcode])
    }

    fn has_vex_opcode(code: &[u8], opcode: u8) -> bool {
        code.windows(4).any(|bytes| {
            (bytes[0] == 0xc5 && bytes[2] == opcode) || (bytes[0] == 0xc4 && bytes[3] == opcode)
        })
    }

    #[test]
    fn function_signatures_match_the_upstream_vector_emitters() {
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_broadcast_element_lower8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_broadcast_element_lower16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_broadcast_element_lower32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_broadcast_element8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_broadcast_element16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_broadcast_element32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_broadcast_element64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_reduce_add8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_reduce_add16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_reduce_add32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_reduce_add64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_signed_multiply16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_signed_multiply32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_unsigned_multiply16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_unsigned_multiply32;
    }

    #[test]
    fn signed_multiply16_selects_edens_avx_and_sse_paths() {
        let avx = emit_multiply_with_features(
            Opcode::VectorSignedMultiply16,
            true,
            true,
            HostFeature::AVX,
        );
        assert!(has_vex_opcode(&avx, 0xe5), "{avx:02x?}");
        assert!(has_vex_opcode(&avx, 0xd5), "{avx:02x?}");

        let sse = emit_multiply_with_features(
            Opcode::VectorSignedMultiply16,
            true,
            true,
            HostFeature::empty(),
        );
        assert!(has_legacy_opcode(&sse, 0xe5), "{sse:02x?}");
        assert!(has_legacy_opcode(&sse, 0xd5), "{sse:02x?}");
    }

    #[test]
    fn signed_multiply32_preserves_edens_result_sensitive_paths() {
        let lower_only = emit_multiply_with_features(
            Opcode::VectorSignedMultiply32,
            false,
            true,
            HostFeature::AVX,
        );
        assert!(has_vex_opcode(&lower_only, 0x40), "{lower_only:02x?}");
        assert!(!has_vex_opcode(&lower_only, 0x28), "{lower_only:02x?}");

        let both = emit_multiply_with_features(
            Opcode::VectorSignedMultiply32,
            true,
            true,
            HostFeature::empty(),
        );
        assert!(has_legacy_opcode(&both, 0xf4), "{both:02x?}");
        assert!(has_legacy_opcode(&both, 0xfa), "{both:02x?}");
    }

    #[test]
    fn unsigned_multiply16_selects_edens_high_and_low_products() {
        let sse = emit_multiply_with_features(
            Opcode::VectorUnsignedMultiply16,
            true,
            true,
            HostFeature::empty(),
        );
        assert!(has_legacy_opcode(&sse, 0xe4), "{sse:02x?}");
        assert!(has_legacy_opcode(&sse, 0xd5), "{sse:02x?}");
    }

    #[test]
    fn unsigned_multiply32_preserves_edens_result_sensitive_paths() {
        let lower_only = emit_multiply_with_features(
            Opcode::VectorUnsignedMultiply32,
            false,
            true,
            HostFeature::AVX,
        );
        assert!(has_vex_opcode(&lower_only, 0x40), "{lower_only:02x?}");
        assert!(!has_vex_opcode(&lower_only, 0xf4), "{lower_only:02x?}");

        let both = emit_multiply_with_features(
            Opcode::VectorUnsignedMultiply32,
            true,
            true,
            HostFeature::empty(),
        );
        assert!(has_legacy_opcode(&both, 0xf4), "{both:02x?}");
        assert!(!has_legacy_opcode(&both, 0xfa), "{both:02x?}");
    }

    #[test]
    fn lower8_avx2_uses_vpbroadcastb_then_vmovq() {
        let code = emit_with_features(Opcode::VectorBroadcastElementLower8, 0, HostFeature::AVX2);
        assert!(
            code.windows(4)
                .any(|bytes| bytes[0] == 0xc4 && bytes[3] == 0x78),
            "{code:02x?}"
        );
        assert!(
            code.windows(4)
                .any(|bytes| bytes[0] == 0xc5 && bytes[2] == 0x7e),
            "{code:02x?}"
        );
    }

    #[test]
    fn lower8_selects_edens_ssse3_and_sse2_fallbacks() {
        let ssse3 = emit_with_features(Opcode::VectorBroadcastElementLower8, 1, HostFeature::SSSE3);
        assert!(ssse3
            .windows(4)
            .any(|bytes| bytes[..4] == [0x66, 0x0f, 0x38, 0x00]));
        assert!(ssse3
            .windows(3)
            .any(|bytes| bytes[..3] == [0xf3, 0x0f, 0x7e]));

        let sse2 = emit_with_features(
            Opcode::VectorBroadcastElementLower8,
            1,
            HostFeature::empty(),
        );
        assert!(sse2
            .windows(3)
            .any(|bytes| bytes[..3] == [0x66, 0x0f, 0x60]));
        assert!(sse2
            .windows(3)
            .any(|bytes| bytes[..3] == [0xf2, 0x0f, 0x70]));
        assert!(!sse2
            .windows(3)
            .any(|bytes| bytes[..3] == [0x66, 0x0f, 0x6c]));
    }

    #[test]
    fn full16_selects_edens_avx2_and_upper_half_fallbacks() {
        let avx2 = emit_with_features(Opcode::VectorBroadcastElement16, 0, HostFeature::AVX2);
        assert!(avx2
            .windows(4)
            .any(|bytes| bytes[0] == 0xc4 && bytes[3] == 0x79));

        let upper = emit_with_features(Opcode::VectorBroadcastElement16, 5, HostFeature::empty());
        assert!(upper
            .windows(3)
            .any(|bytes| bytes[..3] == [0xf3, 0x0f, 0x70]));
        assert!(upper
            .windows(3)
            .any(|bytes| bytes[..3] == [0x66, 0x0f, 0x6d]));
    }

    #[test]
    fn lower32_shifts_selected_lane_then_uses_upstream_shuffle() {
        let code = emit_with_features(
            Opcode::VectorBroadcastElementLower32,
            2,
            HostFeature::empty(),
        );
        assert!(code.windows(5).any(|bytes| bytes[0] == 0x66
            && bytes[1] == 0x0f
            && bytes[2] == 0x73
            && bytes[4] == 8));
        assert_eq!(code.last(), Some(&0b01_00_01_00));
    }

    #[test]
    fn full32_replicates_the_selected_lane_in_the_shuffle_control() {
        let code = emit_with_features(Opcode::VectorBroadcastElement32, 3, HostFeature::empty());
        assert_eq!(code.last(), Some(&0xff));
    }

    #[test]
    fn full64_avx_uses_the_immediate_vpermilpd_form() {
        let code = emit_with_features(Opcode::VectorBroadcastElement64, 1, HostFeature::AVX);
        assert!(
            code.windows(6)
                .any(|bytes| bytes[0] == 0xc4 && bytes[3] == 0x05 && bytes[5] == 3),
            "{code:02x?}"
        );
    }

    #[test]
    fn reduce_add8_uses_edens_byte_sum_sequence() {
        let code = emit_with_features(Opcode::VectorReduceAdd8, 0, HostFeature::empty());
        assert!(code
            .windows(3)
            .any(|bytes| bytes[..3] == [0x66, 0x0f, 0xfc]));
        assert!(code
            .windows(3)
            .any(|bytes| bytes[..3] == [0x66, 0x0f, 0xf6]));
        assert_eq!(code.last(), Some(&15));
    }

    #[test]
    fn reduce_add16_ssse3_emits_three_horizontal_adds() {
        let code = emit_with_features(Opcode::VectorReduceAdd16, 0, HostFeature::SSSE3);
        assert_eq!(
            code.windows(4)
                .filter(|bytes| bytes[..4] == [0x66, 0x0f, 0x38, 0x01])
                .count(),
            3
        );
    }

    #[test]
    fn reduce_add16_sse2_uses_edens_multiply_add_fallback() {
        let code = emit_with_features(Opcode::VectorReduceAdd16, 0, HostFeature::empty());
        assert!(code
            .windows(3)
            .any(|bytes| bytes[..3] == [0x66, 0x0f, 0xf5]));
        assert!(code.windows(5).any(|bytes| bytes[0] == 0x66
            && bytes[1] == 0x0f
            && bytes[2] == 0x73
            && bytes[4] == 14));
        assert_eq!(code.last(), Some(&14));
    }

    #[test]
    fn reduce_add32_and_64_use_edens_final_reduction_steps() {
        let reduce32 = emit_with_features(Opcode::VectorReduceAdd32, 0, HostFeature::SSSE3);
        assert!(reduce32
            .windows(4)
            .any(|bytes| bytes[..4] == [0x66, 0x0f, 0x38, 0x02]));
        assert_eq!(reduce32.last(), Some(&12));

        let reduce64 = emit_with_features(Opcode::VectorReduceAdd64, 0, HostFeature::empty());
        assert!(reduce64
            .windows(3)
            .any(|bytes| bytes[..3] == [0x66, 0x0f, 0xd4]));
        assert!(reduce64
            .windows(3)
            .any(|bytes| bytes[..3] == [0xf3, 0x0f, 0x7e]));
    }
}

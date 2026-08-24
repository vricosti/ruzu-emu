use rxbyak::{byte_ptr, dword_ptr, qword_ptr, xmmword_ptr, Reg, RegExp, R15, RSP};

use crate::backend::x64::abi;
use crate::backend::x64::emit_context::EmitContext;
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::ir::inst::Inst;
use crate::ir::value::InstRef;

// ---------------------------------------------------------------------------
// Native SSE binary op: result = op(arg0, arg1)
// UseScratchXmm(arg0) + UseXmm(arg1) → op(result, op2) → DefineValue
// ---------------------------------------------------------------------------

pub fn emit_vector_op(
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    op: fn(&mut rxbyak::CodeAssembler, Reg, Reg) -> rxbyak::Result<()>,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let op2 = ra.use_xmm(&mut args[1]);
    op(&mut *ra.asm, result, op2).unwrap();
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// Native SSE binary op with immediate: result = op(arg0, imm)
// UseScratchXmm(arg0) → op(result, imm) → DefineValue
// ---------------------------------------------------------------------------

pub fn emit_vector_op_imm(
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    op: fn(&mut rxbyak::CodeAssembler, Reg, u8) -> rxbyak::Result<()>,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let imm = args[1].get_immediate_u8();
    op(&mut *ra.asm, result, imm).unwrap();
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// Native SSE unary op: result = op(arg0)
// For ops where dst and src are the same register (e.g., pabsb dst,src)
// ---------------------------------------------------------------------------

pub fn emit_vector_unary_op(
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    op: fn(&mut rxbyak::CodeAssembler, Reg, Reg) -> rxbyak::Result<()>,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let src = ra.use_xmm(&mut args[0]);
    let result = ra.scratch_xmm();
    op(&mut *ra.asm, result, src).unwrap();
    ra.release(src);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// Stack-based 1-arg vector fallback
// fn(result: *mut [u8;16], a: *const [u8;16])
// Stack layout: [result:16][a:16] = 32 bytes
// ---------------------------------------------------------------------------

pub fn emit_one_arg_fallback(ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst, func: usize) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let arg1 = ra.use_xmm(&mut args[0]);
    let result = ra.scratch_xmm();
    ra.end_of_alloc_scope();

    // Spill all caller-saved
    ra.host_call(None, &mut [None, None, None, None]);

    let result_offset = abi::ABI_SHADOW_SPACE as i32;
    let operand_offset = result_offset + 16;
    let frame_size = abi::ABI_SHADOW_SPACE + 32;
    ra.alloc_stack_space(frame_size);

    ra.asm
        .movaps(
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand_offset),
            arg1,
        )
        .unwrap();

    let result_param = abi::ABI_PARAMS[0].to_reg64();
    let operand_param = abi::ABI_PARAMS[1].to_reg64();
    ra.asm
        .lea(
            result_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();
    ra.asm
        .lea(
            operand_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand_offset),
        )
        .unwrap();

    // Call
    ra.asm.mov(rxbyak::RAX, func as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();

    ra.asm
        .movaps(
            result,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();

    ra.release_stack_space(frame_size);
    ra.define_value(inst_ref, result);
}

/// FP-aware one-vector-argument fallback.
///
/// Matches upstream `EmitTwoOpFallback`: the IR's second argument selects the
/// current FPCR or the standard ASIMD value, and the fourth host argument
/// points at the architecture-specific sticky FPSR exception field.
pub fn emit_two_op_fallback(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    func: usize,
) {
    emit_two_op_fallback_with_fpcr_arg(ctx, ra, inst_ref, inst, 1, func);
}

pub fn emit_two_op_fallback_with_fpcr_arg(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    fpcr_arg_index: usize,
    func: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let fpcr_controlled = args[fpcr_arg_index].get_immediate_u1();
    let arg1 = ra.use_xmm(&mut args[0]);
    let result = ra.scratch_xmm();
    ra.end_of_alloc_scope();
    ra.host_call(None, &mut [None, None, None, None]);

    let result_offset = abi::ABI_SHADOW_SPACE as i32;
    let operand_offset = result_offset + 16;
    let frame_size = abi::ABI_SHADOW_SPACE + 32;
    ra.alloc_stack_space(frame_size);
    ra.asm
        .movaps(
            rxbyak::xmmword_ptr(RegExp::from(rxbyak::RSP) + operand_offset),
            arg1,
        )
        .unwrap();

    let result_param = abi::ABI_PARAMS[0].to_reg64();
    let operand_param = abi::ABI_PARAMS[1].to_reg64();
    let fpcr_param = abi::ABI_PARAMS[2].to_reg64();
    let fpsr_param = abi::ABI_PARAMS[3].to_reg64();
    ra.asm
        .lea(
            result_param,
            rxbyak::xmmword_ptr(RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();
    ra.asm
        .lea(
            operand_param,
            rxbyak::xmmword_ptr(RegExp::from(rxbyak::RSP) + operand_offset),
        )
        .unwrap();
    ra.asm
        .mov(
            Reg::gpr32(fpcr_param.get_idx()),
            ctx.fpcr(fpcr_controlled).value() as i32,
        )
        .unwrap();
    ra.asm
        .lea(
            fpsr_param,
            rxbyak::dword_ptr(RegExp::from(R15) + ctx.arch.fpsr_exc_offset() as i32),
        )
        .unwrap();
    ra.asm.mov(rxbyak::RAX, func as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();

    ra.asm
        .movaps(
            result,
            rxbyak::xmmword_ptr(RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();
    ra.release_stack_space(frame_size);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// Stack-based 2-arg vector fallback
// fn(result: *mut [u8;16], a: *const [u8;16], b: *const [u8;16])
// Stack layout: [result:16][a:16][b:16] = 48 bytes
// ---------------------------------------------------------------------------

pub fn emit_two_arg_fallback(ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst, func: usize) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let arg1 = ra.use_xmm(&mut args[0]);
    let arg2 = ra.use_xmm(&mut args[1]);
    let result = ra.scratch_xmm();
    ra.end_of_alloc_scope();

    ra.host_call(None, &mut [None, None, None, None]);

    let result_offset = abi::ABI_SHADOW_SPACE as i32;
    let operand1_offset = result_offset + 16;
    let operand2_offset = result_offset + 32;
    let frame_size = abi::ABI_SHADOW_SPACE + 48;
    ra.alloc_stack_space(frame_size);

    ra.asm
        .movaps(
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand1_offset),
            arg1,
        )
        .unwrap();
    ra.asm
        .movaps(
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand2_offset),
            arg2,
        )
        .unwrap();

    let result_param = abi::ABI_PARAMS[0].to_reg64();
    let operand1_param = abi::ABI_PARAMS[1].to_reg64();
    let operand2_param = abi::ABI_PARAMS[2].to_reg64();
    ra.asm
        .lea(
            result_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();
    ra.asm
        .lea(
            operand1_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand1_offset),
        )
        .unwrap();
    ra.asm
        .lea(
            operand2_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand2_offset),
        )
        .unwrap();

    ra.asm.mov(rxbyak::RAX, func as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();

    ra.asm
        .movaps(
            result,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();

    ra.release_stack_space(frame_size);
    ra.define_value(inst_ref, result);
}

/// FP-aware two-vector-argument fallback.
///
/// Matches upstream `EmitThreeOpFallback`: the IR's third argument selects the
/// current FPCR or the standard ASIMD value, and the fifth host argument points
/// at the architecture-specific sticky FPSR exception field.
pub fn emit_three_op_fallback(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    func: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let fpcr_controlled = args[2].get_immediate_u1();
    let arg1 = ra.use_xmm(&mut args[0]);
    let arg2 = ra.use_xmm(&mut args[1]);
    let result = ra.scratch_xmm();
    ra.end_of_alloc_scope();
    ra.host_call(None, &mut [None, None, None, None]);

    // Windows places the fifth integer argument immediately after its 32-byte
    // shadow space. Keep another eight bytes of padding so the vector slots
    // remain 16-byte aligned.
    #[cfg(target_os = "windows")]
    let stack_argument_space = 16usize;
    #[cfg(not(target_os = "windows"))]
    let stack_argument_space = 0usize;

    let result_offset = (abi::ABI_SHADOW_SPACE + stack_argument_space) as i32;
    let operand1_offset = result_offset + 16;
    let operand2_offset = result_offset + 32;
    let frame_size = abi::ABI_SHADOW_SPACE + stack_argument_space + 48;
    ra.alloc_stack_space(frame_size);

    ra.asm
        .movaps(
            rxbyak::xmmword_ptr(RegExp::from(rxbyak::RSP) + operand1_offset),
            arg1,
        )
        .unwrap();
    ra.asm
        .movaps(
            rxbyak::xmmword_ptr(RegExp::from(rxbyak::RSP) + operand2_offset),
            arg2,
        )
        .unwrap();

    let result_param = abi::ABI_PARAMS[0].to_reg64();
    let operand1_param = abi::ABI_PARAMS[1].to_reg64();
    let operand2_param = abi::ABI_PARAMS[2].to_reg64();
    let fpcr_param = abi::ABI_PARAMS[3].to_reg64();
    ra.asm
        .lea(
            result_param,
            rxbyak::xmmword_ptr(RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();
    ra.asm
        .lea(
            operand1_param,
            rxbyak::xmmword_ptr(RegExp::from(rxbyak::RSP) + operand1_offset),
        )
        .unwrap();
    ra.asm
        .lea(
            operand2_param,
            rxbyak::xmmword_ptr(RegExp::from(rxbyak::RSP) + operand2_offset),
        )
        .unwrap();
    ra.asm
        .mov(
            Reg::gpr32(fpcr_param.get_idx()),
            ctx.fpcr(fpcr_controlled).value() as i32,
        )
        .unwrap();

    #[cfg(target_os = "windows")]
    {
        ra.asm
            .lea(
                rxbyak::RAX,
                rxbyak::dword_ptr(RegExp::from(R15) + ctx.arch.fpsr_exc_offset() as i32),
            )
            .unwrap();
        ra.asm
            .mov(
                rxbyak::qword_ptr(RegExp::from(rxbyak::RSP) + abi::ABI_SHADOW_SPACE as i32),
                rxbyak::RAX,
            )
            .unwrap();
    }
    #[cfg(not(target_os = "windows"))]
    ra.asm
        .lea(
            abi::ABI_PARAMS[4].to_reg64(),
            rxbyak::dword_ptr(RegExp::from(R15) + ctx.arch.fpsr_exc_offset() as i32),
        )
        .unwrap();

    ra.asm.mov(rxbyak::RAX, func as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();
    ra.asm
        .movaps(
            result,
            rxbyak::xmmword_ptr(RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();

    ra.release_stack_space(frame_size);
    ra.define_value(inst_ref, result);
}

/// FP-aware one-vector fallback with packed immediate parameters.
///
/// This is the Rust ABI adaptation of upstream `EmitTwoOpFallback<3>` for
/// vector FP-to-fixed conversions. The packed value carries `fbits` and the
/// rounding mode; FPCR and the sticky FPSR pointer retain upstream ordering.
pub fn emit_fp_one_arg_fallback_with_params(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    func: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let fbits = args[1].get_immediate_u8();
    let rounding = args[2].get_immediate_u8();
    let fpcr_controlled = args[3].get_immediate_u1();
    let operand = ra.use_xmm(&mut args[0]);
    let result = ra.scratch_xmm();
    ra.end_of_alloc_scope();
    ra.host_call(None, &mut [None, None, None, None]);

    #[cfg(target_os = "windows")]
    let stack_argument_space = 16usize;
    #[cfg(not(target_os = "windows"))]
    let stack_argument_space = 0usize;

    let result_offset = (abi::ABI_SHADOW_SPACE + stack_argument_space) as i32;
    let operand_offset = result_offset + 16;
    let frame_size = abi::ABI_SHADOW_SPACE + stack_argument_space + 32;
    ra.alloc_stack_space(frame_size);
    ra.asm
        .movaps(
            rxbyak::xmmword_ptr(RegExp::from(rxbyak::RSP) + operand_offset),
            operand,
        )
        .unwrap();

    ra.asm
        .lea(
            abi::ABI_PARAMS[0].to_reg64(),
            rxbyak::xmmword_ptr(RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();
    ra.asm
        .lea(
            abi::ABI_PARAMS[1].to_reg64(),
            rxbyak::xmmword_ptr(RegExp::from(rxbyak::RSP) + operand_offset),
        )
        .unwrap();
    ra.asm
        .mov(
            abi::ABI_PARAMS[2].to_reg64(),
            (u64::from(fbits) | (u64::from(rounding) << 8)) as i64,
        )
        .unwrap();
    ra.asm
        .mov(
            Reg::gpr32(abi::ABI_PARAMS[3].to_reg64().get_idx()),
            ctx.fpcr(fpcr_controlled).value() as i32,
        )
        .unwrap();

    #[cfg(target_os = "windows")]
    {
        ra.asm
            .lea(
                rxbyak::RAX,
                rxbyak::dword_ptr(RegExp::from(R15) + ctx.arch.fpsr_exc_offset() as i32),
            )
            .unwrap();
        ra.asm
            .mov(
                rxbyak::qword_ptr(RegExp::from(rxbyak::RSP) + abi::ABI_SHADOW_SPACE as i32),
                rxbyak::RAX,
            )
            .unwrap();
    }
    #[cfg(not(target_os = "windows"))]
    ra.asm
        .lea(
            abi::ABI_PARAMS[4].to_reg64(),
            rxbyak::dword_ptr(RegExp::from(R15) + ctx.arch.fpsr_exc_offset() as i32),
        )
        .unwrap();

    ra.asm.mov(rxbyak::RAX, func as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();
    ra.asm
        .movaps(
            result,
            rxbyak::xmmword_ptr(RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();
    ra.release_stack_space(frame_size);
    ra.define_value(inst_ref, result);
}

/// Port of upstream `EmitThreeOpFallbackWithoutRegAlloc`.
///
/// The caller has already ended register allocation and preserved the live
/// caller-save registers. `fpcr_value` and `fpsr_exc_offset` are captured as
/// plain values so deferred emitters do not retain a borrow of `EmitContext`.
pub fn emit_three_op_fallback_without_reg_alloc(
    asm: &mut rxbyak::CodeAssembler,
    result: Reg,
    arg1: Reg,
    arg2: Reg,
    func: usize,
    fpcr_value: u32,
    fpsr_exc_offset: i32,
) {
    #[cfg(target_os = "windows")]
    const STACK_SPACE: usize = 4 * 16;
    #[cfg(not(target_os = "windows"))]
    const STACK_SPACE: usize = 3 * 16;

    let frame_size = STACK_SPACE + abi::ABI_SHADOW_SPACE;
    asm.lea(RSP, qword_ptr(RegExp::from(RSP) - frame_size as i32))
        .unwrap();

    #[cfg(target_os = "windows")]
    let result_offset = abi::ABI_SHADOW_SPACE + 16;
    #[cfg(not(target_os = "windows"))]
    let result_offset = abi::ABI_SHADOW_SPACE;
    let arg1_offset = result_offset + 16;
    let arg2_offset = result_offset + 32;

    asm.lea(
        abi::ABI_PARAMS[0].to_reg64(),
        xmmword_ptr(RegExp::from(RSP) + result_offset as i32),
    )
    .unwrap();
    asm.lea(
        abi::ABI_PARAMS[1].to_reg64(),
        xmmword_ptr(RegExp::from(RSP) + arg1_offset as i32),
    )
    .unwrap();
    asm.lea(
        abi::ABI_PARAMS[2].to_reg64(),
        xmmword_ptr(RegExp::from(RSP) + arg2_offset as i32),
    )
    .unwrap();
    asm.mov(
        Reg::gpr32(abi::ABI_PARAMS[3].to_reg64().get_idx()),
        fpcr_value as i32,
    )
    .unwrap();

    #[cfg(target_os = "windows")]
    {
        asm.lea(rxbyak::RAX, dword_ptr(RegExp::from(R15) + fpsr_exc_offset))
            .unwrap();
        asm.mov(
            qword_ptr(RegExp::from(RSP) + abi::ABI_SHADOW_SPACE as i32),
            rxbyak::RAX,
        )
        .unwrap();
    }
    #[cfg(not(target_os = "windows"))]
    asm.lea(
        abi::ABI_PARAMS[4].to_reg64(),
        dword_ptr(RegExp::from(R15) + fpsr_exc_offset),
    )
    .unwrap();

    asm.movaps(
        xmmword_ptr(RegExp::from(abi::ABI_PARAMS[1].to_reg64())),
        arg1,
    )
    .unwrap();
    asm.movaps(
        xmmword_ptr(RegExp::from(abi::ABI_PARAMS[2].to_reg64())),
        arg2,
    )
    .unwrap();
    asm.mov(rxbyak::RAX, func as i64).unwrap();
    asm.call_reg(rxbyak::RAX).unwrap();
    asm.movaps(
        result,
        xmmword_ptr(RegExp::from(RSP) + result_offset as i32),
    )
    .unwrap();
    asm.add(RSP, frame_size as i32).unwrap();
}

// ---------------------------------------------------------------------------
// Stack-based 2-arg + immediate vector fallback
// fn(result: *mut [u8;16], a: *const [u8;16], b: *const [u8;16], imm: u8)
// imm goes in RCX
// ---------------------------------------------------------------------------

pub fn emit_two_arg_fallback_with_imm(
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    func: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let arg1 = ra.use_xmm(&mut args[0]);
    let arg2 = ra.use_xmm(&mut args[1]);
    let imm = args[2].get_immediate_u8();
    let result = ra.scratch_xmm();
    ra.end_of_alloc_scope();

    ra.host_call(None, &mut [None, None, None, None]);

    let result_offset = abi::ABI_SHADOW_SPACE as i32;
    let operand1_offset = result_offset + 16;
    let operand2_offset = result_offset + 32;
    let frame_size = abi::ABI_SHADOW_SPACE + 48;
    ra.alloc_stack_space(frame_size);

    ra.asm
        .movaps(
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand1_offset),
            arg1,
        )
        .unwrap();
    ra.asm
        .movaps(
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand2_offset),
            arg2,
        )
        .unwrap();

    let result_param = abi::ABI_PARAMS[0].to_reg64();
    let operand1_param = abi::ABI_PARAMS[1].to_reg64();
    let operand2_param = abi::ABI_PARAMS[2].to_reg64();
    let immediate_param = abi::ABI_PARAMS[3].to_reg64();
    ra.asm
        .lea(
            result_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();
    ra.asm
        .lea(
            operand1_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand1_offset),
        )
        .unwrap();
    ra.asm
        .lea(
            operand2_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand2_offset),
        )
        .unwrap();
    ra.asm.mov(immediate_param, imm as i64).unwrap();

    ra.asm.mov(rxbyak::RAX, func as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();

    ra.asm
        .movaps(
            result,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();

    ra.release_stack_space(frame_size);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// Stack-based 3-arg vector fallback
// fn(result: *mut [u8;16], a: *const [u8;16], b: *const [u8;16], c: *const [u8;16])
// ---------------------------------------------------------------------------

pub fn emit_three_arg_fallback(ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst, func: usize) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let arg1 = ra.use_xmm(&mut args[0]);
    let arg2 = ra.use_xmm(&mut args[1]);
    let arg3 = ra.use_xmm(&mut args[2]);
    let result = ra.scratch_xmm();
    ra.end_of_alloc_scope();

    ra.host_call(None, &mut [None, None, None, None]);

    let result_offset = abi::ABI_SHADOW_SPACE as i32;
    let operand1_offset = result_offset + 16;
    let operand2_offset = result_offset + 32;
    let operand3_offset = result_offset + 48;
    let frame_size = abi::ABI_SHADOW_SPACE + 64;
    ra.alloc_stack_space(frame_size);

    ra.asm
        .movaps(
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand1_offset),
            arg1,
        )
        .unwrap();
    ra.asm
        .movaps(
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand2_offset),
            arg2,
        )
        .unwrap();
    ra.asm
        .movaps(
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand3_offset),
            arg3,
        )
        .unwrap();

    let result_param = abi::ABI_PARAMS[0].to_reg64();
    let operand1_param = abi::ABI_PARAMS[1].to_reg64();
    let operand2_param = abi::ABI_PARAMS[2].to_reg64();
    let operand3_param = abi::ABI_PARAMS[3].to_reg64();
    ra.asm
        .lea(
            result_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();
    ra.asm
        .lea(
            operand1_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand1_offset),
        )
        .unwrap();
    ra.asm
        .lea(
            operand2_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand2_offset),
        )
        .unwrap();
    ra.asm
        .lea(
            operand3_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand3_offset),
        )
        .unwrap();

    ra.asm.mov(rxbyak::RAX, func as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();

    ra.asm
        .movaps(
            result,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();

    ra.release_stack_space(frame_size);
    ra.define_value(inst_ref, result);
}

/// FP-aware three-vector fallback.
///
/// Rust ABI counterpart of upstream `EmitFourOpFallback`: result, three input
/// vectors, FPCR, and the sticky FPSR exception field are passed in that order.
pub fn emit_four_op_fallback(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    func: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let fpcr_controlled = args[3].get_immediate_u1();
    let arg1 = ra.use_xmm(&mut args[0]);
    let arg2 = ra.use_xmm(&mut args[1]);
    let arg3 = ra.use_xmm(&mut args[2]);
    let result = ra.scratch_xmm();
    ra.end_of_alloc_scope();
    ra.host_call(None, &mut [None, None, None, None]);

    #[cfg(target_os = "windows")]
    let stack_argument_space = 16usize;
    #[cfg(not(target_os = "windows"))]
    let stack_argument_space = 0usize;

    let result_offset = (abi::ABI_SHADOW_SPACE + stack_argument_space) as i32;
    let operand1_offset = result_offset + 16;
    let operand2_offset = result_offset + 32;
    let operand3_offset = result_offset + 48;
    let frame_size = abi::ABI_SHADOW_SPACE + stack_argument_space + 64;
    ra.alloc_stack_space(frame_size);

    for (offset, operand) in [
        (operand1_offset, arg1),
        (operand2_offset, arg2),
        (operand3_offset, arg3),
    ] {
        ra.asm
            .movaps(
                rxbyak::xmmword_ptr(RegExp::from(rxbyak::RSP) + offset),
                operand,
            )
            .unwrap();
    }

    for (index, offset) in [
        result_offset,
        operand1_offset,
        operand2_offset,
        operand3_offset,
    ]
    .into_iter()
    .enumerate()
    {
        ra.asm
            .lea(
                abi::ABI_PARAMS[index].to_reg64(),
                rxbyak::xmmword_ptr(RegExp::from(rxbyak::RSP) + offset),
            )
            .unwrap();
    }

    #[cfg(target_os = "windows")]
    {
        ra.asm
            .mov(
                rxbyak::qword_ptr(RegExp::from(rxbyak::RSP) + abi::ABI_SHADOW_SPACE as i32),
                ctx.fpcr(fpcr_controlled).value() as i32,
            )
            .unwrap();
        ra.asm
            .lea(
                rxbyak::RAX,
                rxbyak::dword_ptr(RegExp::from(R15) + ctx.arch.fpsr_exc_offset() as i32),
            )
            .unwrap();
        ra.asm
            .mov(
                rxbyak::qword_ptr(RegExp::from(rxbyak::RSP) + abi::ABI_SHADOW_SPACE as i32 + 8),
                rxbyak::RAX,
            )
            .unwrap();
    }
    #[cfg(not(target_os = "windows"))]
    {
        ra.asm
            .mov(
                Reg::gpr32(abi::ABI_PARAMS[4].to_reg64().get_idx()),
                ctx.fpcr(fpcr_controlled).value() as i32,
            )
            .unwrap();
        ra.asm
            .lea(
                abi::ABI_PARAMS[5].to_reg64(),
                rxbyak::dword_ptr(RegExp::from(R15) + ctx.arch.fpsr_exc_offset() as i32),
            )
            .unwrap();
    }

    ra.asm.mov(rxbyak::RAX, func as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();
    ra.asm
        .movaps(
            result,
            rxbyak::xmmword_ptr(RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();
    ra.release_stack_space(frame_size);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// 1-arg fallback with immediate
// fn(result: *mut [u8;16], a: *const [u8;16], imm: u8)
// ---------------------------------------------------------------------------

pub fn emit_one_arg_fallback_with_imm(
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    func: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let arg1 = ra.use_xmm(&mut args[0]);
    let imm = args[1].get_immediate_u8();
    let result = ra.scratch_xmm();
    ra.end_of_alloc_scope();

    ra.host_call(None, &mut [None, None, None, None]);

    let result_offset = abi::ABI_SHADOW_SPACE as i32;
    let operand_offset = result_offset + 16;
    let frame_size = abi::ABI_SHADOW_SPACE + 32;
    ra.alloc_stack_space(frame_size);

    ra.asm
        .movaps(
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand_offset),
            arg1,
        )
        .unwrap();

    let result_param = abi::ABI_PARAMS[0].to_reg64();
    let operand_param = abi::ABI_PARAMS[1].to_reg64();
    let immediate_param = abi::ABI_PARAMS[2].to_reg64();
    ra.asm
        .lea(
            result_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();
    ra.asm
        .lea(
            operand_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand_offset),
        )
        .unwrap();
    ra.asm.mov(immediate_param, imm as i64).unwrap();

    ra.asm.mov(rxbyak::RAX, func as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();

    ra.asm
        .movaps(
            result,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();

    ra.release_stack_space(frame_size);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// Saturation fallback: same as 2-arg but ORs QC flag into fpsr_qc after call.
// The fallback fn returns a u32 QC flag as its return value (RAX).
// fn(result: *mut [u8;16], a: *const [u8;16], b: *const [u8;16]) -> u32
// ---------------------------------------------------------------------------

pub fn emit_two_arg_fallback_saturated(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    func: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let arg1 = ra.use_xmm(&mut args[0]);
    let arg2 = ra.use_xmm(&mut args[1]);
    let result = ra.scratch_xmm();
    ra.end_of_alloc_scope();

    ra.host_call(None, &mut [None, None, None, None]);

    let result_offset = abi::ABI_SHADOW_SPACE as i32;
    let operand1_offset = result_offset + 16;
    let operand2_offset = result_offset + 32;
    let frame_size = abi::ABI_SHADOW_SPACE + 48;
    ra.alloc_stack_space(frame_size);

    ra.asm
        .movaps(
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand1_offset),
            arg1,
        )
        .unwrap();
    ra.asm
        .movaps(
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand2_offset),
            arg2,
        )
        .unwrap();

    let result_param = abi::ABI_PARAMS[0].to_reg64();
    let operand1_param = abi::ABI_PARAMS[1].to_reg64();
    let operand2_param = abi::ABI_PARAMS[2].to_reg64();
    ra.asm
        .lea(
            result_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();
    ra.asm
        .lea(
            operand1_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand1_offset),
        )
        .unwrap();
    ra.asm
        .lea(
            operand2_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand2_offset),
        )
        .unwrap();

    ra.asm.mov(rxbyak::RAX, func as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();

    // OR QC flag: fpsr_qc |= RAX
    let qc_offset = ctx.arch.fpsr_qc_offset() as i32;
    ra.asm
        .or_(
            byte_ptr(rxbyak::RegExp::from(rxbyak::R15) + qc_offset),
            rxbyak::EAX.cvt8().unwrap(),
        )
        .unwrap();

    ra.asm
        .movaps(
            result,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();

    ra.release_stack_space(frame_size);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// Saturation fallback with an immediate second argument.
// fn(result: *mut [u8;16], a: *const [u8;16], imm: u8) -> u32
// Mirrors upstream EmitTwoArgumentFallbackWithSaturationAndImmediate.
// ---------------------------------------------------------------------------

pub fn emit_two_arg_fallback_with_saturation_and_immediate(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    func: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let arg1 = ra.use_xmm(&mut args[0]);
    let imm = args[1].get_immediate_u8();
    let result = ra.scratch_xmm();
    ra.end_of_alloc_scope();

    ra.host_call(None, &mut [None, None, None, None]);

    let result_offset = abi::ABI_SHADOW_SPACE as i32;
    let operand_offset = result_offset + 16;
    let frame_size = abi::ABI_SHADOW_SPACE + 32;
    ra.alloc_stack_space(frame_size);

    ra.asm
        .movaps(
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand_offset),
            arg1,
        )
        .unwrap();

    let result_param = abi::ABI_PARAMS[0].to_reg64();
    let operand_param = abi::ABI_PARAMS[1].to_reg64();
    let immediate_param = abi::ABI_PARAMS[2].to_reg64();
    ra.asm
        .lea(
            result_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();
    ra.asm
        .lea(
            operand_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand_offset),
        )
        .unwrap();
    ra.asm.mov(immediate_param, imm as i64).unwrap();

    ra.asm.mov(rxbyak::RAX, func as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();

    let qc_offset = ctx.arch.fpsr_qc_offset() as i32;
    ra.asm
        .or_(
            byte_ptr(rxbyak::RegExp::from(rxbyak::R15) + qc_offset),
            rxbyak::EAX.cvt8().unwrap(),
        )
        .unwrap();

    ra.asm
        .movaps(
            result,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();

    ra.release_stack_space(frame_size);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// 1-arg saturation fallback
// fn(result: *mut [u8;16], a: *const [u8;16]) -> u32
// ---------------------------------------------------------------------------

pub fn emit_one_arg_fallback_saturated(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    func: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let arg1 = ra.use_xmm(&mut args[0]);
    let result = ra.scratch_xmm();
    ra.end_of_alloc_scope();

    ra.host_call(None, &mut [None, None, None, None]);

    let result_offset = abi::ABI_SHADOW_SPACE as i32;
    let operand_offset = result_offset + 16;
    let frame_size = abi::ABI_SHADOW_SPACE + 32;
    ra.alloc_stack_space(frame_size);

    ra.asm
        .movaps(
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand_offset),
            arg1,
        )
        .unwrap();

    let result_param = abi::ABI_PARAMS[0].to_reg64();
    let operand_param = abi::ABI_PARAMS[1].to_reg64();
    ra.asm
        .lea(
            result_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();
    ra.asm
        .lea(
            operand_param,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + operand_offset),
        )
        .unwrap();

    ra.asm.mov(rxbyak::RAX, func as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();

    let qc_offset = ctx.arch.fpsr_qc_offset() as i32;
    ra.asm
        .or_(
            byte_ptr(rxbyak::RegExp::from(rxbyak::R15) + qc_offset),
            rxbyak::EAX.cvt8().unwrap(),
        )
        .unwrap();

    ra.asm
        .movaps(
            result,
            rxbyak::xmmword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + result_offset),
        )
        .unwrap();

    ra.release_stack_space(frame_size);
    ra.define_value(inst_ref, result);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxbyak::CodeAssembler;

    #[test]
    fn test_helper_fn_signatures() {
        let _: fn(
            &mut RegAlloc,
            InstRef,
            &Inst,
            fn(&mut rxbyak::CodeAssembler, Reg, Reg) -> rxbyak::Result<()>,
        ) = emit_vector_op;
        let _: fn(
            &mut RegAlloc,
            InstRef,
            &Inst,
            fn(&mut rxbyak::CodeAssembler, Reg, u8) -> rxbyak::Result<()>,
        ) = emit_vector_op_imm;
        let _: fn(
            &mut RegAlloc,
            InstRef,
            &Inst,
            fn(&mut rxbyak::CodeAssembler, Reg, Reg) -> rxbyak::Result<()>,
        ) = emit_vector_unary_op;
        let _: fn(&mut RegAlloc, InstRef, &Inst, usize) = emit_one_arg_fallback;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst, usize) = emit_two_op_fallback;
        let _: fn(&mut RegAlloc, InstRef, &Inst, usize) = emit_two_arg_fallback;
        let _: fn(&mut RegAlloc, InstRef, &Inst, usize) = emit_two_arg_fallback_with_imm;
        let _: fn(&mut RegAlloc, InstRef, &Inst, usize) = emit_three_arg_fallback;
        let _: fn(&mut RegAlloc, InstRef, &Inst, usize) = emit_one_arg_fallback_with_imm;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst, usize) =
            emit_two_arg_fallback_saturated;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst, usize) =
            emit_two_arg_fallback_with_saturation_and_immediate;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst, usize) =
            emit_one_arg_fallback_saturated;
    }

    #[test]
    fn test_fallback_style_result_reservation_after_scope_end() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let mut ra = RegAlloc::new_default(&mut asm, vec![]);

        // Apply enough XMM pressure to mirror the fallback path without depending on
        // private reg-order constants from reg_alloc.rs.
        for _ in 0..8 {
            let _ = ra.scratch_xmm();
        }

        let _result = ra.scratch_xmm();
        ra.end_of_alloc_scope();
        let _post_scope = ra.scratch_xmm();
    }
}

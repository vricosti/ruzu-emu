//! ARM64 cryptography emission.
//!
//! Upstream owner: `backend/arm64/emit_arm64_cryptography.cpp`.

use rhazel::{CodeGenerator, WReg};

use crate::backend::arm64::block_of_code::BlockOfCode;
use crate::backend::arm64::emit_context::EmitContext;
use crate::backend::arm64::reg_alloc::{RAReg, RegAlloc};
use crate::ir::value::InstRef;

/// Upstream `EmitCRC<bitsize>`: `emit` receives the realized W output and
/// input plus the data register, read as W or X per `data_is_64_bit`.
fn emit_crc(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    data_is_64_bit: bool,
    emit: fn(&mut CodeGenerator<'_>, WReg, WReg, &RAReg) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut output = ctx.reg_alloc.write_w(inst_ref);
    let mut input = ctx.reg_alloc.read_w(args[0]);
    let mut data = if data_is_64_bit {
        ctx.reg_alloc.read_x(args[1])
    } else {
        ctx.reg_alloc.read_w(args[1])
    };
    RegAlloc::realize_all(code, ctx.block, &mut [&mut output, &mut input, &mut data])?;

    emit(code, output.w(), input.w(), &data)
}

pub fn emit_crc32_castagnoli_8(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_crc(code, ctx, inst_ref, false, |code, output, input, data| {
        code.crc32cb(output, input, data.w())
    })
}

pub fn emit_crc32_castagnoli_16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_crc(code, ctx, inst_ref, false, |code, output, input, data| {
        code.crc32ch(output, input, data.w())
    })
}

pub fn emit_crc32_castagnoli_32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_crc(code, ctx, inst_ref, false, |code, output, input, data| {
        code.crc32cw(output, input, data.w())
    })
}

pub fn emit_crc32_castagnoli_64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_crc(code, ctx, inst_ref, true, |code, output, input, data| {
        code.crc32cx(output, input, data.x())
    })
}

pub fn emit_crc32_iso_8(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_crc(code, ctx, inst_ref, false, |code, output, input, data| {
        code.crc32b(output, input, data.w())
    })
}

pub fn emit_crc32_iso_16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_crc(code, ctx, inst_ref, false, |code, output, input, data| {
        code.crc32h(output, input, data.w())
    })
}

pub fn emit_crc32_iso_32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_crc(code, ctx, inst_ref, false, |code, output, input, data| {
        code.crc32w(output, input, data.w())
    })
}

pub fn emit_crc32_iso_64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_crc(code, ctx, inst_ref, true, |code, output, input, data| {
        code.crc32x(output, input, data.x())
    })
}

/// Upstream `EmitAES` with `MOVI Doutput, #0` before the round: the round
/// operates on `Voutput.16B` in place, so the destination is zeroed first.
fn emit_aes_single_round(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: fn(&mut CodeGenerator<'_>, &RAReg, &RAReg) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut output = ctx.reg_alloc.write_q(inst_ref);
    let mut input = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut output, &mut input])?;

    code.movi_zero(output.d())?;
    emit(code, &output, &input)
}

fn emit_aes_mix(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: fn(&mut CodeGenerator<'_>, &RAReg, &RAReg) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut output = ctx.reg_alloc.write_q(inst_ref);
    let mut input = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut output, &mut input])?;

    emit(code, &output, &input)
}

pub fn emit_aes_decrypt_single_round(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_aes_single_round(code, ctx, inst_ref, |code, output, input| {
        code.aesd(output.v().b16(), input.v().b16())
    })
}

pub fn emit_aes_encrypt_single_round(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_aes_single_round(code, ctx, inst_ref, |code, output, input| {
        code.aese(output.v().b16(), input.v().b16())
    })
}

pub fn emit_aes_inverse_mix_columns(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_aes_mix(code, ctx, inst_ref, |code, output, input| {
        code.aesimc(output.v().b16(), input.v().b16())
    })
}

pub fn emit_aes_mix_columns(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_aes_mix(code, ctx, inst_ref, |code, output, input| {
        code.aesmc(output.v().b16(), input.v().b16())
    })
}

pub fn emit_sha256_hash(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let part1 = args[3].get_immediate_u1();

    if part1 {
        let mut x = ctx.reg_alloc.read_write_q(args[0], inst_ref);
        let mut y = ctx.reg_alloc.read_q(args[1]);
        let mut w = ctx.reg_alloc.read_q(args[2]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut x, &mut y, &mut w])?;
        code.sha256h(x.q(), y.q(), w.v().s4())
    } else {
        let mut x = ctx.reg_alloc.read_q(args[0]);
        let mut y = ctx.reg_alloc.read_write_q(args[1], inst_ref);
        let mut w = ctx.reg_alloc.read_q(args[2]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut x, &mut y, &mut w])?;
        code.sha256h2(y.q(), x.q(), w.v().s4())
    }
}

pub fn emit_sha256_message_schedule_0(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut a = ctx.reg_alloc.read_write_q(args[0], inst_ref);
    let mut b = ctx.reg_alloc.read_q(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut a, &mut b])?;
    code.sha256su0(a.v().s4(), b.v().s4())
}

pub fn emit_sha256_message_schedule_1(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut a = ctx.reg_alloc.read_write_q(args[0], inst_ref);
    let mut b = ctx.reg_alloc.read_q(args[1]);
    let mut c = ctx.reg_alloc.read_q(args[2]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut a, &mut b, &mut c])?;
    code.sha256su1(a.v().s4(), b.v().s4(), c.v().s4())
}

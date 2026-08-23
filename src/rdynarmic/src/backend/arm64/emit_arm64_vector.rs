//! ARM64 vector emission.
//!
//! Upstream owner: `backend/arm64/emit_arm64_vector.cpp`.

use crate::backend::arm64::block_of_code::BlockOfCode;
use crate::backend::arm64::emit_context::EmitContext;
use crate::backend::arm64::inst;
use crate::backend::arm64::reg_alloc::RegAlloc;
use crate::ir::opcode::Opcode;
use crate::ir::value::{InstRef, Value};

type ThreeOpEmitter = fn(u8, u8, u8, u8, bool) -> u32;
type ThreeOpWidenEmitter = fn(u8, u8, u8, u8) -> u32;
type TwoOpEmitter = fn(u8, u8, u8, bool) -> u32;
type TwoOpWidenEmitter = fn(u8, u8, u8) -> u32;
type ImmShiftEmitter = fn(u8, u8, u8, u8, bool) -> u32;

fn emit_two_op(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(u8, u8) -> u32,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut operand = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    code.write_u32(emit(
        result.index().expect("result realized") as u8,
        operand.index().expect("operand realized") as u8,
    ))?;
    Ok(())
}

fn emit_two_op_arranged(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: u8,
    emit: TwoOpEmitter,
) -> Result<(), String> {
    emit_two_op(code, ctx, inst_ref, |rd, rn| emit(rd, rn, size, true))
}

fn emit_two_op_arranged_saturated(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: u8,
    emit: TwoOpEmitter,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut operand = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.fpsr.load(code)?;
    code.write_u32(emit(
        result.index().expect("result realized") as u8,
        operand.index().expect("operand realized") as u8,
        size,
        true,
    ))?;
    Ok(())
}

fn emit_three_op(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(u8, u8, u8) -> u32,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut a = ctx.reg_alloc.read_q(args[0]);
    let mut b = ctx.reg_alloc.read_q(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    code.write_u32(emit(
        result.index().expect("result realized") as u8,
        a.index().expect("a realized") as u8,
        b.index().expect("b realized") as u8,
    ))?;
    Ok(())
}

fn emit_three_op_arranged(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: u8,
    emit: ThreeOpEmitter,
) -> Result<(), String> {
    emit_three_op(code, ctx, inst_ref, |rd, rn, rm| {
        emit(rd, rn, rm, size, true)
    })
}

fn emit_three_op_arranged_lower(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: u8,
    emit: ThreeOpEmitter,
) -> Result<(), String> {
    emit_three_op(code, ctx, inst_ref, |rd, rn, rm| {
        emit(rd, rn, rm, size, false)
    })
}

fn emit_three_op_arranged_widen(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: u8,
    emit: ThreeOpWidenEmitter,
) -> Result<(), String> {
    emit_three_op(code, ctx, inst_ref, |rd, rn, rm| emit(rd, rn, rm, size))
}

fn emit_three_op_arranged_saturated(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: u8,
    emit: ThreeOpEmitter,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut a = ctx.reg_alloc.read_q(args[0]);
    let mut b = ctx.reg_alloc.read_q(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    ctx.fpsr.load(code)?;
    code.write_u32(emit(
        result.index().expect("result realized") as u8,
        a.index().expect("a realized") as u8,
        b.index().expect("b realized") as u8,
        size,
        true,
    ))?;
    Ok(())
}

fn emit_three_op_arranged_saturated_widen(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: u8,
    emit: ThreeOpWidenEmitter,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut a = ctx.reg_alloc.read_q(args[0]);
    let mut b = ctx.reg_alloc.read_q(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    ctx.fpsr.load(code)?;
    code.write_u32(emit(
        result.index().expect("result realized") as u8,
        a.index().expect("a realized") as u8,
        b.index().expect("b realized") as u8,
        size,
    ))?;
    Ok(())
}

fn emit_three_op_arranged_swapped(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: u8,
    emit: ThreeOpEmitter,
) -> Result<(), String> {
    emit_three_op(code, ctx, inst_ref, |rd, rn, rm| {
        emit(rd, rm, rn, size, true)
    })
}

fn emit_imm_shift_saturated(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: u8,
    emit: ImmShiftEmitter,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let shift = args[1].get_immediate_u8();
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut operand = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.fpsr.load(code)?;
    code.write_u32(emit(
        result.index().expect("result realized") as u8,
        operand.index().expect("operand realized") as u8,
        size,
        shift,
        true,
    ))?;
    Ok(())
}

fn emit_imm_shift(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: u8,
    emit: ImmShiftEmitter,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let shift = args[1].get_immediate_u8();
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut operand = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    code.write_u32(emit(
        result.index().expect("result realized") as u8,
        operand.index().expect("operand realized") as u8,
        size,
        shift,
        true,
    ))?;
    Ok(())
}

fn emit_get_element(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: u8,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let index = args[1].get_immediate_u8();
    let mut result = ctx.reg_alloc.write_x(inst_ref);
    let mut value = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut value])?;
    code.write_u32(inst::umov_from_v(
        result.index().expect("result realized") as u8,
        value.index().expect("value realized") as u8,
        size,
        index,
    ))?;
    Ok(())
}

fn emit_set_element(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: u8,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let index = args[1].get_immediate_u8();
    let mut result = ctx.reg_alloc.read_write_q(args[0], inst_ref);
    let mut elem = ctx.reg_alloc.read_x(args[2]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut elem])?;
    code.write_u32(inst::mov_to_v_element(
        result.index().expect("result realized") as u8,
        elem.index().expect("element realized") as u8,
        size,
        index,
    ))?;
    Ok(())
}

fn emit_broadcast(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: u8,
    q: bool,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut value = ctx.reg_alloc.read_x(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut value])?;
    code.write_u32(inst::dup_v_from_reg(
        result.index().expect("result realized") as u8,
        value.index().expect("value realized") as u8,
        size,
        q,
    ))?;
    Ok(())
}

fn emit_broadcast_element(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: u8,
    q: bool,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let index = args[1].get_immediate_u8();
    assert!((index as u16) * (size as u16) < 128);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut value = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut value])?;
    code.write_u32(inst::dup_v_from_element(
        result.index().expect("result realized") as u8,
        value.index().expect("value realized") as u8,
        size,
        index,
        q,
    ))?;
    Ok(())
}

pub fn emit_zero_vector(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let result_reg = result.realize(code, ctx.block)? as u8;
    code.write_u32(inst::movi_d_imm0(result_reg))?;
    Ok(())
}

fn emit_extract(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    q: bool,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let position = args[2].get_immediate_u8();
    if position % 8 != 0 {
        return Err(format!(
            "VectorExtract position must be byte-aligned: {position}"
        ));
    }
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut a = ctx.reg_alloc.read_q(args[0]);
    let mut b = ctx.reg_alloc.read_q(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    code.write_u32(inst::ext_v16b(
        result.index().expect("result realized") as u8,
        a.index().expect("a realized") as u8,
        b.index().expect("b realized") as u8,
        position / 8,
        q,
    ))?;
    Ok(())
}

fn emit_widen(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: u8,
    emit: fn(u8, u8, u8) -> u32,
) -> Result<(), String> {
    emit_two_op(code, ctx, inst_ref, |rd, rn| emit(rd, rn, size))
}

fn emit_narrow(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: u8,
    emit: TwoOpWidenEmitter,
) -> Result<(), String> {
    emit_two_op(code, ctx, inst_ref, |rd, rn| emit(rd, rn, size))
}

fn emit_narrow_saturated(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: u8,
    emit: TwoOpWidenEmitter,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut operand = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.fpsr.load(code)?;
    code.write_u32(emit(
        result.index().expect("result realized") as u8,
        operand.index().expect("operand realized") as u8,
        size,
    ))?;
    Ok(())
}

fn emit_pair_widen(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: u8,
    emit: TwoOpWidenEmitter,
) -> Result<(), String> {
    emit_two_op(code, ctx, inst_ref, |rd, rn| emit(rd, rn, size))
}

fn emit_saturated_accumulate(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: u8,
    emit: TwoOpEmitter,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut accumulator = ctx.reg_alloc.read_write_q(args[1], inst_ref);
    let mut operand = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut accumulator, &mut operand])?;
    ctx.fpsr.load(code)?;
    code.write_u32(emit(
        accumulator.index().expect("accumulator realized") as u8,
        operand.index().expect("operand realized") as u8,
        size,
        true,
    ))?;
    Ok(())
}

fn emit_zero_upper(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op(code, ctx, inst_ref, inst::fmov_d)
}

fn emit_transpose(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: u8,
) -> Result<(), String> {
    let part = ctx.block.get(inst_ref).args[2].get_u1();
    if part {
        emit_three_op_arranged(code, ctx, inst_ref, size, inst::trn2_v)
    } else {
        emit_three_op_arranged(code, ctx, inst_ref, size, inst::trn1_v)
    }
}

fn is_default_zero(block: &crate::ir::block::Block, value: Value) -> bool {
    if value.is_zero() {
        return true;
    }

    let Value::Inst(inst_ref) = value else {
        return false;
    };
    block.get(inst_ref).opcode == Opcode::ZeroVector
}

fn table_ref_from_lookup(ctx: &EmitContext<'_>, inst_ref: InstRef) -> Result<InstRef, String> {
    let Value::Inst(table_ref) = ctx.block.get(inst_ref).args[1] else {
        return Err("VectorTableLookup arg1 must be a VectorTable instruction".to_string());
    };
    if ctx.block.get(table_ref).opcode != Opcode::VectorTable {
        return Err("VectorTableLookup arg1 must be a VectorTable instruction".to_string());
    }
    Ok(table_ref)
}

fn emit_vector_table(
    _code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let use_count = ctx.block.get(inst_ref).use_count;
    if use_count != 1 {
        return Err(format!(
            "VectorTable cannot be used multiple times: {use_count}"
        ));
    }
    Ok(())
}

fn emit_vector_table_lookup64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let table_ref = table_ref_from_lookup(ctx, inst_ref)?;
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let table = ctx.reg_alloc.get_argument_info(ctx.block, table_ref);
    let table_size = table.iter().filter(|arg| !arg.is_void()).count();
    let is_defaults_zero = is_default_zero(ctx.block, args[0].value);

    let mut result = if is_defaults_zero {
        ctx.reg_alloc.write_d(inst_ref)
    } else {
        ctx.reg_alloc.read_write_d(args[0], inst_ref)
    };
    let mut indices = ctx.reg_alloc.read_d(args[2]);
    let mut table_regs = Vec::with_capacity(table_size);
    for arg in table.iter().take(table_size) {
        table_regs.push(ctx.reg_alloc.read_d(*arg));
    }

    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut indices])?;
    for table_reg in &mut table_regs {
        table_reg.realize(code, ctx.block)?;
    }

    let result = result.index().expect("result realized") as u8;
    let indices = indices.index().expect("indices realized") as u8;
    let table_regs: Vec<u8> = table_regs
        .iter()
        .map(|reg| reg.index().expect("table register realized") as u8)
        .collect();

    match table_size {
        1 => {
            code.write_u32(inst::movi_v16b_imm(2, 0x08))?;
            code.write_u32(inst::cmge_v(2, indices, 2, 8, false))?;
            code.write_u32(inst::orr_v8b(2, indices, 2))?;
            code.write_u32(inst::fmov_d(0, table_regs[0]))?;
            code.write_u32(if is_defaults_zero {
                inst::tbl_v(result, 0, 2, 1, false)
            } else {
                inst::tbx_v(result, 0, 2, 1, false)
            })?;
        }
        2 => {
            code.write_u32(inst::zip1_v(0, table_regs[0], table_regs[1], 64, true))?;
            code.write_u32(if is_defaults_zero {
                inst::tbl_v(result, 0, indices, 1, false)
            } else {
                inst::tbx_v(result, 0, indices, 1, false)
            })?;
        }
        3 => {
            code.write_u32(inst::movi_v16b_imm(2, 0x18))?;
            code.write_u32(inst::cmge_v(2, indices, 2, 8, false))?;
            code.write_u32(inst::orr_v8b(2, indices, 2))?;
            code.write_u32(inst::zip1_v(0, table_regs[0], table_regs[1], 64, true))?;
            code.write_u32(inst::fmov_d(1, table_regs[2]))?;
            code.write_u32(if is_defaults_zero {
                inst::tbl_v(result, 0, 2, 2, false)
            } else {
                inst::tbx_v(result, 0, 2, 2, false)
            })?;
        }
        4 => {
            code.write_u32(inst::zip1_v(0, table_regs[0], table_regs[1], 64, true))?;
            code.write_u32(inst::zip1_v(1, table_regs[2], table_regs[3], 64, true))?;
            code.write_u32(if is_defaults_zero {
                inst::tbl_v(result, 0, indices, 2, false)
            } else {
                inst::tbx_v(result, 0, indices, 2, false)
            })?;
        }
        _ => {
            return Err(format!(
                "unsupported VectorTableLookup64 table size: {table_size}"
            ))
        }
    }

    Ok(())
}

fn emit_vector_table_lookup128(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let table_ref = table_ref_from_lookup(ctx, inst_ref)?;
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let table = ctx.reg_alloc.get_argument_info(ctx.block, table_ref);
    let table_size = table.iter().filter(|arg| !arg.is_void()).count();
    let is_defaults_zero = is_default_zero(ctx.block, args[0].value);

    let mut result = if is_defaults_zero {
        ctx.reg_alloc.write_q(inst_ref)
    } else {
        ctx.reg_alloc.read_write_q(args[0], inst_ref)
    };
    let mut indices = ctx.reg_alloc.read_q(args[2]);
    let mut table_regs = Vec::with_capacity(table_size);
    for arg in table.iter().take(table_size) {
        table_regs.push(ctx.reg_alloc.read_q(*arg));
    }

    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut indices])?;
    for table_reg in &mut table_regs {
        table_reg.realize(code, ctx.block)?;
    }

    let result = result.index().expect("result realized") as u8;
    let indices = indices.index().expect("indices realized") as u8;
    let table_regs: Vec<u8> = table_regs
        .iter()
        .map(|reg| reg.index().expect("table register realized") as u8)
        .collect();

    match table_size {
        1 => {
            code.write_u32(if is_defaults_zero {
                inst::tbl_v(result, table_regs[0], indices, 1, true)
            } else {
                inst::tbx_v(result, table_regs[0], indices, 1, true)
            })?;
        }
        2 => {
            code.write_u32(inst::mov_v16b(0, table_regs[0]))?;
            code.write_u32(inst::mov_v16b(1, table_regs[1]))?;
            code.write_u32(if is_defaults_zero {
                inst::tbl_v(result, 0, indices, 2, true)
            } else {
                inst::tbx_v(result, 0, indices, 2, true)
            })?;
        }
        3 => {
            code.write_u32(inst::mov_v16b(0, table_regs[0]))?;
            code.write_u32(inst::mov_v16b(1, table_regs[1]))?;
            code.write_u32(inst::mov_v16b(2, table_regs[2]))?;
            code.write_u32(if is_defaults_zero {
                inst::tbl_v(result, 0, indices, 3, true)
            } else {
                inst::tbx_v(result, 0, indices, 3, true)
            })?;
        }
        4 => {
            code.write_u32(inst::mov_v16b(0, table_regs[0]))?;
            code.write_u32(inst::mov_v16b(1, table_regs[1]))?;
            code.write_u32(inst::mov_v16b(2, table_regs[2]))?;
            code.write_u32(inst::mov_v16b(3, table_regs[3]))?;
            code.write_u32(if is_defaults_zero {
                inst::tbl_v(result, 0, indices, 4, true)
            } else {
                inst::tbx_v(result, 0, indices, 4, true)
            })?;
        }
        _ => {
            return Err(format!(
                "unsupported VectorTableLookup128 table size: {table_size}"
            ))
        }
    }

    Ok(())
}

pub fn emit_vector_instruction(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    match ctx.block.get(inst_ref).opcode {
        Opcode::VectorGetElement8 => emit_get_element(code, ctx, inst_ref, 8),
        Opcode::VectorGetElement16 => emit_get_element(code, ctx, inst_ref, 16),
        Opcode::VectorGetElement32 => emit_get_element(code, ctx, inst_ref, 32),
        Opcode::VectorGetElement64 => emit_get_element(code, ctx, inst_ref, 64),
        Opcode::VectorSetElement8 => emit_set_element(code, ctx, inst_ref, 8),
        Opcode::VectorSetElement16 => emit_set_element(code, ctx, inst_ref, 16),
        Opcode::VectorSetElement32 => emit_set_element(code, ctx, inst_ref, 32),
        Opcode::VectorSetElement64 => emit_set_element(code, ctx, inst_ref, 64),
        Opcode::VectorBroadcastLower8 => emit_broadcast(code, ctx, inst_ref, 8, false),
        Opcode::VectorBroadcastLower16 => emit_broadcast(code, ctx, inst_ref, 16, false),
        Opcode::VectorBroadcastLower32 => emit_broadcast(code, ctx, inst_ref, 32, false),
        Opcode::VectorBroadcast8 => emit_broadcast(code, ctx, inst_ref, 8, true),
        Opcode::VectorBroadcast16 => emit_broadcast(code, ctx, inst_ref, 16, true),
        Opcode::VectorBroadcast32 => emit_broadcast(code, ctx, inst_ref, 32, true),
        Opcode::VectorBroadcast64 => emit_broadcast(code, ctx, inst_ref, 64, true),
        Opcode::VectorBroadcastElementLower8 => {
            emit_broadcast_element(code, ctx, inst_ref, 8, false)
        }
        Opcode::VectorBroadcastElementLower16 => {
            emit_broadcast_element(code, ctx, inst_ref, 16, false)
        }
        Opcode::VectorBroadcastElementLower32 => {
            emit_broadcast_element(code, ctx, inst_ref, 32, false)
        }
        Opcode::VectorBroadcastElement8 => {
            emit_broadcast_element(code, ctx, inst_ref, 8, true)
        }
        Opcode::VectorBroadcastElement16 => {
            emit_broadcast_element(code, ctx, inst_ref, 16, true)
        }
        Opcode::VectorBroadcastElement32 => {
            emit_broadcast_element(code, ctx, inst_ref, 32, true)
        }
        Opcode::VectorBroadcastElement64 => {
            emit_broadcast_element(code, ctx, inst_ref, 64, true)
        }
        Opcode::VectorAbs8 => emit_two_op_arranged(code, ctx, inst_ref, 8, inst::abs_v),
        Opcode::VectorAbs16 => emit_two_op_arranged(code, ctx, inst_ref, 16, inst::abs_v),
        Opcode::VectorAbs32 => emit_two_op_arranged(code, ctx, inst_ref, 32, inst::abs_v),
        Opcode::VectorAbs64 => emit_two_op_arranged(code, ctx, inst_ref, 64, inst::abs_v),
        Opcode::VectorNot => emit_two_op(code, ctx, inst_ref, inst::not_v16b),
        Opcode::VectorCountLeadingZeros8 => {
            emit_two_op_arranged(code, ctx, inst_ref, 8, inst::clz_v)
        }
        Opcode::VectorCountLeadingZeros16 => {
            emit_two_op_arranged(code, ctx, inst_ref, 16, inst::clz_v)
        }
        Opcode::VectorCountLeadingZeros32 => {
            emit_two_op_arranged(code, ctx, inst_ref, 32, inst::clz_v)
        }
        Opcode::VectorPopulationCount => emit_two_op(code, ctx, inst_ref, inst::cnt_v16b),
        Opcode::VectorReverseBits => emit_two_op(code, ctx, inst_ref, inst::rbit_v16b),
        Opcode::VectorReverseElementsInHalfGroups8 => {
            emit_two_op(code, ctx, inst_ref, inst::rev16_v16b)
        }
        Opcode::VectorReverseElementsInWordGroups8 => {
            emit_two_op_arranged(code, ctx, inst_ref, 8, inst::rev32_v)
        }
        Opcode::VectorReverseElementsInWordGroups16 => {
            emit_two_op_arranged(code, ctx, inst_ref, 16, inst::rev32_v)
        }
        Opcode::VectorReverseElementsInLongGroups8 => {
            emit_two_op_arranged(code, ctx, inst_ref, 8, inst::rev64_v)
        }
        Opcode::VectorReverseElementsInLongGroups16 => {
            emit_two_op_arranged(code, ctx, inst_ref, 16, inst::rev64_v)
        }
        Opcode::VectorReverseElementsInLongGroups32 => {
            emit_two_op_arranged(code, ctx, inst_ref, 32, inst::rev64_v)
        }
        Opcode::VectorZeroExtend8 => emit_widen(code, ctx, inst_ref, 8, inst::uxtl_v),
        Opcode::VectorZeroExtend16 => emit_widen(code, ctx, inst_ref, 16, inst::uxtl_v),
        Opcode::VectorZeroExtend32 => emit_widen(code, ctx, inst_ref, 32, inst::uxtl_v),
        Opcode::VectorSignExtend8 => emit_widen(code, ctx, inst_ref, 8, inst::sxtl_v),
        Opcode::VectorSignExtend16 => emit_widen(code, ctx, inst_ref, 16, inst::sxtl_v),
        Opcode::VectorSignExtend32 => emit_widen(code, ctx, inst_ref, 32, inst::sxtl_v),
        Opcode::VectorZeroExtend64 | Opcode::VectorZeroUpper => {
            emit_zero_upper(code, ctx, inst_ref)
        }
        Opcode::VectorNarrow16 => emit_narrow(code, ctx, inst_ref, 16, inst::xtn_v),
        Opcode::VectorNarrow32 => emit_narrow(code, ctx, inst_ref, 32, inst::xtn_v),
        Opcode::VectorNarrow64 => emit_narrow(code, ctx, inst_ref, 64, inst::xtn_v),
        Opcode::VectorAdd8 => emit_three_op_arranged(code, ctx, inst_ref, 8, inst::add_v),
        Opcode::VectorAdd16 => emit_three_op_arranged(code, ctx, inst_ref, 16, inst::add_v),
        Opcode::VectorAdd32 => emit_three_op_arranged(code, ctx, inst_ref, 32, inst::add_v),
        Opcode::VectorAdd64 => emit_three_op_arranged(code, ctx, inst_ref, 64, inst::add_v),
        Opcode::VectorSub8 => emit_three_op_arranged(code, ctx, inst_ref, 8, inst::sub_v),
        Opcode::VectorSub16 => emit_three_op_arranged(code, ctx, inst_ref, 16, inst::sub_v),
        Opcode::VectorSub32 => emit_three_op_arranged(code, ctx, inst_ref, 32, inst::sub_v),
        Opcode::VectorSub64 => emit_three_op_arranged(code, ctx, inst_ref, 64, inst::sub_v),
        Opcode::VectorMultiply8 => emit_three_op_arranged(code, ctx, inst_ref, 8, inst::mul_v),
        Opcode::VectorMultiply16 => emit_three_op_arranged(code, ctx, inst_ref, 16, inst::mul_v),
        Opcode::VectorMultiply32 => emit_three_op_arranged(code, ctx, inst_ref, 32, inst::mul_v),
        Opcode::VectorMultiplySignedWiden8 => {
            emit_three_op_arranged_widen(code, ctx, inst_ref, 8, inst::smull_v)
        }
        Opcode::VectorMultiplySignedWiden16 => {
            emit_three_op_arranged_widen(code, ctx, inst_ref, 16, inst::smull_v)
        }
        Opcode::VectorMultiplySignedWiden32 => {
            emit_three_op_arranged_widen(code, ctx, inst_ref, 32, inst::smull_v)
        }
        Opcode::VectorMultiplyUnsignedWiden8 => {
            emit_three_op_arranged_widen(code, ctx, inst_ref, 8, inst::umull_v)
        }
        Opcode::VectorMultiplyUnsignedWiden16 => {
            emit_three_op_arranged_widen(code, ctx, inst_ref, 16, inst::umull_v)
        }
        Opcode::VectorMultiplyUnsignedWiden32 => {
            emit_three_op_arranged_widen(code, ctx, inst_ref, 32, inst::umull_v)
        }
        Opcode::VectorAnd => emit_three_op(code, ctx, inst_ref, inst::and_v16b),
        Opcode::VectorAndNot => emit_three_op(code, ctx, inst_ref, inst::bic_v16b),
        Opcode::VectorEor => emit_three_op(code, ctx, inst_ref, inst::eor_v16b),
        Opcode::VectorOr => emit_three_op(code, ctx, inst_ref, inst::orr_v16b),
        Opcode::VectorEqual8 => emit_three_op_arranged(code, ctx, inst_ref, 8, inst::cmeq_v),
        Opcode::VectorEqual16 => emit_three_op_arranged(code, ctx, inst_ref, 16, inst::cmeq_v),
        Opcode::VectorEqual32 => emit_three_op_arranged(code, ctx, inst_ref, 32, inst::cmeq_v),
        Opcode::VectorEqual64 => emit_three_op_arranged(code, ctx, inst_ref, 64, inst::cmeq_v),
        Opcode::VectorGreaterS8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::cmgt_v)
        }
        Opcode::VectorGreaterS16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::cmgt_v)
        }
        Opcode::VectorGreaterS32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::cmgt_v)
        }
        Opcode::VectorGreaterS64 => {
            emit_three_op_arranged(code, ctx, inst_ref, 64, inst::cmgt_v)
        }
        Opcode::VectorGreaterEqualSigned8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::cmge_v)
        }
        Opcode::VectorGreaterEqualSigned16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::cmge_v)
        }
        Opcode::VectorGreaterEqualSigned32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::cmge_v)
        }
        Opcode::VectorGreaterEqualSigned64 => {
            emit_three_op_arranged(code, ctx, inst_ref, 64, inst::cmge_v)
        }
        Opcode::VectorGreaterEqualUnsigned8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::cmhs_v)
        }
        Opcode::VectorGreaterEqualUnsigned16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::cmhs_v)
        }
        Opcode::VectorGreaterEqualUnsigned32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::cmhs_v)
        }
        Opcode::VectorGreaterEqualUnsigned64 => {
            emit_three_op_arranged(code, ctx, inst_ref, 64, inst::cmhs_v)
        }
        Opcode::VectorLessSigned8 => {
            emit_three_op_arranged_swapped(code, ctx, inst_ref, 8, inst::cmgt_v)
        }
        Opcode::VectorLessSigned16 => {
            emit_three_op_arranged_swapped(code, ctx, inst_ref, 16, inst::cmgt_v)
        }
        Opcode::VectorLessSigned32 => {
            emit_three_op_arranged_swapped(code, ctx, inst_ref, 32, inst::cmgt_v)
        }
        Opcode::VectorLessSigned64 => {
            emit_three_op_arranged_swapped(code, ctx, inst_ref, 64, inst::cmgt_v)
        }
        Opcode::VectorLessEqualSigned8 => {
            emit_three_op_arranged_swapped(code, ctx, inst_ref, 8, inst::cmge_v)
        }
        Opcode::VectorLessEqualSigned16 => {
            emit_three_op_arranged_swapped(code, ctx, inst_ref, 16, inst::cmge_v)
        }
        Opcode::VectorLessEqualSigned32 => {
            emit_three_op_arranged_swapped(code, ctx, inst_ref, 32, inst::cmge_v)
        }
        Opcode::VectorLessEqualSigned64 => {
            emit_three_op_arranged_swapped(code, ctx, inst_ref, 64, inst::cmge_v)
        }
        Opcode::VectorHalvingAddS8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::shadd_v)
        }
        Opcode::VectorHalvingAddS16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::shadd_v)
        }
        Opcode::VectorHalvingAddS32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::shadd_v)
        }
        Opcode::VectorHalvingAddU8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::uhadd_v)
        }
        Opcode::VectorHalvingAddU16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::uhadd_v)
        }
        Opcode::VectorHalvingAddU32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::uhadd_v)
        }
        Opcode::VectorHalvingSubS8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::shsub_v)
        }
        Opcode::VectorHalvingSubS16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::shsub_v)
        }
        Opcode::VectorHalvingSubS32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::shsub_v)
        }
        Opcode::VectorHalvingSubU8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::uhsub_v)
        }
        Opcode::VectorHalvingSubU16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::uhsub_v)
        }
        Opcode::VectorHalvingSubU32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::uhsub_v)
        }
        Opcode::VectorMaxS8 => emit_three_op_arranged(code, ctx, inst_ref, 8, inst::smax_v),
        Opcode::VectorMaxS16 => emit_three_op_arranged(code, ctx, inst_ref, 16, inst::smax_v),
        Opcode::VectorMaxS32 => emit_three_op_arranged(code, ctx, inst_ref, 32, inst::smax_v),
        Opcode::VectorMaxU8 => emit_three_op_arranged(code, ctx, inst_ref, 8, inst::umax_v),
        Opcode::VectorMaxU16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::umax_v)
        }
        Opcode::VectorMaxU32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::umax_v)
        }
        Opcode::VectorMinS8 => emit_three_op_arranged(code, ctx, inst_ref, 8, inst::smin_v),
        Opcode::VectorMinS16 => emit_three_op_arranged(code, ctx, inst_ref, 16, inst::smin_v),
        Opcode::VectorMinS32 => emit_three_op_arranged(code, ctx, inst_ref, 32, inst::smin_v),
        Opcode::VectorMinU8 => emit_three_op_arranged(code, ctx, inst_ref, 8, inst::umin_v),
        Opcode::VectorMinU16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::umin_v)
        }
        Opcode::VectorMinU32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::umin_v)
        }
        Opcode::VectorPairedAddLower8 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 8, inst::addp_v)
        }
        Opcode::VectorPairedAddLower16 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 16, inst::addp_v)
        }
        Opcode::VectorPairedAddLower32 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 32, inst::addp_v)
        }
        Opcode::VectorPairedAddSignedWiden8 => {
            emit_pair_widen(code, ctx, inst_ref, 8, inst::saddlp_v)
        }
        Opcode::VectorPairedAddSignedWiden16 => {
            emit_pair_widen(code, ctx, inst_ref, 16, inst::saddlp_v)
        }
        Opcode::VectorPairedAddSignedWiden32 => {
            emit_pair_widen(code, ctx, inst_ref, 32, inst::saddlp_v)
        }
        Opcode::VectorPairedAddUnsignedWiden8 => {
            emit_pair_widen(code, ctx, inst_ref, 8, inst::uaddlp_v)
        }
        Opcode::VectorPairedAddUnsignedWiden16 => {
            emit_pair_widen(code, ctx, inst_ref, 16, inst::uaddlp_v)
        }
        Opcode::VectorPairedAddUnsignedWiden32 => {
            emit_pair_widen(code, ctx, inst_ref, 32, inst::uaddlp_v)
        }
        Opcode::VectorPairedAdd8 => emit_three_op_arranged(code, ctx, inst_ref, 8, inst::addp_v),
        Opcode::VectorPairedAdd16 => emit_three_op_arranged(code, ctx, inst_ref, 16, inst::addp_v),
        Opcode::VectorPairedAdd32 => emit_three_op_arranged(code, ctx, inst_ref, 32, inst::addp_v),
        Opcode::VectorPairedAdd64 => emit_three_op_arranged(code, ctx, inst_ref, 64, inst::addp_v),
        Opcode::VectorPairedMaxS8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::smaxp_v)
        }
        Opcode::VectorPairedMaxS16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::smaxp_v)
        }
        Opcode::VectorPairedMaxS32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::smaxp_v)
        }
        Opcode::VectorPairedMaxU8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::umaxp_v)
        }
        Opcode::VectorPairedMaxU16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::umaxp_v)
        }
        Opcode::VectorPairedMaxU32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::umaxp_v)
        }
        Opcode::VectorPairedMaxLowerS8 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 8, inst::smaxp_v)
        }
        Opcode::VectorPairedMaxLowerS16 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 16, inst::smaxp_v)
        }
        Opcode::VectorPairedMaxLowerS32 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 32, inst::smaxp_v)
        }
        Opcode::VectorPairedMaxLowerU8 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 8, inst::umaxp_v)
        }
        Opcode::VectorPairedMaxLowerU16 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 16, inst::umaxp_v)
        }
        Opcode::VectorPairedMaxLowerU32 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 32, inst::umaxp_v)
        }
        Opcode::VectorPairedMinS8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::sminp_v)
        }
        Opcode::VectorPairedMinS16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::sminp_v)
        }
        Opcode::VectorPairedMinS32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::sminp_v)
        }
        Opcode::VectorPairedMinU8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::uminp_v)
        }
        Opcode::VectorPairedMinU16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::uminp_v)
        }
        Opcode::VectorPairedMinU32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::uminp_v)
        }
        Opcode::VectorPairedMinLowerS8 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 8, inst::sminp_v)
        }
        Opcode::VectorPairedMinLowerS16 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 16, inst::sminp_v)
        }
        Opcode::VectorPairedMinLowerS32 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 32, inst::sminp_v)
        }
        Opcode::VectorPairedMinLowerU8 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 8, inst::uminp_v)
        }
        Opcode::VectorPairedMinLowerU16 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 16, inst::uminp_v)
        }
        Opcode::VectorPairedMinLowerU32 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 32, inst::uminp_v)
        }
        Opcode::VectorPolynomialMultiply8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::pmul_v)
        }
        Opcode::VectorPolynomialMultiplyLong8 => {
            emit_three_op_arranged_widen(code, ctx, inst_ref, 8, inst::pmull_v)
        }
        Opcode::VectorPolynomialMultiplyLong64 => {
            emit_three_op_arranged_widen(code, ctx, inst_ref, 64, inst::pmull_v)
        }
        Opcode::VectorArithmeticVShift8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::sshl_v)
        }
        Opcode::VectorArithmeticVShift16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::sshl_v)
        }
        Opcode::VectorArithmeticVShift32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::sshl_v)
        }
        Opcode::VectorArithmeticVShift64 => {
            emit_three_op_arranged(code, ctx, inst_ref, 64, inst::sshl_v)
        }
        Opcode::VectorLogicalVShift8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::ushl_v)
        }
        Opcode::VectorLogicalVShift16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::ushl_v)
        }
        Opcode::VectorLogicalVShift32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::ushl_v)
        }
        Opcode::VectorLogicalVShift64 => {
            emit_three_op_arranged(code, ctx, inst_ref, 64, inst::ushl_v)
        }
        Opcode::VectorRoundingShiftLeftS8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::srshl_v)
        }
        Opcode::VectorRoundingShiftLeftS16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::srshl_v)
        }
        Opcode::VectorRoundingShiftLeftS32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::srshl_v)
        }
        Opcode::VectorRoundingShiftLeftS64 => {
            emit_three_op_arranged(code, ctx, inst_ref, 64, inst::srshl_v)
        }
        Opcode::VectorRoundingShiftLeftU8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::urshl_v)
        }
        Opcode::VectorRoundingShiftLeftU16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::urshl_v)
        }
        Opcode::VectorRoundingShiftLeftU32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::urshl_v)
        }
        Opcode::VectorRoundingShiftLeftU64 => {
            emit_three_op_arranged(code, ctx, inst_ref, 64, inst::urshl_v)
        }
        Opcode::VectorSignedAbsoluteDifference8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::sabd_v)
        }
        Opcode::VectorSignedAbsoluteDifference16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::sabd_v)
        }
        Opcode::VectorSignedAbsoluteDifference32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::sabd_v)
        }
        Opcode::VectorUnsignedAbsoluteDifference8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::uabd_v)
        }
        Opcode::VectorUnsignedAbsoluteDifference16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::uabd_v)
        }
        Opcode::VectorUnsignedAbsoluteDifference32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::uabd_v)
        }
        Opcode::VectorRoundingHalvingAddS8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::srhadd_v)
        }
        Opcode::VectorRoundingHalvingAddS16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::srhadd_v)
        }
        Opcode::VectorRoundingHalvingAddS32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::srhadd_v)
        }
        Opcode::VectorRoundingHalvingAddU8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::urhadd_v)
        }
        Opcode::VectorRoundingHalvingAddU16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::urhadd_v)
        }
        Opcode::VectorRoundingHalvingAddU32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::urhadd_v)
        }
        Opcode::VectorSignedSaturatedAbs8 => {
            emit_two_op_arranged_saturated(code, ctx, inst_ref, 8, inst::sqabs_v)
        }
        Opcode::VectorSignedSaturatedAbs16 => {
            emit_two_op_arranged_saturated(code, ctx, inst_ref, 16, inst::sqabs_v)
        }
        Opcode::VectorSignedSaturatedAbs32 => {
            emit_two_op_arranged_saturated(code, ctx, inst_ref, 32, inst::sqabs_v)
        }
        Opcode::VectorSignedSaturatedAbs64 => {
            emit_two_op_arranged_saturated(code, ctx, inst_ref, 64, inst::sqabs_v)
        }
        Opcode::VectorSignedSaturatedAccumulateUnsigned8 => {
            emit_saturated_accumulate(code, ctx, inst_ref, 8, inst::suqadd_v)
        }
        Opcode::VectorSignedSaturatedAccumulateUnsigned16 => {
            emit_saturated_accumulate(code, ctx, inst_ref, 16, inst::suqadd_v)
        }
        Opcode::VectorSignedSaturatedAccumulateUnsigned32 => {
            emit_saturated_accumulate(code, ctx, inst_ref, 32, inst::suqadd_v)
        }
        Opcode::VectorSignedSaturatedAccumulateUnsigned64 => {
            emit_saturated_accumulate(code, ctx, inst_ref, 64, inst::suqadd_v)
        }
        Opcode::VectorSignedSaturatedDoublingMultiplyHigh16 => {
            emit_three_op_arranged_saturated(code, ctx, inst_ref, 16, inst::sqdmulh_v)
        }
        Opcode::VectorSignedSaturatedDoublingMultiplyHigh32 => {
            emit_three_op_arranged_saturated(code, ctx, inst_ref, 32, inst::sqdmulh_v)
        }
        Opcode::VectorSignedSaturatedDoublingMultiplyHighRounding16 => {
            emit_three_op_arranged_saturated(code, ctx, inst_ref, 16, inst::sqrdmulh_v)
        }
        Opcode::VectorSignedSaturatedDoublingMultiplyHighRounding32 => {
            emit_three_op_arranged_saturated(code, ctx, inst_ref, 32, inst::sqrdmulh_v)
        }
        Opcode::VectorSignedSaturatedDoublingMultiplyLong16 => {
            emit_three_op_arranged_saturated_widen(code, ctx, inst_ref, 16, inst::sqdmull_v)
        }
        Opcode::VectorSignedSaturatedDoublingMultiplyLong32 => {
            emit_three_op_arranged_saturated_widen(code, ctx, inst_ref, 32, inst::sqdmull_v)
        }
        Opcode::VectorSignedSaturatedNarrowToSigned16 => {
            emit_narrow_saturated(code, ctx, inst_ref, 16, inst::sqxtn_v)
        }
        Opcode::VectorSignedSaturatedNarrowToSigned32 => {
            emit_narrow_saturated(code, ctx, inst_ref, 32, inst::sqxtn_v)
        }
        Opcode::VectorSignedSaturatedNarrowToSigned64 => {
            emit_narrow_saturated(code, ctx, inst_ref, 64, inst::sqxtn_v)
        }
        Opcode::VectorSignedSaturatedNarrowToUnsigned16 => {
            emit_narrow_saturated(code, ctx, inst_ref, 16, inst::sqxtun_v)
        }
        Opcode::VectorSignedSaturatedNarrowToUnsigned32 => {
            emit_narrow_saturated(code, ctx, inst_ref, 32, inst::sqxtun_v)
        }
        Opcode::VectorSignedSaturatedNarrowToUnsigned64 => {
            emit_narrow_saturated(code, ctx, inst_ref, 64, inst::sqxtun_v)
        }
        Opcode::VectorSignedSaturatedNeg8 => {
            emit_two_op_arranged_saturated(code, ctx, inst_ref, 8, inst::sqneg_v)
        }
        Opcode::VectorSignedSaturatedNeg16 => {
            emit_two_op_arranged_saturated(code, ctx, inst_ref, 16, inst::sqneg_v)
        }
        Opcode::VectorSignedSaturatedNeg32 => {
            emit_two_op_arranged_saturated(code, ctx, inst_ref, 32, inst::sqneg_v)
        }
        Opcode::VectorSignedSaturatedNeg64 => {
            emit_two_op_arranged_saturated(code, ctx, inst_ref, 64, inst::sqneg_v)
        }
        Opcode::VectorSignedSaturatedShiftLeft8 => {
            emit_three_op_arranged_saturated(code, ctx, inst_ref, 8, inst::sqshl_v)
        }
        Opcode::VectorSignedSaturatedShiftLeft16 => {
            emit_three_op_arranged_saturated(code, ctx, inst_ref, 16, inst::sqshl_v)
        }
        Opcode::VectorSignedSaturatedShiftLeft32 => {
            emit_three_op_arranged_saturated(code, ctx, inst_ref, 32, inst::sqshl_v)
        }
        Opcode::VectorSignedSaturatedShiftLeft64 => {
            emit_three_op_arranged_saturated(code, ctx, inst_ref, 64, inst::sqshl_v)
        }
        Opcode::VectorSignedSaturatedShiftLeftUnsigned8 => {
            emit_imm_shift_saturated(code, ctx, inst_ref, 8, inst::sqshlu_v)
        }
        Opcode::VectorSignedSaturatedShiftLeftUnsigned16 => {
            emit_imm_shift_saturated(code, ctx, inst_ref, 16, inst::sqshlu_v)
        }
        Opcode::VectorSignedSaturatedShiftLeftUnsigned32 => {
            emit_imm_shift_saturated(code, ctx, inst_ref, 32, inst::sqshlu_v)
        }
        Opcode::VectorSignedSaturatedShiftLeftUnsigned64 => {
            emit_imm_shift_saturated(code, ctx, inst_ref, 64, inst::sqshlu_v)
        }
        Opcode::VectorTable => emit_vector_table(code, ctx, inst_ref),
        Opcode::VectorTableLookup64 => emit_vector_table_lookup64(code, ctx, inst_ref),
        Opcode::VectorTableLookup128 => emit_vector_table_lookup128(code, ctx, inst_ref),
        Opcode::VectorUnsignedRecipEstimate => emit_two_op(code, ctx, inst_ref, inst::urecpe_v4s),
        Opcode::VectorUnsignedRecipSqrtEstimate => {
            emit_two_op(code, ctx, inst_ref, inst::ursqrte_v4s)
        }
        Opcode::VectorUnsignedSaturatedAccumulateSigned8 => {
            emit_saturated_accumulate(code, ctx, inst_ref, 8, inst::usqadd_v)
        }
        Opcode::VectorUnsignedSaturatedAccumulateSigned16 => {
            emit_saturated_accumulate(code, ctx, inst_ref, 16, inst::usqadd_v)
        }
        Opcode::VectorUnsignedSaturatedAccumulateSigned32 => {
            emit_saturated_accumulate(code, ctx, inst_ref, 32, inst::usqadd_v)
        }
        Opcode::VectorUnsignedSaturatedAccumulateSigned64 => {
            emit_saturated_accumulate(code, ctx, inst_ref, 64, inst::usqadd_v)
        }
        Opcode::VectorUnsignedSaturatedNarrow16 => {
            emit_narrow_saturated(code, ctx, inst_ref, 16, inst::uqxtn_v)
        }
        Opcode::VectorUnsignedSaturatedNarrow32 => {
            emit_narrow_saturated(code, ctx, inst_ref, 32, inst::uqxtn_v)
        }
        Opcode::VectorUnsignedSaturatedNarrow64 => {
            emit_narrow_saturated(code, ctx, inst_ref, 64, inst::uqxtn_v)
        }
        Opcode::VectorUnsignedSaturatedShiftLeft8 => {
            emit_three_op_arranged_saturated(code, ctx, inst_ref, 8, inst::uqshl_v)
        }
        Opcode::VectorUnsignedSaturatedShiftLeft16 => {
            emit_three_op_arranged_saturated(code, ctx, inst_ref, 16, inst::uqshl_v)
        }
        Opcode::VectorUnsignedSaturatedShiftLeft32 => {
            emit_three_op_arranged_saturated(code, ctx, inst_ref, 32, inst::uqshl_v)
        }
        Opcode::VectorUnsignedSaturatedShiftLeft64 => {
            emit_three_op_arranged_saturated(code, ctx, inst_ref, 64, inst::uqshl_v)
        }
        Opcode::VectorInterleaveLower8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::zip1_v)
        }
        Opcode::VectorInterleaveLower16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::zip1_v)
        }
        Opcode::VectorInterleaveLower32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::zip1_v)
        }
        Opcode::VectorInterleaveLower64 => {
            emit_three_op_arranged(code, ctx, inst_ref, 64, inst::zip1_v)
        }
        Opcode::VectorInterleaveUpper8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::zip2_v)
        }
        Opcode::VectorInterleaveUpper16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::zip2_v)
        }
        Opcode::VectorInterleaveUpper32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::zip2_v)
        }
        Opcode::VectorInterleaveUpper64 => {
            emit_three_op_arranged(code, ctx, inst_ref, 64, inst::zip2_v)
        }
        Opcode::VectorDeinterleaveEven8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::uzp1_v)
        }
        Opcode::VectorDeinterleaveEven16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::uzp1_v)
        }
        Opcode::VectorDeinterleaveEven32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::uzp1_v)
        }
        Opcode::VectorDeinterleaveEven64 => {
            emit_three_op_arranged(code, ctx, inst_ref, 64, inst::uzp1_v)
        }
        Opcode::VectorDeinterleaveEvenLower8 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 8, inst::uzp1_v)
        }
        Opcode::VectorDeinterleaveEvenLower16 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 16, inst::uzp1_v)
        }
        Opcode::VectorDeinterleaveEvenLower32 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 32, inst::uzp1_v)
        }
        Opcode::VectorDeinterleaveOdd8 => {
            emit_three_op_arranged(code, ctx, inst_ref, 8, inst::uzp2_v)
        }
        Opcode::VectorDeinterleaveOdd16 => {
            emit_three_op_arranged(code, ctx, inst_ref, 16, inst::uzp2_v)
        }
        Opcode::VectorDeinterleaveOdd32 => {
            emit_three_op_arranged(code, ctx, inst_ref, 32, inst::uzp2_v)
        }
        Opcode::VectorDeinterleaveOdd64 => {
            emit_three_op_arranged(code, ctx, inst_ref, 64, inst::uzp2_v)
        }
        Opcode::VectorDeinterleaveOddLower8 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 8, inst::uzp2_v)
        }
        Opcode::VectorDeinterleaveOddLower16 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 16, inst::uzp2_v)
        }
        Opcode::VectorDeinterleaveOddLower32 => {
            emit_three_op_arranged_lower(code, ctx, inst_ref, 32, inst::uzp2_v)
        }
        Opcode::VectorTranspose8 => emit_transpose(code, ctx, inst_ref, 8),
        Opcode::VectorTranspose16 => emit_transpose(code, ctx, inst_ref, 16),
        Opcode::VectorTranspose32 => emit_transpose(code, ctx, inst_ref, 32),
        Opcode::VectorTranspose64 => emit_transpose(code, ctx, inst_ref, 64),
        Opcode::VectorLogicalShiftLeft8 => emit_imm_shift(code, ctx, inst_ref, 8, inst::shl_v),
        Opcode::VectorLogicalShiftLeft16 => emit_imm_shift(code, ctx, inst_ref, 16, inst::shl_v),
        Opcode::VectorLogicalShiftLeft32 => emit_imm_shift(code, ctx, inst_ref, 32, inst::shl_v),
        Opcode::VectorLogicalShiftLeft64 => emit_imm_shift(code, ctx, inst_ref, 64, inst::shl_v),
        Opcode::VectorLogicalShiftRight8 => emit_imm_shift(code, ctx, inst_ref, 8, inst::ushr_v),
        Opcode::VectorLogicalShiftRight16 => emit_imm_shift(code, ctx, inst_ref, 16, inst::ushr_v),
        Opcode::VectorLogicalShiftRight32 => emit_imm_shift(code, ctx, inst_ref, 32, inst::ushr_v),
        Opcode::VectorLogicalShiftRight64 => emit_imm_shift(code, ctx, inst_ref, 64, inst::ushr_v),
        Opcode::VectorArithmeticShiftRight8 => emit_imm_shift(code, ctx, inst_ref, 8, inst::sshr_v),
        Opcode::VectorArithmeticShiftRight16 => {
            emit_imm_shift(code, ctx, inst_ref, 16, inst::sshr_v)
        }
        Opcode::VectorArithmeticShiftRight32 => {
            emit_imm_shift(code, ctx, inst_ref, 32, inst::sshr_v)
        }
        Opcode::VectorArithmeticShiftRight64 => {
            emit_imm_shift(code, ctx, inst_ref, 64, inst::sshr_v)
        }
        Opcode::VectorExtract => emit_extract(code, ctx, inst_ref, true),
        Opcode::VectorExtractLower => emit_extract(code, ctx, inst_ref, false),
        Opcode::ZeroVector => emit_zero_vector(code, ctx, inst_ref),
        opcode => Err(format!("unimplemented ARM64 vector opcode: {opcode:?}")),
    }
}

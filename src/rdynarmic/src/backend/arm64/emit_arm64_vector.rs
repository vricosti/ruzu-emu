//! ARM64 vector emission.
//!
//! Upstream owner: `backend/arm64/emit_arm64_vector.cpp`.

use crate::backend::arm64::emit_context::EmitContext;
use crate::backend::arm64::reg_alloc::RegAlloc;
use crate::ir::opcode::Opcode;
use crate::ir::value::{InstRef, Value};
use rhazel::{
    CodeGenerator, NarrowingSource, VReg, VReg16B, VReg2D, VReg2S, VReg4H, VReg4S, VReg8B, VReg8H,
    VRegArranged, VRegBytes, WideningSource,
};

fn emit_two_op(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, VReg, VReg) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut operand = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    emit(code, result.v(), operand.v())
}

fn emit_three_op(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, VReg, VReg, VReg) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut a = ctx.reg_alloc.read_q(args[0]);
    let mut b = ctx.reg_alloc.read_q(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    emit(code, result.v(), a.v(), b.v())
}

fn emit_two_op_arranged<V: VRegArranged>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, V, V) -> Result<(), String>,
) -> Result<(), String> {
    emit_two_op(code, ctx, inst_ref, |code, rd, rn| {
        emit(code, V::from_vreg(rd), V::from_vreg(rn))
    })
}

fn emit_two_op_arranged_saturated<V: VRegArranged>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, V, V) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut operand = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.fpsr.load(code)?;
    emit(code, V::from_vreg(result.v()), V::from_vreg(operand.v()))
}

fn emit_three_op_arranged<V: VRegArranged>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, V, V, V) -> Result<(), String>,
) -> Result<(), String> {
    emit_three_op(code, ctx, inst_ref, |code, rd, rn, rm| {
        emit(code, V::from_vreg(rd), V::from_vreg(rn), V::from_vreg(rm))
    })
}

fn emit_three_op_arranged_lower<V: VRegArranged>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, V, V, V) -> Result<(), String>,
) -> Result<(), String> {
    emit_three_op(code, ctx, inst_ref, |code, rd, rn, rm| {
        emit(code, V::from_vreg(rd), V::from_vreg(rn), V::from_vreg(rm))
    })
}

fn emit_three_op_arranged_saturated<V: VRegArranged>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, V, V, V) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut operand = ctx.reg_alloc.read_q(args[0]);
    let mut b = ctx.reg_alloc.read_q(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand, &mut b])?;
    ctx.fpsr.load(code)?;
    emit(
        code,
        V::from_vreg(result.v()),
        V::from_vreg(operand.v()),
        V::from_vreg(b.v()),
    )
}

fn emit_three_op_arranged_widen<V: WideningSource>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, V::Wide, V, V) -> Result<(), String>,
) -> Result<(), String> {
    emit_three_op(code, ctx, inst_ref, |code, rd, rn, rm| {
        emit(
            code,
            <V::Wide>::from_vreg(rd),
            V::from_vreg(rn),
            V::from_vreg(rm),
        )
    })
}

fn emit_three_op_arranged_saturated_widen<V: WideningSource>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, V::Wide, V, V) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut operand = ctx.reg_alloc.read_q(args[0]);
    let mut b = ctx.reg_alloc.read_q(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand, &mut b])?;
    ctx.fpsr.load(code)?;
    emit(
        code,
        <V::Wide>::from_vreg(result.v()),
        V::from_vreg(operand.v()),
        V::from_vreg(b.v()),
    )
}

fn emit_widen<V: WideningSource>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, V::Wide, V) -> Result<(), String>,
) -> Result<(), String> {
    emit_two_op(code, ctx, inst_ref, |code, rd, rn| {
        emit(code, <V::Wide>::from_vreg(rd), V::from_vreg(rn))
    })
}

fn emit_narrow<V: NarrowingSource>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, V::Narrow, V) -> Result<(), String>,
) -> Result<(), String> {
    emit_two_op(code, ctx, inst_ref, |code, rd, rn| {
        emit(code, <V::Narrow>::from_vreg(rd), V::from_vreg(rn))
    })
}

fn emit_narrow_saturated<V: NarrowingSource>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, V::Narrow, V) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut operand = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.fpsr.load(code)?;
    emit(
        code,
        <V::Narrow>::from_vreg(result.v()),
        V::from_vreg(operand.v()),
    )
}

fn emit_pair_widen<V: VRegArranged, W: VRegArranged>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, W, V) -> Result<(), String>,
) -> Result<(), String> {
    emit_two_op(code, ctx, inst_ref, |code, rd, rn| {
        emit(code, <W>::from_vreg(rd), V::from_vreg(rn))
    })
}

fn emit_imm_shift<V: VRegArranged>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, V, V, u8) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let shift = args[1].get_immediate_u8();
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut operand = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    emit(
        code,
        V::from_vreg(result.v()),
        V::from_vreg(operand.v()),
        shift,
    )
}

fn emit_imm_shift_saturated<V: VRegArranged>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, V, V, u8) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let shift = args[1].get_immediate_u8();
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut operand = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.fpsr.load(code)?;
    emit(
        code,
        V::from_vreg(result.v()),
        V::from_vreg(operand.v()),
        shift,
    )
}

fn emit_reduce<V: VRegArranged>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = match V::SIZE {
        8 => ctx.reg_alloc.write_b(inst_ref),
        16 => ctx.reg_alloc.write_h(inst_ref),
        32 => ctx.reg_alloc.write_s(inst_ref),
        64 => ctx.reg_alloc.write_d(inst_ref),
        _ => unreachable!("invalid reduction element size"),
    };
    let mut operand = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    match V::SIZE {
        8 => code.addv_scalar(result.b(), operand.v().b16()),
        16 => code.addv_scalar(result.h(), operand.v().h8()),
        32 => code.addv_scalar(result.s(), operand.v().s4()),
        64 => code.addp_scalar(result.d(), operand.v().d2()),
        _ => unreachable!("invalid reduction element size"),
    }
}

fn emit_get_element<V: VRegArranged>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    assert!(args[1].is_immediate());
    let index = args[1].get_immediate_u8();
    let mut result = if V::SIZE == 64 {
        ctx.reg_alloc.write_x(inst_ref)
    } else {
        ctx.reg_alloc.write_w(inst_ref)
    };
    let mut value = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut value])?;
    if V::SIZE == 64 {
        code.umov(result.x(), V::from_vreg(value.v()), index)
    } else {
        code.umov(result.w(), V::from_vreg(value.v()), index)
    }
}

fn emit_set_element<V: VRegArranged>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    assert!(args[1].is_immediate());
    let index = args[1].get_immediate_u8();
    let mut result = ctx.reg_alloc.read_write_q(args[0], inst_ref);
    let mut elem = if V::SIZE == 64 {
        ctx.reg_alloc.read_x(args[2])
    } else {
        ctx.reg_alloc.read_w(args[2])
    };
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut elem])?;
    if V::SIZE == 64 {
        code.mov_to_element(V::from_vreg(result.v()), index, elem.x())
    } else {
        code.mov_to_element(V::from_vreg(result.v()), index, elem.w())
    }
}

fn emit_broadcast<V: VRegArranged>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut value = if V::SIZE == 64 {
        ctx.reg_alloc.read_x(args[0])
    } else {
        ctx.reg_alloc.read_w(args[0])
    };
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut value])?;
    if V::SIZE == 64 {
        code.dup_from_gp(V::from_vreg(result.v()), value.x())
    } else {
        code.dup_from_gp(V::from_vreg(result.v()), value.w())
    }
}

fn emit_broadcast_element<V: VRegArranged>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let index = args[1].get_immediate_u8();
    assert!((index as u16) * (V::SIZE as u16) < 128);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut value = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut value])?;
    code.dup_element(V::from_vreg(result.v()), value.v(), index)
}

pub fn emit_zero_vector(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    result.realize(code, ctx.block)?;
    code.movi_zero(result.d())
}

fn emit_extract<V: VRegBytes>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
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
    code.ext(
        V::from_vreg(result.v()),
        V::from_vreg(a.v()),
        V::from_vreg(b.v()),
        position / 8,
    )
}

fn emit_saturated_accumulate<V: VRegArranged>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, V, V) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    // Upstream swaps the operands: argument 1 is the read/write accumulator.
    let mut accumulator = ctx.reg_alloc.read_write_q(args[1], inst_ref);
    let mut operand = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut accumulator, &mut operand])?;
    ctx.fpsr.load(code)?;
    emit(
        code,
        V::from_vreg(accumulator.v()),
        V::from_vreg(operand.v()),
    )
}

fn emit_zero_upper(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op(code, ctx, inst_ref, |code, rd, rn| {
        code.fmov(rd.d(), rn.d())
    })
}

fn emit_transpose<V: VRegArranged>(
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let part = ctx.block.get(inst_ref).args[2].get_u1();
    if part {
        emit_three_op_arranged::<V>(code, ctx, inst_ref, |code, rd, rn, rm| {
            code.trn2_v(rd, rn, rm)
        })
    } else {
        emit_three_op_arranged::<V>(code, ctx, inst_ref, |code, rd, rn, rm| {
            code.trn1_v(rd, rn, rm)
        })
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
    _code: &mut rhazel::CodeGenerator<'_>,
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
    code: &mut rhazel::CodeGenerator<'_>,
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

    let result = result.v();
    let indices = indices.v();
    let table_regs: Vec<VReg> = table_regs.iter().map(|reg| reg.v()).collect();

    match table_size {
        1 => {
            code.movi(VReg::new(2).b16(), 0x08)?;
            code.cmge_v(VReg::new(2).b8(), indices.b8(), VReg::new(2).b8())?;
            code.orr_v(VReg::new(2).b8(), indices.b8(), VReg::new(2).b8())?;
            code.fmov(VReg::new(0).d(), table_regs[0].d())?;
            (if is_defaults_zero {
                code.tbl_v(result.b8(), VReg::new(0).b16(), VReg::new(2).b8(), 1)
            } else {
                code.tbx_v(result.b8(), VReg::new(0).b16(), VReg::new(2).b8(), 1)
            })?;
        }
        2 => {
            code.zip1(VReg::new(0).d2(), table_regs[0].d2(), table_regs[1].d2())?;
            (if is_defaults_zero {
                code.tbl_v(result.b8(), VReg::new(0).b16(), indices.b8(), 1)
            } else {
                code.tbx_v(result.b8(), VReg::new(0).b16(), indices.b8(), 1)
            })?;
        }
        3 => {
            code.movi(VReg::new(2).b16(), 0x18)?;
            code.cmge_v(VReg::new(2).b8(), indices.b8(), VReg::new(2).b8())?;
            code.orr_v(VReg::new(2).b8(), indices.b8(), VReg::new(2).b8())?;
            code.zip1(VReg::new(0).d2(), table_regs[0].d2(), table_regs[1].d2())?;
            code.fmov(VReg::new(1).d(), table_regs[2].d())?;
            (if is_defaults_zero {
                code.tbl_v(result.b8(), VReg::new(0).b16(), VReg::new(2).b8(), 2)
            } else {
                code.tbx_v(result.b8(), VReg::new(0).b16(), VReg::new(2).b8(), 2)
            })?;
        }
        4 => {
            code.zip1(VReg::new(0).d2(), table_regs[0].d2(), table_regs[1].d2())?;
            code.zip1(VReg::new(1).d2(), table_regs[2].d2(), table_regs[3].d2())?;
            (if is_defaults_zero {
                code.tbl_v(result.b8(), VReg::new(0).b16(), indices.b8(), 2)
            } else {
                code.tbx_v(result.b8(), VReg::new(0).b16(), indices.b8(), 2)
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
    code: &mut rhazel::CodeGenerator<'_>,
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

    let result = result.v();
    let indices = indices.v();
    let table_regs: Vec<VReg> = table_regs.iter().map(|reg| reg.v()).collect();

    match table_size {
        1 => {
            (if is_defaults_zero {
                code.tbl_v(result.b16(), table_regs[0].b16(), indices.b16(), 1)
            } else {
                code.tbx_v(result.b16(), table_regs[0].b16(), indices.b16(), 1)
            })?;
        }
        2 => {
            code.mov_v(VReg::new(0).b16(), table_regs[0].b16())?;
            code.mov_v(VReg::new(1).b16(), table_regs[1].b16())?;
            (if is_defaults_zero {
                code.tbl_v(result.b16(), VReg::new(0).b16(), indices.b16(), 2)
            } else {
                code.tbx_v(result.b16(), VReg::new(0).b16(), indices.b16(), 2)
            })?;
        }
        3 => {
            code.mov_v(VReg::new(0).b16(), table_regs[0].b16())?;
            code.mov_v(VReg::new(1).b16(), table_regs[1].b16())?;
            code.mov_v(VReg::new(2).b16(), table_regs[2].b16())?;
            (if is_defaults_zero {
                code.tbl_v(result.b16(), VReg::new(0).b16(), indices.b16(), 3)
            } else {
                code.tbx_v(result.b16(), VReg::new(0).b16(), indices.b16(), 3)
            })?;
        }
        4 => {
            code.mov_v(VReg::new(0).b16(), table_regs[0].b16())?;
            code.mov_v(VReg::new(1).b16(), table_regs[1].b16())?;
            code.mov_v(VReg::new(2).b16(), table_regs[2].b16())?;
            code.mov_v(VReg::new(3).b16(), table_regs[3].b16())?;
            (if is_defaults_zero {
                code.tbl_v(result.b16(), VReg::new(0).b16(), indices.b16(), 4)
            } else {
                code.tbx_v(result.b16(), VReg::new(0).b16(), indices.b16(), 4)
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
    code: &mut rhazel::CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    match ctx.block.get(inst_ref).opcode {
        Opcode::VectorGetElement8 => emit_get_element::<VReg16B>(code, ctx, inst_ref),
        Opcode::VectorGetElement16 => emit_get_element::<VReg8H>(code, ctx, inst_ref),
        Opcode::VectorGetElement32 => emit_get_element::<VReg4S>(code, ctx, inst_ref),
        Opcode::VectorGetElement64 => emit_get_element::<VReg2D>(code, ctx, inst_ref),
        Opcode::VectorSetElement8 => emit_set_element::<VReg16B>(code, ctx, inst_ref),
        Opcode::VectorSetElement16 => emit_set_element::<VReg8H>(code, ctx, inst_ref),
        Opcode::VectorSetElement32 => emit_set_element::<VReg4S>(code, ctx, inst_ref),
        Opcode::VectorSetElement64 => emit_set_element::<VReg2D>(code, ctx, inst_ref),
        Opcode::VectorBroadcastLower8 => emit_broadcast::<VReg8B>(code, ctx, inst_ref),
        Opcode::VectorBroadcastLower16 => emit_broadcast::<VReg4H>(code, ctx, inst_ref),
        Opcode::VectorBroadcastLower32 => emit_broadcast::<VReg2S>(code, ctx, inst_ref),
        Opcode::VectorBroadcast8 => emit_broadcast::<VReg16B>(code, ctx, inst_ref),
        Opcode::VectorBroadcast16 => emit_broadcast::<VReg8H>(code, ctx, inst_ref),
        Opcode::VectorBroadcast32 => emit_broadcast::<VReg4S>(code, ctx, inst_ref),
        Opcode::VectorBroadcast64 => emit_broadcast::<VReg2D>(code, ctx, inst_ref),
        Opcode::VectorBroadcastElementLower8 => {
            emit_broadcast_element::<VReg8B>(code, ctx, inst_ref)
        }
        Opcode::VectorBroadcastElementLower16 => {
            emit_broadcast_element::<VReg4H>(code, ctx, inst_ref)
        }
        Opcode::VectorBroadcastElementLower32 => {
            emit_broadcast_element::<VReg2S>(code, ctx, inst_ref)
        }
        Opcode::VectorBroadcastElement8 => emit_broadcast_element::<VReg16B>(code, ctx, inst_ref),
        Opcode::VectorBroadcastElement16 => emit_broadcast_element::<VReg8H>(code, ctx, inst_ref),
        Opcode::VectorBroadcastElement32 => emit_broadcast_element::<VReg4S>(code, ctx, inst_ref),
        Opcode::VectorBroadcastElement64 => emit_broadcast_element::<VReg2D>(code, ctx, inst_ref),
        Opcode::VectorAbs8 => {
            emit_two_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn| code.abs_v(rd, rn))
        }
        Opcode::VectorAbs16 => {
            emit_two_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn| code.abs_v(rd, rn))
        }
        Opcode::VectorAbs32 => {
            emit_two_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn| code.abs_v(rd, rn))
        }
        Opcode::VectorAbs64 => {
            emit_two_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, rd, rn| code.abs_v(rd, rn))
        }
        Opcode::VectorNot => emit_two_op(code, ctx, inst_ref, |code, rd, rn| {
            code.not_v(rd.b16(), rn.b16())
        }),
        Opcode::VectorCountLeadingZeros8 => {
            emit_two_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn| code.clz_v(rd, rn))
        }
        Opcode::VectorCountLeadingZeros16 => {
            emit_two_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn| code.clz_v(rd, rn))
        }
        Opcode::VectorCountLeadingZeros32 => {
            emit_two_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn| code.clz_v(rd, rn))
        }
        Opcode::VectorPopulationCount => emit_two_op(code, ctx, inst_ref, |code, rd, rn| {
            code.cnt_v(rd.b16(), rn.b16())
        }),
        Opcode::VectorReverseBits => emit_two_op(code, ctx, inst_ref, |code, rd, rn| {
            code.rbit_v(rd.b16(), rn.b16())
        }),
        Opcode::VectorReverseElementsInHalfGroups8 => {
            emit_two_op(code, ctx, inst_ref, |code, rd, rn| {
                code.rev16_v(rd.b16(), rn.b16())
            })
        }
        Opcode::VectorReverseElementsInWordGroups8 => {
            emit_two_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn| {
                code.rev32_v(rd, rn)
            })
        }
        Opcode::VectorReverseElementsInWordGroups16 => {
            emit_two_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn| code.rev32_v(rd, rn))
        }
        Opcode::VectorReverseElementsInLongGroups8 => {
            emit_two_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn| {
                code.rev64_v(rd, rn)
            })
        }
        Opcode::VectorReverseElementsInLongGroups16 => {
            emit_two_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn| code.rev64_v(rd, rn))
        }
        Opcode::VectorReverseElementsInLongGroups32 => {
            emit_two_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn| code.rev64_v(rd, rn))
        }
        Opcode::VectorReduceAdd8 => emit_reduce::<VReg16B>(code, ctx, inst_ref),
        Opcode::VectorReduceAdd16 => emit_reduce::<VReg8H>(code, ctx, inst_ref),
        Opcode::VectorReduceAdd32 => emit_reduce::<VReg4S>(code, ctx, inst_ref),
        Opcode::VectorReduceAdd64 => emit_reduce::<VReg2D>(code, ctx, inst_ref),
        Opcode::VectorZeroExtend8 => {
            emit_widen::<VReg8B>(code, ctx, inst_ref, |code, rd, rn| code.uxtl(rd, rn))
        }
        Opcode::VectorZeroExtend16 => {
            emit_widen::<VReg4H>(code, ctx, inst_ref, |code, rd, rn| code.uxtl(rd, rn))
        }
        Opcode::VectorZeroExtend32 => {
            emit_widen::<VReg2S>(code, ctx, inst_ref, |code, rd, rn| code.uxtl(rd, rn))
        }
        Opcode::VectorSignExtend8 => {
            emit_widen::<VReg8B>(code, ctx, inst_ref, |code, rd, rn| code.sxtl(rd, rn))
        }
        Opcode::VectorSignExtend16 => {
            emit_widen::<VReg4H>(code, ctx, inst_ref, |code, rd, rn| code.sxtl(rd, rn))
        }
        Opcode::VectorSignExtend32 => {
            emit_widen::<VReg2S>(code, ctx, inst_ref, |code, rd, rn| code.sxtl(rd, rn))
        }
        Opcode::VectorZeroExtend64 | Opcode::VectorZeroUpper => {
            emit_zero_upper(code, ctx, inst_ref)
        }
        Opcode::VectorNarrow16 => {
            emit_narrow::<VReg8H>(code, ctx, inst_ref, |code, rd, rn| code.xtn(rd, rn))
        }
        Opcode::VectorNarrow32 => {
            emit_narrow::<VReg4S>(code, ctx, inst_ref, |code, rd, rn| code.xtn(rd, rn))
        }
        Opcode::VectorNarrow64 => {
            emit_narrow::<VReg2D>(code, ctx, inst_ref, |code, rd, rn| code.xtn(rd, rn))
        }
        Opcode::VectorAdd8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.add_v(rd, rn, rm)
            })
        }
        Opcode::VectorAdd16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.add_v(rd, rn, rm)
            })
        }
        Opcode::VectorAdd32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.add_v(rd, rn, rm)
            })
        }
        Opcode::VectorAdd64 => {
            emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.add_v(rd, rn, rm)
            })
        }
        Opcode::VectorSub8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sub_v(rd, rn, rm)
            })
        }
        Opcode::VectorSub16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sub_v(rd, rn, rm)
            })
        }
        Opcode::VectorSub32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sub_v(rd, rn, rm)
            })
        }
        Opcode::VectorSub64 => {
            emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sub_v(rd, rn, rm)
            })
        }
        Opcode::VectorMultiply8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.mul_v(rd, rn, rm)
            })
        }
        Opcode::VectorMultiply16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.mul_v(rd, rn, rm)
            })
        }
        Opcode::VectorMultiply32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.mul_v(rd, rn, rm)
            })
        }
        Opcode::VectorMultiplySignedWiden8 => {
            emit_three_op_arranged_widen::<VReg8B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.smull_v(rd, rn, rm)
            })
        }
        Opcode::VectorMultiplySignedWiden16 => {
            emit_three_op_arranged_widen::<VReg4H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.smull_v(rd, rn, rm)
            })
        }
        Opcode::VectorMultiplySignedWiden32 => {
            emit_three_op_arranged_widen::<VReg2S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.smull_v(rd, rn, rm)
            })
        }
        Opcode::VectorMultiplyUnsignedWiden8 => {
            emit_three_op_arranged_widen::<VReg8B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.umull_v(rd, rn, rm)
            })
        }
        Opcode::VectorMultiplyUnsignedWiden16 => {
            emit_three_op_arranged_widen::<VReg4H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.umull_v(rd, rn, rm)
            })
        }
        Opcode::VectorMultiplyUnsignedWiden32 => {
            emit_three_op_arranged_widen::<VReg2S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.umull_v(rd, rn, rm)
            })
        }
        Opcode::VectorAnd => emit_three_op(code, ctx, inst_ref, |code, rd, rn, rm| {
            code.and_v(rd.b16(), rn.b16(), rm.b16())
        }),
        Opcode::VectorAndNot => emit_three_op(code, ctx, inst_ref, |code, rd, rn, rm| {
            code.bic_v(rd.b16(), rn.b16(), rm.b16())
        }),
        Opcode::VectorEor => emit_three_op(code, ctx, inst_ref, |code, rd, rn, rm| {
            code.eor_v(rd.b16(), rn.b16(), rm.b16())
        }),
        Opcode::VectorOr => emit_three_op(code, ctx, inst_ref, |code, rd, rn, rm| {
            code.orr_v(rd.b16(), rn.b16(), rm.b16())
        }),
        Opcode::VectorEqual8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.cmeq_v(rd, rn, rm)
            })
        }
        Opcode::VectorEqual16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.cmeq_v(rd, rn, rm)
            })
        }
        Opcode::VectorEqual32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.cmeq_v(rd, rn, rm)
            })
        }
        Opcode::VectorEqual64 => {
            emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.cmeq_v(rd, rn, rm)
            })
        }
        Opcode::VectorGreaterS8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.cmgt_v(rd, rn, rm)
            })
        }
        Opcode::VectorGreaterS16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.cmgt_v(rd, rn, rm)
            })
        }
        Opcode::VectorGreaterS32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.cmgt_v(rd, rn, rm)
            })
        }
        Opcode::VectorGreaterS64 => {
            emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.cmgt_v(rd, rn, rm)
            })
        }
        Opcode::VectorHalvingAddS8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.shadd(rd, rn, rm)
            })
        }
        Opcode::VectorHalvingAddS16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.shadd(rd, rn, rm)
            })
        }
        Opcode::VectorHalvingAddS32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.shadd(rd, rn, rm)
            })
        }
        Opcode::VectorHalvingAddU8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uhadd(rd, rn, rm)
            })
        }
        Opcode::VectorHalvingAddU16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uhadd(rd, rn, rm)
            })
        }
        Opcode::VectorHalvingAddU32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uhadd(rd, rn, rm)
            })
        }
        Opcode::VectorHalvingSubS8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.shsub(rd, rn, rm)
            })
        }
        Opcode::VectorHalvingSubS16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.shsub(rd, rn, rm)
            })
        }
        Opcode::VectorHalvingSubS32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.shsub(rd, rn, rm)
            })
        }
        Opcode::VectorHalvingSubU8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uhsub(rd, rn, rm)
            })
        }
        Opcode::VectorHalvingSubU16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uhsub(rd, rn, rm)
            })
        }
        Opcode::VectorHalvingSubU32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uhsub(rd, rn, rm)
            })
        }
        Opcode::VectorMaxS8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.smax_v(rd, rn, rm)
            })
        }
        Opcode::VectorMaxS16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.smax_v(rd, rn, rm)
            })
        }
        Opcode::VectorMaxS32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.smax_v(rd, rn, rm)
            })
        }
        Opcode::VectorMaxU8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.umax_v(rd, rn, rm)
            })
        }
        Opcode::VectorMaxU16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.umax_v(rd, rn, rm)
            })
        }
        Opcode::VectorMaxU32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.umax_v(rd, rn, rm)
            })
        }
        Opcode::VectorMinS8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.smin_v(rd, rn, rm)
            })
        }
        Opcode::VectorMinS16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.smin_v(rd, rn, rm)
            })
        }
        Opcode::VectorMinS32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.smin_v(rd, rn, rm)
            })
        }
        Opcode::VectorMinU8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.umin_v(rd, rn, rm)
            })
        }
        Opcode::VectorMinU16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.umin_v(rd, rn, rm)
            })
        }
        Opcode::VectorMinU32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.umin_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedAddLower8 => {
            emit_three_op_arranged_lower::<VReg8B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.addp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedAddLower16 => {
            emit_three_op_arranged_lower::<VReg4H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.addp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedAddLower32 => {
            emit_three_op_arranged_lower::<VReg2S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.addp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedAddSignedWiden8 => {
            emit_pair_widen::<VReg16B, VReg8H>(code, ctx, inst_ref, |code, rd, rn| {
                code.saddlp_v(rd, rn)
            })
        }
        Opcode::VectorPairedAddSignedWiden16 => {
            emit_pair_widen::<VReg8H, VReg4S>(code, ctx, inst_ref, |code, rd, rn| {
                code.saddlp_v(rd, rn)
            })
        }
        Opcode::VectorPairedAddSignedWiden32 => {
            emit_pair_widen::<VReg4S, VReg2D>(code, ctx, inst_ref, |code, rd, rn| {
                code.saddlp_v(rd, rn)
            })
        }
        Opcode::VectorPairedAddUnsignedWiden8 => {
            emit_pair_widen::<VReg16B, VReg8H>(code, ctx, inst_ref, |code, rd, rn| {
                code.uaddlp_v(rd, rn)
            })
        }
        Opcode::VectorPairedAddUnsignedWiden16 => {
            emit_pair_widen::<VReg8H, VReg4S>(code, ctx, inst_ref, |code, rd, rn| {
                code.uaddlp_v(rd, rn)
            })
        }
        Opcode::VectorPairedAddUnsignedWiden32 => {
            emit_pair_widen::<VReg4S, VReg2D>(code, ctx, inst_ref, |code, rd, rn| {
                code.uaddlp_v(rd, rn)
            })
        }
        Opcode::VectorPairedAdd8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.addp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedAdd16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.addp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedAdd32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.addp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedAdd64 => {
            emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.addp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMaxS8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.smaxp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMaxS16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.smaxp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMaxS32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.smaxp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMaxU8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.umaxp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMaxU16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.umaxp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMaxU32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.umaxp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMaxLowerS8 => {
            emit_three_op_arranged_lower::<VReg8B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.smaxp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMaxLowerS16 => {
            emit_three_op_arranged_lower::<VReg4H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.smaxp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMaxLowerS32 => {
            emit_three_op_arranged_lower::<VReg2S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.smaxp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMaxLowerU8 => {
            emit_three_op_arranged_lower::<VReg8B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.umaxp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMaxLowerU16 => {
            emit_three_op_arranged_lower::<VReg4H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.umaxp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMaxLowerU32 => {
            emit_three_op_arranged_lower::<VReg2S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.umaxp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMinS8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sminp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMinS16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sminp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMinS32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sminp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMinU8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uminp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMinU16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uminp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMinU32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uminp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMinLowerS8 => {
            emit_three_op_arranged_lower::<VReg8B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sminp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMinLowerS16 => {
            emit_three_op_arranged_lower::<VReg4H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sminp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMinLowerS32 => {
            emit_three_op_arranged_lower::<VReg2S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sminp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMinLowerU8 => {
            emit_three_op_arranged_lower::<VReg8B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uminp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMinLowerU16 => {
            emit_three_op_arranged_lower::<VReg4H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uminp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPairedMinLowerU32 => {
            emit_three_op_arranged_lower::<VReg2S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uminp_v(rd, rn, rm)
            })
        }
        Opcode::VectorPolynomialMultiply8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.pmul_v(rd, rn, rm)
            })
        }
        Opcode::VectorPolynomialMultiplyLong8 => {
            emit_three_op_arranged_widen::<VReg8B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.pmull_v(rd, rn, rm)
            })
        }
        Opcode::VectorPolynomialMultiplyLong64 => {
            emit_three_op(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.pmull_64(rd.q(), rn.d1(), rm.d1())
            })
        }
        Opcode::VectorArithmeticVShift8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sshl_v(rd, rn, rm)
            })
        }
        Opcode::VectorArithmeticVShift16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sshl_v(rd, rn, rm)
            })
        }
        Opcode::VectorArithmeticVShift32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sshl_v(rd, rn, rm)
            })
        }
        Opcode::VectorArithmeticVShift64 => {
            emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sshl_v(rd, rn, rm)
            })
        }
        Opcode::VectorLogicalVShift8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.ushl_v(rd, rn, rm)
            })
        }
        Opcode::VectorLogicalVShift16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.ushl_v(rd, rn, rm)
            })
        }
        Opcode::VectorLogicalVShift32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.ushl_v(rd, rn, rm)
            })
        }
        Opcode::VectorLogicalVShift64 => {
            emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.ushl_v(rd, rn, rm)
            })
        }
        Opcode::VectorRoundingShiftLeftS8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.srshl_v(rd, rn, rm)
            })
        }
        Opcode::VectorRoundingShiftLeftS16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.srshl_v(rd, rn, rm)
            })
        }
        Opcode::VectorRoundingShiftLeftS32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.srshl_v(rd, rn, rm)
            })
        }
        Opcode::VectorRoundingShiftLeftS64 => {
            emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.srshl_v(rd, rn, rm)
            })
        }
        Opcode::VectorRoundingShiftLeftU8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.urshl_v(rd, rn, rm)
            })
        }
        Opcode::VectorRoundingShiftLeftU16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.urshl_v(rd, rn, rm)
            })
        }
        Opcode::VectorRoundingShiftLeftU32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.urshl_v(rd, rn, rm)
            })
        }
        Opcode::VectorRoundingShiftLeftU64 => {
            emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.urshl_v(rd, rn, rm)
            })
        }
        Opcode::VectorSignedAbsoluteDifference8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sabd_v(rd, rn, rm)
            })
        }
        Opcode::VectorSignedAbsoluteDifference16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sabd_v(rd, rn, rm)
            })
        }
        Opcode::VectorSignedAbsoluteDifference32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sabd_v(rd, rn, rm)
            })
        }
        Opcode::VectorSignedMultiply16 => {
            unreachable!("Eden marks VectorSignedMultiply16 unreachable on ARM64")
        }
        Opcode::VectorSignedMultiply32 => {
            unreachable!("Eden marks VectorSignedMultiply32 unreachable on ARM64")
        }
        Opcode::VectorUnsignedAbsoluteDifference8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uabd(rd, rn, rm)
            })
        }
        Opcode::VectorUnsignedAbsoluteDifference16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uabd(rd, rn, rm)
            })
        }
        Opcode::VectorUnsignedAbsoluteDifference32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uabd(rd, rn, rm)
            })
        }
        Opcode::VectorUnsignedMultiply16 => {
            unreachable!("Eden marks VectorUnsignedMultiply16 unreachable on ARM64")
        }
        Opcode::VectorUnsignedMultiply32 => {
            unreachable!("Eden marks VectorUnsignedMultiply32 unreachable on ARM64")
        }
        Opcode::VectorRoundingHalvingAddS8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.srhadd_v(rd, rn, rm)
            })
        }
        Opcode::VectorRoundingHalvingAddS16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.srhadd_v(rd, rn, rm)
            })
        }
        Opcode::VectorRoundingHalvingAddS32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.srhadd_v(rd, rn, rm)
            })
        }
        Opcode::VectorRoundingHalvingAddU8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.urhadd_v(rd, rn, rm)
            })
        }
        Opcode::VectorRoundingHalvingAddU16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.urhadd_v(rd, rn, rm)
            })
        }
        Opcode::VectorRoundingHalvingAddU32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.urhadd_v(rd, rn, rm)
            })
        }
        Opcode::VectorSignedSaturatedAbs8 => {
            emit_two_op_arranged_saturated::<VReg16B>(code, ctx, inst_ref, |code, rd, rn| {
                code.sqabs_v(rd, rn)
            })
        }
        Opcode::VectorSignedSaturatedAbs16 => {
            emit_two_op_arranged_saturated::<VReg8H>(code, ctx, inst_ref, |code, rd, rn| {
                code.sqabs_v(rd, rn)
            })
        }
        Opcode::VectorSignedSaturatedAbs32 => {
            emit_two_op_arranged_saturated::<VReg4S>(code, ctx, inst_ref, |code, rd, rn| {
                code.sqabs_v(rd, rn)
            })
        }
        Opcode::VectorSignedSaturatedAbs64 => {
            emit_two_op_arranged_saturated::<VReg2D>(code, ctx, inst_ref, |code, rd, rn| {
                code.sqabs_v(rd, rn)
            })
        }
        Opcode::VectorSignedSaturatedAccumulateUnsigned8 => {
            emit_saturated_accumulate::<VReg16B>(code, ctx, inst_ref, |code, rd, rn| {
                code.suqadd_v(rd, rn)
            })
        }
        Opcode::VectorSignedSaturatedAccumulateUnsigned16 => {
            emit_saturated_accumulate::<VReg8H>(code, ctx, inst_ref, |code, rd, rn| {
                code.suqadd_v(rd, rn)
            })
        }
        Opcode::VectorSignedSaturatedAccumulateUnsigned32 => {
            emit_saturated_accumulate::<VReg4S>(code, ctx, inst_ref, |code, rd, rn| {
                code.suqadd_v(rd, rn)
            })
        }
        Opcode::VectorSignedSaturatedAccumulateUnsigned64 => {
            emit_saturated_accumulate::<VReg2D>(code, ctx, inst_ref, |code, rd, rn| {
                code.suqadd_v(rd, rn)
            })
        }
        Opcode::VectorSignedSaturatedDoublingMultiplyHigh16 => {
            emit_three_op_arranged_saturated::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sqdmulh_v(rd, rn, rm)
            })
        }
        Opcode::VectorSignedSaturatedDoublingMultiplyHigh32 => {
            emit_three_op_arranged_saturated::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sqdmulh_v(rd, rn, rm)
            })
        }
        Opcode::VectorSignedSaturatedDoublingMultiplyHighRounding16 => {
            emit_three_op_arranged_saturated::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sqrdmulh_v(rd, rn, rm)
            })
        }
        Opcode::VectorSignedSaturatedDoublingMultiplyHighRounding32 => {
            emit_three_op_arranged_saturated::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sqrdmulh_v(rd, rn, rm)
            })
        }
        Opcode::VectorSignedSaturatedDoublingMultiplyLong16 => {
            emit_three_op_arranged_saturated_widen::<VReg4H>(
                code,
                ctx,
                inst_ref,
                |code, rd, rn, rm| code.sqdmull_v(rd, rn, rm),
            )
        }
        Opcode::VectorSignedSaturatedDoublingMultiplyLong32 => {
            emit_three_op_arranged_saturated_widen::<VReg2S>(
                code,
                ctx,
                inst_ref,
                |code, rd, rn, rm| code.sqdmull_v(rd, rn, rm),
            )
        }
        Opcode::VectorSignedSaturatedNarrowToSigned16 => {
            emit_narrow_saturated::<VReg8H>(code, ctx, inst_ref, |code, rd, rn| {
                code.sqxtn_v(rd, rn)
            })
        }
        Opcode::VectorSignedSaturatedNarrowToSigned32 => {
            emit_narrow_saturated::<VReg4S>(code, ctx, inst_ref, |code, rd, rn| {
                code.sqxtn_v(rd, rn)
            })
        }
        Opcode::VectorSignedSaturatedNarrowToSigned64 => {
            emit_narrow_saturated::<VReg2D>(code, ctx, inst_ref, |code, rd, rn| {
                code.sqxtn_v(rd, rn)
            })
        }
        Opcode::VectorSignedSaturatedNarrowToUnsigned16 => {
            emit_narrow_saturated::<VReg8H>(code, ctx, inst_ref, |code, rd, rn| {
                code.sqxtun_v(rd, rn)
            })
        }
        Opcode::VectorSignedSaturatedNarrowToUnsigned32 => {
            emit_narrow_saturated::<VReg4S>(code, ctx, inst_ref, |code, rd, rn| {
                code.sqxtun_v(rd, rn)
            })
        }
        Opcode::VectorSignedSaturatedNarrowToUnsigned64 => {
            emit_narrow_saturated::<VReg2D>(code, ctx, inst_ref, |code, rd, rn| {
                code.sqxtun_v(rd, rn)
            })
        }
        Opcode::VectorSignedSaturatedNeg8 => {
            emit_two_op_arranged_saturated::<VReg16B>(code, ctx, inst_ref, |code, rd, rn| {
                code.sqneg_v(rd, rn)
            })
        }
        Opcode::VectorSignedSaturatedNeg16 => {
            emit_two_op_arranged_saturated::<VReg8H>(code, ctx, inst_ref, |code, rd, rn| {
                code.sqneg_v(rd, rn)
            })
        }
        Opcode::VectorSignedSaturatedNeg32 => {
            emit_two_op_arranged_saturated::<VReg4S>(code, ctx, inst_ref, |code, rd, rn| {
                code.sqneg_v(rd, rn)
            })
        }
        Opcode::VectorSignedSaturatedNeg64 => {
            emit_two_op_arranged_saturated::<VReg2D>(code, ctx, inst_ref, |code, rd, rn| {
                code.sqneg_v(rd, rn)
            })
        }
        Opcode::VectorSignedSaturatedShiftLeft8 => {
            emit_three_op_arranged_saturated::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sqshl_v(rd, rn, rm)
            })
        }
        Opcode::VectorSignedSaturatedShiftLeft16 => {
            emit_three_op_arranged_saturated::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sqshl_v(rd, rn, rm)
            })
        }
        Opcode::VectorSignedSaturatedShiftLeft32 => {
            emit_three_op_arranged_saturated::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sqshl_v(rd, rn, rm)
            })
        }
        Opcode::VectorSignedSaturatedShiftLeft64 => {
            emit_three_op_arranged_saturated::<VReg2D>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.sqshl_v(rd, rn, rm)
            })
        }
        Opcode::VectorSignedSaturatedShiftLeftUnsigned8 => {
            emit_imm_shift_saturated::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, shift| {
                code.sqshlu_v(rd, rn, shift)
            })
        }
        Opcode::VectorSignedSaturatedShiftLeftUnsigned16 => {
            emit_imm_shift_saturated::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, shift| {
                code.sqshlu_v(rd, rn, shift)
            })
        }
        Opcode::VectorSignedSaturatedShiftLeftUnsigned32 => {
            emit_imm_shift_saturated::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, shift| {
                code.sqshlu_v(rd, rn, shift)
            })
        }
        Opcode::VectorSignedSaturatedShiftLeftUnsigned64 => {
            emit_imm_shift_saturated::<VReg2D>(code, ctx, inst_ref, |code, rd, rn, shift| {
                code.sqshlu_v(rd, rn, shift)
            })
        }
        Opcode::VectorTable => emit_vector_table(code, ctx, inst_ref),
        Opcode::VectorTableLookup64 => emit_vector_table_lookup64(code, ctx, inst_ref),
        Opcode::VectorTableLookup128 => emit_vector_table_lookup128(code, ctx, inst_ref),
        Opcode::VectorUnsignedRecipEstimate => emit_two_op(code, ctx, inst_ref, |code, rd, rn| {
            code.urecpe_v(rd.s4(), rn.s4())
        }),
        Opcode::VectorUnsignedRecipSqrtEstimate => {
            emit_two_op(code, ctx, inst_ref, |code, rd, rn| {
                code.ursqrte_v(rd.s4(), rn.s4())
            })
        }
        Opcode::VectorUnsignedSaturatedAccumulateSigned8 => {
            emit_saturated_accumulate::<VReg16B>(code, ctx, inst_ref, |code, rd, rn| {
                code.usqadd_v(rd, rn)
            })
        }
        Opcode::VectorUnsignedSaturatedAccumulateSigned16 => {
            emit_saturated_accumulate::<VReg8H>(code, ctx, inst_ref, |code, rd, rn| {
                code.usqadd_v(rd, rn)
            })
        }
        Opcode::VectorUnsignedSaturatedAccumulateSigned32 => {
            emit_saturated_accumulate::<VReg4S>(code, ctx, inst_ref, |code, rd, rn| {
                code.usqadd_v(rd, rn)
            })
        }
        Opcode::VectorUnsignedSaturatedAccumulateSigned64 => {
            emit_saturated_accumulate::<VReg2D>(code, ctx, inst_ref, |code, rd, rn| {
                code.usqadd_v(rd, rn)
            })
        }
        Opcode::VectorUnsignedSaturatedNarrow16 => {
            emit_narrow_saturated::<VReg8H>(code, ctx, inst_ref, |code, rd, rn| {
                code.uqxtn_v(rd, rn)
            })
        }
        Opcode::VectorUnsignedSaturatedNarrow32 => {
            emit_narrow_saturated::<VReg4S>(code, ctx, inst_ref, |code, rd, rn| {
                code.uqxtn_v(rd, rn)
            })
        }
        Opcode::VectorUnsignedSaturatedNarrow64 => {
            emit_narrow_saturated::<VReg2D>(code, ctx, inst_ref, |code, rd, rn| {
                code.uqxtn_v(rd, rn)
            })
        }
        Opcode::VectorUnsignedSaturatedShiftLeft8 => {
            emit_three_op_arranged_saturated::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uqshl_v(rd, rn, rm)
            })
        }
        Opcode::VectorUnsignedSaturatedShiftLeft16 => {
            emit_three_op_arranged_saturated::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uqshl_v(rd, rn, rm)
            })
        }
        Opcode::VectorUnsignedSaturatedShiftLeft32 => {
            emit_three_op_arranged_saturated::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uqshl_v(rd, rn, rm)
            })
        }
        Opcode::VectorUnsignedSaturatedShiftLeft64 => {
            emit_three_op_arranged_saturated::<VReg2D>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uqshl_v(rd, rn, rm)
            })
        }
        Opcode::VectorInterleaveLower8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.zip1(rd, rn, rm)
            })
        }
        Opcode::VectorInterleaveLower16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.zip1(rd, rn, rm)
            })
        }
        Opcode::VectorInterleaveLower32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.zip1(rd, rn, rm)
            })
        }
        Opcode::VectorInterleaveLower64 => {
            emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.zip1(rd, rn, rm)
            })
        }
        Opcode::VectorInterleaveUpper8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.zip2_v(rd, rn, rm)
            })
        }
        Opcode::VectorInterleaveUpper16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.zip2_v(rd, rn, rm)
            })
        }
        Opcode::VectorInterleaveUpper32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.zip2_v(rd, rn, rm)
            })
        }
        Opcode::VectorInterleaveUpper64 => {
            emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.zip2_v(rd, rn, rm)
            })
        }
        Opcode::VectorDeinterleaveEven8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uzp1_v(rd, rn, rm)
            })
        }
        Opcode::VectorDeinterleaveEven16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uzp1_v(rd, rn, rm)
            })
        }
        Opcode::VectorDeinterleaveEven32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uzp1_v(rd, rn, rm)
            })
        }
        Opcode::VectorDeinterleaveEven64 => {
            emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uzp1_v(rd, rn, rm)
            })
        }
        Opcode::VectorDeinterleaveEvenLower8 => {
            emit_three_op_arranged_lower::<VReg8B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uzp1_v(rd, rn, rm)
            })
        }
        Opcode::VectorDeinterleaveEvenLower16 => {
            emit_three_op_arranged_lower::<VReg4H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uzp1_v(rd, rn, rm)
            })
        }
        Opcode::VectorDeinterleaveEvenLower32 => {
            emit_three_op_arranged_lower::<VReg2S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uzp1_v(rd, rn, rm)
            })
        }
        Opcode::VectorDeinterleaveOdd8 => {
            emit_three_op_arranged::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uzp2_v(rd, rn, rm)
            })
        }
        Opcode::VectorDeinterleaveOdd16 => {
            emit_three_op_arranged::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uzp2_v(rd, rn, rm)
            })
        }
        Opcode::VectorDeinterleaveOdd32 => {
            emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uzp2_v(rd, rn, rm)
            })
        }
        Opcode::VectorDeinterleaveOdd64 => {
            emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uzp2_v(rd, rn, rm)
            })
        }
        Opcode::VectorDeinterleaveOddLower8 => {
            emit_three_op_arranged_lower::<VReg8B>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uzp2_v(rd, rn, rm)
            })
        }
        Opcode::VectorDeinterleaveOddLower16 => {
            emit_three_op_arranged_lower::<VReg4H>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uzp2_v(rd, rn, rm)
            })
        }
        Opcode::VectorDeinterleaveOddLower32 => {
            emit_three_op_arranged_lower::<VReg2S>(code, ctx, inst_ref, |code, rd, rn, rm| {
                code.uzp2_v(rd, rn, rm)
            })
        }
        Opcode::VectorTranspose8 => emit_transpose::<VReg16B>(code, ctx, inst_ref),
        Opcode::VectorTranspose16 => emit_transpose::<VReg8H>(code, ctx, inst_ref),
        Opcode::VectorTranspose32 => emit_transpose::<VReg4S>(code, ctx, inst_ref),
        Opcode::VectorTranspose64 => emit_transpose::<VReg2D>(code, ctx, inst_ref),
        Opcode::VectorLogicalShiftLeft8 => {
            emit_imm_shift::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, shift| {
                code.shl_v(rd, rn, shift)
            })
        }
        Opcode::VectorLogicalShiftLeft16 => {
            emit_imm_shift::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, shift| {
                code.shl_v(rd, rn, shift)
            })
        }
        Opcode::VectorLogicalShiftLeft32 => {
            emit_imm_shift::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, shift| {
                code.shl_v(rd, rn, shift)
            })
        }
        Opcode::VectorLogicalShiftLeft64 => {
            emit_imm_shift::<VReg2D>(code, ctx, inst_ref, |code, rd, rn, shift| {
                code.shl_v(rd, rn, shift)
            })
        }
        Opcode::VectorLogicalShiftRight8 => {
            emit_imm_shift::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, shift| {
                code.ushr(rd, rn, shift)
            })
        }
        Opcode::VectorLogicalShiftRight16 => {
            emit_imm_shift::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, shift| {
                code.ushr(rd, rn, shift)
            })
        }
        Opcode::VectorLogicalShiftRight32 => {
            emit_imm_shift::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, shift| {
                code.ushr(rd, rn, shift)
            })
        }
        Opcode::VectorLogicalShiftRight64 => {
            emit_imm_shift::<VReg2D>(code, ctx, inst_ref, |code, rd, rn, shift| {
                code.ushr(rd, rn, shift)
            })
        }
        Opcode::VectorArithmeticShiftRight8 => {
            emit_imm_shift::<VReg16B>(code, ctx, inst_ref, |code, rd, rn, shift| {
                code.sshr(rd, rn, shift)
            })
        }
        Opcode::VectorArithmeticShiftRight16 => {
            emit_imm_shift::<VReg8H>(code, ctx, inst_ref, |code, rd, rn, shift| {
                code.sshr(rd, rn, shift)
            })
        }
        Opcode::VectorArithmeticShiftRight32 => {
            emit_imm_shift::<VReg4S>(code, ctx, inst_ref, |code, rd, rn, shift| {
                code.sshr(rd, rn, shift)
            })
        }
        Opcode::VectorArithmeticShiftRight64 => {
            emit_imm_shift::<VReg2D>(code, ctx, inst_ref, |code, rd, rn, shift| {
                code.sshr(rd, rn, shift)
            })
        }
        Opcode::VectorExtract => emit_extract::<VReg16B>(code, ctx, inst_ref),
        Opcode::VectorExtractLower => emit_extract::<VReg8B>(code, ctx, inst_ref),
        Opcode::ZeroVector => emit_zero_vector(code, ctx, inst_ref),
        opcode => Err(format!("unimplemented ARM64 vector opcode: {opcode:?}")),
    }
}

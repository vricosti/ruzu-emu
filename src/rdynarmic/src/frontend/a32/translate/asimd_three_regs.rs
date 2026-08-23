use crate::frontend::a32::decoder::DecodedArm;
use crate::frontend::a32::types::ExtReg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

use super::asimd::to_vector_reg;

enum Comparison {
    GreaterEqual,
    GreaterThan,
    Equal,
}

enum AccumulateBehavior {
    None,
    Accumulate,
}

fn decode_asimd_floating_point_operands(
    inst: &DecodedArm,
) -> (bool, bool, u32, u32, bool, bool, bool, u32) {
    let d = ((inst.raw >> 22) & 1) != 0;
    let sz = ((inst.raw >> 20) & 1) != 0;
    let vn = (inst.raw >> 16) & 0xF;
    let vd = (inst.raw >> 12) & 0xF;
    let n = ((inst.raw >> 7) & 1) != 0;
    let q = ((inst.raw >> 6) & 1) != 0;
    let m = ((inst.raw >> 5) & 1) != 0;
    let vm = inst.raw & 0xF;
    (d, sz, vn, vd, n, q, m, vm)
}

fn asimd_floating_point_instruction<F>(ir: &mut A32IREmitter, inst: &DecodedArm, mut f: F) -> bool
where
    F: FnMut(&mut A32IREmitter, Value, Value, Value) -> Value,
{
    let (d, sz, vn, vd, n, q, m, vm) = decode_asimd_floating_point_operands(inst);

    if q && ((vd & 1) != 0 || (vn & 1) != 0 || (vm & 1) != 0) {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }

    if sz {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }

    let dest = match to_vector_reg(q, d, vd) {
        Some(reg) => reg,
        None => {
            ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
            ir.set_term(Terminal::CheckHalt {
                else_: Box::new(Terminal::ReturnToDispatch),
            });
            return false;
        }
    };
    let src_n = match to_vector_reg(q, n, vn) {
        Some(reg) => reg,
        None => {
            ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
            ir.set_term(Terminal::CheckHalt {
                else_: Box::new(Terminal::ReturnToDispatch),
            });
            return false;
        }
    };
    let src_m = match to_vector_reg(q, m, vm) {
        Some(reg) => reg,
        None => {
            ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
            ir.set_term(Terminal::CheckHalt {
                else_: Box::new(Terminal::ReturnToDispatch),
            });
            return false;
        }
    };

    let reg_d = ir.get_vector(dest);
    let reg_n = ir.get_vector(src_n);
    let reg_m = ir.get_vector(src_m);
    let result = f(ir, reg_d, reg_n, reg_m);
    ir.set_vector(dest, result);
    true
}

fn decode_three_reg_same(inst: &DecodedArm) -> (bool, bool, u32, u32, u32, bool, bool, bool, u32) {
    let u = ((inst.raw >> 24) & 1) != 0;
    let d = ((inst.raw >> 22) & 1) != 0;
    let sz = (inst.raw >> 20) & 0x3;
    let vn = (inst.raw >> 16) & 0xF;
    let vd = (inst.raw >> 12) & 0xF;
    let n = ((inst.raw >> 7) & 1) != 0;
    let q = ((inst.raw >> 6) & 1) != 0;
    let m = ((inst.raw >> 5) & 1) != 0;
    let vm = inst.raw & 0xF;
    (u, d, sz, vn, vd, n, q, m, vm)
}

fn validate_three_reg_int(
    ir: &mut A32IREmitter,
    d: bool,
    sz: u32,
    vn: u32,
    vd: u32,
    n: bool,
    q: bool,
    m: bool,
    vm: u32,
) -> Option<(usize, ExtReg, ExtReg, ExtReg)> {
    if sz == 0b11 {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return None;
    }
    if q && ((vd & 1) != 0 || (vn & 1) != 0 || (vm & 1) != 0) {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return None;
    }
    let esize = 8usize << sz;
    let d_reg = to_vector_reg(q, d, vd)?;
    let n_reg = to_vector_reg(q, n, vn)?;
    let m_reg = to_vector_reg(q, m, vm)?;
    Some((esize, d_reg, n_reg, m_reg))
}

fn bitwise_instruction<const WITH_DST: bool, F>(
    ir: &mut A32IREmitter,
    inst: &DecodedArm,
    mut f: F,
) -> bool
where
    F: FnMut(&mut A32IREmitter, Value, Value, Value) -> Value,
{
    let (_u, d, _sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    if q && ((vd & 1) != 0 || (vn & 1) != 0 || (vm & 1) != 0) {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }

    let Some(d_reg) = to_vector_reg(q, d, vd) else {
        return false;
    };
    let Some(n_reg) = to_vector_reg(q, n, vn) else {
        return false;
    };
    let Some(m_reg) = to_vector_reg(q, m, vm) else {
        return false;
    };

    let reg_d = if WITH_DST {
        ir.get_vector(d_reg)
    } else {
        ir.ir().zero_vector()
    };
    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    let result = f(ir, reg_d, reg_n, reg_m);
    ir.set_vector(d_reg, result);
    true
}

fn integer_comparison(ir: &mut A32IREmitter, inst: &DecodedArm, comparison: Comparison) -> bool {
    let (u, d, sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    let Some((esize, d_reg, n_reg, m_reg)) = validate_three_reg_int(ir, d, sz, vn, vd, n, q, m, vm)
    else {
        return false;
    };

    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    let result = match comparison {
        Comparison::GreaterThan => {
            if u {
                ir.ir().vector_greater_unsigned(esize, reg_n, reg_m)
            } else {
                ir.ir().vector_greater_signed(esize, reg_n, reg_m)
            }
        }
        Comparison::GreaterEqual => {
            if u {
                ir.ir().vector_greater_equal_unsigned(esize, reg_n, reg_m)
            } else {
                ir.ir().vector_greater_equal_signed(esize, reg_n, reg_m)
            }
        }
        Comparison::Equal => ir.ir().vector_equal(esize, reg_n, reg_m),
    };

    ir.set_vector(d_reg, result);
    true
}

fn absolute_difference(
    ir: &mut A32IREmitter,
    inst: &DecodedArm,
    accumulate: AccumulateBehavior,
) -> bool {
    let (u, d, sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    let Some((esize, d_reg, n_reg, m_reg)) = validate_three_reg_int(ir, d, sz, vn, vd, n, q, m, vm)
    else {
        return false;
    };

    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    let absdiff = if u {
        ir.ir()
            .vector_unsigned_absolute_difference(esize, reg_n, reg_m)
    } else {
        ir.ir()
            .vector_signed_absolute_difference(esize, reg_n, reg_m)
    };

    let result = match accumulate {
        AccumulateBehavior::None => absdiff,
        AccumulateBehavior::Accumulate => {
            let reg_d = ir.get_vector(d_reg);
            ir.ir().vector_add(esize, reg_d, absdiff)
        }
    };

    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vadd_float(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    asimd_floating_point_instruction(ir, inst, |ir, _reg_d, reg_n, reg_m| {
        ir.ir().fp_vector_add(32, reg_n, reg_m, false)
    })
}

pub fn arm_asimd_vsub_float(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    asimd_floating_point_instruction(ir, inst, |ir, _reg_d, reg_n, reg_m| {
        ir.ir().fp_vector_sub(32, reg_n, reg_m, false)
    })
}

pub fn arm_asimd_vmla_float(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    asimd_floating_point_instruction(ir, inst, |ir, reg_d, reg_n, reg_m| {
        let product = ir.ir().fp_vector_mul(32, reg_n, reg_m, false);
        ir.ir().fp_vector_add(32, reg_d, product, false)
    })
}

pub fn arm_asimd_vmls_float(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    asimd_floating_point_instruction(ir, inst, |ir, reg_d, reg_n, reg_m| {
        let product = ir.ir().fp_vector_mul(32, reg_n, reg_m, false);
        ir.ir().fp_vector_sub(32, reg_d, product, false)
    })
}

pub fn arm_asimd_vcgt_reg_float(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    asimd_floating_point_instruction(ir, inst, |ir, _reg_d, reg_n, reg_m| {
        ir.ir().fp_vector_greater(32, reg_n, reg_m, false)
    })
}

pub fn arm_asimd_vmul_float(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    asimd_floating_point_instruction(ir, inst, |ir, _reg_d, reg_n, reg_m| {
        ir.ir().fp_vector_mul(32, reg_n, reg_m, false)
    })
}

pub fn arm_asimd_vpadd_float(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let q = ((inst.raw >> 6) & 1) != 0;
    if q {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    asimd_floating_point_instruction(ir, inst, |ir, _reg_d, reg_n, reg_m| {
        ir.ir().fp_vector_paired_add_lower(32, reg_n, reg_m, false)
    })
}

pub fn arm_asimd_vorr_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    bitwise_instruction::<false, _>(ir, inst, |ir, _reg_d, reg_n, reg_m| {
        ir.ir().vector_or(reg_n, reg_m)
    })
}

pub fn arm_asimd_vrsqrts(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    asimd_floating_point_instruction(ir, inst, |ir, _reg_d, reg_n, reg_m| {
        ir.ir().fp_vector_rsqrt_step_fused(32, reg_n, reg_m, false)
    })
}

pub fn arm_asimd_vbsl(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    bitwise_instruction::<true, _>(ir, inst, |ir, reg_d, reg_n, reg_m| {
        let a = ir.ir().vector_and(reg_n, reg_d);
        let b = ir.ir().vector_and_not(reg_m, reg_d);
        ir.ir().vector_or(a, b)
    })
}

pub fn arm_asimd_vqrdmulh(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let d = ((inst.raw >> 22) & 1) != 0;
    let sz = (inst.raw >> 20) & 0x3;
    let vn = (inst.raw >> 16) & 0xF;
    let vd = (inst.raw >> 12) & 0xF;
    let n = ((inst.raw >> 7) & 1) != 0;
    let q = ((inst.raw >> 6) & 1) != 0;
    let m = ((inst.raw >> 5) & 1) != 0;
    let vm = inst.raw & 0xF;
    if q && ((vd & 1) != 0 || (vn & 1) != 0 || (vm & 1) != 0) {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    if sz == 0b00 || sz == 0b11 {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    let esize = 8usize << sz;
    let Some(dest) = to_vector_reg(q, d, vd) else {
        return false;
    };
    let Some(src_n) = to_vector_reg(q, n, vn) else {
        return false;
    };
    let Some(src_m) = to_vector_reg(q, m, vm) else {
        return false;
    };
    let reg_n = ir.get_vector(src_n);
    let reg_m = ir.get_vector(src_m);
    let result = ir
        .ir()
        .vector_signed_saturated_doubling_multiply_high_rounding(esize, reg_n, reg_m);
    ir.set_vector(dest, result);
    true
}

pub fn arm_asimd_vmax_float(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    asimd_floating_point_instruction(ir, inst, |ir, _reg_d, reg_n, reg_m| {
        ir.ir().fp_vector_max(32, reg_n, reg_m, false)
    })
}

pub fn arm_asimd_vmin_float(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    asimd_floating_point_instruction(ir, inst, |ir, _reg_d, reg_n, reg_m| {
        ir.ir().fp_vector_min(32, reg_n, reg_m, false)
    })
}

pub fn arm_asimd_vadd_int(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (_u, d, sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    let Some((esize, d_reg, n_reg, m_reg)) = validate_three_reg_int(ir, d, sz, vn, vd, n, q, m, vm)
    else {
        return false;
    };
    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    let result = ir.ir().vector_add(esize, reg_n, reg_m);
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vsub_int(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (_u, d, sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    let Some((esize, d_reg, n_reg, m_reg)) = validate_three_reg_int(ir, d, sz, vn, vd, n, q, m, vm)
    else {
        return false;
    };
    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    let result = ir.ir().vector_sub(esize, reg_n, reg_m);
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vmul_int(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (p, d, sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    if sz == 0b11 || (p && sz != 0b00) {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    if q && ((vd & 1) != 0 || (vn & 1) != 0 || (vm & 1) != 0) {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    let esize = 8usize << sz;
    let Some(d_reg) = to_vector_reg(q, d, vd) else {
        return false;
    };
    let Some(n_reg) = to_vector_reg(q, n, vn) else {
        return false;
    };
    let Some(m_reg) = to_vector_reg(q, m, vm) else {
        return false;
    };
    if p {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    let result = ir.ir().vector_multiply(esize, reg_n, reg_m);
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vmla_int(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (op, d, sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    let Some((esize, d_reg, n_reg, m_reg)) = validate_three_reg_int(ir, d, sz, vn, vd, n, q, m, vm)
    else {
        return false;
    };
    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    let reg_d = ir.get_vector(d_reg);
    let multiply = ir.ir().vector_multiply(esize, reg_n, reg_m);
    let result = if op {
        ir.ir().vector_sub(esize, reg_d, multiply)
    } else {
        ir.ir().vector_add(esize, reg_d, multiply)
    };
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vand_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (_u, d, _sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    if q && ((vd & 1) != 0 || (vn & 1) != 0 || (vm & 1) != 0) {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    let Some(d_reg) = to_vector_reg(q, d, vd) else {
        return false;
    };
    let Some(n_reg) = to_vector_reg(q, n, vn) else {
        return false;
    };
    let Some(m_reg) = to_vector_reg(q, m, vm) else {
        return false;
    };
    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    let result = ir.ir().vector_and(reg_n, reg_m);
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vbic_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    bitwise_instruction::<false, _>(ir, inst, |ir, _reg_d, reg_n, reg_m| {
        ir.ir().vector_and_not(reg_n, reg_m)
    })
}

pub fn arm_asimd_vorn_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    bitwise_instruction::<false, _>(ir, inst, |ir, _reg_d, reg_n, reg_m| {
        let not_m = ir.ir().vector_not(reg_m);
        ir.ir().vector_or(reg_n, not_m)
    })
}

pub fn arm_asimd_veor_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    bitwise_instruction::<false, _>(ir, inst, |ir, _reg_d, reg_n, reg_m| {
        ir.ir().vector_eor(reg_n, reg_m)
    })
}

pub fn arm_asimd_vbit(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    bitwise_instruction::<true, _>(ir, inst, |ir, reg_d, reg_n, reg_m| {
        let a = ir.ir().vector_and(reg_n, reg_m);
        let b = ir.ir().vector_and_not(reg_d, reg_m);
        ir.ir().vector_or(a, b)
    })
}

pub fn arm_asimd_vbif(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    bitwise_instruction::<true, _>(ir, inst, |ir, reg_d, reg_n, reg_m| {
        let a = ir.ir().vector_and(reg_d, reg_m);
        let b = ir.ir().vector_and_not(reg_n, reg_m);
        ir.ir().vector_or(a, b)
    })
}

pub fn arm_asimd_vcgt_reg_int(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    integer_comparison(ir, inst, Comparison::GreaterThan)
}

pub fn arm_asimd_vcge_reg_int(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    integer_comparison(ir, inst, Comparison::GreaterEqual)
}

pub fn arm_asimd_vceq_reg_int(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    integer_comparison(ir, inst, Comparison::Equal)
}

pub fn arm_asimd_vtst(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (_u, d, sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    let Some((esize, d_reg, n_reg, m_reg)) = validate_three_reg_int(ir, d, sz, vn, vd, n, q, m, vm)
    else {
        return false;
    };
    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    let anded = ir.ir().vector_and(reg_n, reg_m);
    let zero = ir.ir().zero_vector();
    let eq_zero = ir.ir().vector_equal(esize, anded, zero);
    let result = ir.ir().vector_not(eq_zero);
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vabd_int(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    absolute_difference(ir, inst, AccumulateBehavior::None)
}

pub fn arm_asimd_vmax_min_int(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (u, d, sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    let op = ((inst.raw >> 4) & 1) != 0;
    let Some((esize, d_reg, n_reg, m_reg)) = validate_three_reg_int(ir, d, sz, vn, vd, n, q, m, vm)
    else {
        return false;
    };
    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    let result = match (op, u) {
        (false, false) => ir.ir().vector_max_signed(esize, reg_n, reg_m),
        (false, true) => ir.ir().vector_max_unsigned(esize, reg_n, reg_m),
        (true, false) => ir.ir().vector_min_signed(esize, reg_n, reg_m),
        (true, true) => ir.ir().vector_min_unsigned(esize, reg_n, reg_m),
    };
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vpadd_int(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (_u, d, sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    if sz == 0b11 || q {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    let esize = 8usize << sz;
    let Some(d_reg) = to_vector_reg(false, d, vd) else {
        return false;
    };
    let Some(n_reg) = to_vector_reg(false, n, vn) else {
        return false;
    };
    let Some(m_reg) = to_vector_reg(false, m, vm) else {
        return false;
    };
    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    let result = ir.ir().vector_paired_add(esize, reg_n, reg_m);
    ir.set_vector(d_reg, result);
    true
}

// --- Newly implemented three-register same (integer) ---

pub fn arm_asimd_vhadd(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (u, d, sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    let Some((esize, d_reg, n_reg, m_reg)) = validate_three_reg_int(ir, d, sz, vn, vd, n, q, m, vm)
    else {
        return false;
    };
    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    let result = if u {
        ir.ir().vector_halving_add_unsigned(esize, reg_n, reg_m)
    } else {
        ir.ir().vector_halving_add_signed(esize, reg_n, reg_m)
    };
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vqadd(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (u, d, sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    let Some((esize, d_reg, n_reg, m_reg)) = validate_three_reg_int(ir, d, sz, vn, vd, n, q, m, vm)
    else {
        return false;
    };
    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    let result = if u {
        ir.ir().vector_unsigned_saturated_add(esize, reg_n, reg_m)
    } else {
        ir.ir().vector_signed_saturated_add(esize, reg_n, reg_m)
    };
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vrhadd(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (u, d, sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    let Some((esize, d_reg, n_reg, m_reg)) = validate_three_reg_int(ir, d, sz, vn, vd, n, q, m, vm)
    else {
        return false;
    };
    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    let result = if u {
        ir.ir()
            .vector_rounding_halving_add_unsigned(esize, reg_n, reg_m)
    } else {
        ir.ir()
            .vector_rounding_halving_add_signed(esize, reg_n, reg_m)
    };
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vhsub(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (u, d, sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    let Some((esize, d_reg, n_reg, m_reg)) = validate_three_reg_int(ir, d, sz, vn, vd, n, q, m, vm)
    else {
        return false;
    };
    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    let result = if u {
        ir.ir().vector_halving_sub_unsigned(esize, reg_n, reg_m)
    } else {
        ir.ir().vector_halving_sub_signed(esize, reg_n, reg_m)
    };
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vqsub(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (u, d, sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    let Some((esize, d_reg, n_reg, m_reg)) = validate_three_reg_int(ir, d, sz, vn, vd, n, q, m, vm)
    else {
        return false;
    };
    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    let result = if u {
        ir.ir().vector_unsigned_saturated_sub(esize, reg_n, reg_m)
    } else {
        ir.ir().vector_signed_saturated_sub(esize, reg_n, reg_m)
    };
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vshl_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (u, d, sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    let Some((esize, d_reg, n_reg, m_reg)) = validate_three_reg_int(ir, d, sz, vn, vd, n, q, m, vm)
    else {
        return false;
    };
    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    // Upstream: U ? VectorLogicalVShift(esize, reg_m, reg_n) : VectorArithmeticVShift(esize, reg_m, reg_n)
    // Note: operand order is (Vm, Vn) — Vm is the data, Vn is the shift amount
    let result = if u {
        ir.ir().vector_logical_v_shift(esize, reg_m, reg_n)
    } else {
        ir.ir().vector_arithmetic_v_shift(esize, reg_m, reg_n)
    };
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vqshl_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (u, d, sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    let Some((esize, d_reg, n_reg, m_reg)) = validate_three_reg_int(ir, d, sz, vn, vd, n, q, m, vm)
    else {
        return false;
    };
    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    // Upstream: U ? VectorUnsignedSaturatedShiftLeft(esize, reg_m, reg_n) : VectorSignedSaturatedShiftLeft(esize, reg_m, reg_n)
    // Note: operand order is (Vm, Vn) — Vm is the data, Vn is the shift amount
    let result = if u {
        ir.ir()
            .vector_unsigned_saturated_shift_left(esize, reg_m, reg_n)
    } else {
        ir.ir()
            .vector_signed_saturated_shift_left(esize, reg_m, reg_n)
    };
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vrshl(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (u, d, sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    let Some((esize, d_reg, n_reg, m_reg)) = validate_three_reg_int(ir, d, sz, vn, vd, n, q, m, vm)
    else {
        return false;
    };
    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    // Upstream: U ? VectorRoundingShiftLeftUnsigned(esize, reg_m, reg_n) : VectorRoundingShiftLeftSigned(esize, reg_m, reg_n)
    // Note: operand order is (Vm, Vn)
    let result = if u {
        ir.ir()
            .vector_rounding_shift_left_unsigned(esize, reg_m, reg_n)
    } else {
        ir.ir()
            .vector_rounding_shift_left_signed(esize, reg_m, reg_n)
    };
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vaba(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    absolute_difference(ir, inst, AccumulateBehavior::Accumulate)
}

pub fn arm_asimd_vpmax_int(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (u, d, sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    let op = ((inst.raw >> 4) & 1) != 0;
    if sz == 0b11 || q {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    let esize = 8usize << sz;
    let Some(d_reg) = to_vector_reg(false, d, vd) else {
        return false;
    };
    let Some(n_reg) = to_vector_reg(false, n, vn) else {
        return false;
    };
    let Some(m_reg) = to_vector_reg(false, m, vm) else {
        return false;
    };
    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    let bottom = ir.ir().vector_deinterleave_even_lower(esize, reg_n, reg_m);
    let top = ir.ir().vector_deinterleave_odd_lower(esize, reg_n, reg_m);
    let result = match (op, u) {
        (false, false) => ir.ir().vector_max_signed(esize, bottom, top),
        (false, true) => ir.ir().vector_max_unsigned(esize, bottom, top),
        (true, false) => ir.ir().vector_min_signed(esize, bottom, top),
        (true, true) => ir.ir().vector_min_unsigned(esize, bottom, top),
    };
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vqdmulh(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (_u, d, sz, vn, vd, n, q, m, vm) = decode_three_reg_same(inst);
    if q && ((vd & 1) != 0 || (vn & 1) != 0 || (vm & 1) != 0) {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    if sz == 0b00 || sz == 0b11 {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    let esize = 8usize << sz;
    let Some(dest) = to_vector_reg(q, d, vd) else {
        return false;
    };
    let Some(src_n) = to_vector_reg(q, n, vn) else {
        return false;
    };
    let Some(src_m) = to_vector_reg(q, m, vm) else {
        return false;
    };
    let reg_n = ir.get_vector(src_n);
    let reg_m = ir.get_vector(src_m);
    let result = ir
        .ir()
        .vector_signed_saturated_doubling_multiply_high(esize, reg_n, reg_m);
    ir.set_vector(dest, result);
    true
}

// --- Newly implemented three-register same (float) ---

pub fn arm_asimd_vabd_float(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    asimd_floating_point_instruction(ir, inst, |ir, _reg_d, reg_n, reg_m| {
        let diff = ir.ir().fp_vector_sub(32, reg_n, reg_m, false);
        ir.ir().fp_vector_abs(32, diff)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::decoder::ArmInstId;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;

    fn translate_with(
        inst: DecodedArm,
        f: fn(&mut A32IREmitter, &DecodedArm) -> bool,
    ) -> Vec<Opcode> {
        let loc = A32LocationDescriptor::at(0x1000);
        let mut block = Block::new(loc.to_location());
        let ok = {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            f(&mut ir, &inst)
        };
        assert!(ok);
        block.instructions.iter().map(|inst| inst.opcode).collect()
    }

    #[test]
    fn vbic_reg_emits_vector_and_not() {
        let opcodes = translate_with(
            DecodedArm {
                raw: 0xF210_0110,
                id: ArmInstId::ASIMD_VBIC_reg,
            },
            arm_asimd_vbic_reg,
        );
        assert!(opcodes.contains(&Opcode::VectorAndNot));
        assert_eq!(opcodes.last(), Some(&Opcode::A32SetVector));
    }

    #[test]
    fn vbsl_emits_and_andnot_or_chain() {
        let opcodes = translate_with(
            DecodedArm {
                raw: 0xF310_0110,
                id: ArmInstId::ASIMD_VBSL,
            },
            arm_asimd_vbsl,
        );
        assert!(opcodes.contains(&Opcode::VectorAnd));
        assert!(opcodes.contains(&Opcode::VectorAndNot));
        assert!(opcodes.contains(&Opcode::VectorOr));
        assert_eq!(opcodes.last(), Some(&Opcode::A32SetVector));
    }

    #[test]
    fn vbit_and_vbif_emit_vector_and_not() {
        let vbit = translate_with(
            DecodedArm {
                raw: 0xF320_0110,
                id: ArmInstId::ASIMD_VBIT,
            },
            arm_asimd_vbit,
        );
        let vbif = translate_with(
            DecodedArm {
                raw: 0xF330_0110,
                id: ArmInstId::ASIMD_VBIF,
            },
            arm_asimd_vbif,
        );
        assert!(vbit.contains(&Opcode::VectorAndNot));
        assert!(vbif.contains(&Opcode::VectorAndNot));
    }

    #[test]
    fn vabd_and_vaba_emit_absolute_difference_ops() {
        let vabd = translate_with(
            DecodedArm {
                raw: 0xF200_0700,
                id: ArmInstId::ASIMD_VABD_int,
            },
            arm_asimd_vabd_int,
        );
        let vaba = translate_with(
            DecodedArm {
                raw: 0xF200_0710,
                id: ArmInstId::ASIMD_VABA,
            },
            arm_asimd_vaba,
        );
        assert!(vabd.contains(&Opcode::VectorSignedAbsoluteDifference8));
        assert!(vaba.contains(&Opcode::VectorSignedAbsoluteDifference8));
        assert!(vaba.contains(&Opcode::VectorAdd8));
    }

    #[test]
    fn vabd_float_emits_sub_then_abs() {
        let opcodes = translate_with(
            DecodedArm {
                raw: 0xF320_0D00,
                id: ArmInstId::ASIMD_VABD_float,
            },
            arm_asimd_vabd_float,
        );
        assert!(opcodes.contains(&Opcode::FPVectorSub32));
        assert!(opcodes.contains(&Opcode::FPVectorAbs32));
    }

    #[test]
    fn unsigned_vcgt_uses_edens_min_equal_not_helper_sequence() {
        let opcodes = translate_with(
            DecodedArm {
                raw: 0xF300_0300,
                id: ArmInstId::ASIMD_VCGT_reg_int,
            },
            arm_asimd_vcgt_reg_int,
        );
        for opcode in [Opcode::VectorMinU8, Opcode::VectorEqual8, Opcode::VectorNot] {
            assert!(opcodes.contains(&opcode));
        }
        assert_eq!(opcodes.last(), Some(&Opcode::A32SetVector));
    }
}

pub fn arm_asimd_vceq_reg_float(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    asimd_floating_point_instruction(ir, inst, |ir, _reg_d, reg_n, reg_m| {
        ir.ir().fp_vector_equal(32, reg_n, reg_m, false)
    })
}

pub fn arm_asimd_vcge_reg_float(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    asimd_floating_point_instruction(ir, inst, |ir, _reg_d, reg_n, reg_m| {
        ir.ir().fp_vector_greater_equal(32, reg_n, reg_m, false)
    })
}

pub fn arm_asimd_vacge(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    // op bit is bit 21
    let op = ((inst.raw >> 21) & 1) != 0;
    let d = ((inst.raw >> 22) & 1) != 0;
    let sz = ((inst.raw >> 20) & 1) != 0;
    let vn = (inst.raw >> 16) & 0xF;
    let vd = (inst.raw >> 12) & 0xF;
    let n = ((inst.raw >> 7) & 1) != 0;
    let q = ((inst.raw >> 6) & 1) != 0;
    let m = ((inst.raw >> 5) & 1) != 0;
    let vm = inst.raw & 0xF;

    if sz {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    if q && ((vd & 1) != 0 || (vn & 1) != 0 || (vm & 1) != 0) {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }

    let Some(dest) = to_vector_reg(q, d, vd) else {
        return false;
    };
    let Some(src_n) = to_vector_reg(q, n, vn) else {
        return false;
    };
    let Some(src_m) = to_vector_reg(q, m, vm) else {
        return false;
    };

    let reg_n = ir.get_vector(src_n);
    let reg_m = ir.get_vector(src_m);
    let abs_n = ir.ir().fp_vector_abs(32, reg_n);
    let abs_m = ir.ir().fp_vector_abs(32, reg_m);
    // op=0 -> AbsoluteGE, op=1 -> AbsoluteGT
    let result = if op {
        ir.ir().fp_vector_greater(32, abs_n, abs_m, false)
    } else {
        ir.ir().fp_vector_greater_equal(32, abs_n, abs_m, false)
    };
    ir.set_vector(dest, result);
    true
}

pub fn arm_asimd_vfma(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    asimd_floating_point_instruction(ir, inst, |ir, reg_d, reg_n, reg_m| {
        ir.ir().fp_vector_mul_add(32, reg_d, reg_n, reg_m, false)
    })
}

pub fn arm_asimd_vfms(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    asimd_floating_point_instruction(ir, inst, |ir, reg_d, reg_n, reg_m| {
        let neg_n = ir.ir().fp_vector_neg(32, reg_n);
        ir.ir().fp_vector_mul_add(32, reg_d, neg_n, reg_m, false)
    })
}

pub fn arm_asimd_vpmax_float(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let q = ((inst.raw >> 6) & 1) != 0;
    if q {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    asimd_floating_point_instruction(ir, inst, |ir, _reg_d, reg_n, reg_m| {
        let bottom = ir.ir().vector_deinterleave_even_lower(32, reg_n, reg_m);
        let top = ir.ir().vector_deinterleave_odd_lower(32, reg_n, reg_m);
        ir.ir().fp_vector_max(32, bottom, top, false)
    })
}

pub fn arm_asimd_vpmin_float(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let q = ((inst.raw >> 6) & 1) != 0;
    if q {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    asimd_floating_point_instruction(ir, inst, |ir, _reg_d, reg_n, reg_m| {
        let bottom = ir.ir().vector_deinterleave_even_lower(32, reg_n, reg_m);
        let top = ir.ir().vector_deinterleave_odd_lower(32, reg_n, reg_m);
        ir.ir().fp_vector_min(32, bottom, top, false)
    })
}

pub fn arm_asimd_vrecps(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    asimd_floating_point_instruction(ir, inst, |ir, _reg_d, reg_n, reg_m| {
        ir.ir().fp_vector_recip_step_fused(32, reg_n, reg_m, false)
    })
}

// --- Three registers of different length ---

fn decode_three_reg_diff(inst: &DecodedArm) -> (bool, bool, u32, u32, u32, bool, bool, u32) {
    let u = ((inst.raw >> 24) & 1) != 0;
    let d = ((inst.raw >> 22) & 1) != 0;
    let sz = (inst.raw >> 20) & 0x3;
    let vn = (inst.raw >> 16) & 0xF;
    let vd = (inst.raw >> 12) & 0xF;
    let n = ((inst.raw >> 7) & 1) != 0;
    let m = ((inst.raw >> 5) & 1) != 0;
    let vm = inst.raw & 0xF;
    (u, d, sz, vn, vd, n, m, vm)
}

fn wide_instruction<F>(ir: &mut A32IREmitter, inst: &DecodedArm, widen_first: bool, f: F) -> bool
where
    F: FnOnce(&mut A32IREmitter, usize, Value, Value, Value) -> Value,
{
    let (u, d, sz, vn, vd, n, m, vm) = decode_three_reg_diff(inst);
    let esize = 8usize << sz;

    if sz == 0b11 {
        // DecodeError
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }

    if (vd & 1) != 0 || (!widen_first && (vn & 1) != 0) {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }

    let Some(d_reg) = to_vector_reg(true, d, vd) else {
        return false;
    };
    let Some(m_reg) = to_vector_reg(false, m, vm) else {
        return false;
    };
    let Some(n_reg) = to_vector_reg(!widen_first, n, vn) else {
        return false;
    };

    let reg_d = ir.get_vector(d_reg);
    let reg_m = ir.get_vector(m_reg);
    let reg_n = ir.get_vector(n_reg);
    let wide_n = if u {
        ir.ir().vector_zero_extend(esize, reg_n)
    } else {
        ir.ir().vector_sign_extend(esize, reg_n)
    };
    let wide_m = if u {
        ir.ir().vector_zero_extend(esize, reg_m)
    } else {
        ir.ir().vector_sign_extend(esize, reg_m)
    };
    let result = f(
        ir,
        esize * 2,
        reg_d,
        if widen_first { wide_n } else { reg_n },
        wide_m,
    );
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vaddl(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    // op bit distinguishes VADDL (op=0) vs VADDW (op=1)
    let op = ((inst.raw >> 8) & 1) != 0;
    let widen_first = !op; // VADDL widens both, VADDW widens only second
    wide_instruction(ir, inst, widen_first, |ir, esize, _reg_d, reg_n, reg_m| {
        ir.ir().vector_add(esize, reg_n, reg_m)
    })
}

pub fn arm_asimd_vsubl(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    // op bit distinguishes VSUBL (op=0) vs VSUBW (op=1)
    let op = ((inst.raw >> 8) & 1) != 0;
    let widen_first = !op;
    wide_instruction(ir, inst, widen_first, |ir, esize, _reg_d, reg_n, reg_m| {
        ir.ir().vector_sub(esize, reg_n, reg_m)
    })
}

fn absolute_difference_long(ir: &mut A32IREmitter, inst: &DecodedArm, accumulate: bool) -> bool {
    let (u, d, sz, vn, vd, n, m, vm) = decode_three_reg_diff(inst);

    if sz == 0b11 {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }

    if (vd & 1) != 0 {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }

    let esize = 8usize << sz;
    let Some(d_reg) = to_vector_reg(true, d, vd) else {
        return false;
    };
    let Some(m_reg) = to_vector_reg(false, m, vm) else {
        return false;
    };
    let Some(n_reg) = to_vector_reg(false, n, vn) else {
        return false;
    };

    let reg_m = ir.get_vector(m_reg);
    let reg_n = ir.get_vector(n_reg);
    // Extract lower 64 bits and zero-extend each element
    let elem_m = ir.ir().vector_get_element(64, reg_m, 0);
    let quad_m = ir.ir().zero_extend_to_quad(elem_m);
    let operand_m = ir.ir().vector_zero_extend(esize, quad_m);
    let elem_n = ir.ir().vector_get_element(64, reg_n, 0);
    let quad_n = ir.ir().zero_extend_to_quad(elem_n);
    let operand_n = ir.ir().vector_zero_extend(esize, quad_n);
    let absdiff = if u {
        ir.ir()
            .vector_unsigned_absolute_difference(esize, operand_m, operand_n)
    } else {
        ir.ir()
            .vector_signed_absolute_difference(esize, operand_m, operand_n)
    };

    let result = if accumulate {
        let reg_d = ir.get_vector(d_reg);
        ir.ir().vector_add(2 * esize, reg_d, absdiff)
    } else {
        absdiff
    };

    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vabal(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    absolute_difference_long(ir, inst, true)
}

pub fn arm_asimd_vabdl(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    absolute_difference_long(ir, inst, false)
}

pub fn arm_asimd_vmlal(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (u, d, sz, vn, vd, n, m, vm) = decode_three_reg_diff(inst);
    // op bit at position 9 distinguishes VMLAL (op=0) vs VMLSL (op=1)
    let op = ((inst.raw >> 9) & 1) != 0;
    let esize = 8usize << sz;

    if sz == 0b11 {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }

    if (vd & 1) != 0 {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }

    let Some(d_reg) = to_vector_reg(true, d, vd) else {
        return false;
    };
    let Some(m_reg) = to_vector_reg(false, m, vm) else {
        return false;
    };
    let Some(n_reg) = to_vector_reg(false, n, vn) else {
        return false;
    };

    let reg_d = ir.get_vector(d_reg);
    let reg_m = ir.get_vector(m_reg);
    let reg_n = ir.get_vector(n_reg);
    let multiply = if u {
        ir.ir().vector_multiply_unsigned_widen(esize, reg_n, reg_m)
    } else {
        ir.ir().vector_multiply_signed_widen(esize, reg_n, reg_m)
    };
    let result = if op {
        ir.ir().vector_sub(esize * 2, reg_d, multiply)
    } else {
        ir.ir().vector_add(esize * 2, reg_d, multiply)
    };
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vmull(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (u, d, sz, vn, vd, n, m, vm) = decode_three_reg_diff(inst);
    // P bit at position 9
    let p = ((inst.raw >> 9) & 1) != 0;

    if sz == 0b11 {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }

    if (p && (u || sz == 0b10)) || (vd & 1) != 0 {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }

    let esize = if p {
        if sz == 0b00 {
            8
        } else {
            64
        }
    } else {
        8usize << sz
    };
    let Some(d_reg) = to_vector_reg(true, d, vd) else {
        return false;
    };
    let Some(m_reg) = to_vector_reg(false, m, vm) else {
        return false;
    };
    let Some(n_reg) = to_vector_reg(false, n, vn) else {
        return false;
    };

    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    let result = if p {
        ir.ir().vector_polynomial_multiply_long(esize, reg_n, reg_m)
    } else if u {
        ir.ir().vector_multiply_unsigned_widen(esize, reg_n, reg_m)
    } else {
        ir.ir().vector_multiply_signed_widen(esize, reg_n, reg_m)
    };
    ir.set_vector(d_reg, result);
    true
}

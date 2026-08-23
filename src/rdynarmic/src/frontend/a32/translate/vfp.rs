//! VFP (floating-point) load/store instructions: VPUSH, VPOP, VLDR, VSTR, VSTM, VLDM.
//!
//! Port of dynarmic's vfp.cpp VPUSH/VPOP/VLDR/VSTR/VSTM/VLDM.

use crate::frontend::a32::decoder::DecodedArm;
use crate::frontend::a32::types::{ExtReg, Reg};
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::acc_type::AccType;
use crate::ir::cond::Cond;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

/// Extract VFP fields: (D-bit, Vd, sz, imm8).
///   sz: true = double-precision (D regs, coproc=0b1011), false = single (S regs)
fn vfp_fields(inst: &DecodedArm) -> (bool, u32, bool, u32) {
    let d_bit = (inst.raw >> 22) & 1 != 0;
    let vd = (inst.raw >> 12) & 0xF;
    let sz = (inst.raw >> 8) & 0xF == 0b1011;
    let imm8 = inst.raw & 0xFF;
    (d_bit, vd, sz, imm8)
}

/// Compute the ExtReg for a VFP register index.
///   Double: Dd where d = D:Vd
///   Single: Sd where d = Vd:D
fn to_ext_reg(d_bit: bool, vd: u32, sz: bool) -> ExtReg {
    if sz {
        ExtReg::from_double(((if d_bit { 16u32 } else { 0 }) + vd) as u8)
    } else {
        ExtReg::from_single(((vd << 1) | (if d_bit { 1 } else { 0 })) as u8)
    }
}

/// Number of registers in the list.
fn reg_count(sz: bool, imm8: u32) -> u32 {
    if sz {
        imm8 / 2
    } else {
        imm8
    }
}

/// Advance an ExtReg by `n` within its category.
fn advance_ext_reg(base: ExtReg, n: u32) -> ExtReg {
    if base.is_double() {
        ExtReg::from_double((base.index() as u32 + n) as u8)
    } else {
        ExtReg::from_single((base.index() as u32 + n) as u8)
    }
}

/// Extract VFP unary data-processing fields: (D-bit, Vd, sz, M-bit, Vm).
/// Upstream owner: frontend/A32/translate/impl/vfp.cpp unary helpers.
fn vfp_unary_fields(inst: &DecodedArm) -> (bool, u32, bool, bool, u32) {
    let d_bit = (inst.raw >> 22) & 1 != 0;
    let vd = (inst.raw >> 12) & 0xF;
    let sz = (inst.raw >> 8) & 1 != 0;
    let m_bit = (inst.raw >> 5) & 1 != 0;
    let vm = inst.raw & 0xF;
    (d_bit, vd, sz, m_bit, vm)
}

fn unary_src_dst(inst: &DecodedArm) -> (ExtReg, ExtReg, bool) {
    let (d_bit, vd, sz, m_bit, vm) = vfp_unary_fields(inst);
    let d = to_ext_reg(d_bit, vd, sz);
    let m = to_ext_reg(m_bit, vm, sz);
    (d, m, sz)
}

/// Extract VFP three-register data-processing fields: (D-bit, Vn, Vd, sz, N-bit, M-bit, Vm).
/// Upstream owner: frontend/A32/translate/impl/vfp.cpp three-register helpers.
fn vfp_ternary_fields(inst: &DecodedArm) -> (bool, u32, u32, bool, bool, bool, u32) {
    let d_bit = (inst.raw >> 22) & 1 != 0;
    let vn = (inst.raw >> 16) & 0xF;
    let vd = (inst.raw >> 12) & 0xF;
    let sz = (inst.raw >> 8) & 1 != 0;
    let n_bit = (inst.raw >> 7) & 1 != 0;
    let m_bit = (inst.raw >> 5) & 1 != 0;
    let vm = inst.raw & 0xF;
    (d_bit, vn, vd, sz, n_bit, m_bit, vm)
}

fn ternary_src_dst(inst: &DecodedArm) -> (ExtReg, ExtReg, ExtReg, bool) {
    let (d_bit, vn, vd, sz, n_bit, m_bit, vm) = vfp_ternary_fields(inst);
    let d = to_ext_reg(d_bit, vd, sz);
    let n = to_ext_reg(n_bit, vn, sz);
    let m = to_ext_reg(m_bit, vm, sz);
    (d, n, m, sz)
}

fn vfp_vsel_fields(inst: &DecodedArm) -> (ExtReg, ExtReg, ExtReg, bool, Cond) {
    let (d_bit, vn, vd, sz, n_bit, m_bit, vm) = vfp_ternary_fields(inst);
    let cc = ((inst.raw >> 20) & 0x3) as u8;
    let cond = Cond::from_u8((cc << 2) | (((cc & 1) ^ ((cc >> 1) & 1)) << 1));
    let d = to_ext_reg(d_bit, vd, sz);
    let n = to_ext_reg(n_bit, vn, sz);
    let m = to_ext_reg(m_bit, vm, sz);
    (d, n, m, sz, cond)
}

/// Extract VFP core-register move fields shared by the upstream
/// `vfp_VMOV_{u32,f64}_{f64,u32}` / `vfp_VMOV_{u32,f32}_{f32,u32}` helpers.
fn vfp_core_move_fields(inst: &DecodedArm) -> (u32, Reg, bool) {
    let vn = (inst.raw >> 16) & 0xF;
    let rt = inst.rt();
    let n_bit = (inst.raw >> 7) & 1 != 0;
    (vn, rt, n_bit)
}

fn get_ext_value(ir: &mut A32IREmitter, reg: ExtReg, sz: bool) -> Value {
    if sz {
        ir.get_extended_register_64(reg)
    } else {
        ir.get_extended_register_32(reg)
    }
}

fn set_ext_value(ir: &mut A32IREmitter, reg: ExtReg, sz: bool, value: Value) {
    if sz {
        ir.set_extended_register_64(reg, value);
    } else {
        ir.set_extended_register_32(reg, value);
    }
}

// Port of dynarmic's EmitVfpVectorOperation for the currently implemented scalar/vector subset.
fn emit_vfp_vector_operation<F>(
    ir: &mut A32IREmitter,
    sz: bool,
    mut d: ExtReg,
    mut n: ExtReg,
    mut m: ExtReg,
    mut f: F,
) -> bool
where
    F: FnMut(&mut A32IREmitter, ExtReg, ExtReg, ExtReg, bool),
{
    let Some(location) = ir.current_location else {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        return true;
    };
    let fpscr = location.fpscr();

    let Some(vector_stride) = fpscr.stride() else {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        return true;
    };

    let register_bank_size = if sz { 4usize } else { 8usize };
    let mut vector_length = fpscr.len();

    if vector_stride * vector_length > register_bank_size {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        return true;
    }

    if vector_length == 1 {
        if vector_stride != 1 {
            ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
            return true;
        }
        f(ir, d, n, m, sz);
        return true;
    }

    let bank_increment = |reg: ExtReg, stride: usize| -> ExtReg {
        let reg_number = reg.index();
        let bank_index = reg_number % register_bank_size;
        let bank_start = reg_number - bank_index;
        let next_reg_number = bank_start + ((bank_index + stride) % register_bank_size);
        if sz {
            ExtReg::from_double(next_reg_number as u8)
        } else {
            ExtReg::from_single(next_reg_number as u8)
        }
    };

    let belongs_to_scalar_bank = |reg: ExtReg| -> bool {
        matches!(
            reg,
            ExtReg::D0
                | ExtReg::D1
                | ExtReg::D2
                | ExtReg::D3
                | ExtReg::D16
                | ExtReg::D17
                | ExtReg::D18
                | ExtReg::D19
        ) || matches!(
            reg,
            ExtReg::S0
                | ExtReg::S1
                | ExtReg::S2
                | ExtReg::S3
                | ExtReg::S4
                | ExtReg::S5
                | ExtReg::S6
                | ExtReg::S7
        )
    };

    let d_is_scalar = belongs_to_scalar_bank(d);
    let m_is_scalar = belongs_to_scalar_bank(m);

    if d_is_scalar {
        vector_length = 1;
    }

    for _ in 0..vector_length {
        f(ir, d, n, m, sz);
        d = bank_increment(d, vector_stride);
        n = bank_increment(n, vector_stride);
        if !m_is_scalar {
            m = bank_increment(m, vector_stride);
        }
    }

    true
}

pub fn arm_vadd_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, n, m, sz) = ternary_src_dst(inst);
    emit_vfp_vector_operation(ir, sz, d, n, m, |ir, d, n, m, sz| {
        let reg_n = get_ext_value(ir, n, sz);
        let reg_m = get_ext_value(ir, m, sz);
        let result = ir.ir().fp_add(if sz { 64 } else { 32 }, reg_n, reg_m);
        set_ext_value(ir, d, sz, result);
    })
}

pub fn arm_vsub_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, n, m, sz) = ternary_src_dst(inst);
    emit_vfp_vector_operation(ir, sz, d, n, m, |ir, d, n, m, sz| {
        let reg_n = get_ext_value(ir, n, sz);
        let reg_m = get_ext_value(ir, m, sz);
        let result = ir.ir().fp_sub(if sz { 64 } else { 32 }, reg_n, reg_m);
        set_ext_value(ir, d, sz, result);
    })
}

pub fn arm_vmul_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, n, m, sz) = ternary_src_dst(inst);
    emit_vfp_vector_operation(ir, sz, d, n, m, |ir, d, n, m, sz| {
        let reg_n = get_ext_value(ir, n, sz);
        let reg_m = get_ext_value(ir, m, sz);
        let result = ir.ir().fp_mul(if sz { 64 } else { 32 }, reg_n, reg_m);
        set_ext_value(ir, d, sz, result);
    })
}

pub fn arm_vmla_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, n, m, sz) = ternary_src_dst(inst);
    emit_vfp_vector_operation(ir, sz, d, n, m, |ir, d, n, m, sz| {
        let reg_n = get_ext_value(ir, n, sz);
        let reg_m = get_ext_value(ir, m, sz);
        let reg_d = get_ext_value(ir, d, sz);
        let mul = ir.ir().fp_mul(if sz { 64 } else { 32 }, reg_n, reg_m);
        let result = ir.ir().fp_add(if sz { 64 } else { 32 }, reg_d, mul);
        set_ext_value(ir, d, sz, result);
    })
}

pub fn arm_vmls_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, n, m, sz) = ternary_src_dst(inst);
    emit_vfp_vector_operation(ir, sz, d, n, m, |ir, d, n, m, sz| {
        let reg_n = get_ext_value(ir, n, sz);
        let reg_m = get_ext_value(ir, m, sz);
        let reg_d = get_ext_value(ir, d, sz);
        let mul = ir.ir().fp_mul(if sz { 64 } else { 32 }, reg_n, reg_m);
        let neg_mul = ir.ir().fp_neg(if sz { 64 } else { 32 }, mul);
        let result = ir.ir().fp_add(if sz { 64 } else { 32 }, reg_d, neg_mul);
        set_ext_value(ir, d, sz, result);
    })
}

pub fn arm_vnmul_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, n, m, sz) = ternary_src_dst(inst);
    emit_vfp_vector_operation(ir, sz, d, n, m, |ir, d, n, m, sz| {
        let reg_n = get_ext_value(ir, n, sz);
        let reg_m = get_ext_value(ir, m, sz);
        let mul = ir.ir().fp_mul(if sz { 64 } else { 32 }, reg_n, reg_m);
        let result = ir.ir().fp_neg(if sz { 64 } else { 32 }, mul);
        set_ext_value(ir, d, sz, result);
    })
}

pub fn arm_vnmla_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, n, m, sz) = ternary_src_dst(inst);
    emit_vfp_vector_operation(ir, sz, d, n, m, |ir, d, n, m, sz| {
        let reg_n = get_ext_value(ir, n, sz);
        let reg_m = get_ext_value(ir, m, sz);
        let reg_d = get_ext_value(ir, d, sz);
        let neg_d = ir.ir().fp_neg(if sz { 64 } else { 32 }, reg_d);
        let mul = ir.ir().fp_mul(if sz { 64 } else { 32 }, reg_n, reg_m);
        let neg_mul = ir.ir().fp_neg(if sz { 64 } else { 32 }, mul);
        let result = ir.ir().fp_add(if sz { 64 } else { 32 }, neg_d, neg_mul);
        set_ext_value(ir, d, sz, result);
    })
}

pub fn arm_vnmls_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, n, m, sz) = ternary_src_dst(inst);
    emit_vfp_vector_operation(ir, sz, d, n, m, |ir, d, n, m, sz| {
        let reg_n = get_ext_value(ir, n, sz);
        let reg_m = get_ext_value(ir, m, sz);
        let reg_d = get_ext_value(ir, d, sz);
        let neg_d = ir.ir().fp_neg(if sz { 64 } else { 32 }, reg_d);
        let mul = ir.ir().fp_mul(if sz { 64 } else { 32 }, reg_n, reg_m);
        let result = ir.ir().fp_add(if sz { 64 } else { 32 }, neg_d, mul);
        set_ext_value(ir, d, sz, result);
    })
}

pub fn arm_vdiv_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, n, m, sz) = ternary_src_dst(inst);
    emit_vfp_vector_operation(ir, sz, d, n, m, |ir, d, n, m, sz| {
        let reg_n = get_ext_value(ir, n, sz);
        let reg_m = get_ext_value(ir, m, sz);
        let result = ir.ir().fp_div(if sz { 64 } else { 32 }, reg_n, reg_m);
        set_ext_value(ir, d, sz, result);
    })
}

// VFPv4 fused multiply-accumulate (single-rounding). Port of upstream
// `TranslatorVisitor::vfp_VFMA/VFMS/VFNMA/VFNMS` in vfp.cpp. These differ from
// VMLA/VMLS/VNMLA/VNMLS in that the multiply and add are a SINGLE rounding step
// (`fp_mul_add`/`fp_mul_sub`), not two separately-rounded ops. Compilers lower
// matrix math (`-mfpu=neon-vfpv4`) to these, so omitting them corrupts guest
// transform math — they previously fell through to CDP (no-op).

// VFMA: Sd = Sd + (Sn * Sm)  — fused.
pub fn arm_vfma_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, n, m, sz) = ternary_src_dst(inst);
    emit_vfp_vector_operation(ir, sz, d, n, m, |ir, d, n, m, sz| {
        let reg_n = get_ext_value(ir, n, sz);
        let reg_m = get_ext_value(ir, m, sz);
        let reg_d = get_ext_value(ir, d, sz);
        let result = ir
            .ir()
            .fp_mul_add(if sz { 64 } else { 32 }, reg_d, reg_n, reg_m);
        set_ext_value(ir, d, sz, result);
    })
}

// VFMS: Sd = Sd - (Sn * Sm)  — fused.
pub fn arm_vfms_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, n, m, sz) = ternary_src_dst(inst);
    emit_vfp_vector_operation(ir, sz, d, n, m, |ir, d, n, m, sz| {
        let reg_n = get_ext_value(ir, n, sz);
        let reg_m = get_ext_value(ir, m, sz);
        let reg_d = get_ext_value(ir, d, sz);
        let result = ir
            .ir()
            .fp_mul_sub(if sz { 64 } else { 32 }, reg_d, reg_n, reg_m);
        set_ext_value(ir, d, sz, result);
    })
}

// VFNMS: Sd = -Sd + (Sn * Sm)  — fused.
pub fn arm_vfnms_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, n, m, sz) = ternary_src_dst(inst);
    emit_vfp_vector_operation(ir, sz, d, n, m, |ir, d, n, m, sz| {
        let reg_n = get_ext_value(ir, n, sz);
        let reg_m = get_ext_value(ir, m, sz);
        let reg_d = get_ext_value(ir, d, sz);
        let neg_d = ir.ir().fp_neg(if sz { 64 } else { 32 }, reg_d);
        let result = ir
            .ir()
            .fp_mul_add(if sz { 64 } else { 32 }, neg_d, reg_n, reg_m);
        set_ext_value(ir, d, sz, result);
    })
}

// VFNMA: Sd = -Sd - (Sn * Sm)  — fused.
pub fn arm_vfnma_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, n, m, sz) = ternary_src_dst(inst);
    emit_vfp_vector_operation(ir, sz, d, n, m, |ir, d, n, m, sz| {
        let reg_n = get_ext_value(ir, n, sz);
        let reg_m = get_ext_value(ir, m, sz);
        let reg_d = get_ext_value(ir, d, sz);
        let neg_d = ir.ir().fp_neg(if sz { 64 } else { 32 }, reg_d);
        let result = ir
            .ir()
            .fp_mul_sub(if sz { 64 } else { 32 }, neg_d, reg_n, reg_m);
        set_ext_value(ir, d, sz, result);
    })
}

pub fn arm_vsel_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, n, m, sz, cond) = vfp_vsel_fields(inst);
    emit_vfp_vector_operation(ir, sz, d, n, m, |ir, d, n, m, sz| {
        let reg_n = get_ext_value(ir, n, sz);
        let reg_m = get_ext_value(ir, m, sz);
        let result = if sz {
            ir.ir()
                .conditional_select_64(Value::from(cond), reg_n, reg_m)
        } else {
            ir.ir()
                .conditional_select_32(Value::from(cond), reg_n, reg_m)
        };
        set_ext_value(ir, d, sz, result);
    })
}

pub fn arm_vmaxnm_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, n, m, sz) = ternary_src_dst(inst);
    emit_vfp_vector_operation(ir, sz, d, n, m, |ir, d, n, m, sz| {
        let reg_n = get_ext_value(ir, n, sz);
        let reg_m = get_ext_value(ir, m, sz);
        let result = ir
            .ir()
            .fp_max_numeric(if sz { 64 } else { 32 }, reg_n, reg_m);
        set_ext_value(ir, d, sz, result);
    })
}

pub fn arm_vminnm_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, n, m, sz) = ternary_src_dst(inst);
    emit_vfp_vector_operation(ir, sz, d, n, m, |ir, d, n, m, sz| {
        let reg_n = get_ext_value(ir, n, sz);
        let reg_m = get_ext_value(ir, m, sz);
        let result = ir
            .ir()
            .fp_min_numeric(if sz { 64 } else { 32 }, reg_n, reg_m);
        set_ext_value(ir, d, sz, result);
    })
}

pub fn arm_vmov_fp_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, m, sz) = unary_src_dst(inst);
    if sz {
        let value = ir.get_extended_register_64(m);
        ir.set_extended_register_64(d, value);
    } else {
        let value = ir.get_extended_register_32(m);
        ir.set_extended_register_32(d, value);
    }
    true
}

// VMOV<c>.32 <Dd[0]>, <Rt>
pub fn arm_vmov_u32_f64(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (vd, rt, d_bit) = vfp_core_move_fields(inst);
    if rt == Reg::R15 {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        return true;
    }

    let d = to_ext_reg(d_bit, vd, true);
    let reg_d = ir.get_extended_register_64(d);
    let reg_t = ir.get_register(rt);
    let hi = ir.ir().most_significant_word(reg_d);
    let result = ir.ir().pack_2x32_to_1x64(reg_t, hi);
    ir.set_extended_register_64(d, result);
    true
}

// VMOV<c>.32 <Rt>, <Dn[0]>
pub fn arm_vmov_f64_u32(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (vn, rt, n_bit) = vfp_core_move_fields(inst);
    if rt == Reg::R15 {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        return true;
    }

    let n = to_ext_reg(n_bit, vn, true);
    let reg_n = ir.get_extended_register_64(n);
    let lo = ir.ir().least_significant_word(reg_n);
    ir.set_register(rt, lo);
    true
}

// VMOV<c> <Sn>, <Rt>
pub fn arm_vmov_u32_f32(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (vn, rt, n_bit) = vfp_core_move_fields(inst);
    if rt == Reg::R15 {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        return true;
    }

    let n = to_ext_reg(n_bit, vn, false);
    let reg_t = ir.get_register(rt);
    ir.set_extended_register_32(n, reg_t);
    true
}

// VMOV<c> <Rt>, <Sn>
pub fn arm_vmov_f32_u32(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (vn, rt, n_bit) = vfp_core_move_fields(inst);
    if rt == Reg::R15 {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        return true;
    }

    let n = to_ext_reg(n_bit, vn, false);
    let reg_n = ir.get_extended_register_32(n);
    ir.set_register(rt, reg_n);
    true
}

fn vfp_two_word_move_fields(raw: u32, sz: bool) -> (Reg, Reg, ExtReg) {
    let t2 = Reg::from_u32((raw >> 16) & 0xF);
    let t = Reg::from_u32((raw >> 12) & 0xF);
    let m_bit = raw & (1 << 5) != 0;
    let vm = raw & 0xF;
    (t2, t, to_ext_reg(m_bit, vm, sz))
}

// VMOV<c> <Sm>, <Sm1>, <Rt>, <Rt2>
pub fn vfp_vmov_2u32_2f32(ir: &mut A32IREmitter, raw: u32) -> bool {
    let (t2, t, m) = vfp_two_word_move_fields(raw, false);
    if t == Reg::R15 || t2 == Reg::R15 || m == ExtReg::S31 {
        return super::unpredictable_instruction(ir);
    }

    let word1 = ir.get_register(t);
    let word2 = ir.get_register(t2);
    ir.set_extended_register_32(m, word1);
    ir.set_extended_register_32(advance_ext_reg(m, 1), word2);
    true
}

// VMOV<c> <Rt>, <Rt2>, <Sm>, <Sm1>
pub fn vfp_vmov_2f32_2u32(ir: &mut A32IREmitter, raw: u32) -> bool {
    let (t2, t, m) = vfp_two_word_move_fields(raw, false);
    if t == Reg::R15 || t2 == Reg::R15 || m == ExtReg::S31 || t == t2 {
        return super::unpredictable_instruction(ir);
    }

    let word1 = ir.get_extended_register_32(m);
    let word2 = ir.get_extended_register_32(advance_ext_reg(m, 1));
    ir.set_register(t, word1);
    ir.set_register(t2, word2);
    true
}

// VMOV<c> <Dm>, <Rt>, <Rt2>
pub fn vfp_vmov_2u32_f64(ir: &mut A32IREmitter, raw: u32) -> bool {
    let (t2, t, m) = vfp_two_word_move_fields(raw, true);
    if t == Reg::R15 || t2 == Reg::R15 || m == ExtReg::S31 {
        return super::unpredictable_instruction(ir);
    }

    let word1 = ir.get_register(t);
    let word2 = ir.get_register(t2);
    let value = ir.ir().pack_2x32_to_1x64(word1, word2);
    ir.set_extended_register_64(m, value);
    true
}

// VMOV<c> <Rt>, <Rt2>, <Dm>
pub fn vfp_vmov_f64_2u32(ir: &mut A32IREmitter, raw: u32) -> bool {
    let (t2, t, m) = vfp_two_word_move_fields(raw, true);
    if t == Reg::R15 || t2 == Reg::R15 || m == ExtReg::S31 || t == t2 {
        return super::unpredictable_instruction(ir);
    }

    let value = ir.get_extended_register_64(m);
    let word1 = ir.ir().least_significant_word(value);
    let word2 = ir.ir().most_significant_word(value);
    ir.set_register(t, word1);
    ir.set_register(t2, word2);
    true
}

// VMSR FPSCR, <Rt>
pub fn vfp_vmsr(ir: &mut A32IREmitter, raw: u32) -> bool {
    let t = Reg::from_u32((raw >> 12) & 0xF);
    if t == Reg::R15 {
        return super::unpredictable_instruction(ir);
    }

    let next_location = ir
        .current_location
        .expect("current_location not set")
        .advance_pc(4)
        .advance_it();
    ir.base.push_rsb(next_location.into());
    ir.update_upper_location_descriptor();
    let value = ir.get_register(t);
    ir.set_fpscr(value);
    ir.branch_write_pc(Value::ImmU32(next_location.pc()));
    ir.set_term(Terminal::PopRSBHint);
    false
}

// VMRS <Rt>, FPSCR
pub fn vfp_vmrs(ir: &mut A32IREmitter, raw: u32) -> bool {
    let t = Reg::from_u32((raw >> 12) & 0xF);
    if t == Reg::R15 {
        let nzcv = ir.get_fpscr_nzcv();
        ir.set_cpsr_nzcv_raw(nzcv);
    } else {
        let value = ir.get_fpscr();
        ir.set_register(t, value);
    }
    true
}

// VMOV<c>.32 <Dn[x]>, <Rt>
pub fn arm_vmov_from_i32(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let index = ((inst.raw >> 21) & 1) as u8;
    let vd = (inst.raw >> 16) & 0xF;
    let rt = inst.rt();
    let d_bit = (inst.raw >> 7) & 1 != 0;

    if rt == Reg::R15 {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        return true;
    }

    let d = match crate::frontend::a32::translate::asimd::to_vector_reg(false, d_bit, vd) {
        Some(r) => r,
        None => return false,
    };
    let reg_d = ir.get_vector(d);
    let scalar = ir.get_register(rt);
    let result = ir.ir().vector_set_element(32, reg_d, index, scalar);
    ir.set_vector(d, result);
    true
}

// VMOV<c>.32 <Rt>, <Dn[x]>
pub fn arm_vmov_to_i32(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let index = ((inst.raw >> 21) & 1) as u8;
    let vn = (inst.raw >> 16) & 0xF;
    let rt = inst.rt();
    let n_bit = (inst.raw >> 7) & 1 != 0;

    if rt == Reg::R15 {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        return true;
    }

    let n = match crate::frontend::a32::translate::asimd::to_vector_reg(false, n_bit, vn) {
        Some(r) => r,
        None => return false,
    };
    let reg_n = ir.get_vector(n);
    let result = ir.ir().vector_get_element(32, reg_n, index);
    ir.set_register(rt, result);
    true
}

// VDUP<c>.{8,16,32} <Qd>, <Rt>
// VDUP<c>.{8,16,32} <Dd>, <Rt>
pub fn arm_vdup(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let b = ((inst.raw >> 22) & 1) != 0;
    let q = ((inst.raw >> 21) & 1) != 0;
    let vd = (inst.raw >> 16) & 0xF;
    let rt = inst.rt();
    let d_bit = ((inst.raw >> 7) & 1) != 0;
    let e = (inst.raw >> 5) & 1;

    if q && (vd & 1) != 0 {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        return true;
    }
    if rt == Reg::R15 {
        ir.exception_raised(crate::frontend::a32::types::Exception::UnpredictableInstruction);
        return true;
    }

    let d = match crate::frontend::a32::translate::asimd::to_vector_reg(q, d_bit, vd) {
        Some(r) => r,
        None => return false,
    };
    let be = ((b as u32) << 1) | e;
    if be == 0b11 {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        return true;
    }

    let esize = 32usize >> be;
    let reg_t = ir.get_register(rt);
    let scalar = match esize {
        8 => ir.ir().least_significant_byte(reg_t),
        16 => ir.ir().least_significant_half(reg_t),
        32 => reg_t,
        _ => unreachable!(),
    };
    let result = ir.ir().vector_broadcast(esize, scalar);
    ir.set_vector(d, result);
    true
}

pub fn arm_vmov_fp_imm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let d_bit = (inst.raw >> 22) & 1 != 0;
    let imm4h = (inst.raw >> 16) & 0xF;
    let vd = (inst.raw >> 12) & 0xF;
    let sz = (inst.raw >> 8) & 1 != 0;
    let imm4l = inst.raw & 0xF;

    let Some(location) = ir.current_location else {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    };
    let fpscr = location.fpscr();
    if fpscr.stride() != Some(1) || fpscr.len() != 1 {
        ir.exception_raised(crate::frontend::a32::types::Exception::UnpredictableInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }

    let d = to_ext_reg(d_bit, vd, sz);
    let imm8 = (imm4h << 4) | imm4l;

    if sz {
        let sign = ((imm8 >> 7) & 1) as u64;
        let exp = (if (imm8 >> 6) & 1 != 0 {
            0x3FCu64
        } else {
            0x400u64
        }) | (((imm8 >> 4) & 0x3) as u64);
        let fract = ((imm8 & 0xF) as u64) << 48;
        let immediate = (sign << 63) | (exp << 52) | fract;
        ir.set_extended_register_64(d, Value::ImmU64(immediate));
    } else {
        let sign = ((imm8 >> 7) & 1) as u32;
        let exp = (if (imm8 >> 6) & 1 != 0 {
            0x7Cu32
        } else {
            0x80u32
        }) | ((imm8 >> 4) & 0x3);
        let fract = (imm8 & 0xF) << 19;
        let immediate = (sign << 31) | (exp << 23) | fract;
        ir.set_extended_register_32(d, Value::ImmU32(immediate));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::decoder::ArmInstId;
    use crate::frontend::a32::types::ExtReg;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::value::Value;

    #[test]
    fn vdup_from_core_uses_vd_bits_not_rt_bits() {
        // EE A0 4B 90 = vdup.32 q8, r4.
        // Encoding ownership is upstream `decoder/vfp.inc`:
        // cccc11101BQ0ddddtttt1011D0E10000
        // where Vd is bits[19:16] and Rt is bits[15:12]. A previous port
        // used bits[15:12] for both, writing Q12 instead of Q8.
        let loc = A32LocationDescriptor::at(0x2000);
        let mut block = Block::new(loc.to_location());
        let ok = {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            arm_vdup(
                &mut ir,
                &DecodedArm {
                    raw: 0xEEA0_4B90,
                    id: ArmInstId::VFP_VDUP,
                },
            )
        };
        assert!(ok);

        let set_vector = block
            .instructions
            .iter()
            .find(|inst| inst.opcode == Opcode::A32SetVector)
            .expect("VDUP should write a vector register");
        assert_eq!(set_vector.args[0], Value::ImmA32ExtReg(ExtReg::Q8));
    }

    #[test]
    fn vcvt_to_u32_uses_unary_vd_bits() {
        // EE BC 0B C8 = vcvt.u32.f64 s0, d8.
        // For VFP unary data-processing, upstream `Vd` is bits[15:12];
        // bits[19:16] are opcode bits and must not select the destination.
        let loc = A32LocationDescriptor::at(0x2000);
        let mut block = Block::new(loc.to_location());
        let ok = {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            arm_vcvt_to_u32(
                &mut ir,
                &DecodedArm {
                    raw: 0xEEBC_0BC8,
                    id: ArmInstId::VCVT_to_u32,
                },
            )
        };
        assert!(ok);

        let set_ext = block
            .instructions
            .iter()
            .find(|inst| inst.opcode == Opcode::A32SetExtendedRegister32)
            .expect("VCVT.U32.F64 should write a single register");
        assert_eq!(set_ext.args[0], Value::ImmA32ExtReg(ExtReg::S0));
    }
}

pub fn arm_vabs_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, m, sz) = unary_src_dst(inst);
    if sz {
        let value = ir.get_extended_register_64(m);
        let result = ir.ir().fp_abs(64, value);
        ir.set_extended_register_64(d, result);
    } else {
        let value = ir.get_extended_register_32(m);
        let result = ir.ir().fp_abs(32, value);
        ir.set_extended_register_32(d, result);
    }
    true
}

pub fn arm_vneg_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, m, sz) = unary_src_dst(inst);
    if sz {
        let value = ir.get_extended_register_64(m);
        let result = ir.ir().fp_neg(64, value);
        ir.set_extended_register_64(d, result);
    } else {
        let value = ir.get_extended_register_32(m);
        let result = ir.ir().fp_neg(32, value);
        ir.set_extended_register_32(d, result);
    }
    true
}

pub fn arm_vsqrt_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, m, sz) = unary_src_dst(inst);
    if sz {
        let value = ir.get_extended_register_64(m);
        let result = ir.ir().fp_sqrt(64, value);
        ir.set_extended_register_64(d, result);
    } else {
        let value = ir.get_extended_register_32(m);
        let result = ir.ir().fp_sqrt(32, value);
        ir.set_extended_register_32(d, result);
    }
    true
}

// VCMP{E}.F32 <Sd>, <Sm>
// VCMP{E}.F64 <Dd>, <Dm>
pub fn arm_vcmp_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d, m, sz) = unary_src_dst(inst);
    let exc_on_qnan = Value::ImmU1((inst.raw >> 7) & 1 != 0);

    if sz {
        let reg_d = ir.get_extended_register_64(d);
        let reg_m = ir.get_extended_register_64(m);
        let nzcv = ir.ir().fp_compare(64, reg_d, reg_m, exc_on_qnan);
        ir.set_fpscr_nzcv(nzcv);
    } else {
        let reg_d = ir.get_extended_register_32(d);
        let reg_m = ir.get_extended_register_32(m);
        let nzcv = ir.ir().fp_compare(32, reg_d, reg_m, exc_on_qnan);
        ir.set_fpscr_nzcv(nzcv);
    }
    true
}

// VCMP{E}.F32 <Sd>, #0.0
// VCMP{E}.F64 <Dd>, #0.0
pub fn arm_vcmp_zero_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d_bit, vd, sz, _, _) = vfp_unary_fields(inst);
    let d = to_ext_reg(d_bit, vd, sz);
    let exc_on_qnan = Value::ImmU1((inst.raw >> 7) & 1 != 0);

    if sz {
        let reg_d = ir.get_extended_register_64(d);
        let nzcv = ir.ir().fp_compare(64, reg_d, Value::ImmU64(0), exc_on_qnan);
        ir.set_fpscr_nzcv(nzcv);
    } else {
        let reg_d = ir.get_extended_register_32(d);
        let nzcv = ir.ir().fp_compare(32, reg_d, Value::ImmU32(0), exc_on_qnan);
        ir.set_fpscr_nzcv(nzcv);
    }
    true
}

// VCVT<c>.F64.F32 <Dd>, <Sm>
// VCVT<c>.F32.F64 <Sd>, <Dm>
pub fn arm_vcvt_f_to_f(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d_bit, vd, sz, m_bit, vm) = vfp_unary_fields(inst);
    let d = to_ext_reg(d_bit, vd, !sz);
    let m = to_ext_reg(m_bit, vm, sz);
    let rounding_mode = ir
        .current_location
        .expect("current_location not set")
        .fpscr()
        .rmode() as u8;

    if sz {
        let reg_m = ir.get_extended_register_64(m);
        let result = ir.ir().fp_double_to_single(reg_m, rounding_mode);
        ir.set_extended_register_32(d, result);
    } else {
        let reg_m = ir.get_extended_register_32(m);
        let result = ir.ir().fp_single_to_double(reg_m, rounding_mode);
        ir.set_extended_register_64(d, result);
    }
    true
}

// VCVT.F32.{S32,U32} <Sd>, <Sm>
// VCVT.F64.{S32,U32} <Dd>, <Dm>
pub fn arm_vcvt_from_int(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d_bit, vd, sz, m_bit, vm) = vfp_unary_fields(inst);
    let d = to_ext_reg(d_bit, vd, sz);
    let m = to_ext_reg(m_bit, vm, false);
    let is_signed = (inst.raw >> 7) & 1 != 0;
    let rounding_mode = ir
        .current_location
        .expect("current_location not set")
        .fpscr()
        .rmode() as u8;

    let reg_m = ir.get_extended_register_32(m);
    if sz {
        let result = ir
            .ir()
            .fp_fixed_to_double(reg_m, 32, is_signed, 0, rounding_mode);
        ir.set_extended_register_64(d, result);
    } else {
        let result = ir
            .ir()
            .fp_fixed_to_single(reg_m, 32, is_signed, 0, rounding_mode);
        ir.set_extended_register_32(d, result);
    }
    true
}

// VCVT{,R}.U32.F32 <Sd>, <Sm>
// VCVT{,R}.U32.F64 <Sd>, <Dm>
pub fn arm_vcvt_to_u32(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d_bit, vd, sz, m_bit, vm) = vfp_unary_fields(inst);
    let d = to_ext_reg(d_bit, vd, false);
    let m = to_ext_reg(m_bit, vm, sz);
    let round_towards_zero = (inst.raw >> 7) & 1 != 0;
    let rounding_mode = if round_towards_zero {
        3
    } else {
        ir.current_location
            .expect("current_location not set")
            .fpscr()
            .rmode() as u8
    };

    let result = if sz {
        let reg_m = ir.get_extended_register_64(m);
        ir.ir().fp_to_fixed_u32(reg_m, 64, 0, rounding_mode)
    } else {
        let reg_m = ir.get_extended_register_32(m);
        ir.ir().fp_to_fixed_u32(reg_m, 32, 0, rounding_mode)
    };
    ir.set_extended_register_32(d, result);
    true
}

// VCVT{,R}.S32.F32 <Sd>, <Sm>
// VCVT{,R}.S32.F64 <Sd>, <Dm>
pub fn arm_vcvt_to_s32(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d_bit, vd, sz, m_bit, vm) = vfp_unary_fields(inst);
    let d = to_ext_reg(d_bit, vd, false);
    let m = to_ext_reg(m_bit, vm, sz);
    let round_towards_zero = (inst.raw >> 7) & 1 != 0;
    let rounding_mode = if round_towards_zero {
        3
    } else {
        ir.current_location
            .expect("current_location not set")
            .fpscr()
            .rmode() as u8
    };

    let result = if sz {
        let reg_m = ir.get_extended_register_64(m);
        ir.ir().fp_to_fixed_s32(reg_m, 64, 0, rounding_mode)
    } else {
        let reg_m = ir.get_extended_register_32(m);
        ir.ir().fp_to_fixed_s32(reg_m, 32, 0, rounding_mode)
    };
    ir.set_extended_register_32(d, result);
    true
}

// ---------------------------------------------------------------------------
// VPUSH
// ---------------------------------------------------------------------------

pub fn arm_vpush(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d_bit, vd, sz, imm8) = vfp_fields(inst);
    let regs = reg_count(sz, imm8);
    if regs == 0 {
        return true;
    }

    let imm32 = imm8 * 4;
    let sp = ir.get_register(Reg::R13);
    let new_sp = ir.ir().sub_32(sp, Value::ImmU32(imm32), Value::ImmU1(true));
    ir.set_register(Reg::R13, new_sp);

    let d = to_ext_reg(d_bit, vd, sz);
    let mut addr = new_sp;

    for i in 0..regs {
        let reg = advance_ext_reg(d, i);
        if sz {
            let val = ir.get_extended_register_64(reg);
            let lo = ir.ir().least_significant_word(val);
            let hi = ir.ir().most_significant_word(val);
            ir.write_memory_32(addr, lo, AccType::Normal);
            addr = ir.ir().add_32(addr, Value::ImmU32(4), Value::ImmU1(false));
            ir.write_memory_32(addr, hi, AccType::Normal);
            addr = ir.ir().add_32(addr, Value::ImmU32(4), Value::ImmU1(false));
        } else {
            let val = ir.get_extended_register_32(reg);
            ir.write_memory_32(addr, val, AccType::Normal);
            addr = ir.ir().add_32(addr, Value::ImmU32(4), Value::ImmU1(false));
        }
    }

    true
}

// ---------------------------------------------------------------------------
// VPOP
// ---------------------------------------------------------------------------

pub fn arm_vpop(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d_bit, vd, sz, imm8) = vfp_fields(inst);
    let regs = reg_count(sz, imm8);
    if regs == 0 {
        return true;
    }

    let imm32 = imm8 * 4;
    let sp = ir.get_register(Reg::R13);
    let d = to_ext_reg(d_bit, vd, sz);
    let mut addr = sp;

    for i in 0..regs {
        let reg = advance_ext_reg(d, i);
        if sz {
            let lo = ir.read_memory_32(addr, AccType::Normal);
            addr = ir.ir().add_32(addr, Value::ImmU32(4), Value::ImmU1(false));
            let hi = ir.read_memory_32(addr, AccType::Normal);
            addr = ir.ir().add_32(addr, Value::ImmU32(4), Value::ImmU1(false));
            let val = ir.ir().pack_2x32_to_1x64(lo, hi);
            ir.set_extended_register_64(reg, val);
        } else {
            let val = ir.read_memory_32(addr, AccType::Normal);
            ir.set_extended_register_32(reg, val);
            addr = ir.ir().add_32(addr, Value::ImmU32(4), Value::ImmU1(false));
        }
    }

    let new_sp = ir
        .ir()
        .add_32(sp, Value::ImmU32(imm32), Value::ImmU1(false));
    ir.set_register(Reg::R13, new_sp);
    true
}

// ---------------------------------------------------------------------------
// VLDR (floating-point)
// ---------------------------------------------------------------------------

pub fn arm_vldr_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d_bit, vd, sz, imm8) = vfp_fields(inst);
    let u = (inst.raw >> 23) & 1 != 0;
    let rn = inst.rn();

    let imm32 = imm8 * 4;
    let base = if rn == Reg::R15 {
        let loc = ir.current_location.expect("location not set");
        let pc_aligned = loc.pc().wrapping_add(8) & !3;
        Value::ImmU32(pc_aligned)
    } else {
        ir.get_register(rn)
    };

    let address = if u {
        ir.ir()
            .add_32(base, Value::ImmU32(imm32), Value::ImmU1(false))
    } else {
        ir.ir()
            .sub_32(base, Value::ImmU32(imm32), Value::ImmU1(true))
    };

    let d = to_ext_reg(d_bit, vd, sz);

    if sz {
        let lo = ir.read_memory_32(address, AccType::Normal);
        let hi_addr = ir
            .ir()
            .add_32(address, Value::ImmU32(4), Value::ImmU1(false));
        let hi = ir.read_memory_32(hi_addr, AccType::Normal);
        let val = ir.ir().pack_2x32_to_1x64(lo, hi);
        ir.set_extended_register_64(d, val);
    } else {
        let val = ir.read_memory_32(address, AccType::Normal);
        ir.set_extended_register_32(d, val);
    }

    true
}

// ---------------------------------------------------------------------------
// VSTR (floating-point)
// ---------------------------------------------------------------------------

pub fn arm_vstr_fp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d_bit, vd, sz, imm8) = vfp_fields(inst);
    let u = (inst.raw >> 23) & 1 != 0;
    let rn = inst.rn();

    let imm32 = imm8 * 4;
    let base = ir.get_register(rn);

    let address = if u {
        ir.ir()
            .add_32(base, Value::ImmU32(imm32), Value::ImmU1(false))
    } else {
        ir.ir()
            .sub_32(base, Value::ImmU32(imm32), Value::ImmU1(true))
    };

    let d = to_ext_reg(d_bit, vd, sz);

    if sz {
        let val = ir.get_extended_register_64(d);
        let lo = ir.ir().least_significant_word(val);
        let hi = ir.ir().most_significant_word(val);
        ir.write_memory_32(address, lo, AccType::Normal);
        let hi_addr = ir
            .ir()
            .add_32(address, Value::ImmU32(4), Value::ImmU1(false));
        ir.write_memory_32(hi_addr, hi, AccType::Normal);
    } else {
        let val = ir.get_extended_register_32(d);
        ir.write_memory_32(address, val, AccType::Normal);
    }

    true
}

// ---------------------------------------------------------------------------
// VSTM (floating-point store multiple)
// ---------------------------------------------------------------------------

pub fn arm_vstm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d_bit, vd, sz, imm8) = vfp_fields(inst);
    let u = (inst.raw >> 23) & 1 != 0;
    let w = (inst.raw >> 21) & 1 != 0;
    let rn = inst.rn();
    let regs = reg_count(sz, imm8);
    if regs == 0 {
        return true;
    }

    let imm32 = imm8 * 4;
    let base_val = ir.get_register(rn);
    let d = to_ext_reg(d_bit, vd, sz);

    let start = if u {
        base_val
    } else {
        ir.ir()
            .sub_32(base_val, Value::ImmU32(imm32), Value::ImmU1(true))
    };

    let mut addr = start;
    for i in 0..regs {
        let reg = advance_ext_reg(d, i);
        if sz {
            let val = ir.get_extended_register_64(reg);
            let lo = ir.ir().least_significant_word(val);
            let hi = ir.ir().most_significant_word(val);
            ir.write_memory_32(addr, lo, AccType::Normal);
            addr = ir.ir().add_32(addr, Value::ImmU32(4), Value::ImmU1(false));
            ir.write_memory_32(addr, hi, AccType::Normal);
            addr = ir.ir().add_32(addr, Value::ImmU32(4), Value::ImmU1(false));
        } else {
            let val = ir.get_extended_register_32(reg);
            ir.write_memory_32(addr, val, AccType::Normal);
            addr = ir.ir().add_32(addr, Value::ImmU32(4), Value::ImmU1(false));
        }
    }

    if w {
        let wb = if u {
            ir.ir()
                .add_32(base_val, Value::ImmU32(imm32), Value::ImmU1(false))
        } else {
            ir.ir()
                .sub_32(base_val, Value::ImmU32(imm32), Value::ImmU1(true))
        };
        ir.set_register(rn, wb);
    }

    true
}

// ---------------------------------------------------------------------------
// VLDM (floating-point load multiple)
// ---------------------------------------------------------------------------

pub fn arm_vldm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (d_bit, vd, sz, imm8) = vfp_fields(inst);
    let u = (inst.raw >> 23) & 1 != 0;
    let w = (inst.raw >> 21) & 1 != 0;
    let rn = inst.rn();
    let regs = reg_count(sz, imm8);
    if regs == 0 {
        return true;
    }

    let imm32 = imm8 * 4;
    let base_val = ir.get_register(rn);
    let d = to_ext_reg(d_bit, vd, sz);

    let start = if u {
        base_val
    } else {
        ir.ir()
            .sub_32(base_val, Value::ImmU32(imm32), Value::ImmU1(true))
    };

    let mut addr = start;
    for i in 0..regs {
        let reg = advance_ext_reg(d, i);
        if sz {
            let lo = ir.read_memory_32(addr, AccType::Normal);
            addr = ir.ir().add_32(addr, Value::ImmU32(4), Value::ImmU1(false));
            let hi = ir.read_memory_32(addr, AccType::Normal);
            addr = ir.ir().add_32(addr, Value::ImmU32(4), Value::ImmU1(false));
            let val = ir.ir().pack_2x32_to_1x64(lo, hi);
            ir.set_extended_register_64(reg, val);
        } else {
            let val = ir.read_memory_32(addr, AccType::Normal);
            ir.set_extended_register_32(reg, val);
            addr = ir.ir().add_32(addr, Value::ImmU32(4), Value::ImmU1(false));
        }
    }

    if w {
        let wb = if u {
            ir.ir()
                .add_32(base_val, Value::ImmU32(imm32), Value::ImmU1(false))
        } else {
            ir.ir()
                .sub_32(base_val, Value::ImmU32(imm32), Value::ImmU1(true))
        };
        ir.set_register(rn, wb);
    }

    true
}

/// VFP VRINT{A,N,P,M} — unconditional FP rounding.
/// Upstream: `vfp_VRINT_rm` in vfp.cpp.
/// Encoding: 111111101D1110mmdddd101z01M0mmmm
/// rm (bits 17:16): 00=VRINTA, 01=VRINTN, 10=VRINTP, 11=VRINTM
pub fn arm_vfp_vrint_rm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let d = (inst.raw >> 22) & 1 != 0;
    let rm = (inst.raw >> 16) & 0x3;
    let vd = (inst.raw >> 12) & 0xF;
    let sz = (inst.raw >> 8) & 1 != 0; // 0=F32, 1=F64
    let m = (inst.raw >> 5) & 1 != 0;
    let vm = inst.raw & 0xF;

    // Map rm to IR rounding mode values:
    // ARM rm: 00=VRINTA(tie away), 01=VRINTN(tie even), 10=VRINTP(ceil), 11=VRINTM(floor)
    // IR rounding: 0=nearest(tie even), 1=+inf, 2=-inf, 3=zero
    let rmode: u8 = match rm {
        0b00 => 0, // VRINTA: tie away → approximate as tie even
        0b01 => 0, // VRINTN: tie even
        0b10 => 1, // VRINTP: ceil
        0b11 => 2, // VRINTM: floor
        _ => unreachable!(),
    };

    if sz {
        // F64: D register = D:Vd
        let d_reg = ExtReg::from_double(((if d { 16 } else { 0 }) + vd) as u8);
        let m_reg = ExtReg::from_double(((if m { 16 } else { 0 }) + vm) as u8);
        let val = ir.get_extended_register_64(m_reg);
        let result = ir.ir().fp_round_int(64, val, rmode, false);
        ir.set_extended_register_64(d_reg, result);
    } else {
        // F32: S register = (Vd << 1) | D
        let d_idx = (vd << 1) | (if d { 1 } else { 0 });
        let m_idx = (vm << 1) | (if m { 1 } else { 0 });
        let d_reg = ExtReg::from_single(d_idx as u8);
        let m_reg = ExtReg::from_single(m_idx as u8);
        let val = ir.get_extended_register_32(m_reg);
        let result = ir.ir().fp_round_int(32, val, rmode, false);
        ir.set_extended_register_32(d_reg, result);
    }

    true
}

/// VFP VCVT{A,N,P,M} — unconditional FP to integer conversion with rounding mode.
/// Upstream: `vfp_VCVT_rm` in vfp.cpp.
/// Encoding: 111111101D1111mmdddd101zU1M0mmmm
/// rm (bits 17:16): 00=VCVTA, 01=VCVTN, 10=VCVTP, 11=VCVTM
/// U (bit 7): 0=unsigned, 1=signed (NOTE: upstream inverts: unsigned_ = !U)
pub fn arm_vfp_vcvt_rm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let d = (inst.raw >> 22) & 1 != 0;
    let rm = (inst.raw >> 16) & 0x3;
    let vd = (inst.raw >> 12) & 0xF;
    let sz = (inst.raw >> 8) & 1 != 0; // 0=F32 source, 1=F64 source
    let u_bit = (inst.raw >> 7) & 1 != 0;
    let m = (inst.raw >> 5) & 1 != 0;
    let vm = inst.raw & 0xF;

    // Upstream: const bool unsigned_ = !U;
    let is_unsigned = !u_bit;

    // Map rm to rounding mode (same as VRINT_rm)
    let rmode: u8 = match rm {
        0b00 => 0, // VCVTA: tie away → approximate as tie even
        0b01 => 0, // VCVTN: tie even
        0b10 => 1, // VCVTP: ceil
        0b11 => 2, // VCVTM: floor
        _ => unreachable!(),
    };

    // Destination is always S register (32-bit int result)
    let d_idx = (vd << 1) | (if d { 1 } else { 0 });
    let d_reg = ExtReg::from_single(d_idx as u8);

    let source_size = if sz { 64 } else { 32 };

    let val = if sz {
        // F64 source
        let m_reg = ExtReg::from_double(((if m { 16 } else { 0 }) + vm) as u8);
        ir.get_extended_register_64(m_reg)
    } else {
        // F32 source
        let m_idx = (vm << 1) | (if m { 1 } else { 0 });
        let m_reg = ExtReg::from_single(m_idx as u8);
        ir.get_extended_register_32(m_reg)
    };

    let result = if is_unsigned {
        ir.ir().fp_to_fixed_u32(val, source_size, 0, rmode)
    } else {
        ir.ir().fp_to_fixed_s32(val, source_size, 0, rmode)
    };

    ir.set_extended_register_32(d_reg, result);
    true
}

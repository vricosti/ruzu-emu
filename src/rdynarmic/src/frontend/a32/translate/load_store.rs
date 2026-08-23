use super::helpers::{emit_imm_shift, get_address};
use super::unpredictable_instruction;
use crate::frontend::a32::decoder::DecodedArm;
use crate::frontend::a32::types::Reg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::acc_type::AccType;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

// --- LDR ---

/// ARM `LDR <Rt>, [PC, #+/-<imm>]`.
///
/// Upstream owner: `TranslatorVisitor::arm_LDR_lit`.
pub fn arm_ldr_lit(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let base = ir.pc() & !3;
    let address = if inst.u_flag() {
        base.wrapping_add(inst.imm12())
    } else {
        base.wrapping_sub(inst.imm12())
    };
    let value = ir.read_memory_32(Value::ImmU32(address), AccType::Normal);

    if rt == Reg::R15 {
        ir.load_write_pc(value);
        ir.set_term(Terminal::FastDispatchHint);
        return false;
    }

    ir.set_register(rt, value);
    true
}

pub fn arm_ldr_imm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();
    let imm12 = inst.imm12();

    if rn == Reg::R15 {
        return unpredictable_instruction(ir);
    }
    assert!(!(!p && w), "T form of instruction unimplemented");
    if (!p || w) && rn == rt {
        return unpredictable_instruction(ir);
    }

    let offset = Value::ImmU32(imm12);
    let address = get_address(ir, p, u, w, rn, offset);

    let value = ir.read_memory_32(address, AccType::Normal);

    if rt == Reg::R15 {
        ir.load_write_pc(value);
        if !p && w && rn == Reg::R13 {
            ir.set_term(Terminal::PopRSBHint);
        } else {
            ir.set_term(Terminal::FastDispatchHint);
        }
        return false;
    }

    ir.set_register(rt, value);
    true
}

pub fn arm_ldr_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let rm = inst.rm();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();
    let shift_type = inst.shift_type();
    let imm5 = inst.imm5();

    assert!(!(!p && w), "T form of instruction unimplemented");
    if rm == Reg::R15 {
        return unpredictable_instruction(ir);
    }
    if (!p || w) && (rn == Reg::R15 || rn == rt) {
        return unpredictable_instruction(ir);
    }

    let carry_in = ir.get_c_flag();
    let rm_val = ir.get_register(rm);
    let (offset, _) = emit_imm_shift(ir, rm_val, shift_type, imm5, carry_in);
    let address = get_address(ir, p, u, w, rn, offset);

    let value = ir.read_memory_32(address, AccType::Normal);

    if rt == Reg::R15 {
        ir.load_write_pc(value);
        if !p && w && rn == Reg::R13 {
            ir.set_term(Terminal::PopRSBHint);
        } else {
            ir.set_term(Terminal::FastDispatchHint);
        }
        return false;
    }

    ir.set_register(rt, value);
    true
}

// --- STR ---

pub fn arm_str_imm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();
    let imm12 = inst.imm12();

    let offset = Value::ImmU32(imm12);
    let address = get_address(ir, p, u, w, rn, offset);
    let value = ir.get_register(rt);
    ir.write_memory_32(address, value, AccType::Normal);
    true
}

pub fn arm_str_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let rm = inst.rm();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();
    let shift_type = inst.shift_type();
    let imm5 = inst.imm5();

    let carry_in = ir.get_c_flag();
    let rm_val = ir.get_register(rm);
    let (offset, _) = emit_imm_shift(ir, rm_val, shift_type, imm5, carry_in);
    let address = get_address(ir, p, u, w, rn, offset);
    let value = ir.get_register(rt);
    ir.write_memory_32(address, value, AccType::Normal);
    true
}

// --- LDRB ---

/// ARM `LDRB <Rt>, [PC, #+/-<imm>]`.
///
/// Upstream owner: `TranslatorVisitor::arm_LDRB_lit`.
pub fn arm_ldrb_lit(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    if rt == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let base = ir.pc() & !3;
    let address = if inst.u_flag() {
        base.wrapping_add(inst.imm12())
    } else {
        base.wrapping_sub(inst.imm12())
    };
    let value = ir.read_memory_8(Value::ImmU32(address), AccType::Normal);
    let extended = ir.ir().zero_extend_byte_to_word(value);
    ir.set_register(rt, extended);
    true
}

pub fn arm_ldrb_imm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();
    let imm12 = inst.imm12();

    if rn == Reg::R15 {
        return unpredictable_instruction(ir);
    }
    assert!(!(!p && w), "T form of instruction unimplemented");
    if (!p || w) && rn == rt {
        return unpredictable_instruction(ir);
    }
    if rt == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let offset = Value::ImmU32(imm12);
    let address = get_address(ir, p, u, w, rn, offset);

    let value = ir.read_memory_8(address, AccType::Normal);
    let extended = ir.ir().zero_extend_byte_to_word(value);
    ir.set_register(rt, extended);
    true
}

pub fn arm_ldrb_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let rm = inst.rm();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();
    let shift_type = inst.shift_type();
    let imm5 = inst.imm5();

    assert!(!(!p && w), "T form of instruction unimplemented");
    if rt == Reg::R15 || rm == Reg::R15 {
        return unpredictable_instruction(ir);
    }
    if (!p || w) && (rn == Reg::R15 || rn == rt) {
        return unpredictable_instruction(ir);
    }

    let carry_in = ir.get_c_flag();
    let rm_val = ir.get_register(rm);
    let (offset, _) = emit_imm_shift(ir, rm_val, shift_type, imm5, carry_in);
    let address = get_address(ir, p, u, w, rn, offset);

    let value = ir.read_memory_8(address, AccType::Normal);
    let extended = ir.ir().zero_extend_byte_to_word(value);
    ir.set_register(rt, extended);
    true
}

// --- STRB ---

pub fn arm_strb_imm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();
    let imm12 = inst.imm12();

    let offset = Value::ImmU32(imm12);
    let address = get_address(ir, p, u, w, rn, offset);
    let value = ir.get_register(rt);
    let byte = ir.ir().least_significant_byte(value);
    ir.write_memory_8(address, byte, AccType::Normal);
    true
}

pub fn arm_strb_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let rm = inst.rm();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();
    let shift_type = inst.shift_type();
    let imm5 = inst.imm5();

    let carry_in = ir.get_c_flag();
    let rm_val = ir.get_register(rm);
    let (offset, _) = emit_imm_shift(ir, rm_val, shift_type, imm5, carry_in);
    let address = get_address(ir, p, u, w, rn, offset);
    let value = ir.get_register(rt);
    let byte = ir.ir().least_significant_byte(value);
    ir.write_memory_8(address, byte, AccType::Normal);
    true
}

// --- LDRH ---

/// ARM `LDRH <Rt>, [PC, #+/-<imm>]`.
///
/// Upstream owner: `TranslatorVisitor::arm_LDRH_lit`.
pub fn arm_ldrh_lit(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let p = inst.p_flag();
    let w = inst.w_flag();
    let rt = inst.rt();
    assert!(!(!p && w), "T form of instruction unimplemented");
    if p == w || rt == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let imm8 = ((inst.raw >> 4) & 0xF0) | (inst.raw & 0xF);
    let base = ir.pc() & !3;
    let address = if inst.u_flag() {
        base.wrapping_add(imm8)
    } else {
        base.wrapping_sub(imm8)
    };
    let value = ir.read_memory_16(Value::ImmU32(address), AccType::Normal);
    let extended = ir.ir().zero_extend_half_to_word(value);
    ir.set_register(rt, extended);
    true
}

pub fn arm_ldrh_imm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();
    // For extra load/store, immediate is imm4H:imm4L
    let imm4h = (inst.raw >> 8) & 0xF;
    let imm4l = inst.raw & 0xF;
    let imm8 = (imm4h << 4) | imm4l;

    if rn == Reg::R15 {
        return unpredictable_instruction(ir);
    }
    assert!(!(!p && w), "T form of instruction unimplemented");
    if (!p || w) && rn == rt {
        return unpredictable_instruction(ir);
    }
    if rt == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let offset = Value::ImmU32(imm8);
    let address = get_address(ir, p, u, w, rn, offset);

    let value = ir.read_memory_16(address, AccType::Normal);
    let extended = ir.ir().zero_extend_half_to_word(value);
    ir.set_register(rt, extended);
    true
}

pub fn arm_ldrh_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let rm = inst.rm();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();

    assert!(!(!p && w), "T form of instruction unimplemented");
    if rt == Reg::R15 || rm == Reg::R15 {
        return unpredictable_instruction(ir);
    }
    if (!p || w) && (rn == Reg::R15 || rn == rt) {
        return unpredictable_instruction(ir);
    }

    let offset = ir.get_register(rm);
    let address = get_address(ir, p, u, w, rn, offset);

    let value = ir.read_memory_16(address, AccType::Normal);
    let extended = ir.ir().zero_extend_half_to_word(value);
    ir.set_register(rt, extended);
    true
}

// --- STRH ---

pub fn arm_strh_imm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();
    let imm4h = (inst.raw >> 8) & 0xF;
    let imm4l = inst.raw & 0xF;
    let imm8 = (imm4h << 4) | imm4l;

    let offset = Value::ImmU32(imm8);
    let address = get_address(ir, p, u, w, rn, offset);
    let value = ir.get_register(rt);
    let half = ir.ir().least_significant_half(value);
    ir.write_memory_16(address, half, AccType::Normal);
    true
}

pub fn arm_strh_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let rm = inst.rm();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();

    let offset = ir.get_register(rm);
    let address = get_address(ir, p, u, w, rn, offset);
    let value = ir.get_register(rt);
    let half = ir.ir().least_significant_half(value);
    ir.write_memory_16(address, half, AccType::Normal);
    true
}

// --- LDRSB ---

/// ARM `LDRSB <Rt>, [PC, #+/-<imm>]`.
///
/// Upstream owner: `TranslatorVisitor::arm_LDRSB_lit`.
pub fn arm_ldrsb_lit(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    if rt == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let imm8 = ((inst.raw >> 4) & 0xF0) | (inst.raw & 0xF);
    let base = ir.pc() & !3;
    let address = if inst.u_flag() {
        base.wrapping_add(imm8)
    } else {
        base.wrapping_sub(imm8)
    };
    let value = ir.read_memory_8(Value::ImmU32(address), AccType::Normal);
    let extended = ir.ir().sign_extend_byte_to_word(value);
    ir.set_register(rt, extended);
    true
}

pub fn arm_ldrsb_imm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();
    let imm4h = (inst.raw >> 8) & 0xF;
    let imm4l = inst.raw & 0xF;
    let imm8 = (imm4h << 4) | imm4l;

    if rn == Reg::R15 {
        return unpredictable_instruction(ir);
    }
    assert!(!(!p && w), "T form of instruction unimplemented");
    if (!p || w) && rn == rt {
        return unpredictable_instruction(ir);
    }
    if rt == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let offset = Value::ImmU32(imm8);
    let address = get_address(ir, p, u, w, rn, offset);

    let value = ir.read_memory_8(address, AccType::Normal);
    let extended = ir.ir().sign_extend_byte_to_word(value);
    ir.set_register(rt, extended);
    true
}

pub fn arm_ldrsb_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let rm = inst.rm();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();

    assert!(!(!p && w), "T form of instruction unimplemented");
    if rt == Reg::R15 || rm == Reg::R15 {
        return unpredictable_instruction(ir);
    }
    if (!p || w) && (rn == Reg::R15 || rn == rt) {
        return unpredictable_instruction(ir);
    }

    let offset = ir.get_register(rm);
    let address = get_address(ir, p, u, w, rn, offset);

    let value = ir.read_memory_8(address, AccType::Normal);
    let extended = ir.ir().sign_extend_byte_to_word(value);
    ir.set_register(rt, extended);
    true
}

// --- LDRSH ---

/// ARM `LDRSH <Rt>, [PC, #+/-<imm>]`.
///
/// Upstream owner: `TranslatorVisitor::arm_LDRSH_lit`.
pub fn arm_ldrsh_lit(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    if rt == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let imm8 = ((inst.raw >> 4) & 0xF0) | (inst.raw & 0xF);
    let base = ir.pc() & !3;
    let address = if inst.u_flag() {
        base.wrapping_add(imm8)
    } else {
        base.wrapping_sub(imm8)
    };
    let value = ir.read_memory_16(Value::ImmU32(address), AccType::Normal);
    let extended = ir.ir().sign_extend_half_to_word(value);
    ir.set_register(rt, extended);
    true
}

pub fn arm_ldrsh_imm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();
    let imm4h = (inst.raw >> 8) & 0xF;
    let imm4l = inst.raw & 0xF;
    let imm8 = (imm4h << 4) | imm4l;

    if rn == Reg::R15 {
        return unpredictable_instruction(ir);
    }
    assert!(!(!p && w), "T form of instruction unimplemented");
    if (!p || w) && rn == rt {
        return unpredictable_instruction(ir);
    }
    if rt == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let offset = Value::ImmU32(imm8);
    let address = get_address(ir, p, u, w, rn, offset);

    let value = ir.read_memory_16(address, AccType::Normal);
    let extended = ir.ir().sign_extend_half_to_word(value);
    ir.set_register(rt, extended);
    true
}

pub fn arm_ldrsh_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let rm = inst.rm();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();

    assert!(!(!p && w), "T form of instruction unimplemented");
    if rt == Reg::R15 || rm == Reg::R15 {
        return unpredictable_instruction(ir);
    }
    if (!p || w) && (rn == Reg::R15 || rn == rt) {
        return unpredictable_instruction(ir);
    }

    let offset = ir.get_register(rm);
    let address = get_address(ir, p, u, w, rn, offset);

    let value = ir.read_memory_16(address, AccType::Normal);
    let extended = ir.ir().sign_extend_half_to_word(value);
    ir.set_register(rt, extended);
    true
}

// --- LDRD ---

/// ARM `LDRD <Rt>, <Rt2>, [PC, #+/-<imm>]`.
///
/// Upstream owner: `TranslatorVisitor::arm_LDRD_lit`.
pub fn arm_ldrd_lit(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    if (rt as u32) % 2 == 1 {
        return unpredictable_instruction(ir);
    }
    if rt == Reg::R14 {
        return unpredictable_instruction(ir);
    }
    let rt2 = Reg::from_u32((rt as u32) + 1);
    let imm8 = ((inst.raw >> 4) & 0xF0) | (inst.raw & 0xF);
    let base = ir.pc() & !3;
    let address = if inst.u_flag() {
        base.wrapping_add(imm8)
    } else {
        base.wrapping_sub(imm8)
    };

    let data = ir.read_memory_64(Value::ImmU32(address), AccType::Atomic);
    let e_flag = ir
        .current_location
        .expect("current_location not set")
        .e_flag();
    if e_flag {
        let hi_word = ir.ir().most_significant_word(data);
        ir.set_register(rt, hi_word);
        let lo_word = ir.ir().least_significant_word(data);
        ir.set_register(rt2, lo_word);
    } else {
        let lo_word = ir.ir().least_significant_word(data);
        ir.set_register(rt, lo_word);
        let hi_word = ir.ir().most_significant_word(data);
        ir.set_register(rt2, hi_word);
    }
    true
}

pub fn arm_ldrd_imm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();
    let imm4h = (inst.raw >> 8) & 0xF;
    let imm4l = inst.raw & 0xF;
    let imm8 = (imm4h << 4) | imm4l;

    if rn == Reg::R15 {
        return unpredictable_instruction(ir);
    }
    if (rt as u32) % 2 == 1 {
        return unpredictable_instruction(ir);
    }
    if !p && w {
        return unpredictable_instruction(ir);
    }
    if (!p || w) && (rn == rt || rn as u32 == rt as u32 + 1) {
        return unpredictable_instruction(ir);
    }
    if rt == Reg::R14 {
        return unpredictable_instruction(ir);
    }

    let rt2 = Reg::from_u32((rt as u32) + 1);
    let offset = Value::ImmU32(imm8);
    let address = get_address(ir, p, u, w, rn, offset);

    // Upstream `arm_LDRD_imm` issues a single 64-bit ATOMIC read and splits
    // the result across the two destination registers, with most/least
    // significant word ordering driven by CPSR.E. Two separate 32-bit Normal
    // reads break atomicity and the backend pattern matchers that fold
    // adjacent halves back into 64-bit ops, so the game's 64-bit atomic
    // accesses behave incorrectly. Match the upstream emit shape exactly.
    let data = ir.read_memory_64(address, AccType::Atomic);
    let e_flag = ir
        .current_location
        .expect("current_location not set")
        .e_flag();
    if e_flag {
        let hi_word = ir.ir().most_significant_word(data);
        ir.set_register(rt, hi_word);
        let lo_word = ir.ir().least_significant_word(data);
        ir.set_register(rt2, lo_word);
    } else {
        let lo_word = ir.ir().least_significant_word(data);
        ir.set_register(rt, lo_word);
        let hi_word = ir.ir().most_significant_word(data);
        ir.set_register(rt2, hi_word);
    }
    true
}

pub fn arm_ldrd_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let rm = inst.rm();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();

    if (rt as u32) % 2 == 1 {
        return unpredictable_instruction(ir);
    }
    if !p && w {
        return unpredictable_instruction(ir);
    }
    if rt == Reg::R14 || rm == Reg::R15 || rm == rt || rm as u32 == rt as u32 + 1 {
        return unpredictable_instruction(ir);
    }
    if (!p || w) && (rn == Reg::R15 || rn == rt || rn as u32 == rt as u32 + 1) {
        return unpredictable_instruction(ir);
    }

    let rt2 = Reg::from_u32((rt as u32) + 1);
    let offset = ir.get_register(rm);
    let address = get_address(ir, p, u, w, rn, offset);

    // Match upstream `arm_LDRD_reg`: single 64-bit ATOMIC read, split
    // most/least significant word per CPSR.E.
    let data = ir.read_memory_64(address, AccType::Atomic);
    let e_flag = ir
        .current_location
        .expect("current_location not set")
        .e_flag();
    if e_flag {
        let hi_word = ir.ir().most_significant_word(data);
        ir.set_register(rt, hi_word);
        let lo_word = ir.ir().least_significant_word(data);
        ir.set_register(rt2, lo_word);
    } else {
        let lo_word = ir.ir().least_significant_word(data);
        ir.set_register(rt, lo_word);
        let hi_word = ir.ir().most_significant_word(data);
        ir.set_register(rt2, hi_word);
    }
    true
}

// --- STRD ---

pub fn arm_strd_imm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rt2 = Reg::from_u32((rt as u32) + 1);
    let rn = inst.rn();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();
    let imm4h = (inst.raw >> 8) & 0xF;
    let imm4l = inst.raw & 0xF;
    let imm8 = (imm4h << 4) | imm4l;

    let offset = Value::ImmU32(imm8);
    let address = get_address(ir, p, u, w, rn, offset);

    // Match upstream `arm_STRD_imm`: pack the two source registers into a
    // single 64-bit value (low/high order driven by CPSR.E) and emit one
    // 64-bit ATOMIC write. Two separate 32-bit Normal writes break atomicity
    // and prevent the backend from recognising 64-bit atomic accesses.
    let value_a = ir.get_register(rt);
    let value_b = ir.get_register(rt2);
    let e_flag = ir
        .current_location
        .expect("current_location not set")
        .e_flag();
    let data = if e_flag {
        ir.ir().pack_2x32_to_1x64(value_b, value_a)
    } else {
        ir.ir().pack_2x32_to_1x64(value_a, value_b)
    };
    ir.write_memory_64(address, data, AccType::Atomic);
    true
}

pub fn arm_strd_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rt2 = Reg::from_u32((rt as u32) + 1);
    let rn = inst.rn();
    let rm = inst.rm();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();

    let offset = ir.get_register(rm);
    let address = get_address(ir, p, u, w, rn, offset);

    // Match upstream `arm_STRD_reg`: single 64-bit ATOMIC write of the packed
    // register pair, with low/high order driven by CPSR.E.
    let value_a = ir.get_register(rt);
    let value_b = ir.get_register(rt2);
    let e_flag = ir
        .current_location
        .expect("current_location not set")
        .e_flag();
    let data = if e_flag {
        ir.ir().pack_2x32_to_1x64(value_b, value_a)
    } else {
        ir.ir().pack_2x32_to_1x64(value_a, value_b)
    };
    ir.write_memory_64(address, data, AccType::Atomic);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::decoder::ArmInstId;
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::psr::PSR;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::terminal::Terminal;

    fn translate_literal(
        raw: u32,
        id: ArmInstId,
        visitor: fn(&mut A32IREmitter, &DecodedArm) -> bool,
    ) -> (Block, bool) {
        let loc = A32LocationDescriptor::new(0x1002, PSR::default(), FPSCR::default(), false);
        let mut block = Block::new(loc.to_location());
        let result = {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            visitor(&mut ir, &DecodedArm { raw, id })
        };
        (block, result)
    }

    fn assert_literal_address(
        raw: u32,
        id: ArmInstId,
        visitor: fn(&mut A32IREmitter, &DecodedArm) -> bool,
        read_opcode: Opcode,
        expected_address: u32,
    ) {
        let (block, result) = translate_literal(raw, id, visitor);
        assert!(result);
        let read = block
            .instructions
            .iter()
            .find(|inst| inst.opcode == read_opcode)
            .expect("literal visitor must emit its memory read");
        assert_eq!(read.args[1], Value::ImmU32(expected_address));
        assert!(block
            .instructions
            .iter()
            .all(|inst| !matches!(inst.opcode, Opcode::Add32 | Opcode::Sub32)));
    }

    fn assert_unpredictable_load(
        raw: u32,
        id: ArmInstId,
        visitor: fn(&mut A32IREmitter, &DecodedArm) -> bool,
    ) {
        let (block, result) = translate_literal(raw, id, visitor);
        assert!(!result);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32ExceptionRaised));
        assert!(matches!(block.terminal, Terminal::CheckHalt { .. }));
    }

    #[test]
    fn arm_literal_loads_use_aligned_immediate_addresses() {
        // At guest PC 0x1002, ARM's visible PC is 0x100A and AlignPC(4)
        // therefore yields 0x1008. The split immediate forms below encode
        // 0x24 as imm8a:imm8b.
        assert_literal_address(
            0xE59F_0014,
            ArmInstId::LDR_lit,
            arm_ldr_lit,
            Opcode::A32ReadMemory32,
            0x101C,
        );
        assert_literal_address(
            0xE51F_0014,
            ArmInstId::LDR_lit,
            arm_ldr_lit,
            Opcode::A32ReadMemory32,
            0x0FF4,
        );
        assert_literal_address(
            0xE5DF_0024,
            ArmInstId::LDRB_lit,
            arm_ldrb_lit,
            Opcode::A32ReadMemory8,
            0x102C,
        );
        assert_literal_address(
            0xE1DF_02B4,
            ArmInstId::LDRH_lit,
            arm_ldrh_lit,
            Opcode::A32ReadMemory16,
            0x102C,
        );
        assert_literal_address(
            0xE1DF_02D4,
            ArmInstId::LDRSB_lit,
            arm_ldrsb_lit,
            Opcode::A32ReadMemory8,
            0x102C,
        );
        assert_literal_address(
            0xE1DF_02F4,
            ArmInstId::LDRSH_lit,
            arm_ldrsh_lit,
            Opcode::A32ReadMemory16,
            0x102C,
        );
        assert_literal_address(
            0xE1CF_02D4,
            ArmInstId::LDRD_lit,
            arm_ldrd_lit,
            Opcode::A32ReadMemory64,
            0x102C,
        );
    }

    #[test]
    fn arm_ldr_literal_to_pc_uses_fast_dispatch_hint() {
        let (block, result) = translate_literal(0xE59F_F000, ArmInstId::LDR_lit, arm_ldr_lit);
        assert!(!result);
        assert!(matches!(block.terminal, Terminal::FastDispatchHint));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32BXWritePC));
    }

    #[test]
    fn arm_nonword_literal_to_pc_is_unpredictable() {
        let (block, result) = translate_literal(0xE5DF_F000, ArmInstId::LDRB_lit, arm_ldrb_lit);
        assert!(!result);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32ExceptionRaised));
        assert!(matches!(block.terminal, Terminal::CheckHalt { .. }));
    }

    #[test]
    fn arm_immediate_and_register_loads_preserve_upstream_validation() {
        for (raw, id, visitor) in [
            (
                0xE59F_0000,
                ArmInstId::LDR_imm,
                arm_ldr_imm as fn(&mut A32IREmitter, &DecodedArm) -> bool,
            ),
            (0xE5DF_0000, ArmInstId::LDRB_imm, arm_ldrb_imm),
            (0xE1DF_00B0, ArmInstId::LDRH_imm, arm_ldrh_imm),
            (0xE1DF_00D0, ArmInstId::LDRSB_imm, arm_ldrsb_imm),
            (0xE1DF_00F0, ArmInstId::LDRSH_imm, arm_ldrsh_imm),
            (0xE1CF_00D0, ArmInstId::LDRD_imm, arm_ldrd_imm),
        ] {
            assert_unpredictable_load(raw, id, visitor);
        }

        // P=1, Rn=R1, Rt=R0, Rm=PC. Each register-offset load visitor
        // rejects Rm=PC before reading it, as Eden does.
        let register_fields = (1 << 24) | (1 << 23) | (1 << 16) | 15;
        for (id, visitor) in [
            (
                ArmInstId::LDR_reg,
                arm_ldr_reg as fn(&mut A32IREmitter, &DecodedArm) -> bool,
            ),
            (ArmInstId::LDRB_reg, arm_ldrb_reg),
            (ArmInstId::LDRH_reg, arm_ldrh_reg),
            (ArmInstId::LDRSB_reg, arm_ldrsb_reg),
            (ArmInstId::LDRSH_reg, arm_ldrsh_reg),
            (ArmInstId::LDRD_reg, arm_ldrd_reg),
        ] {
            assert_unpredictable_load(register_fields, id, visitor);
        }
    }

    #[test]
    fn arm_ldrd_literal_splits_words_in_guest_endian_order() {
        for (big_endian, expected) in [
            (
                false,
                [
                    Opcode::LeastSignificantWord,
                    Opcode::A32SetRegister,
                    Opcode::MostSignificantWord,
                    Opcode::A32SetRegister,
                ],
            ),
            (
                true,
                [
                    Opcode::MostSignificantWord,
                    Opcode::A32SetRegister,
                    Opcode::LeastSignificantWord,
                    Opcode::A32SetRegister,
                ],
            ),
        ] {
            let mut psr = PSR::default();
            psr.set_e(big_endian);
            let loc = A32LocationDescriptor::new(0x1000, psr, FPSCR::default(), false);
            let mut block = Block::new(loc.to_location());
            {
                let mut ir = A32IREmitter::with_location(&mut block, loc);
                assert!(arm_ldrd_lit(
                    &mut ir,
                    &DecodedArm {
                        raw: 0xE1CF_00D0,
                        id: ArmInstId::LDRD_lit,
                    },
                ));
            }
            let actual = block
                .instructions
                .iter()
                .filter_map(|inst| {
                    matches!(
                        inst.opcode,
                        Opcode::LeastSignificantWord
                            | Opcode::MostSignificantWord
                            | Opcode::A32SetRegister
                    )
                    .then_some(inst.opcode)
                })
                .collect::<Vec<_>>();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn arm_strd_imm_emits_single_atomic_write_memory_64() {
        let loc = A32LocationDescriptor::new(0x1000, PSR::default(), FPSCR::default(), false);
        let mut block = Block::new(loc.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, loc);
        let inst = DecodedArm {
            raw: 0xE1A1_20F4,
            id: ArmInstId::STRD_imm,
        };

        assert!(arm_strd_imm(&mut ir, &inst));
        assert_eq!(
            block
                .instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::A32WriteMemory64)
                .count(),
            1
        );
        assert_eq!(
            block
                .instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::A32WriteMemory32)
                .count(),
            0
        );
    }

    #[test]
    fn arm_ldrd_imm_emits_single_atomic_read_memory_64() {
        let loc = A32LocationDescriptor::new(0x1000, PSR::default(), FPSCR::default(), false);
        let mut block = Block::new(loc.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, loc);
        let inst = DecodedArm {
            raw: 0xE1B1_20D4,
            id: ArmInstId::LDRD_imm,
        };

        assert!(arm_ldrd_imm(&mut ir, &inst));
        assert_eq!(
            block
                .instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::A32ReadMemory64)
                .count(),
            1
        );
        assert_eq!(
            block
                .instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::A32ReadMemory32)
                .count(),
            0
        );
    }
}

use crate::frontend::a32::decoder::DecodedArm;
use crate::frontend::a32::types::Reg;
use crate::interface::a32::coprocessor_util::CoprocReg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::value::Value;

/// ARM MCR — Move CPU Register to Coprocessor Register.
///
/// Encoding: cond 1110 opc1(3) 0 CRn(4) Rt(4) coproc(4) opc2(3) 1 CRm(4)
///
pub fn arm_mcr(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let coproc_no = inst.coproc_no();

    if coproc_no == 10 || coproc_no == 11 {
        return super::undefined_instruction(ir);
    }

    let rt = inst.rt();

    if rt == Reg::R15 {
        return super::unpredictable_instruction(ir);
    }

    let word = ir.get_register(rt);
    ir.coproc_send_one_word(
        coproc_no as usize,
        inst.coproc_two(),
        inst.coproc_opc1() as usize,
        CoprocReg::from_u8(inst.crn() as u8),
        CoprocReg::from_u8(inst.crm() as u8),
        inst.coproc_opc2() as usize,
        word,
    );
    true
}

/// ARM MRC — Move Coprocessor Register to CPU Register.
///
/// Encoding: cond 1110 opc1(3) 1 CRn(4) Rt(4) coproc(4) opc2(3) 1 CRm(4)
///
pub fn arm_mrc(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let coproc_no = inst.coproc_no();

    if coproc_no == 10 || coproc_no == 11 {
        return super::undefined_instruction(ir);
    }

    let rt = inst.rt();

    let word = ir.coproc_get_one_word(
        coproc_no as usize,
        inst.coproc_two(),
        inst.coproc_opc1() as usize,
        CoprocReg::from_u8(inst.crn() as u8),
        CoprocReg::from_u8(inst.crm() as u8),
        inst.coproc_opc2() as usize,
    );

    if rt == Reg::R15 {
        // MRC with Rt=PC: result goes to NZCV flags, not a register.
        // Extract bits [31:28] (ARM NZCV) and write to CPSR NZCV.
        let mask = ir.ir().imm32(0xF000_0000);
        let nzcv = ir.ir().and_32(word, mask);
        ir.set_cpsr_nzcv_raw(nzcv);
    } else {
        ir.set_register(rt, word);
    }

    true
}

/// ARM CDP — Coprocessor Data Processing.
///
pub fn arm_cdp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let coproc_no = inst.coproc_no();
    if coproc_no == 10 || coproc_no == 11 {
        return super::undefined_instruction(ir);
    }

    ir.coproc_internal_operation(
        coproc_no as usize,
        inst.coproc_two(),
        inst.coproc_dp_opc1() as usize,
        CoprocReg::from_u8(((inst.raw >> 12) & 0xF) as u8),
        CoprocReg::from_u8(inst.crn() as u8),
        CoprocReg::from_u8(inst.crm() as u8),
        inst.coproc_opc2() as usize,
    );
    true
}

/// ARM MRRC — Move to two ARM Registers from Coprocessor.
///
/// Encoding: cond 1100 0101 Rt2(4) Rt(4) coproc(4) opc(4) CRm(4)
///
/// Reads a 64-bit value from the coprocessor and writes the low 32 bits to Rt
/// and the high 32 bits to Rt2. Used for CNTPCT (CP15 C14 opc=0).
pub fn arm_mrrc(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let coproc_no = inst.coproc_no();
    if coproc_no == 10 || coproc_no == 11 {
        return super::undefined_instruction(ir);
    }

    let rt = inst.rt();
    let rt2 = inst.rt2();

    if rt == Reg::R15 || rt2 == Reg::R15 || rt == rt2 {
        return super::unpredictable_instruction(ir);
    }

    // Get 64-bit value from coprocessor
    let val64 = ir.coproc_get_two_words(
        coproc_no as usize,
        inst.coproc_two(),
        inst.mrrc_opc() as usize,
        CoprocReg::from_u8(inst.crm() as u8),
    );

    // Split into low and high 32-bit halves
    let lo = ir.ir().least_significant_word(val64);
    let hi = ir.ir().most_significant_word(val64);

    ir.set_register(rt, lo);
    ir.set_register(rt2, hi);

    true
}

/// ARM MCRR — Move to Coprocessor from two ARM Registers.
///
/// Encoding: cond 1100 0100 Rt2(4) Rt(4) coproc(4) opc(4) CRm(4)
///
/// Writes two 32-bit register values to the coprocessor as a 64-bit value.
pub fn arm_mcrr(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let coproc_no = inst.coproc_no();
    if coproc_no == 10 || coproc_no == 11 {
        return super::undefined_instruction(ir);
    }

    let rt = inst.rt();
    let rt2 = inst.rt2();

    if rt == Reg::R15 || rt2 == Reg::R15 {
        return super::unpredictable_instruction(ir);
    }

    let word1 = ir.get_register(rt);
    let word2 = ir.get_register(rt2);
    ir.coproc_send_two_words(
        coproc_no as usize,
        inst.coproc_two(),
        inst.mrrc_opc() as usize,
        CoprocReg::from_u8(inst.crm() as u8),
        word1,
        word2,
    );
    true
}

/// ARM LDC — load words from memory into a coprocessor.
pub fn arm_ldc(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let p = inst.p_flag();
    let u = inst.u_flag();
    let d = inst.raw & (1 << 22) != 0;
    let w = inst.w_flag();
    let coproc_no = inst.coproc_no();

    if !p && !u && !d && !w {
        return super::undefined_instruction(ir);
    }
    if coproc_no == 10 || coproc_no == 11 {
        return super::undefined_instruction(ir);
    }

    let n = inst.rn();
    let imm8 = inst.imm8();
    let imm32 = imm8 << 2;
    let reg_n = ir.get_register(n);
    let offset_address = if u {
        ir.ir()
            .add_32(reg_n, Value::ImmU32(imm32), Value::ImmU1(false))
    } else {
        ir.ir()
            .sub_32(reg_n, Value::ImmU32(imm32), Value::ImmU1(true))
    };
    let address = if p { offset_address } else { reg_n };
    ir.coproc_load_words(
        coproc_no as usize,
        inst.coproc_two(),
        d,
        CoprocReg::from_u8(((inst.raw >> 12) & 0xF) as u8),
        address,
        !p && !w && u,
        imm8 as u8,
    );
    if w {
        ir.set_register(n, offset_address);
    }
    true
}

/// ARM STC — store words from a coprocessor into memory.
pub fn arm_stc(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let p = inst.p_flag();
    let u = inst.u_flag();
    let d = inst.raw & (1 << 22) != 0;
    let w = inst.w_flag();
    let coproc_no = inst.coproc_no();

    if coproc_no == 10 || coproc_no == 11 {
        return super::undefined_instruction(ir);
    }
    if !p && !u && !d && !w {
        return super::undefined_instruction(ir);
    }

    let n = inst.rn();
    if n == Reg::R15 && w {
        return super::unpredictable_instruction(ir);
    }

    let imm8 = inst.imm8();
    let imm32 = imm8 << 2;
    let reg_n = ir.get_register(n);
    let offset_address = if u {
        ir.ir()
            .add_32(reg_n, Value::ImmU32(imm32), Value::ImmU1(false))
    } else {
        ir.ir()
            .sub_32(reg_n, Value::ImmU32(imm32), Value::ImmU1(true))
    };
    let address = if p { offset_address } else { reg_n };
    ir.coproc_store_words(
        coproc_no as usize,
        inst.coproc_two(),
        d,
        CoprocReg::from_u8(((inst.raw >> 12) & 0xF) as u8),
        address,
        !p && !w && u,
        imm8 as u8,
    );
    if w {
        ir.set_register(n, offset_address);
    }
    true
}

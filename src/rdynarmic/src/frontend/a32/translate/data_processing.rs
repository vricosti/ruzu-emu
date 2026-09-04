use super::helpers::{emit_imm_shift, emit_reg_shift};
use crate::frontend::a32::decoder::{arm_expand_imm, arm_expand_imm_c, ArmInstId, DecodedArm};
use crate::frontend::a32::types::Reg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

/// Instruction category — determines which operands are needed.
/// Matches upstream's per-function approach where each instruction
/// reads only the operands it requires.
#[derive(Clone, Copy, PartialEq)]
enum DpCategory {
    /// AND, EOR, SUB, RSB, ADD, ORR, BIC: result = f(Rn, operand2), writes Rd
    TwoOp,
    /// ADC, SBC, RSC: result = f(Rn, operand2, C), writes Rd
    TwoOpCarry,
    /// MOV: result = operand2, writes Rd (does NOT read Rn)
    MovOp,
    /// MVN: result = NOT(operand2), writes Rd (does NOT read Rn)
    MvnOp,
    /// TST, TEQ: result = f(Rn, operand2), sets flags, does NOT write Rd
    TestOp,
    /// CMP, CMN: result = f(Rn, operand2), sets flags (NZCV), does NOT write Rd
    CompareOp,
}

/// Operation type for the ALU computation.
#[derive(Clone, Copy, PartialEq)]
enum DpOp {
    And,
    Eor,
    Sub,
    Rsb,
    Add,
    Adc,
    Sbc,
    Rsc,
    Tst,
    Teq,
    Cmp,
    Cmn,
    Orr,
    Mov,
    Bic,
    Mvn,
}

fn classify(id: ArmInstId) -> Option<(DpOp, DpCategory)> {
    use ArmInstId::*;
    match id {
        AndImm | AndReg | AndRsr => Some((DpOp::And, DpCategory::TwoOp)),
        EorImm | EorReg | EorRsr => Some((DpOp::Eor, DpCategory::TwoOp)),
        SubImm | SubReg | SubRsr => Some((DpOp::Sub, DpCategory::TwoOp)),
        RsbImm | RsbReg | RsbRsr => Some((DpOp::Rsb, DpCategory::TwoOp)),
        AddImm | AddReg | AddRsr => Some((DpOp::Add, DpCategory::TwoOp)),
        OrrImm | OrrReg | OrrRsr => Some((DpOp::Orr, DpCategory::TwoOp)),
        BicImm | BicReg | BicRsr => Some((DpOp::Bic, DpCategory::TwoOp)),
        AdcImm | AdcReg | AdcRsr => Some((DpOp::Adc, DpCategory::TwoOpCarry)),
        SbcImm | SbcReg | SbcRsr => Some((DpOp::Sbc, DpCategory::TwoOpCarry)),
        RscImm | RscReg | RscRsr => Some((DpOp::Rsc, DpCategory::TwoOpCarry)),
        MovImm | MovReg | MovRsr => Some((DpOp::Mov, DpCategory::MovOp)),
        MvnImm | MvnReg | MvnRsr => Some((DpOp::Mvn, DpCategory::MvnOp)),
        TstImm | TstReg | TstRsr => Some((DpOp::Tst, DpCategory::TestOp)),
        TeqImm | TeqReg | TeqRsr => Some((DpOp::Teq, DpCategory::TestOp)),
        CmpImm | CmpReg | CmpRsr => Some((DpOp::Cmp, DpCategory::CompareOp)),
        CmnImm | CmnReg | CmnRsr => Some((DpOp::Cmn, DpCategory::CompareOp)),
        _ => None,
    }
}

/// ARM data processing - immediate operand.
/// Matches upstream's per-instruction functions: only reads operands needed.
pub fn arm_dp_imm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = inst.rd();
    let rn = inst.rn();
    let s = inst.s_flag();
    let rotate = inst.rotate();
    let imm8 = inst.imm8();

    let Some((op, cat)) = classify(inst.id) else {
        return true;
    };

    // Upstream uses ArmExpandImm_C for every logical immediate operation,
    // even when S is clear, and plain ArmExpandImm for arithmetic operations.
    let is_logic_cat = match cat {
        DpCategory::TwoOp => !matches!(op, DpOp::Add | DpOp::Sub | DpOp::Rsb),
        DpCategory::MovOp | DpCategory::MvnOp | DpCategory::TestOp => true,
        _ => false,
    };
    let (imm_val, carry) = if is_logic_cat {
        let carry_in = ir.get_c_flag();
        let (imm_val, carry_bit) = arm_expand_imm_c(rotate, imm8, false);
        let carry = if rotate == 0 {
            carry_in
        } else {
            ir.ir().imm1(carry_bit)
        };
        (imm_val, carry)
    } else {
        (arm_expand_imm(rotate, imm8), Value::ImmU1(false))
    };
    let operand2 = Value::ImmU32(imm_val);

    // Upstream: MOV/MVN don't read Rn. TST/TEQ/CMP/CMN read Rn but not Rd.
    let operand1 = if matches!(cat, DpCategory::MovOp | DpCategory::MvnOp) {
        Value::ImmU32(0) // not used
    } else {
        ir.get_register(rn)
    };

    dp_emit(ir, op, cat, rd, s, operand1, operand2, carry)
}

/// ARM data processing - register operand (with immediate shift).
pub fn arm_dp_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = inst.rd();
    let rn = inst.rn();
    let rm = inst.rm();
    let s = inst.s_flag();
    let shift_type = inst.shift_type();
    let imm5 = inst.imm5();

    let Some((op, cat)) = classify(inst.id) else {
        return true;
    };

    // Upstream: GetCFlag is called for the barrel shifter.
    // For MovReg with LSL#0 and S=0, upstream still calls GetCFlag
    // because EmitImmShift always takes carry_in. Match upstream.
    let carry_in = ir.get_c_flag();
    let rm_val = ir.get_register(rm);
    let (shifted, carry) = emit_imm_shift(ir, rm_val, shift_type, imm5, carry_in);

    // Only read Rn when the instruction actually uses it.
    // Upstream: arm_MOV_reg and arm_MVN_reg do NOT call ir.GetRegister(n).
    let operand1 = if matches!(cat, DpCategory::MovOp | DpCategory::MvnOp) {
        Value::ImmU32(0)
    } else {
        ir.get_register(rn)
    };

    dp_emit(ir, op, cat, rd, s, operand1, shifted, carry)
}

/// ARM data processing - register-shifted register operand.
pub fn arm_dp_rsr(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = inst.rd();
    let rn = inst.rn();
    let rm = inst.rm();
    let rs = inst.rs();
    let s = inst.s_flag();
    let shift_type = inst.shift_type();

    let Some((op, cat)) = classify(inst.id) else {
        return true;
    };

    let writes_rd = matches!(
        cat,
        DpCategory::TwoOp | DpCategory::TwoOpCarry | DpCategory::MovOp | DpCategory::MvnOp
    );
    let reads_rn = !matches!(cat, DpCategory::MovOp | DpCategory::MvnOp);
    if rm == Reg::PC || rs == Reg::PC || (reads_rn && rn == Reg::PC) || (writes_rd && rd == Reg::PC)
    {
        return super::unpredictable_instruction(ir);
    }

    let rs_val = ir.get_register(rs);
    let rs_amount = ir.ir().least_significant_byte(rs_val);
    let carry_in = ir.get_c_flag();
    let rm_val = ir.get_register(rm);
    let (shifted, carry) = emit_reg_shift(ir, rm_val, shift_type, rs_amount, carry_in);

    let operand1 = if matches!(cat, DpCategory::MovOp | DpCategory::MvnOp) {
        Value::ImmU32(0)
    } else {
        ir.get_register(rn)
    };

    dp_emit(ir, op, cat, rd, s, operand1, shifted, carry)
}

/// Emit the ALU operation, flags update, and result write.
/// The shared dispatcher is pre-existing structural parity debt; its ordering
/// follows the corresponding upstream per-instruction methods.
fn dp_emit(
    ir: &mut A32IREmitter,
    op: DpOp,
    cat: DpCategory,
    rd: Reg,
    s: bool,
    operand1: Value,
    operand2: Value,
    carry: Value,
) -> bool {
    // Compute the result. Matches upstream's per-instruction logic.
    let result = match op {
        DpOp::And | DpOp::Tst => ir.ir().and_32(operand1, operand2),
        DpOp::Eor | DpOp::Teq => ir.ir().eor_32(operand1, operand2),
        DpOp::Sub | DpOp::Cmp => ir.ir().sub_32(operand1, operand2, Value::ImmU1(true)),
        DpOp::Rsb => ir.ir().sub_32(operand2, operand1, Value::ImmU1(true)),
        DpOp::Add | DpOp::Cmn => ir.ir().add_32(operand1, operand2, Value::ImmU1(false)),
        DpOp::Adc => {
            let c = ir.get_c_flag();
            ir.ir().add_32(operand1, operand2, c)
        }
        DpOp::Sbc => {
            let c = ir.get_c_flag();
            ir.ir().sub_32(operand1, operand2, c)
        }
        DpOp::Rsc => {
            let c = ir.get_c_flag();
            ir.ir().sub_32(operand2, operand1, c)
        }
        DpOp::Orr => ir.ir().or_32(operand1, operand2),
        DpOp::Mov => operand2,
        DpOp::Bic => ir.ir().and_not_32(operand1, operand2),
        DpOp::Mvn => ir.ir().not_32(operand2),
    };

    // Upstream handles PC destinations immediately after computing the result,
    // before any flag update or general-register write.
    let writes_rd = matches!(
        cat,
        DpCategory::TwoOp | DpCategory::TwoOpCarry | DpCategory::MovOp | DpCategory::MvnOp
    );
    if writes_rd && rd == Reg::R15 {
        if s {
            return super::unpredictable_instruction(ir);
        }
        ir.alu_write_pc(result);
        ir.set_term(Terminal::ReturnToDispatch);
        return false;
    }

    // Update flags. Upstream: arithmetic ops set NZCV, logic ops set NZC.
    let update_flags = s || matches!(cat, DpCategory::TestOp | DpCategory::CompareOp);
    if update_flags {
        match cat {
            DpCategory::CompareOp | DpCategory::TwoOpCarry => {
                // Arithmetic: set all NZCV from the result.
                let nzcv = ir.ir().get_nzcv_from_op(result);
                ir.set_cpsr_nzcv(nzcv);
            }
            DpCategory::TwoOp if matches!(op, DpOp::Add | DpOp::Sub | DpOp::Rsb) => {
                let nzcv = ir.ir().get_nzcv_from_op(result);
                ir.set_cpsr_nzcv(nzcv);
            }
            _ => {
                // Logic ops: N,Z from result, C from barrel shifter, V unchanged.
                // For MOV/MVN the result is just the shifted value — emit OR with 0
                // to force flag setting (upstream uses the same pattern via
                // SetNZ on the result and SetC from the shifter carry).
                let flags_value = match op {
                    DpOp::Mov | DpOp::Mvn => ir.ir().or_32(result, Value::ImmU32(0)),
                    _ => result,
                };
                let nz = ir.ir().get_nz_from_op(flags_value);
                ir.set_cpsr_nzc(nz, carry);
            }
        }
    }

    // Write result to Rd (test/compare instructions don't write).
    if writes_rd {
        ir.set_register(rd, result);
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::block::Block;
    use crate::ir::location::LocationDescriptor;
    use crate::ir::opcode::Opcode;

    fn immediate_opcodes(raw: u32, id: ArmInstId) -> Vec<Opcode> {
        let mut block = Block::new(LocationDescriptor(0));
        {
            let mut ir = A32IREmitter::new(&mut block);
            assert!(arm_dp_imm(&mut ir, &DecodedArm { raw, id }));
        }
        block.instructions.iter().map(|inst| inst.opcode).collect()
    }

    #[test]
    fn immediate_expansion_matches_upstream_carry_reads() {
        let add = immediate_opcodes(0xe280_1001, ArmInstId::AddImm);
        assert!(!add.contains(&Opcode::A32GetCFlag));

        let adc = immediate_opcodes(0xe2a0_1001, ArmInstId::AdcImm);
        assert_eq!(
            adc.iter()
                .filter(|opcode| **opcode == Opcode::A32GetCFlag)
                .count(),
            1
        );
        let register_read = adc
            .iter()
            .position(|opcode| *opcode == Opcode::A32GetRegister)
            .expect("ADC register read");
        let carry_read = adc
            .iter()
            .position(|opcode| *opcode == Opcode::A32GetCFlag)
            .expect("ADC carry read");
        assert!(register_read < carry_read);

        let and = immediate_opcodes(0xe200_1001, ArmInstId::AndImm);
        assert_eq!(and.first(), Some(&Opcode::A32GetCFlag));
        assert_eq!(
            and.iter()
                .filter(|opcode| **opcode == Opcode::A32GetCFlag)
                .count(),
            1
        );

        let bic = immediate_opcodes(0xe3c0_1001, ArmInstId::BicImm);
        assert!(bic.contains(&Opcode::AndNot32));
        assert!(!bic.contains(&Opcode::Not32));
    }

    #[test]
    fn invalid_pc_destinations_preserve_upstream_unpredictable_ordering() {
        let location = crate::ir::location::A32LocationDescriptor::at(0x1000);
        let mut block = Block::new(location.to_location());
        {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            assert!(!arm_dp_imm(
                &mut ir,
                &DecodedArm {
                    raw: 0xe290_f001,
                    id: ArmInstId::AddImm,
                },
            ));
        }
        assert!(!block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32BXWritePC));
        assert!(!block
            .instructions
            .iter()
            .any(|inst| { matches!(inst.opcode, Opcode::A32SetCpsrNZC | Opcode::A32SetCpsrNZCV) }));

        let mut block = Block::new(location.to_location());
        {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            assert!(!arm_dp_rsr(
                &mut ir,
                &DecodedArm {
                    raw: 0xe0a0_f211,
                    id: ArmInstId::AdcRsr,
                },
            ));
        }
        assert!(!block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32GetRegister));
    }

    #[test]
    fn register_shift_reads_match_upstream_ordering() {
        let mut block = Block::new(LocationDescriptor(0));
        {
            let mut ir = A32IREmitter::new(&mut block);
            assert!(arm_dp_rsr(
                &mut ir,
                &DecodedArm {
                    raw: 0xe0a0_1213,
                    id: ArmInstId::AdcRsr,
                },
            ));
        }

        let opcodes: Vec<_> = block.instructions.iter().map(|inst| inst.opcode).collect();
        assert_eq!(
            &opcodes[..4],
            &[
                Opcode::A32GetRegister,
                Opcode::LeastSignificantByte,
                Opcode::A32GetCFlag,
                Opcode::A32GetRegister,
            ]
        );

        let operand_read = opcodes
            .iter()
            .rposition(|opcode| *opcode == Opcode::A32GetRegister)
            .expect("ADC operand register read");
        let arithmetic_carry = opcodes
            .iter()
            .rposition(|opcode| *opcode == Opcode::A32GetCFlag)
            .expect("ADC arithmetic carry read");
        assert!(operand_read < arithmetic_carry);
    }
}

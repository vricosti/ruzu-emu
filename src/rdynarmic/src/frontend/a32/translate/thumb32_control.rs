use crate::frontend::a32::decoder_thumb32::DecodedThumb32;
use crate::frontend::a32::types::{Exception, Reg};
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

use super::TranslationOptions;

/// Rust counterpart of upstream dynarmic
/// `frontend/A32/translate/impl/thumb32_control.cpp`.

pub fn thumb32_bxj(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let m = inst.rn();
    if m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    super::thumb16::thumb16_bx(ir, m)
}

pub fn thumb32_clrex(ir: &mut A32IREmitter) -> bool {
    ir.clear_exclusive();
    true
}

pub fn thumb32_dmb(ir: &mut A32IREmitter) -> bool {
    ir.data_memory_barrier();
    true
}

pub fn thumb32_dsb(ir: &mut A32IREmitter) -> bool {
    ir.data_synchronization_barrier();
    true
}

pub fn thumb32_isb(ir: &mut A32IREmitter) -> bool {
    let next_pc = ir
        .current_location
        .expect("location not set")
        .pc()
        .wrapping_add(4);
    ir.instruction_synchronization_barrier();
    ir.update_upper_location_descriptor();
    ir.branch_write_pc(Value::ImmU32(next_pc));
    ir.set_term(Terminal::ReturnToDispatch);
    false
}

pub fn thumb32_nop() -> bool {
    true
}

pub fn thumb32_sev(ir: &mut A32IREmitter, options: TranslationOptions) -> bool {
    thumb32_hint(ir, options, Exception::SendEvent)
}

pub fn thumb32_sevl(ir: &mut A32IREmitter, options: TranslationOptions) -> bool {
    thumb32_hint(ir, options, Exception::SendEventLocal)
}

pub fn thumb32_wfe(ir: &mut A32IREmitter, options: TranslationOptions) -> bool {
    thumb32_hint(ir, options, Exception::WaitForEvent)
}

pub fn thumb32_wfi(ir: &mut A32IREmitter, options: TranslationOptions) -> bool {
    thumb32_hint(ir, options, Exception::WaitForInterrupt)
}

pub fn thumb32_yield(ir: &mut A32IREmitter, options: TranslationOptions) -> bool {
    thumb32_hint(ir, options, Exception::Yield)
}

fn thumb32_hint(ir: &mut A32IREmitter, options: TranslationOptions, exception: Exception) -> bool {
    if !options.hook_hint_instructions {
        return true;
    }
    super::raise_exception(ir, exception)
}

pub fn thumb32_udf(ir: &mut A32IREmitter) -> bool {
    // Upstream `thumb32_UDF` delegates to `thumb16_UDF` → `UndefinedInstruction`
    // (not Unpredictable), running the full RaiseException lifecycle
    // (UpdateUpperLocationDescriptor + BranchWritePC(PC+4) + terminal). A
    // Thumb32 instruction is 4 bytes, so PC+4 matches current_instruction_size.
    super::undefined_instruction(ir)
}

pub fn thumb32_msr_reg(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let write_spsr = ((inst.raw >> 20) & 1) != 0;
    let n = inst.rn();
    let mask = (inst.raw >> 8) & 0xF;

    // Upstream `thumb32_MSR_reg`: mask==0 and n==PC are UnpredictableInstruction;
    // write_spsr is UndefinedInstruction. All stop translation (return false)
    // with the full RaiseException lifecycle.
    if mask == 0 {
        return super::unpredictable_instruction(ir);
    }

    if n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    if write_spsr {
        return super::undefined_instruction(ir);
    }

    let write_nzcvq = (mask & 0x8) != 0;
    let write_g = (mask & 0x4) != 0;
    let write_e = (mask & 0x2) != 0;
    let value = ir.get_register(n);

    if !write_e {
        if write_nzcvq {
            let masked = ir.ir().and_32(value, Value::ImmU32(0xF800_0000));
            ir.set_cpsr_nzcvq(masked);
        }

        if write_g {
            let masked = ir.ir().and_32(value, Value::ImmU32(0x000F_0000));
            ir.set_ge_flags_compressed(masked);
        }

        return true;
    }

    ir.update_upper_location_descriptor();

    let cpsr_mask = (if write_nzcvq { 0xF800_0000 } else { 0 })
        | (if write_g { 0x000F_0000 } else { 0 })
        | 0x0000_0200;
    let cpsr = ir.get_cpsr();
    let old_cpsr = ir.ir().and_32(cpsr, Value::ImmU32(!cpsr_mask));
    let new_cpsr = ir.ir().and_32(value, Value::ImmU32(cpsr_mask));
    let merged_cpsr = ir.ir().or_32(old_cpsr, new_cpsr);
    ir.set_cpsr(merged_cpsr);

    let loc = ir.current_location.expect("current_location not set");
    let next_loc = loc.advance_pc(4).advance_it();
    ir.base.push_rsb(next_loc.into());
    ir.branch_write_pc(Value::ImmU32(next_loc.pc()));
    ir.set_term(Terminal::CheckHalt {
        else_: Box::new(Terminal::PopRSBHint),
    });
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::decoder_thumb32::{DecodedThumb32, Thumb32InstId};
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::psr::PSR;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::value::Value;

    fn thumb_loc(pc: u32) -> A32LocationDescriptor {
        let mut psr = PSR::default();
        psr.set_t(true);
        A32LocationDescriptor::new(pc, psr, FPSCR::default(), false)
    }

    #[test]
    fn thumb32_bxj_matches_upstream_validation_and_thumb16_bx_lifecycle() {
        let loc = thumb_loc(0x1000);

        let mut invalid = Block::new(loc.to_location());
        {
            let mut ir = A32IREmitter::with_location(&mut invalid, loc);
            let inst = DecodedThumb32 {
                raw: 0xf3cf_8f00,
                id: Thumb32InstId::BXJ,
            };
            assert!(!thumb32_bxj(&mut ir, &inst));
        }
        assert!(invalid
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32ExceptionRaised));
        assert!(!invalid
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32GetRegister));

        let mut valid = Block::new(loc.to_location());
        {
            let mut ir = A32IREmitter::with_location(&mut valid, loc);
            let inst = DecodedThumb32 {
                raw: 0xf3ce_8f00,
                id: Thumb32InstId::BXJ,
            };
            assert!(!thumb32_bxj(&mut ir, &inst));
        }
        assert!(valid
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32BXWritePC));
        assert!(matches!(valid.terminal, Terminal::PopRSBHint));
    }

    /// Assert `f(raw)` stops translation raising exactly `expected` with the
    /// full RaiseException lifecycle (descriptor update + PC write + terminal).
    fn assert_thumb32_exception(
        raw: u32,
        f: fn(&mut A32IREmitter, &DecodedThumb32) -> bool,
        expected: crate::frontend::a32::types::Exception,
    ) {
        use crate::ir::terminal::Terminal;
        let loc = thumb_loc(0x1000);
        let mut block = Block::new(loc.to_location());
        let cont = {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            f(
                &mut ir,
                &DecodedThumb32 {
                    raw,
                    id: Thumb32InstId::Unknown,
                },
            )
        };
        assert!(!cont);
        let ops: Vec<_> = block.instructions.iter().map(|i| i.opcode).collect();
        assert!(ops.contains(&Opcode::A32UpdateUpperLocationDescriptor));
        assert!(ops.contains(&Opcode::A32SetRegister));
        assert_eq!(ops.last(), Some(&Opcode::A32ExceptionRaised));
        let exc = block
            .instructions
            .iter()
            .rev()
            .find(|i| i.opcode == Opcode::A32ExceptionRaised)
            .expect("exception present");
        match exc.args[1] {
            Value::ImmU64(code) => assert_eq!(code, expected.as_u32() as u64),
            ref other => panic!("exception code not immediate: {other:?}"),
        }
        assert!(matches!(
            block.terminal,
            Terminal::CheckHalt { ref else_ }
                if matches!(else_.as_ref(), Terminal::ReturnToDispatch)
        ));
    }

    #[test]
    fn thumb32_msr_mrs_exception_kinds_match_upstream() {
        use crate::frontend::a32::types::Exception::{
            UndefinedInstruction, UnpredictableInstruction,
        };
        // MSR: mask==0 and n==PC → Unpredictable; write_spsr → Undefined.
        assert_thumb32_exception(0x0001_0000, thumb32_msr_reg, UnpredictableInstruction); // mask==0
        assert_thumb32_exception(0x000F_0800, thumb32_msr_reg, UnpredictableInstruction); // n==PC, mask!=0
        assert_thumb32_exception(0x0010_0800, thumb32_msr_reg, UndefinedInstruction); // write_spsr
                                                                                      // MRS: d==PC → Unpredictable; read_spsr → Undefined.
        assert_thumb32_exception(0x0000_0F00, thumb32_mrs_reg, UnpredictableInstruction); // d==PC
        assert_thumb32_exception(0x0010_0100, thumb32_mrs_reg, UndefinedInstruction);
        // read_spsr
    }

    #[test]
    fn thumb32_udf_raises_undefined_with_full_lifecycle() {
        use crate::ir::terminal::Terminal;
        let loc = thumb_loc(0x1000);
        let mut block = Block::new(loc.to_location());
        let should_continue = {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            thumb32_udf(&mut ir)
        };

        assert!(!should_continue);
        let ops: Vec<_> = block.instructions.iter().map(|i| i.opcode).collect();
        // Full RaiseException lifecycle, not the bare opcode + terminal.
        assert!(ops.contains(&Opcode::A32UpdateUpperLocationDescriptor));
        assert!(ops.contains(&Opcode::A32SetRegister)); // BranchWritePC(PC+4)
        assert_eq!(ops.last(), Some(&Opcode::A32ExceptionRaised));
        // Exact kind = UndefinedInstruction (was wrongly Unpredictable).
        let exc = block
            .instructions
            .iter()
            .rev()
            .find(|i| i.opcode == Opcode::A32ExceptionRaised)
            .expect("exception present");
        match exc.args[1] {
            Value::ImmU64(code) => assert_eq!(
                code,
                crate::frontend::a32::types::Exception::UndefinedInstruction.as_u32() as u64
            ),
            ref other => panic!("exception code not immediate: {other:?}"),
        }
        assert!(matches!(
            block.terminal,
            Terminal::CheckHalt { ref else_ }
                if matches!(else_.as_ref(), Terminal::ReturnToDispatch)
        ));
    }

    #[test]
    fn thumb32_isb_uses_branch_write_pc() {
        let loc = thumb_loc(0x1000);
        let mut block = Block::new(loc.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, loc);

        assert!(!thumb32_isb(&mut ir));
        assert_eq!(
            block.instructions.last().map(|inst| inst.opcode),
            Some(Opcode::A32SetRegister)
        );
        assert_eq!(
            block.instructions.last().map(|inst| inst.args[0]),
            Some(Value::ImmA32Reg(Reg::R15))
        );
    }

    #[test]
    fn thumb32_msr_reg_write_e_uses_branch_write_pc() {
        let loc = thumb_loc(0x2000);
        let mut block = Block::new(loc.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, loc);
        let inst = DecodedThumb32 {
            raw: (1 << 9) | (1 << 8),
            id: Thumb32InstId::MsrReg,
        };

        assert!(!thumb32_msr_reg(&mut ir, &inst));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::PushRSB));
        assert_eq!(
            block.instructions.last().map(|inst| inst.opcode),
            Some(Opcode::A32SetRegister)
        );
        assert!(matches!(
            &block.terminal,
            Terminal::CheckHalt { else_ } if matches!(else_.as_ref(), Terminal::PopRSBHint)
        ));
    }
}

pub fn thumb32_mrs_reg(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let read_spsr = ((inst.raw >> 20) & 1) != 0;
    let d = inst.rd();

    // Upstream `thumb32_MRS_reg`: d==PC is UnpredictableInstruction; read_spsr is
    // UndefinedInstruction. Both stop translation (return false) with the full
    // RaiseException lifecycle.
    if d == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    if read_spsr {
        return super::undefined_instruction(ir);
    }

    let cpsr = ir.get_cpsr();
    ir.set_register(d, cpsr);
    true
}

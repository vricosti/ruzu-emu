// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `shader_recompiler/frontend/maxwell/translate/impl/not_implemented.cpp`.
//!
//! Instructions that are recognized by the decoder but not yet implemented
//! in the upstream translator. Failures retain the shader exception type so
//! pipeline creation can catch them without terminating the GPU worker.

use super::TranslatorVisitor;
use crate::exception::NotImplementedException;
use crate::frontend::maxwell_opcodes::MaxwellOpcode;

/// Upstream file-local `ThrowNotImplemented(Opcode)`.
fn throw_not_implemented(opcode: MaxwellOpcode) -> ! {
    std::panic::panic_any(NotImplementedException::new(format!(
        "Instruction {} is not implemented",
        opcode
    )));
}

impl<'a> TranslatorVisitor<'a> {
    pub fn translate_atoms_cas(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::ATOMS_cas);
    }

    /// NOP — No operation. Upstream is a no-op.
    pub fn translate_nop(&mut self, _insn: u64) {
        // NOP is No-Op.
    }

    /// CAL — Call subroutine. Upstream is a no-op.
    pub fn translate_cal(&mut self, _insn: u64) {
        // CAL is a no-op
    }

    /// KIL — Kill thread. Upstream is a no-op.
    pub fn translate_kil(&mut self, _insn: u64) {
        // KIL is a no-op
    }

    /// PBK — Pre-break. Upstream is a no-op.
    pub fn translate_pbk(&mut self, _insn: u64) {
        // PBK is a no-op
    }

    /// PCNT — Pre-continue. Upstream is a no-op.
    pub fn translate_pcnt(&mut self, _insn: u64) {
        // PCNT is a no-op
    }

    /// SSY — Set synchronization point. Upstream is a no-op.
    pub fn translate_ssy(&mut self, _insn: u64) {
        // SSY is a no-op
    }

    /// RAM — Upstream is stubbed with a warning.
    pub fn translate_ram(&mut self, _insn: u64) {
        log::warn!("(STUBBED) RAM Instruction");
    }

    /// SAM — Upstream is stubbed with a warning.
    pub fn translate_sam(&mut self, _insn: u64) {
        log::warn!("(STUBBED) SAM Instruction");
    }

    /// B2R — Not implemented in upstream.
    pub fn translate_b2r(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::B2R);
    }

    /// BPT — Not implemented in upstream.
    pub fn translate_bpt(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::BPT);
    }

    pub fn translate_bra(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::BRA);
    }

    pub fn translate_brk(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::BRK);
    }

    /// CCTL — Not implemented in upstream.
    pub fn translate_cctl(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::CCTL);
    }

    /// CCTLL — Not implemented in upstream.
    pub fn translate_cctll(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::CCTLL);
    }

    /// CCTLT — Not implemented in upstream.
    pub fn translate_cctlt(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::CCTLT);
    }

    pub fn translate_cont(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::CONT);
    }

    /// CS2R — Not implemented in upstream.
    pub fn translate_cs2r(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::CS2R);
    }

    pub fn translate_fchk_reg(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::FCHK_reg);
    }

    pub fn translate_fchk_cbuf(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::FCHK_cbuf);
    }

    pub fn translate_fchk_imm(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::FCHK_imm);
    }

    /// GETCRSPTR — Not implemented in upstream.
    pub fn translate_getcrsptr(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::GETCRSPTR);
    }

    /// GETLMEMBASE — Not implemented in upstream.
    pub fn translate_getlmembase(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::GETLMEMBASE);
    }

    /// IDE — Not implemented in upstream.
    pub fn translate_ide(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::IDE);
    }

    pub fn translate_idp_reg(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::IDP_reg);
    }

    pub fn translate_idp_imm(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::IDP_imm);
    }

    pub fn translate_imadsp_reg(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::IMADSP_reg);
    }

    pub fn translate_imadsp_rc(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::IMADSP_rc);
    }

    pub fn translate_imadsp_cr(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::IMADSP_cr);
    }

    pub fn translate_imadsp_imm(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::IMADSP_imm);
    }

    pub fn translate_imad_reg(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::IMAD_reg);
    }

    pub fn translate_imad_rc(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::IMAD_rc);
    }

    pub fn translate_imad_cr(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::IMAD_cr);
    }

    pub fn translate_imad_imm(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::IMAD_imm);
    }

    pub fn translate_imad32i(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::IMAD32I);
    }

    pub fn translate_imul_reg(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::IMUL_reg);
    }

    pub fn translate_imul_cbuf(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::IMUL_cbuf);
    }

    pub fn translate_imul_imm(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::IMUL_imm);
    }

    pub fn translate_imul32i(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::IMUL32I);
    }

    /// JCAL — Not implemented in upstream.
    pub fn translate_jcal(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::JCAL);
    }

    /// JMP — Not implemented in upstream.
    pub fn translate_jmp(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::JMP);
    }

    /// LD — Not implemented in upstream.
    pub fn translate_ld(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::LD);
    }

    /// LEPC — Not implemented in upstream.
    pub fn translate_lepc(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::LEPC);
    }

    /// LONGJMP — Not implemented in upstream.
    pub fn translate_longjmp(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::LONGJMP);
    }

    /// PEXIT — Not implemented in upstream.
    pub fn translate_pexit(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::PEXIT);
    }

    /// PLONGJMP — Not implemented in upstream.
    pub fn translate_plongjmp(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::PLONGJMP);
    }

    /// PRET — Not implemented in upstream.
    pub fn translate_pret(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::PRET);
    }

    pub fn translate_prmt_reg(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::PRMT_reg);
    }

    pub fn translate_prmt_rc(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::PRMT_rc);
    }

    pub fn translate_prmt_cr(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::PRMT_cr);
    }

    pub fn translate_prmt_imm(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::PRMT_imm);
    }

    /// R2B — Not implemented in upstream.
    pub fn translate_r2b(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::R2B);
    }

    /// RET — Not implemented in upstream.
    pub fn translate_ret(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::RET);
    }

    /// RTT — Not implemented in upstream.
    pub fn translate_rtt(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::RTT);
    }

    /// SETCRSPTR — Not implemented in upstream.
    pub fn translate_setcrsptr(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::SETCRSPTR);
    }

    /// SETLMEMBASE — Not implemented in upstream.
    pub fn translate_setlmembase(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::SETLMEMBASE);
    }

    /// ST — Not implemented in upstream.
    pub fn translate_st(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::ST);
    }

    /// STP — Not implemented in upstream.
    pub fn translate_stp(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::STP);
    }

    pub fn translate_suatom_cas(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::SUATOM_cas);
    }

    /// SYNC — Not implemented in upstream.
    pub fn translate_sync(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::SYNC);
    }

    /// TXA — Not implemented in upstream.
    pub fn translate_txa(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::TXA);
    }

    /// VABSDIFF — Not implemented in upstream.
    pub fn translate_vabsdiff(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::VABSDIFF);
    }

    /// VABSDIFF4 — Not implemented in upstream.
    pub fn translate_vabsdiff4(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::VABSDIFF4);
    }

    pub fn translate_vadd(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::VADD);
    }

    pub fn translate_vset(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::VSET);
    }

    pub fn translate_vshl(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::VSHL);
    }

    pub fn translate_vshr(&mut self, _insn: u64) {
        throw_not_implemented(MaxwellOpcode::VSHR);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{program::Program, types::ShaderStage};
    use std::panic::{catch_unwind, AssertUnwindSafe};

    #[test]
    fn imad32i_dispatch_raises_the_upstream_shader_exception() {
        let mut program = Program::new(ShaderStage::Fragment);
        let block = program.add_block();
        let mut visitor = TranslatorVisitor::new(&mut program, block);
        let payload = catch_unwind(AssertUnwindSafe(|| {
            // maxwell.inc: IMAD32I "1000 00-- ---- ----".
            visitor.translate_instruction(0x8000_0000_0007_0000);
        }))
        .expect_err("IMAD32I must reject this shader");
        let error = payload
            .downcast_ref::<NotImplementedException>()
            .expect("shader failure must retain its recoverable exception type");
        // Upstream ThrowNotImplemented includes this suffix, and the
        // NotImplementedException constructor appends it again.
        assert_eq!(
            error.to_string(),
            "Instruction IMAD32I is not implemented is not implemented"
        );
        assert!(visitor.ir.program.block(block).is_empty());
    }

    #[test]
    fn upstream_no_ops_do_not_emit_ir_or_throw() {
        let mut program = Program::new(ShaderStage::Fragment);
        let block = program.add_block();
        let mut visitor = TranslatorVisitor::new(&mut program, block);
        visitor.translate_nop(0);
        visitor.translate_cal(0);
        visitor.translate_kil(0);
        visitor.translate_pbk(0);
        visitor.translate_pcnt(0);
        visitor.translate_ssy(0);
        assert!(visitor.ir.program.block(block).is_empty());
    }
}

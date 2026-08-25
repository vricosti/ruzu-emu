// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Not-implemented instruction stubs — maps to zuyu's
//! `frontend/maxwell/translate/impl/not_implemented.cpp`.
//!
//! Instructions that are recognized by the decoder but not yet implemented
//! in the translator. Each function logs a warning or panics, matching
//! upstream behavior.

use super::TranslatorVisitor;

impl<'a> TranslatorVisitor<'a> {
    pub fn translate_atom_cas(&mut self, _insn: u64) {
        panic!("Instruction ATOM_cas not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_atoms_cas(&mut self, _insn: u64) {
        panic!("Instruction ATOMS_cas not implemented (upstream throws NotImplementedException)");
    }

    /// CCTLT — Not implemented in upstream.
    pub fn translate_cctlt(&mut self, _insn: u64) {
        panic!("Instruction CCTLT not implemented (upstream throws NotImplementedException)");
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
        panic!("Instruction B2R not implemented (upstream throws NotImplementedException)");
    }

    /// BPT — Not implemented in upstream.
    pub fn translate_bpt(&mut self, _insn: u64) {
        panic!("Instruction BPT not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_bra(&mut self, _insn: u64) {
        panic!("Instruction BRA not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_brk(&mut self, _insn: u64) {
        panic!("Instruction BRK not implemented (upstream throws NotImplementedException)");
    }

    /// CCTL — Not implemented in upstream.
    pub fn translate_cctl(&mut self, _insn: u64) {
        panic!("Instruction CCTL not implemented (upstream throws NotImplementedException)");
    }

    /// CCTLL — Not implemented in upstream.
    pub fn translate_cctll(&mut self, _insn: u64) {
        panic!("Instruction CCTLL not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_cont(&mut self, _insn: u64) {
        panic!("Instruction CONT not implemented (upstream throws NotImplementedException)");
    }

    /// CS2R — Not implemented in upstream.
    pub fn translate_cs2r(&mut self, _insn: u64) {
        panic!("Instruction CS2R not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_fchk_reg(&mut self, _insn: u64) {
        panic!("Instruction FCHK_reg not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_fchk_cbuf(&mut self, _insn: u64) {
        panic!("Instruction FCHK_cbuf not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_fchk_imm(&mut self, _insn: u64) {
        panic!("Instruction FCHK_imm not implemented (upstream throws NotImplementedException)");
    }

    /// GETCRSPTR — Not implemented in upstream.
    pub fn translate_getcrsptr(&mut self, _insn: u64) {
        panic!("Instruction GETCRSPTR not implemented (upstream throws NotImplementedException)");
    }

    /// GETLMEMBASE — Not implemented in upstream.
    pub fn translate_getlmembase(&mut self, _insn: u64) {
        panic!("Instruction GETLMEMBASE not implemented (upstream throws NotImplementedException)");
    }

    /// IDE — Not implemented in upstream.
    pub fn translate_ide(&mut self, _insn: u64) {
        panic!("Instruction IDE not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_idp_reg(&mut self, _insn: u64) {
        panic!("Instruction IDP_reg not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_idp_imm(&mut self, _insn: u64) {
        panic!("Instruction IDP_imm not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_imadsp_reg(&mut self, _insn: u64) {
        panic!("Instruction IMADSP_reg not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_imadsp_rc(&mut self, _insn: u64) {
        panic!("Instruction IMADSP_rc not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_imadsp_cr(&mut self, _insn: u64) {
        panic!("Instruction IMADSP_cr not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_imadsp_imm(&mut self, _insn: u64) {
        panic!("Instruction IMADSP_imm not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_imad_reg(&mut self, _insn: u64) {
        panic!("Instruction IMAD_reg not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_imad_rc(&mut self, _insn: u64) {
        panic!("Instruction IMAD_rc not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_imad_cr(&mut self, _insn: u64) {
        panic!("Instruction IMAD_cr not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_imad_imm(&mut self, _insn: u64) {
        panic!("Instruction IMAD_imm not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_imad32i(&mut self, _insn: u64) {
        panic!("Instruction IMAD32I not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_imul_reg(&mut self, _insn: u64) {
        panic!("Instruction IMUL_reg not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_imul_cbuf(&mut self, _insn: u64) {
        panic!("Instruction IMUL_cbuf not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_imul_imm(&mut self, _insn: u64) {
        panic!("Instruction IMUL_imm not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_imul32i(&mut self, _insn: u64) {
        panic!("Instruction IMUL32I not implemented (upstream throws NotImplementedException)");
    }

    /// JCAL — Not implemented in upstream.
    pub fn translate_jcal(&mut self, _insn: u64) {
        panic!("Instruction JCAL not implemented (upstream throws NotImplementedException)");
    }

    /// JMP — Not implemented in upstream.
    pub fn translate_jmp(&mut self, _insn: u64) {
        panic!("Instruction JMP not implemented (upstream throws NotImplementedException)");
    }

    /// LD — Not implemented in upstream.
    pub fn translate_ld(&mut self, _insn: u64) {
        panic!("Instruction LD not implemented (upstream throws NotImplementedException)");
    }

    /// LEPC — Not implemented in upstream.
    pub fn translate_lepc(&mut self, _insn: u64) {
        panic!("Instruction LEPC not implemented (upstream throws NotImplementedException)");
    }

    /// LONGJMP — Not implemented in upstream.
    pub fn translate_longjmp(&mut self, _insn: u64) {
        panic!("Instruction LONGJMP not implemented (upstream throws NotImplementedException)");
    }

    /// PEXIT — Not implemented in upstream.
    pub fn translate_pexit(&mut self, _insn: u64) {
        panic!("Instruction PEXIT not implemented (upstream throws NotImplementedException)");
    }

    /// PLONGJMP — Not implemented in upstream.
    pub fn translate_plongjmp(&mut self, _insn: u64) {
        panic!("Instruction PLONGJMP not implemented (upstream throws NotImplementedException)");
    }

    /// PRET — Not implemented in upstream.
    pub fn translate_pret(&mut self, _insn: u64) {
        panic!("Instruction PRET not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_prmt_reg(&mut self, _insn: u64) {
        panic!("Instruction PRMT_reg not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_prmt_rc(&mut self, _insn: u64) {
        panic!("Instruction PRMT_rc not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_prmt_cr(&mut self, _insn: u64) {
        panic!("Instruction PRMT_cr not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_prmt_imm(&mut self, _insn: u64) {
        panic!("Instruction PRMT_imm not implemented (upstream throws NotImplementedException)");
    }

    /// R2B — Not implemented in upstream.
    pub fn translate_r2b(&mut self, _insn: u64) {
        panic!("Instruction R2B not implemented (upstream throws NotImplementedException)");
    }

    /// RET — Not implemented in upstream.
    pub fn translate_ret(&mut self, _insn: u64) {
        panic!("Instruction RET not implemented (upstream throws NotImplementedException)");
    }

    /// RTT — Not implemented in upstream.
    pub fn translate_rtt(&mut self, _insn: u64) {
        panic!("Instruction RTT not implemented (upstream throws NotImplementedException)");
    }

    /// SETCRSPTR — Not implemented in upstream.
    pub fn translate_setcrsptr(&mut self, _insn: u64) {
        panic!("Instruction SETCRSPTR not implemented (upstream throws NotImplementedException)");
    }

    /// SETLMEMBASE — Not implemented in upstream.
    pub fn translate_setlmembase(&mut self, _insn: u64) {
        panic!("Instruction SETLMEMBASE not implemented (upstream throws NotImplementedException)");
    }

    /// ST — Not implemented in upstream.
    pub fn translate_st(&mut self, _insn: u64) {
        panic!("Instruction ST not implemented (upstream throws NotImplementedException)");
    }

    /// STP — Not implemented in upstream.
    pub fn translate_stp(&mut self, _insn: u64) {
        panic!("Instruction STP not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_suatom_cas(&mut self, _insn: u64) {
        panic!("Instruction SUATOM_cas not implemented (upstream throws NotImplementedException)");
    }

    /// SYNC — Not implemented in upstream.
    pub fn translate_sync(&mut self, _insn: u64) {
        panic!("Instruction SYNC not implemented (upstream throws NotImplementedException)");
    }

    /// TXA — Not implemented in upstream.
    pub fn translate_txa(&mut self, _insn: u64) {
        panic!("Instruction TXA not implemented (upstream throws NotImplementedException)");
    }

    /// VABSDIFF — Not implemented in upstream.
    pub fn translate_vabsdiff(&mut self, _insn: u64) {
        panic!("Instruction VABSDIFF not implemented (upstream throws NotImplementedException)");
    }

    /// VABSDIFF4 — Not implemented in upstream.
    pub fn translate_vabsdiff4(&mut self, _insn: u64) {
        panic!("Instruction VABSDIFF4 not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_vadd(&mut self, _insn: u64) {
        panic!("Instruction VADD not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_vset(&mut self, _insn: u64) {
        panic!("Instruction VSET not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_vshl(&mut self, _insn: u64) {
        panic!("Instruction VSHL not implemented (upstream throws NotImplementedException)");
    }

    pub fn translate_vshr(&mut self, _insn: u64) {
        panic!("Instruction VSHR not implemented (upstream throws NotImplementedException)");
    }
}

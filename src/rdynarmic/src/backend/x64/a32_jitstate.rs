//! x64 A32 generated-code state.
//!
//! Upstream owners: `backend/x64/a32_jitstate.h` and `a32_jitstate.cpp`.

use crate::backend::x64::nzcv_util;

const MXCSR_RMODE: [u32; 4] = [0x0000, 0x4000, 0x2000, 0x6000];
const MXCSR_FLUSH_TO_ZERO: u32 = 1 << 15;
const MXCSR_DENORMALS_ARE_ZERO: u32 = 1 << 6;
const FPSCR_QC_BIT: u32 = 27;
const FPSCR_MODE_MASK: u32 = 0x07F7_0000;
const FPSCR_NZCV_MASK: u32 = 0xF000_0000;

const RSB_SIZE: usize = 8;

#[repr(C, align(16))]
pub struct A32JitState {
    pub reg: [u32; 16],
    pub upper_location_descriptor: u32,
    pub cpsr_ge: u32,
    pub cpsr_q: u32,
    pub cpsr_nzcv: u32,
    pub cpsr_jaifm: u32,
    _pad_ext_reg: [u8; 12],
    pub ext_reg: [u32; 64],
    pub guest_mxcsr: u32,
    pub asimd_mxcsr: u32,
    pub halt_reason: u32,
    pub exclusive_state: u32,
    pub rsb_ptr: u32,
    pub rsb_location_descriptors: [u64; RSB_SIZE],
    pub rsb_codeptrs: [u64; RSB_SIZE],
    pub fpsr_exc: u32,
    pub fpsr_qc: u32,
    pub fpsr_nzcv: u32,
}

impl A32JitState {
    pub const RSB_SIZE: usize = RSB_SIZE;
    pub const RSB_PTR_MASK: usize = Self::RSB_SIZE - 1;

    pub fn new() -> Self {
        let mut state = Self {
            reg: [0; 16],
            upper_location_descriptor: 0,
            cpsr_ge: 0,
            cpsr_q: 0,
            cpsr_nzcv: 0,
            cpsr_jaifm: 0,
            _pad_ext_reg: [0; 12],
            ext_reg: [0; 64],
            guest_mxcsr: 0x0000_1F80,
            asimd_mxcsr: 0x0000_9FC0,
            halt_reason: 0,
            exclusive_state: 0,
            rsb_ptr: 0,
            rsb_location_descriptors: [0; RSB_SIZE],
            rsb_codeptrs: [0; RSB_SIZE],
            fpsr_exc: 0,
            fpsr_qc: 0,
            fpsr_nzcv: 0,
        };
        state.reset_rsb();
        state
    }

    pub fn get_cpsr(&self) -> u32 {
        debug_assert_eq!(self.cpsr_q & !1, 0);
        debug_assert_eq!(self.cpsr_jaifm & !0x0100_01DF, 0);

        let mut cpsr = nzcv_util::from_x64(self.cpsr_nzcv);
        cpsr |= ((self.cpsr_q != 0) as u32) << 27;
        cpsr |= ((self.cpsr_ge >> 31) & 1) << 19;
        cpsr |= ((self.cpsr_ge >> 23) & 1) << 18;
        cpsr |= ((self.cpsr_ge >> 15) & 1) << 17;
        cpsr |= ((self.cpsr_ge >> 7) & 1) << 16;
        cpsr |= ((self.upper_location_descriptor >> 1) & 1) << 9;
        cpsr |= (self.upper_location_descriptor & 1) << 5;
        cpsr |= self.upper_location_descriptor & 0b11111100_00000000;
        cpsr |= (self.upper_location_descriptor & 0b00000011_00000000) << 17;
        cpsr | self.cpsr_jaifm
    }

    pub fn set_cpsr(&mut self, cpsr: u32) {
        self.cpsr_nzcv = nzcv_util::to_x64(cpsr);
        self.cpsr_q = (cpsr >> 27) & 1;
        self.cpsr_ge = 0;
        self.cpsr_ge |= if cpsr & (1 << 19) != 0 {
            0xFF00_0000
        } else {
            0
        };
        self.cpsr_ge |= if cpsr & (1 << 18) != 0 {
            0x00FF_0000
        } else {
            0
        };
        self.cpsr_ge |= if cpsr & (1 << 17) != 0 {
            0x0000_FF00
        } else {
            0
        };
        self.cpsr_ge |= if cpsr & (1 << 16) != 0 {
            0x0000_00FF
        } else {
            0
        };

        self.upper_location_descriptor &= 0xFFFF_0000;
        self.upper_location_descriptor |= ((cpsr >> 9) & 1) << 1;
        self.upper_location_descriptor |= (cpsr >> 5) & 1;
        self.upper_location_descriptor |= cpsr & 0b11111100_00000000;
        self.upper_location_descriptor |= (cpsr >> 17) & 0b00000011_00000000;
        self.cpsr_jaifm = cpsr & 0x0100_01DF;
    }

    pub fn reset_rsb(&mut self) {
        self.rsb_location_descriptors.fill(u64::MAX);
        self.rsb_codeptrs.fill(0);
    }

    pub fn get_fpscr(&self) -> u32 {
        debug_assert_eq!(self.fpsr_nzcv & !FPSCR_NZCV_MASK, 0);
        let mxcsr = self.guest_mxcsr | self.asimd_mxcsr;
        let mut fpscr = (self.upper_location_descriptor & FPSCR_MODE_MASK) | self.fpsr_nzcv;
        fpscr |= mxcsr & 0b0000_0000_0001;
        fpscr |= (mxcsr & 0b0000_0011_1100) >> 1;
        fpscr |= self.fpsr_exc;
        fpscr |= ((self.fpsr_qc != 0) as u32) << FPSCR_QC_BIT;
        fpscr
    }

    pub fn set_fpscr(&mut self, fpscr: u32) {
        self.upper_location_descriptor &= 0x0000_FFFF;
        self.upper_location_descriptor |= fpscr & FPSCR_MODE_MASK;
        self.fpsr_nzcv = fpscr & FPSCR_NZCV_MASK;
        self.fpsr_qc = (fpscr >> FPSCR_QC_BIT) & 1;
        self.guest_mxcsr = 0x0000_1F80;
        self.asimd_mxcsr = 0x0000_9FC0;
        self.guest_mxcsr |= MXCSR_RMODE[((fpscr >> 22) & 3) as usize];
        self.fpsr_exc = fpscr & 0x9F;
        if fpscr & (1 << 24) != 0 {
            self.guest_mxcsr |= MXCSR_FLUSH_TO_ZERO;
            self.guest_mxcsr |= MXCSR_DENORMALS_ARE_ZERO;
        }
    }

    pub fn get_unique_hash(&self) -> u64 {
        ((self.upper_location_descriptor as u64) << 32) | self.reg[15] as u64
    }

    pub fn transfer_jit_state(&mut self, src: &Self, reset_rsb: bool) {
        self.reg = src.reg;
        self.upper_location_descriptor = src.upper_location_descriptor;
        self.cpsr_ge = src.cpsr_ge;
        self.cpsr_q = src.cpsr_q;
        self.cpsr_nzcv = src.cpsr_nzcv;
        self.cpsr_jaifm = src.cpsr_jaifm;
        self.ext_reg = src.ext_reg;
        self.guest_mxcsr = src.guest_mxcsr;
        self.asimd_mxcsr = src.asimd_mxcsr;
        self.fpsr_exc = src.fpsr_exc;
        self.fpsr_qc = src.fpsr_qc;
        self.fpsr_nzcv = src.fpsr_nzcv;
        self.exclusive_state = 0;

        if reset_rsb {
            self.reset_rsb();
        } else {
            self.rsb_ptr = src.rsb_ptr;
            self.rsb_location_descriptors = src.rsb_location_descriptors;
            self.rsb_codeptrs = src.rsb_codeptrs;
        }
    }

    pub const fn offset_of_reg() -> usize {
        0
    }
    pub const fn offset_of_ext_reg() -> usize {
        core::mem::offset_of!(Self, ext_reg)
    }
    pub const fn offset_of_cpsr_nzcv() -> usize {
        core::mem::offset_of!(Self, cpsr_nzcv)
    }
    pub const fn offset_of_cpsr_q() -> usize {
        core::mem::offset_of!(Self, cpsr_q)
    }
    pub const fn offset_of_cpsr_ge() -> usize {
        core::mem::offset_of!(Self, cpsr_ge)
    }
    pub const fn offset_of_cpsr_jaifm() -> usize {
        core::mem::offset_of!(Self, cpsr_jaifm)
    }
    pub const fn offset_of_upper_location_descriptor() -> usize {
        core::mem::offset_of!(Self, upper_location_descriptor)
    }
    pub const fn offset_of_fpsr_nzcv() -> usize {
        core::mem::offset_of!(Self, fpsr_nzcv)
    }
    pub const fn offset_of_fpsr_exc() -> usize {
        core::mem::offset_of!(Self, fpsr_exc)
    }
    pub const fn offset_of_fpsr_qc() -> usize {
        core::mem::offset_of!(Self, fpsr_qc)
    }
    pub const fn offset_of_guest_mxcsr() -> usize {
        core::mem::offset_of!(Self, guest_mxcsr)
    }
    pub const fn offset_of_asimd_mxcsr() -> usize {
        core::mem::offset_of!(Self, asimd_mxcsr)
    }
    pub const fn offset_of_halt_reason() -> usize {
        core::mem::offset_of!(Self, halt_reason)
    }
    pub const fn offset_of_exclusive_state() -> usize {
        core::mem::offset_of!(Self, exclusive_state)
    }
    pub const fn offset_of_rsb_ptr() -> usize {
        core::mem::offset_of!(Self, rsb_ptr)
    }
    pub const fn offset_of_rsb_location_descriptors() -> usize {
        core::mem::offset_of!(Self, rsb_location_descriptors)
    }
    pub const fn offset_of_rsb_codeptrs() -> usize {
        core::mem::offset_of!(Self, rsb_codeptrs)
    }

    pub const fn reg_offset(index: usize) -> usize {
        Self::offset_of_reg() + index * 4
    }

    pub const fn ext_reg_offset(index: usize) -> usize {
        Self::offset_of_ext_reg() + index * 4
    }
}

impl Default for A32JitState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_layout_matches_upstream() {
        assert_eq!(core::mem::align_of::<A32JitState>(), 16);
        assert_eq!(core::mem::size_of::<A32JitState>(), 528);
        assert_eq!(A32JitState::offset_of_reg(), 0);
        assert_eq!(A32JitState::offset_of_upper_location_descriptor(), 64);
        assert_eq!(A32JitState::offset_of_cpsr_ge(), 68);
        assert_eq!(A32JitState::offset_of_cpsr_q(), 72);
        assert_eq!(A32JitState::offset_of_cpsr_nzcv(), 76);
        assert_eq!(A32JitState::offset_of_cpsr_jaifm(), 80);
        assert_eq!(A32JitState::offset_of_ext_reg(), 96);
        assert_eq!(A32JitState::offset_of_guest_mxcsr(), 352);
        assert_eq!(A32JitState::offset_of_asimd_mxcsr(), 356);
        assert_eq!(A32JitState::offset_of_halt_reason(), 360);
        assert_eq!(A32JitState::offset_of_exclusive_state(), 364);
        assert_eq!(A32JitState::offset_of_rsb_ptr(), 368);
        assert_eq!(A32JitState::offset_of_rsb_location_descriptors(), 376);
        assert_eq!(A32JitState::offset_of_rsb_codeptrs(), 440);
        assert_eq!(A32JitState::offset_of_fpsr_exc(), 504);
        assert_eq!(A32JitState::offset_of_fpsr_qc(), 508);
        assert_eq!(A32JitState::offset_of_fpsr_nzcv(), 512);
    }

    #[test]
    fn cpsr_and_fpscr_match_upstream() {
        let mut state = A32JitState::new();
        for nzcv in 0..16 {
            state.set_cpsr(nzcv << 28);
            assert_eq!(state.get_cpsr() & 0xF000_0000, nzcv << 28);
        }
        state.set_cpsr(0x0005_0000);
        assert_eq!(state.cpsr_ge, 0x00FF_00FF);
        let fpscr = 0xB000_0000 | (1 << 27) | (1 << 24) | (1 << 22) | 0x91;
        state.set_fpscr(fpscr);
        assert_eq!(
            state.get_fpscr(),
            fpscr & (FPSCR_MODE_MASK | FPSCR_NZCV_MASK | (1 << 27) | 0x9F)
        );
    }

    #[test]
    fn transfer_preserves_upstream_copy_and_reset_contract() {
        let mut src = A32JitState::new();
        src.reg[0] = 7;
        src.ext_reg[1] = 9;
        src.exclusive_state = 1;
        src.halt_reason = 0x55;
        src.rsb_ptr = 3;
        src.rsb_location_descriptors[3] = 0x1234;

        let mut dst = A32JitState::new();
        dst.halt_reason = 0xAA;
        dst.transfer_jit_state(&src, false);
        assert_eq!(dst.reg[0], 7);
        assert_eq!(dst.ext_reg[1], 9);
        assert_eq!(dst.exclusive_state, 0);
        assert_eq!(dst.halt_reason, 0xAA);
        assert_eq!(dst.rsb_ptr, 3);
        assert_eq!(dst.rsb_location_descriptors[3], 0x1234);

        dst.transfer_jit_state(&src, true);
        assert!(dst
            .rsb_location_descriptors
            .iter()
            .all(|&value| value == u64::MAX));
        assert!(dst.rsb_codeptrs.iter().all(|&value| value == 0));
    }
}

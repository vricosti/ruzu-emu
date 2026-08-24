//! x64 A64 generated-code state.
//!
//! Upstream owners: `backend/x64/a64_jitstate.h` and `a64_jitstate.cpp`.

use crate::backend::x64::nzcv_util;

const FPCR_MASK: u32 = 0x07C8_9F00;
const MXCSR_RMODE: [u32; 4] = [0x0000, 0x4000, 0x2000, 0x6000];
const MXCSR_FLUSH_TO_ZERO: u32 = 1 << 15;
const MXCSR_DENORMALS_ARE_ZERO: u32 = 1 << 6;
const MXCSR_EXCEPTION_MASK: u32 = 0x1F80;
const MXCSR_EXCEPTION_FLAGS: u32 = 0x003D;
const FPSR_QC_BIT: u32 = 27;
const PC_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
const FPCR_LOC_MASK: u64 = 0x07C8_0000;
const FPCR_LOC_SHIFT: u64 = 37;

const RSB_SIZE: usize = 8;

#[repr(C, align(16))]
pub struct A64JitState {
    pub reg: [u64; 31],
    pub sp: u64,
    pub pc: u64,
    pub cpsr_nzcv: u32,
    pub vec: [u64; 64],
    pub guest_mxcsr: u32,
    pub asimd_mxcsr: u32,
    pub halt_reason: u32,
    pub exclusive_state: u8,
    _pad_exclusive: [u8; 3],
    pub rsb_ptr: u32,
    pub rsb_location_descriptors: [u64; RSB_SIZE],
    pub rsb_codeptrs: [u64; RSB_SIZE],
    pub fpsr_exc: u32,
    pub fpsr_qc: u32,
    pub fpcr: u32,
}

impl A64JitState {
    pub const RESERVATION_GRANULE_MASK: u64 = 0xFFFF_FFFF_FFFF_FFF0;
    pub const RSB_SIZE: usize = RSB_SIZE;
    pub const RSB_PTR_MASK: usize = Self::RSB_SIZE - 1;

    pub fn new() -> Self {
        let mut state = Self {
            reg: [0; 31],
            sp: 0,
            pc: 0,
            cpsr_nzcv: 0,
            vec: [0; 64],
            guest_mxcsr: 0x0000_1F80,
            asimd_mxcsr: 0x0000_9FC0,
            halt_reason: 0,
            exclusive_state: 0,
            _pad_exclusive: [0; 3],
            rsb_ptr: 0,
            rsb_location_descriptors: [0; RSB_SIZE],
            rsb_codeptrs: [0; RSB_SIZE],
            fpsr_exc: 0,
            fpsr_qc: 0,
            fpcr: 0,
        };
        state.reset_rsb();
        state
    }

    pub fn reset_rsb(&mut self) {
        self.rsb_location_descriptors.fill(u64::MAX);
        self.rsb_codeptrs.fill(0);
    }

    pub fn get_pstate(&self) -> u32 {
        nzcv_util::from_x64(self.cpsr_nzcv)
    }

    pub fn set_pstate(&mut self, new_pstate: u32) {
        self.cpsr_nzcv = nzcv_util::to_x64(new_pstate);
    }

    pub fn get_fpcr(&self) -> u32 {
        self.fpcr
    }

    pub fn set_fpcr(&mut self, value: u32) {
        self.fpcr = value & FPCR_MASK;
        self.asimd_mxcsr &= MXCSR_EXCEPTION_FLAGS;
        self.guest_mxcsr &= MXCSR_EXCEPTION_FLAGS;
        self.asimd_mxcsr |= MXCSR_EXCEPTION_MASK;
        self.guest_mxcsr |= MXCSR_EXCEPTION_MASK;
        self.guest_mxcsr |= MXCSR_RMODE[((value >> 22) & 3) as usize];
        if value & (1 << 24) != 0 {
            self.guest_mxcsr |= MXCSR_FLUSH_TO_ZERO;
            self.guest_mxcsr |= MXCSR_DENORMALS_ARE_ZERO;
        }
    }

    pub fn get_fpsr(&self) -> u32 {
        let mxcsr = self.guest_mxcsr | self.asimd_mxcsr;
        let mut fpsr = mxcsr & 0b0000_0000_0001;
        fpsr |= (mxcsr & 0b0000_0011_1100) >> 1;
        fpsr |= self.fpsr_exc;
        fpsr |= ((self.fpsr_qc != 0) as u32) << FPSR_QC_BIT;
        fpsr
    }

    pub fn set_fpsr(&mut self, value: u32) {
        self.guest_mxcsr &= !MXCSR_EXCEPTION_FLAGS;
        self.asimd_mxcsr &= !MXCSR_EXCEPTION_FLAGS;
        self.fpsr_qc = (value >> FPSR_QC_BIT) & 1;
        self.fpsr_exc = value & 0x9F;
    }

    pub fn get_unique_hash(&self) -> u64 {
        let fpcr = ((self.fpcr as u64) & FPCR_LOC_MASK) << FPCR_LOC_SHIFT;
        let pc = self.pc & PC_MASK;
        pc | fpcr
    }

    pub const fn offset_of_reg() -> usize {
        0
    }
    pub const fn offset_of_sp() -> usize {
        core::mem::offset_of!(Self, sp)
    }
    pub const fn offset_of_pc() -> usize {
        core::mem::offset_of!(Self, pc)
    }
    pub const fn offset_of_cpsr_nzcv() -> usize {
        core::mem::offset_of!(Self, cpsr_nzcv)
    }
    pub const fn offset_of_vec() -> usize {
        core::mem::offset_of!(Self, vec)
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
    pub const fn offset_of_fpsr_exc() -> usize {
        core::mem::offset_of!(Self, fpsr_exc)
    }
    pub const fn offset_of_fpsr_qc() -> usize {
        core::mem::offset_of!(Self, fpsr_qc)
    }
    pub const fn offset_of_fpcr() -> usize {
        core::mem::offset_of!(Self, fpcr)
    }

    pub const fn reg_offset(index: usize) -> usize {
        Self::offset_of_reg() + index * 8
    }

    pub const fn vec_offset(index: usize, element: usize) -> usize {
        Self::offset_of_vec() + (index * 2 + element) * 8
    }
}

impl Default for A64JitState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_layout_matches_upstream() {
        assert_eq!(core::mem::align_of::<A64JitState>(), 16);
        assert_eq!(core::mem::size_of::<A64JitState>(), 960);
        assert_eq!(A64JitState::offset_of_reg(), 0);
        assert_eq!(A64JitState::offset_of_sp(), 248);
        assert_eq!(A64JitState::offset_of_pc(), 256);
        assert_eq!(A64JitState::offset_of_cpsr_nzcv(), 264);
        assert_eq!(A64JitState::offset_of_vec(), 272);
        assert_eq!(A64JitState::offset_of_guest_mxcsr(), 784);
        assert_eq!(A64JitState::offset_of_asimd_mxcsr(), 788);
        assert_eq!(A64JitState::offset_of_halt_reason(), 792);
        assert_eq!(A64JitState::offset_of_exclusive_state(), 796);
        assert_eq!(A64JitState::offset_of_rsb_ptr(), 800);
        assert_eq!(A64JitState::offset_of_rsb_location_descriptors(), 808);
        assert_eq!(A64JitState::offset_of_rsb_codeptrs(), 872);
        assert_eq!(A64JitState::offset_of_fpsr_exc(), 936);
        assert_eq!(A64JitState::offset_of_fpsr_qc(), 940);
        assert_eq!(A64JitState::offset_of_fpcr(), 944);
    }

    #[test]
    fn pstate_and_fp_state_match_upstream() {
        let mut state = A64JitState::new();
        for nzcv in 0..16 {
            state.set_pstate(nzcv << 28);
            assert_eq!(state.get_pstate(), nzcv << 28);
        }
        state.set_fpcr((1 << 22) | (1 << 24));
        assert_eq!(state.fpcr, ((1 << 22) | (1 << 24)) & FPCR_MASK);
        assert_ne!(state.guest_mxcsr & 0x4000, 0);
        assert_ne!(state.guest_mxcsr & MXCSR_FLUSH_TO_ZERO, 0);
        assert_ne!(state.guest_mxcsr & MXCSR_DENORMALS_ARE_ZERO, 0);
        state.set_fpsr(1 << FPSR_QC_BIT);
        assert_eq!(state.get_fpsr() & (1 << FPSR_QC_BIT), 1 << FPSR_QC_BIT);
    }

    #[test]
    fn constructor_resets_rsb_and_hashes_pc_fpcr() {
        let mut state = A64JitState::new();
        assert!(state
            .rsb_location_descriptors
            .iter()
            .all(|&value| value == u64::MAX));
        assert!(state.rsb_codeptrs.iter().all(|&value| value == 0));
        state.pc = 0x1000;
        assert_eq!(state.get_unique_hash(), 0x1000);
        state.fpcr = 0x00C0_0000;
        assert_ne!(state.get_unique_hash(), 0x1000);
    }
}

use crate::frontend::a32::fpscr::FPSCR_MODE_MASK;
use crate::ir::location::LocationDescriptor;

const A64_PC_MASK: u64 = (1u64 << 56) - 1;
const A64_FPCR_MASK: u32 = 0x07C8_0000;
const A64_FPCR_SHIFT: u32 = 37;
const A32_FPSR_MASK: u32 = 0x0800_009f;

#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct A32ExtRegs(pub [u32; 64]);

impl Default for A32ExtRegs {
    fn default() -> Self {
        Self([0; 64])
    }
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct A64VecRegs(pub [u64; 64]);

impl core::ops::Deref for A64VecRegs {
    type Target = [u64; 64];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl core::ops::DerefMut for A64VecRegs {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Default for A64VecRegs {
    fn default() -> Self {
        Self([0; 64])
    }
}

/// Matches upstream Dynarmic `Backend::Arm64::A32JitState`.
#[repr(C)]
pub struct A32JitState {
    pub cpsr_nzcv: u32,
    pub cpsr_q: u32,
    pub cpsr_jaifm: u32,
    pub cpsr_ge: u32,
    pub fpsr: u32,
    pub fpsr_nzcv: u32,
    pub regs: [u32; 16],
    pub upper_location_descriptor: u32,
    pub ext_regs: A32ExtRegs,
    pub exclusive_state: u32,
}

impl A32JitState {
    pub fn new() -> Self {
        Self {
            cpsr_nzcv: 0,
            cpsr_q: 0,
            cpsr_jaifm: 0,
            cpsr_ge: 0,
            fpsr: 0,
            fpsr_nzcv: 0,
            regs: [0; 16],
            upper_location_descriptor: 0,
            ext_regs: A32ExtRegs::default(),
            exclusive_state: 0,
        }
    }

    pub fn cpsr(&self) -> u32 {
        let mut cpsr = 0;
        cpsr |= self.cpsr_nzcv;
        cpsr |= self.cpsr_q;
        cpsr |= if bit(self.cpsr_ge, 31) { 1 << 19 } else { 0 };
        cpsr |= if bit(self.cpsr_ge, 23) { 1 << 18 } else { 0 };
        cpsr |= if bit(self.cpsr_ge, 15) { 1 << 17 } else { 0 };
        cpsr |= if bit(self.cpsr_ge, 7) { 1 << 16 } else { 0 };
        cpsr |= if bit(self.upper_location_descriptor, 1) {
            1 << 9
        } else {
            0
        };
        cpsr |= if bit(self.upper_location_descriptor, 0) {
            1 << 5
        } else {
            0
        };
        cpsr |= self.upper_location_descriptor & 0b11111100_00000000;
        cpsr |= (self.upper_location_descriptor & 0b00000011_00000000) << 17;
        cpsr |= self.cpsr_jaifm;
        cpsr
    }

    pub fn set_cpsr(&mut self, cpsr: u32) {
        self.cpsr_nzcv = cpsr & 0xF000_0000;
        self.cpsr_q = cpsr & (1 << 27);

        self.cpsr_ge = 0;
        self.cpsr_ge |= if bit(cpsr, 19) { 0xFF00_0000 } else { 0 };
        self.cpsr_ge |= if bit(cpsr, 18) { 0x00FF_0000 } else { 0 };
        self.cpsr_ge |= if bit(cpsr, 17) { 0x0000_FF00 } else { 0 };
        self.cpsr_ge |= if bit(cpsr, 16) { 0x0000_00FF } else { 0 };

        self.upper_location_descriptor &= 0xFFFF_0000;
        self.upper_location_descriptor |= if bit(cpsr, 9) { 2 } else { 0 };
        self.upper_location_descriptor |= if bit(cpsr, 5) { 1 } else { 0 };
        self.upper_location_descriptor |= (cpsr >> 0) & 0b11111100_00000000;
        self.upper_location_descriptor |= (cpsr >> 17) & 0b00000011_00000000;

        self.cpsr_jaifm = cpsr & 0x0100_01DF;
    }

    pub fn fpscr(&self) -> u32 {
        (self.upper_location_descriptor & 0xFFFF_0000) | self.fpsr | self.fpsr_nzcv
    }

    pub fn set_fpscr(&mut self, fpscr: u32) {
        self.fpsr_nzcv = fpscr & 0xF000_0000;
        self.fpsr = fpscr & A32_FPSR_MASK;
        self.upper_location_descriptor =
            (self.upper_location_descriptor & 0x0000_FFFF) | (fpscr & FPSCR_MODE_MASK);
    }

    pub fn get_location_descriptor(&self) -> LocationDescriptor {
        LocationDescriptor::new(
            self.regs[15] as u64 | ((self.upper_location_descriptor as u64) << 32),
        )
    }
}

impl Default for A32JitState {
    fn default() -> Self {
        Self::new()
    }
}

/// Matches upstream Dynarmic `Backend::Arm64::A64JitState`.
#[repr(C)]
pub struct A64JitState {
    pub reg: [u64; 31],
    pub sp: u64,
    pub pc: u64,
    pub cpsr_nzcv: u32,
    pub vec: A64VecRegs,
    pub exclusive_state: u32,
    pub fpsr: u32,
    pub fpcr: u32,
}

impl A64JitState {
    pub fn new() -> Self {
        Self {
            reg: [0; 31],
            sp: 0,
            pc: 0,
            cpsr_nzcv: 0,
            vec: A64VecRegs::default(),
            exclusive_state: 0,
            fpsr: 0,
            fpcr: 0,
        }
    }

    pub fn get_location_descriptor(&self) -> LocationDescriptor {
        let fpcr_u64 = ((self.fpcr & A64_FPCR_MASK) as u64) << A64_FPCR_SHIFT;
        let pc_u64 = self.pc & A64_PC_MASK;
        LocationDescriptor::new(pc_u64 | fpcr_u64)
    }

    pub const fn offset_of_pc() -> usize {
        core::mem::offset_of!(Self, pc)
    }

    pub fn get_pstate(&self) -> u32 {
        self.cpsr_nzcv
    }

    pub fn get_fpcr(&self) -> u32 {
        self.fpcr
    }

    pub fn get_fpsr(&self) -> u32 {
        self.fpsr
    }
}

impl Default for A64JitState {
    fn default() -> Self {
        Self::new()
    }
}

fn bit(value: u32, bit: u32) -> bool {
    ((value >> bit) & 1) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a32_layout_matches_upstream_arm64_jitstate() {
        assert_eq!(core::mem::align_of::<A32JitState>(), 16);
        assert_eq!(core::mem::size_of::<A32JitState>(), 368);
        assert_eq!(core::mem::offset_of!(A32JitState, cpsr_nzcv), 0);
        assert_eq!(core::mem::offset_of!(A32JitState, cpsr_q), 4);
        assert_eq!(core::mem::offset_of!(A32JitState, cpsr_jaifm), 8);
        assert_eq!(core::mem::offset_of!(A32JitState, cpsr_ge), 12);
        assert_eq!(core::mem::offset_of!(A32JitState, fpsr), 16);
        assert_eq!(core::mem::offset_of!(A32JitState, fpsr_nzcv), 20);
        assert_eq!(core::mem::offset_of!(A32JitState, regs), 24);
        assert_eq!(
            core::mem::offset_of!(A32JitState, upper_location_descriptor),
            88
        );
        assert_eq!(core::mem::offset_of!(A32JitState, ext_regs), 96);
        assert_eq!(core::mem::offset_of!(A32JitState, exclusive_state), 352);
    }

    #[test]
    fn a64_layout_matches_upstream_arm64_jitstate() {
        assert_eq!(core::mem::align_of::<A64JitState>(), 16);
        assert_eq!(core::mem::size_of::<A64JitState>(), 800);
        assert_eq!(core::mem::offset_of!(A64JitState, reg), 0);
        assert_eq!(core::mem::offset_of!(A64JitState, sp), 248);
        assert_eq!(core::mem::offset_of!(A64JitState, pc), 256);
        assert_eq!(core::mem::offset_of!(A64JitState, cpsr_nzcv), 264);
        assert_eq!(core::mem::offset_of!(A64JitState, vec), 272);
        assert_eq!(core::mem::offset_of!(A64JitState, exclusive_state), 784);
        assert_eq!(core::mem::offset_of!(A64JitState, fpsr), 788);
        assert_eq!(core::mem::offset_of!(A64JitState, fpcr), 792);
    }

    #[test]
    fn a32_status_register_helpers_match_upstream_bit_packing() {
        let mut state = A32JitState::new();
        state.upper_location_descriptor = 0x07C8_0000;
        state.set_cpsr(0xF900_03DF);
        assert_eq!(state.cpsr(), 0xF900_03DF);

        state.set_fpscr(0xF8C0_009F);
        assert_eq!(state.fpscr(), 0xF8C0_009F);
        assert_eq!(state.fpsr_nzcv, 0xF000_0000);
        assert_eq!(state.fpsr, 0x0800_009F);
    }

    #[test]
    fn location_descriptors_match_upstream_arm64_jitstate_helpers() {
        let mut a32 = A32JitState::new();
        a32.regs[15] = 0x1234_5678;
        a32.upper_location_descriptor = 0x07C8_0123;
        assert_eq!(a32.get_location_descriptor().value(), 0x07C8_0123_1234_5678);

        let mut a64 = A64JitState::new();
        a64.pc = 0xABCD_1234_5678_9ABC;
        a64.fpcr = 0xFFFF_FFFF;
        assert_eq!(
            a64.get_location_descriptor().value(),
            (0x00CD_1234_5678_9ABC_u64) | ((A64_FPCR_MASK as u64) << A64_FPCR_SHIFT)
        );
    }
}

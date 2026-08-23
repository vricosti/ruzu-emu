use crate::backend::x64::nzcv_util;

/// Mask for valid FPCR bits.
const FPCR_MASK: u32 = 0x07C8_9F00;

/// FPCR bit positions.
const FPCR_RMODE_SHIFT: u32 = 22;
const FPCR_FZ_BIT: u32 = 24;

/// MXCSR rounding mode values indexed by ARM RMode (0=Nearest, 1=Positive, 2=Negative, 3=Zero).
const MXCSR_RMODE: [u32; 4] = [0x0000, 0x4000, 0x2000, 0x6000];

/// MXCSR bits.
const MXCSR_FLUSH_TO_ZERO: u32 = 1 << 15;
const MXCSR_DENORMALS_ARE_ZERO: u32 = 1 << 6;
const MXCSR_EXCEPTION_MASK: u32 = 0x1F80; // mask all exceptions
const MXCSR_EXCEPTION_FLAGS: u32 = 0x003D; // sticky exception flags

/// FPSR bit positions.
const FPSR_QC_BIT: u32 = 27;

/// LocationDescriptor masks (matching A64LocationDescriptor).
const PC_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
const FPCR_LOC_MASK: u64 = 0x07C8_0000;
const FPCR_LOC_SHIFT: u64 = 37;

/// Return Stack Buffer size (must be power of 2).
pub const RSB_SIZE: usize = 8;
pub const RSB_PTR_MASK: usize = RSB_SIZE - 1;

/// Reservation granule mask for exclusive access.
pub const RESERVATION_GRANULE_MASK: u64 = 0xFFFF_FFFF_FFFF_FFF0;

/// ARM64 JIT state — the in-memory representation used during JIT execution.
///
/// R15 points to this struct during guest code execution.
/// Fields are laid out for efficient access from generated x86-64 code.
///
/// Matches the C++ `A64JitState` struct layout.
#[repr(C, align(16))]
pub struct A64JitState {
    /// General-purpose registers x0-x30.
    pub reg: [u64; 31],
    /// Stack pointer.
    pub sp: u64,
    /// Program counter.
    pub pc: u64,

    /// NZCV flags stored in x86-64 RFLAGS format for efficient flag operations.
    pub cpsr_nzcv: u32,

    /// Extension/vector registers (v0-v31 as 64 × u64 = 32 × 128-bit).
    pub vec: [u64; 64],

    // -- Internal fields for JIT runtime --
    /// Guest MXCSR (x86-64 SSE control/status for guest FP rounding/exceptions).
    pub guest_mxcsr: u32,
    /// ASIMD MXCSR (separate control for standard ASIMD operations).
    pub asimd_mxcsr: u32,
    /// Halt reason flags (checked atomically by dispatcher loop).
    pub halt_reason: u32,

    // -- Exclusive monitor state --
    /// Exclusive state flag (0 = no reservation, 1 = active reservation).
    pub exclusive_state: u8,
    _pad_exclusive: [u8; 7],

    /// Value captured during exclusive read (LDXR), passed as `expected`
    /// to exclusive write (STXR) for CAS. Two u64s for 128-bit support.
    pub exclusive_value: [u64; 2],

    // -- Return Stack Buffer (RSB) for fast return prediction --
    /// Current RSB pointer index.
    pub rsb_ptr: u32,
    /// RSB location descriptors (hashed PC+FPCR for lookup).
    pub rsb_location_descriptors: [u64; RSB_SIZE],
    /// RSB code pointers (native x86-64 addresses).
    pub rsb_codeptrs: [u64; RSB_SIZE],

    // -- Floating-point status/control --
    /// FPSR exception accumulator (sticky bits from MXCSR).
    pub fpsr_exc: u32,
    /// FPSR QC (saturation) flag.
    pub fpsr_qc: u32,
    /// FPCR (floating-point control register).
    pub fpcr: u32,

    // -- System registers --
    /// TPIDR_EL0 (thread pointer, read/write).
    pub tpidr_el0: u64,
    /// TPIDRRO_EL0 (thread pointer, read-only from EL0).
    pub tpidrro_el0: u64,
}

impl A64JitState {
    /// Create a new zeroed JIT state with default MXCSR values.
    pub fn new() -> Self {
        let mut state = Self {
            reg: [0; 31],
            sp: 0,
            pc: 0,
            cpsr_nzcv: 0,
            vec: [0; 64],
            guest_mxcsr: 0x0000_1F80, // all exceptions masked
            asimd_mxcsr: 0x0000_9FC0, // flush-to-zero + denormals-are-zero + all masked
            halt_reason: 0,
            exclusive_state: 0,
            _pad_exclusive: [0; 7],
            exclusive_value: [0; 2],
            rsb_ptr: 0,
            rsb_location_descriptors: [0; RSB_SIZE],
            rsb_codeptrs: [0; RSB_SIZE],
            fpsr_exc: 0,
            fpsr_qc: 0,
            fpcr: 0,
            tpidr_el0: 0,
            tpidrro_el0: 0,
        };
        state.reset_rsb();
        state
    }

    /// Reset the return stack buffer to invalid entries.
    pub fn reset_rsb(&mut self) {
        self.rsb_location_descriptors.fill(0xFFFF_FFFF_FFFF_FFFF);
        self.rsb_codeptrs.fill(0);
    }

    /// Get ARM64 PSTATE (NZCV in bits 31:28) from the x86-64 format.
    pub fn get_pstate(&self) -> u32 {
        nzcv_util::from_x64(self.cpsr_nzcv)
    }

    /// Set ARM64 PSTATE (NZCV in bits 31:28), converting to x86-64 format.
    pub fn set_pstate(&mut self, pstate: u32) {
        self.cpsr_nzcv = nzcv_util::to_x64(pstate);
    }

    /// Get FPCR value.
    pub fn get_fpcr(&self) -> u32 {
        self.fpcr
    }

    /// Set FPCR value and update MXCSR shadow registers accordingly.
    ///
    /// Maps ARM64 FPCR rounding mode and flush-to-zero to x86-64 MXCSR bits.
    pub fn set_fpcr(&mut self, value: u32) {
        self.fpcr = value & FPCR_MASK;

        // Preserve only exception flags, reset control bits
        self.asimd_mxcsr &= MXCSR_EXCEPTION_FLAGS;
        self.guest_mxcsr &= MXCSR_EXCEPTION_FLAGS;
        // Mask all exceptions
        self.asimd_mxcsr |= MXCSR_EXCEPTION_MASK;
        self.guest_mxcsr |= MXCSR_EXCEPTION_MASK;

        // Map ARM RMode to MXCSR rounding mode
        let rmode = ((value >> FPCR_RMODE_SHIFT) & 0x3) as usize;
        self.guest_mxcsr |= MXCSR_RMODE[rmode];

        // Map ARM FZ to MXCSR FZ + DAZ
        if value & (1 << FPCR_FZ_BIT) != 0 {
            self.guest_mxcsr |= MXCSR_FLUSH_TO_ZERO;
            self.guest_mxcsr |= MXCSR_DENORMALS_ARE_ZERO;
        }
    }

    /// Get FPSR value by combining MXCSR exception flags with stored state.
    ///
    /// Maps x86-64 MXCSR exception bits to ARM64 FPSR cumulative bits:
    /// - IOC (bit 0) = IE (MXCSR bit 0)
    /// - DZC (bit 1), OFC (bit 2), UFC (bit 3), IXC (bit 4) from MXCSR bits 2-5
    /// - QC (bit 27) from fpsr_qc
    pub fn get_fpsr(&self) -> u32 {
        let mxcsr = self.guest_mxcsr | self.asimd_mxcsr;
        let mut fpsr = 0u32;
        // IOC = IE (bit 0)
        fpsr |= mxcsr & 0b0000_0000_0001;
        // IXC, UFC, OFC, DZC = PE, UE, OE, ZE (shifted down by 1)
        fpsr |= (mxcsr & 0b0000_0011_1100) >> 1;
        fpsr |= self.fpsr_exc;
        if self.fpsr_qc != 0 {
            fpsr |= 1 << FPSR_QC_BIT;
        }
        fpsr
    }

    /// Set FPSR value, updating MXCSR exception flags and QC bit.
    pub fn set_fpsr(&mut self, value: u32) {
        self.guest_mxcsr &= !MXCSR_EXCEPTION_FLAGS;
        self.asimd_mxcsr &= !MXCSR_EXCEPTION_FLAGS;
        self.fpsr_qc = (value >> FPSR_QC_BIT) & 1;
        self.fpsr_exc = value & 0x9F;
    }

    /// Compute unique hash for block lookup (PC + FPCR bits).
    pub fn get_unique_hash(&self) -> u64 {
        let fpcr_u64 = ((self.fpcr as u64) & FPCR_LOC_MASK) << FPCR_LOC_SHIFT;
        let pc_u64 = self.pc & PC_MASK;
        pc_u64 | fpcr_u64
    }
}

impl Default for A64JitState {
    fn default() -> Self {
        Self::new()
    }
}

/// Field offsets for use by code emitters (accessed via R15 + offset).
///
/// These are computed at compile time and match the C `offsetof` equivalents.
impl A64JitState {
    pub const fn offset_of_reg() -> usize {
        0
    }

    pub const fn offset_of_sp() -> usize {
        core::mem::offset_of!(A64JitState, sp)
    }

    pub const fn offset_of_pc() -> usize {
        core::mem::offset_of!(A64JitState, pc)
    }

    pub const fn offset_of_cpsr_nzcv() -> usize {
        core::mem::offset_of!(A64JitState, cpsr_nzcv)
    }

    pub const fn offset_of_vec() -> usize {
        core::mem::offset_of!(A64JitState, vec)
    }

    pub const fn offset_of_guest_mxcsr() -> usize {
        core::mem::offset_of!(A64JitState, guest_mxcsr)
    }

    pub const fn offset_of_asimd_mxcsr() -> usize {
        core::mem::offset_of!(A64JitState, asimd_mxcsr)
    }

    pub const fn offset_of_halt_reason() -> usize {
        core::mem::offset_of!(A64JitState, halt_reason)
    }

    pub const fn offset_of_exclusive_state() -> usize {
        core::mem::offset_of!(A64JitState, exclusive_state)
    }

    pub const fn offset_of_exclusive_value() -> usize {
        core::mem::offset_of!(A64JitState, exclusive_value)
    }

    pub const fn offset_of_rsb_ptr() -> usize {
        core::mem::offset_of!(A64JitState, rsb_ptr)
    }

    pub const fn offset_of_rsb_location_descriptors() -> usize {
        core::mem::offset_of!(A64JitState, rsb_location_descriptors)
    }

    pub const fn offset_of_rsb_codeptrs() -> usize {
        core::mem::offset_of!(A64JitState, rsb_codeptrs)
    }

    pub const fn offset_of_fpsr_exc() -> usize {
        core::mem::offset_of!(A64JitState, fpsr_exc)
    }

    pub const fn offset_of_fpsr_qc() -> usize {
        core::mem::offset_of!(A64JitState, fpsr_qc)
    }

    pub const fn offset_of_fpcr() -> usize {
        core::mem::offset_of!(A64JitState, fpcr)
    }

    pub const fn offset_of_tpidr_el0() -> usize {
        core::mem::offset_of!(A64JitState, tpidr_el0)
    }

    pub const fn offset_of_tpidrro_el0() -> usize {
        core::mem::offset_of!(A64JitState, tpidrro_el0)
    }

    /// Byte offset of register `reg_index` (0-30) from the base.
    pub const fn reg_offset(reg_index: usize) -> usize {
        Self::offset_of_reg() + reg_index * 8
    }

    /// Byte offset of vector register element.
    /// Each vector register is 2 × u64 (128 bits), so vec_index 0..31,
    /// and element 0 (low) or 1 (high).
    pub const fn vec_offset(vec_index: usize, element: usize) -> usize {
        Self::offset_of_vec() + (vec_index * 2 + element) * 8
    }
}

// ===========================================================================
// A32 JIT State
// ===========================================================================

/// MXCSR rounding mode values for A32 (same as A64).
const A32_MXCSR_RMODE: [u32; 4] = [0x0000, 0x4000, 0x2000, 0x6000];

/// FPSCR bit positions.
const FPSCR_RMODE_SHIFT: u32 = 22;
const FPSCR_FZ_BIT: u32 = 24;
const FPSCR_QC_BIT: u32 = 27;
const FPSCR_MODE_MASK: u32 = 0x07F7_0000;
const FPSCR_NZCV_MASK: u32 = 0xF000_0000;

/// ARM32 JIT state — the in-memory representation used during JIT execution.
///
/// R15 points to this struct during guest code execution.
/// Fields are laid out for efficient access from generated x86-64 code.
///
/// Matches the C++ `A32JitState` struct layout from dynarmic.
///
/// Key differences from A64JitState:
/// - 16 × u32 GPRs (not 31 × u64) — R0-R15 where R15=PC, R13=SP, R14=LR
/// - CPSR split across multiple fields (NZCV, Q, GE, JAIFM)
/// - ext_reg array (S0-S31 / D0-D31 aliased storage)
/// - upper_location_descriptor encodes T/E/IT/FPSCR mode bits
///
/// Field ordering matches upstream a32_jitstate.h exactly:
///   Reg, upper_location_descriptor, cpsr_ge, cpsr_q, cpsr_nzcv, cpsr_jaifm,
///   ExtReg (aligned 16), guest_MXCSR, asimd_MXCSR, halt_reason, exclusive_state, ...
#[repr(C, align(16))]
pub struct A32JitState {
    /// General-purpose registers R0-R15.
    /// R13=SP, R14=LR, R15=PC.
    pub reg: [u32; 16],

    /// Upper location descriptor: T flag, E flag, IT state, FPSCR mode bits.
    /// Used to reconstruct the A32LocationDescriptor for block lookup.
    /// Matches upstream: placed immediately after Reg.
    pub upper_location_descriptor: u32,

    /// CPSR GE[3:0] flags in byte-lane format.
    /// Each GE bit occupies a full byte: 0x00 (clear) or 0xFF (set).
    /// Bit 7 = GE[0], bit 15 = GE[1], bit 23 = GE[2], bit 31 = GE[3].
    /// Matches upstream a32_jitstate.h `u32 cpsr_ge`.
    pub cpsr_ge: u32,
    /// CPSR Q (saturation) flag.
    pub cpsr_q: u32,
    /// CPSR NZCV flags stored in x86-64 RFLAGS format for efficient flag ops.
    pub cpsr_nzcv: u32,
    /// CPSR J, A, I, F, M bits (packed, mask 0x010001DF).
    pub cpsr_jaifm: u32,

    /// Padding to align ext_reg to 16 bytes.
    /// After reg(64) + upper(4) + cpsr_ge(4) + cpsr_q(4) + cpsr_nzcv(4) + cpsr_jaifm(4) = 84 bytes.
    /// Next 16-byte boundary is 96, so 12 bytes of padding.
    /// Matches upstream: `alignas(16) std::array<u32, 64> ExtReg`.
    _pad_ext_reg: [u8; 12],

    /// Extension registers — S0-S31 aliased with D0-D31.
    /// S registers use 1 u32 each (indices 0..31).
    /// D registers use 2 u32 each (even indices 0,2,4,...62).
    /// 16-byte aligned for SSE access to Q (quad) registers.
    pub ext_reg: [u32; 64],

    /// Guest MXCSR (x86-64 SSE control/status for guest FP rounding/exceptions).
    pub guest_mxcsr: u32,
    /// ASIMD MXCSR (separate control for standard ASIMD operations).
    pub asimd_mxcsr: u32,

    /// Halt reason flags (checked atomically by dispatcher loop).
    pub halt_reason: u32,

    /// Exclusive state flag (0 = no reservation, 1 = active reservation).
    pub exclusive_state: u32,

    /// Current RSB pointer index.
    pub rsb_ptr: u32,
    /// RSB location descriptors (hashed PC+state for lookup).
    pub rsb_location_descriptors: [u64; RSB_SIZE],
    /// RSB code pointers (native x86-64 addresses).
    pub rsb_codeptrs: [u64; RSB_SIZE],

    /// FPSCR exception accumulator (sticky bits from MXCSR).
    pub fpsr_exc: u32,
    /// FPSCR QC (saturation) flag.
    pub fpsr_qc: u32,
    /// FPSCR NZCV flags (stored in x86-64 RFLAGS format).
    pub fpsr_nzcv: u32,

    /// Value captured during exclusive read (LDREX), passed as `expected`
    /// to exclusive write (STREX) for CAS. Two u64s for 128-bit support.
    pub exclusive_value: [u64; 2],

    /// CNTPCT (Physical Count Timer).
    /// Set by the host before `run()` to provide the current tick count.
    /// Read by MRRC p15, 0, Rt, Rt2, c14.
    pub cntpct: u64,
}

impl A32JitState {
    /// Create a new zeroed JIT state with default MXCSR values.
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
            guest_mxcsr: 0x0000_1F80, // all exceptions masked
            asimd_mxcsr: 0x0000_9FC0, // flush-to-zero + denormals-are-zero + all masked
            halt_reason: 0,
            exclusive_state: 0,
            rsb_ptr: 0,
            rsb_location_descriptors: [0; RSB_SIZE],
            rsb_codeptrs: [0; RSB_SIZE],
            fpsr_exc: 0,
            fpsr_qc: 0,
            fpsr_nzcv: 0,
            exclusive_value: [0; 2],
            cntpct: 0,
        };
        state.reset_rsb();
        state
    }

    /// Reset the return stack buffer to invalid entries.
    pub fn reset_rsb(&mut self) {
        self.rsb_location_descriptors.fill(0xFFFF_FFFF_FFFF_FFFF);
        self.rsb_codeptrs.fill(0);
    }

    /// Reconstruct the full CPSR from split fields.
    ///
    /// Matches upstream a32_jitstate.cpp Cpsr():
    /// - NZCV from cpsr_nzcv (x86 format)
    /// - Q from cpsr_q
    /// - GE from cpsr_ge
    /// - T flag from upper_location_descriptor bit 0 → CPSR bit 5
    /// - E flag from upper_location_descriptor bit 1 → CPSR bit 9
    /// - IT state from upper_location_descriptor bits 8-15 → CPSR bits 10-15, 25-26
    /// - J, A, I, F, M from cpsr_jaifm
    pub fn get_cpsr(&self) -> u32 {
        let nzcv = nzcv_util::from_x64(self.cpsr_nzcv);
        let q = if self.cpsr_q != 0 { 1u32 << 27 } else { 0 };
        // GE flags: byte-lane format — bit 31=GE[3], bit 23=GE[2], bit 15=GE[1], bit 7=GE[0]
        let ge = (if self.cpsr_ge & (1 << 31) != 0 {
            1u32 << 19
        } else {
            0
        }) | (if self.cpsr_ge & (1 << 23) != 0 {
            1u32 << 18
        } else {
            0
        }) | (if self.cpsr_ge & (1 << 15) != 0 {
            1u32 << 17
        } else {
            0
        }) | (if self.cpsr_ge & (1 << 7) != 0 {
            1u32 << 16
        } else {
            0
        });
        // T flag: upper_location_descriptor bit 0 → CPSR bit 5
        let t = if self.upper_location_descriptor & 1 != 0 {
            1u32 << 5
        } else {
            0
        };
        // E flag: upper_location_descriptor bit 1 → CPSR bit 9
        let e = if self.upper_location_descriptor & 2 != 0 {
            1u32 << 9
        } else {
            0
        };
        // IT state: upper[15:10] = IT[7:2] → CPSR[15:10] (same position)
        //           upper[9:8] = IT[1:0] → CPSR[26:25] (shift left 17)
        let it = (self.upper_location_descriptor & 0xFC00)
            | ((self.upper_location_descriptor & 0x0300) << 17);
        nzcv | q | ge | t | e | it | self.cpsr_jaifm
    }

    /// Decompose and set CPSR from a full 32-bit value.
    ///
    /// Matches upstream a32_jitstate.cpp SetCpsr():
    /// - T, E, IT state → upper_location_descriptor
    /// - J, A, I, F, M → cpsr_jaifm (mask 0x010001DF)
    pub fn set_cpsr(&mut self, cpsr: u32) {
        self.cpsr_nzcv = nzcv_util::to_x64(cpsr);
        self.cpsr_q = if cpsr & (1 << 27) != 0 { 1 } else { 0 };
        // GE flags: expand each bit to a full byte-lane (0x00 or 0xFF)
        // Matches upstream SetCpsr exactly.
        self.cpsr_ge = 0;
        if cpsr & (1 << 19) != 0 {
            self.cpsr_ge |= 0xFF00_0000;
        }
        if cpsr & (1 << 18) != 0 {
            self.cpsr_ge |= 0x00FF_0000;
        }
        if cpsr & (1 << 17) != 0 {
            self.cpsr_ge |= 0x0000_FF00;
        }
        if cpsr & (1 << 16) != 0 {
            self.cpsr_ge |= 0x0000_00FF;
        }
        // T, E, IT state → upper_location_descriptor (preserve FPSCR mode in upper 16 bits)
        self.upper_location_descriptor &= 0xFFFF_0000;
        // T flag: CPSR bit 5 → upper bit 0
        if cpsr & (1 << 5) != 0 {
            self.upper_location_descriptor |= 1;
        }
        // E flag: CPSR bit 9 → upper bit 1
        if cpsr & (1 << 9) != 0 {
            self.upper_location_descriptor |= 2;
        }
        // IT state: CPSR[15:10] = IT[7:2] → upper[15:10], CPSR[26:25] = IT[1:0] → upper[9:8]
        self.upper_location_descriptor |= cpsr & 0xFC00;
        self.upper_location_descriptor |= (cpsr >> 17) & 0x0300;
        // J, A, I, F, M bits only (NOT T, NOT E, NOT IT state)
        self.cpsr_jaifm = cpsr & 0x010001DF;
    }

    /// Get the full FPSCR value by combining MXCSR exception bits with stored state.
    pub fn get_fpscr(&self) -> u32 {
        debug_assert_eq!(self.fpsr_nzcv & !FPSCR_NZCV_MASK, 0);

        let fpscr_mode = self.upper_location_descriptor & FPSCR_MODE_MASK;
        let mxcsr = self.guest_mxcsr | self.asimd_mxcsr;
        let mut fpscr = fpscr_mode | self.fpsr_nzcv;
        // IOC = IE (bit 0)
        fpscr |= mxcsr & 0b0000_0000_0001;
        // IXC, UFC, OFC, DZC = PE, UE, OE, ZE (shifted down by 1)
        fpscr |= (mxcsr & 0b0000_0011_1100) >> 1;
        fpscr |= self.fpsr_exc;
        if self.fpsr_qc != 0 {
            fpscr |= 1 << FPSCR_QC_BIT;
        }

        fpscr
    }

    /// Set FPSCR value and update MXCSR shadow registers accordingly.
    pub fn set_fpscr(&mut self, value: u32) {
        self.upper_location_descriptor &= 0x0000_FFFF;
        self.upper_location_descriptor |= value & FPSCR_MODE_MASK;

        self.fpsr_nzcv = value & FPSCR_NZCV_MASK;
        self.fpsr_qc = (value >> FPSCR_QC_BIT) & 1;

        self.guest_mxcsr = 0x0000_1F80;
        self.asimd_mxcsr = 0x0000_9FC0;

        // Map ARM RMode to MXCSR rounding mode
        let rmode = ((value >> FPSCR_RMODE_SHIFT) & 0x3) as usize;
        self.guest_mxcsr |= A32_MXCSR_RMODE[rmode];

        // Cumulative flags IDC, IOC, IXC, UFC, OFC, DZC.
        self.fpsr_exc = value & 0x9F;

        // Map ARM FZ to MXCSR FZ + DAZ
        if value & (1 << FPSCR_FZ_BIT) != 0 {
            self.guest_mxcsr |= MXCSR_FLUSH_TO_ZERO;
            self.guest_mxcsr |= MXCSR_DENORMALS_ARE_ZERO;
        }
    }

    /// Compute unique hash for block lookup (PC in lower 32 bits, upper_location_descriptor in upper 32).
    pub fn get_unique_hash(&self) -> u64 {
        let lower = self.reg[15] as u64;
        let upper = (self.upper_location_descriptor as u64) << 32;
        lower | upper
    }
}

impl Default for A32JitState {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::backend::common::a32_callbacks::A32ExclusiveState for A32JitState {
    fn exclusive_state(&self) -> u32 {
        self.exclusive_state
    }

    fn set_exclusive_state(&mut self, value: u32) {
        self.exclusive_state = value;
    }

    fn exclusive_value(&self, index: usize) -> u64 {
        self.exclusive_value[index]
    }

    fn set_exclusive_value(&mut self, index: usize, value: u64) {
        self.exclusive_value[index] = value;
    }
}

/// Field offsets for use by code emitters (accessed via R15 + offset).
impl A32JitState {
    pub const fn offset_of_reg() -> usize {
        0
    }

    pub const fn offset_of_ext_reg() -> usize {
        core::mem::offset_of!(A32JitState, ext_reg)
    }

    pub const fn offset_of_cpsr_nzcv() -> usize {
        core::mem::offset_of!(A32JitState, cpsr_nzcv)
    }

    pub const fn offset_of_cpsr_q() -> usize {
        core::mem::offset_of!(A32JitState, cpsr_q)
    }

    pub const fn offset_of_cpsr_ge() -> usize {
        core::mem::offset_of!(A32JitState, cpsr_ge)
    }

    pub const fn offset_of_cpsr_jaifm() -> usize {
        core::mem::offset_of!(A32JitState, cpsr_jaifm)
    }

    pub const fn offset_of_upper_location_descriptor() -> usize {
        core::mem::offset_of!(A32JitState, upper_location_descriptor)
    }

    pub const fn offset_of_fpsr_nzcv() -> usize {
        core::mem::offset_of!(A32JitState, fpsr_nzcv)
    }

    pub const fn offset_of_fpsr_exc() -> usize {
        core::mem::offset_of!(A32JitState, fpsr_exc)
    }

    pub const fn offset_of_fpsr_qc() -> usize {
        core::mem::offset_of!(A32JitState, fpsr_qc)
    }

    pub const fn offset_of_guest_mxcsr() -> usize {
        core::mem::offset_of!(A32JitState, guest_mxcsr)
    }

    pub const fn offset_of_asimd_mxcsr() -> usize {
        core::mem::offset_of!(A32JitState, asimd_mxcsr)
    }

    pub const fn offset_of_halt_reason() -> usize {
        core::mem::offset_of!(A32JitState, halt_reason)
    }

    pub const fn offset_of_exclusive_state() -> usize {
        core::mem::offset_of!(A32JitState, exclusive_state)
    }

    pub const fn offset_of_exclusive_value() -> usize {
        core::mem::offset_of!(A32JitState, exclusive_value)
    }

    pub const fn offset_of_cntpct() -> usize {
        core::mem::offset_of!(A32JitState, cntpct)
    }

    pub const fn offset_of_rsb_ptr() -> usize {
        core::mem::offset_of!(A32JitState, rsb_ptr)
    }

    pub const fn offset_of_rsb_location_descriptors() -> usize {
        core::mem::offset_of!(A32JitState, rsb_location_descriptors)
    }

    pub const fn offset_of_rsb_codeptrs() -> usize {
        core::mem::offset_of!(A32JitState, rsb_codeptrs)
    }

    /// Byte offset of register `reg_index` (0-15) from the base.
    pub const fn reg_offset(reg_index: usize) -> usize {
        Self::offset_of_reg() + reg_index * 4
    }

    /// Byte offset of extension register element (4 bytes each).
    pub const fn ext_reg_offset(ext_index: usize) -> usize {
        Self::offset_of_ext_reg() + ext_index * 4
    }

    // cpsr_ge is a single u32 (byte-lane format), no per-element offset needed.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jit_state_alignment() {
        assert_eq!(core::mem::align_of::<A64JitState>(), 16);
    }

    #[test]
    fn test_default_mxcsr() {
        let state = A64JitState::new();
        assert_eq!(state.guest_mxcsr, 0x0000_1F80);
        assert_eq!(state.asimd_mxcsr, 0x0000_9FC0);
    }

    #[test]
    fn test_pstate_round_trip() {
        let mut state = A64JitState::new();
        for nzcv_bits in 0u32..16 {
            let pstate = nzcv_bits << 28;
            state.set_pstate(pstate);
            assert_eq!(
                state.get_pstate(),
                pstate,
                "Round-trip failed for NZCV={:#x}",
                pstate
            );
        }
    }

    #[test]
    fn test_fpcr_set_clears_and_sets_mxcsr() {
        let mut state = A64JitState::new();

        // Round to Positive (RMode=1), FZ=1
        let fpcr_val = (1 << 22) | (1 << 24);
        state.set_fpcr(fpcr_val);
        assert_eq!(state.fpcr, fpcr_val & FPCR_MASK);
        // MXCSR should have: exception mask (0x1F80), RMode positive (0x4000),
        // FZ (1<<15), DAZ (1<<6)
        assert_ne!(state.guest_mxcsr & 0x4000, 0, "RMode should be positive");
        assert_ne!(state.guest_mxcsr & (1 << 15), 0, "FZ should be set");
        assert_ne!(state.guest_mxcsr & (1 << 6), 0, "DAZ should be set");
    }

    #[test]
    fn test_fpsr_get_set() {
        let mut state = A64JitState::new();

        // Set QC bit
        state.set_fpsr(1 << 27);
        assert_eq!(state.fpsr_qc, 1);
        assert_ne!(state.get_fpsr() & (1 << 27), 0);

        // Clear
        state.set_fpsr(0);
        assert_eq!(state.fpsr_qc, 0);
        assert_eq!(state.get_fpsr(), 0);
    }

    #[test]
    fn test_rsb_reset() {
        let state = A64JitState::new();
        for &loc in &state.rsb_location_descriptors {
            assert_eq!(loc, 0xFFFF_FFFF_FFFF_FFFF);
        }
        for &ptr in &state.rsb_codeptrs {
            assert_eq!(ptr, 0);
        }
    }

    #[test]
    fn test_reg_offsets_sequential() {
        let off0 = A64JitState::reg_offset(0);
        let off1 = A64JitState::reg_offset(1);
        assert_eq!(off1 - off0, 8);
    }

    #[test]
    fn test_vec_offsets_sequential() {
        let off0 = A64JitState::vec_offset(0, 0);
        let off1 = A64JitState::vec_offset(1, 0);
        assert_eq!(off1 - off0, 16); // each vec register is 128 bits
    }

    #[test]
    fn test_unique_hash() {
        let mut state = A64JitState::new();
        state.pc = 0x1000;
        state.fpcr = 0;
        let hash1 = state.get_unique_hash();
        assert_eq!(hash1, 0x1000);

        state.fpcr = 0x00C0_0000; // RMode = 3
        let hash2 = state.get_unique_hash();
        assert_ne!(hash1, hash2, "Different FPCR should produce different hash");
    }

    // --- A32 JIT State Tests ---

    #[test]
    fn test_a32_jit_state_alignment() {
        assert_eq!(core::mem::align_of::<A32JitState>(), 16);
    }

    #[test]
    fn test_a32_jit_state_field_order_matches_upstream() {
        // Verify field ordering matches upstream a32_jitstate.h:
        // Reg[16] at 0, upper_location_descriptor at 64, cpsr_ge at 68,
        // cpsr_q at 72, cpsr_nzcv at 76, cpsr_jaifm at 80,
        // ExtReg[64] at 96 (aligned to 16).
        assert_eq!(A32JitState::offset_of_reg(), 0);
        assert_eq!(A32JitState::offset_of_upper_location_descriptor(), 64);
        assert_eq!(A32JitState::offset_of_cpsr_ge(), 68);
        assert_eq!(A32JitState::offset_of_cpsr_q(), 72);
        assert_eq!(A32JitState::offset_of_cpsr_nzcv(), 76);
        assert_eq!(A32JitState::offset_of_cpsr_jaifm(), 80);
        assert_eq!(
            A32JitState::offset_of_ext_reg(),
            96,
            "ExtReg must be 16-byte aligned"
        );
        assert_eq!(A32JitState::offset_of_ext_reg() % 16, 0, "ExtReg alignment");
    }

    #[test]
    fn test_a32_default_mxcsr() {
        let state = A32JitState::new();
        assert_eq!(state.guest_mxcsr, 0x0000_1F80);
        assert_eq!(state.asimd_mxcsr, 0x0000_9FC0);
    }

    #[test]
    fn test_a32_fpscr_matches_upstream_storage_and_mxcsr_reset() {
        let mut state = A32JitState::new();
        state.upper_location_descriptor = 0xFFFF_FFFF;
        state.guest_mxcsr = u32::MAX;
        state.asimd_mxcsr = u32::MAX;

        let fpscr = 0xB000_0000 | (1 << FPSCR_QC_BIT) | (1 << FPSCR_FZ_BIT) | (1 << 22) | 0x91;
        state.set_fpscr(fpscr);

        assert_eq!(state.fpsr_nzcv, fpscr & FPSCR_NZCV_MASK);
        assert_eq!(state.fpsr_qc, 1);
        assert_eq!(state.fpsr_exc, fpscr & 0x9F);
        assert_eq!(
            state.upper_location_descriptor,
            0x0000_FFFF | (fpscr & FPSCR_MODE_MASK)
        );
        assert_eq!(state.guest_mxcsr, 0x0000_1F80 | 0x4000 | 0x8000 | 0x40);
        assert_eq!(state.asimd_mxcsr, 0x0000_9FC0);
        assert_eq!(
            state.get_fpscr(),
            fpscr & (FPSCR_MODE_MASK | FPSCR_NZCV_MASK | (1 << FPSCR_QC_BIT) | 0x9F)
        );
    }

    #[test]
    fn test_a32_cpsr_round_trip() {
        let mut state = A32JitState::new();
        // Test NZCV bits
        for nzcv_bits in 0u32..16 {
            let cpsr = nzcv_bits << 28;
            state.set_cpsr(cpsr);
            assert_eq!(
                state.get_cpsr() & 0xF000_0000,
                cpsr,
                "NZCV round-trip failed for {:#x}",
                cpsr
            );
        }
        // Test Q flag
        state.set_cpsr(1 << 27);
        assert_eq!(state.cpsr_q, 1);
        assert_ne!(state.get_cpsr() & (1 << 27), 0);

        // Test GE flags (byte-lane format: 0xFF per set bit)
        state.set_cpsr(0x000F_0000);
        assert_eq!(
            state.cpsr_ge, 0xFFFF_FFFF,
            "All GE bits set → all byte-lanes 0xFF"
        );
        assert_eq!(state.get_cpsr() & 0x000F_0000, 0x000F_0000);

        // Test individual GE bits
        state.set_cpsr(0x0005_0000); // GE[2]=1, GE[0]=1
        assert_eq!(state.cpsr_ge, 0x00FF_00FF, "GE[2] and GE[0] set");
        assert_eq!(state.get_cpsr() & 0x000F_0000, 0x0005_0000);
    }

    #[test]
    fn test_a32_rsb_reset() {
        let state = A32JitState::new();
        for &loc in &state.rsb_location_descriptors {
            assert_eq!(loc, 0xFFFF_FFFF_FFFF_FFFF);
        }
        for &ptr in &state.rsb_codeptrs {
            assert_eq!(ptr, 0);
        }
    }

    #[test]
    fn test_a32_reg_offsets_sequential() {
        let off0 = A32JitState::reg_offset(0);
        let off1 = A32JitState::reg_offset(1);
        assert_eq!(off1 - off0, 4); // u32 registers
    }

    #[test]
    fn test_a32_unique_hash() {
        let mut state = A32JitState::new();
        state.reg[15] = 0x0800_1000;
        state.upper_location_descriptor = 0;
        let hash1 = state.get_unique_hash();
        assert_eq!(hash1, 0x0800_1000);

        state.upper_location_descriptor = 1; // T flag
        let hash2 = state.get_unique_hash();
        assert_ne!(
            hash1, hash2,
            "Different upper descriptor should produce different hash"
        );
    }

    #[test]
    fn test_a32_ext_reg_offset() {
        let off0 = A32JitState::ext_reg_offset(0);
        let off1 = A32JitState::ext_reg_offset(1);
        assert_eq!(off1 - off0, 4); // u32 elements
    }
}

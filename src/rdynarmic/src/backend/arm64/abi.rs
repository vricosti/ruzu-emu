//! AArch64 host ABI facts used by the native ARM64 backend.
//!
//! Darwin and ELF AArch64 share the core procedure-call register classes that
//! matter to rdynarmic's generated code: X0-X7 are integer arguments, X19-X28
//! are callee-saved, FP/LR are X29/X30, and SP must be 16-byte aligned.

use super::block_of_code::BlockOfCode;
use super::inst;

pub type RegisterList = u64;

pub const XSTATE: u8 = 28;
pub const XHALT: u8 = 27;
pub const XTICKS: u8 = 26;
pub const XFASTMEM: u8 = 25;
pub const XPAGETABLE: u8 = 24;

pub const XSCRATCH0: u8 = 16;
pub const XSCRATCH1: u8 = 17;
pub const XSCRATCH2: u8 = 30;

/// Matches upstream `GPR_ORDER`.
pub const GPR_ORDER: &[usize] = &[
    19, 20, 21, 22, 23, 9, 10, 11, 12, 13, 14, 15, 0, 1, 2, 3, 4, 5, 6, 7, 8,
];

/// Matches upstream `FPR_ORDER`; V0-V7 are reserved for ABI/table scratch use.
pub const FPR_ORDER: &[usize] = &[
    8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
];

/// Integer argument registers in AAPCS64 / Darwin ARM64 order.
pub const ABI_PARAMS: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, 7];

/// Caller-saved general-purpose registers available across normal calls.
pub const CALLER_SAVE_GPRS: &[u8] = &[
    0, 1, 2, 3, 4, 5, 6, 7, // arguments / returns
    8, 9, 10, 11, 12, 13, 14, 15, 16, 17, // temporaries / intra-procedure call scratch
];

/// Callee-saved general-purpose registers.
pub const CALLEE_SAVE_GPRS: &[u8] = &[19, 20, 21, 22, 23, 24, 25, 26, 27, 28];

/// Frame pointer register.
pub const FP: u8 = 29;

/// Link register.
pub const LR: u8 = 30;

/// Stack alignment required at public call boundaries.
pub const STACK_ALIGNMENT: usize = 16;

/// SIMD/floating-point argument registers.
pub const ABI_VEC_PARAMS: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, 7];

/// Low 64 bits of V8-V15 are callee-saved by AAPCS64.
pub const CALLEE_SAVE_LOW64_VEC_REGS: &[u8] = &[8, 9, 10, 11, 12, 13, 14, 15];

/// Matches upstream `ABI_CALLEE_SAVE`.
pub const ABI_CALLEE_SAVE: RegisterList = 0x0000_ff00_7ff8_0000;

/// Matches upstream `ABI_CALLER_SAVE`.
pub const ABI_CALLER_SAVE: RegisterList = 0xffff_ffff_4000_ffff;

pub fn to_reg_list_gpr(reg: u8) -> RegisterList {
    assert!(reg < 31, "ZR/SP is not allowed in an ABI register list");
    1u64 << reg
}

pub fn to_reg_list_vec(reg: u8) -> RegisterList {
    assert!(reg < 32, "vector register out of range");
    1u64 << (reg + 32)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameInfo {
    pub gprs: Vec<u8>,
    pub fprs: Vec<u8>,
    pub frame_size: usize,
    pub gprs_size: usize,
    pub fprs_size: usize,
}

pub fn calculate_frame_info(registers: RegisterList, frame_size: usize) -> FrameInfo {
    let gprs = list_to_indexes(registers as u32);
    let fprs = list_to_indexes((registers >> 32) as u32);
    let gprs_size = gprs.len().div_ceil(2) * 16;
    let fprs_size = fprs.len() * 16;
    FrameInfo {
        gprs,
        fprs,
        frame_size,
        gprs_size,
        fprs_size,
    }
}

pub fn emit_push_registers(
    code: &mut BlockOfCode,
    registers: RegisterList,
    frame_size: usize,
) -> Result<(), String> {
    let frame_info = calculate_frame_info(registers, frame_size);

    code.write_u32(inst::sub_sp_imm(
        (frame_info.gprs_size + frame_info.fprs_size) as u32,
    ))?;
    emit_store_gprs(code, &frame_info.gprs, 0)?;
    emit_store_fprs(code, &frame_info.fprs, frame_info.gprs_size)?;
    code.write_u32(inst::sub_sp_imm(frame_info.frame_size as u32))?;
    Ok(())
}

pub fn emit_pop_registers(
    code: &mut BlockOfCode,
    registers: RegisterList,
    frame_size: usize,
) -> Result<(), String> {
    let frame_info = calculate_frame_info(registers, frame_size);

    code.write_u32(inst::add_sp_imm(frame_info.frame_size as u32))?;
    emit_load_gprs(code, &frame_info.gprs, 0)?;
    emit_load_fprs(code, &frame_info.fprs, frame_info.gprs_size)?;
    code.write_u32(inst::add_sp_imm(
        (frame_info.gprs_size + frame_info.fprs_size) as u32,
    ))?;
    Ok(())
}

fn list_to_indexes(list: u32) -> Vec<u8> {
    (0..32)
        .filter(|index| ((list >> index) & 1) != 0)
        .map(|index| index as u8)
        .collect()
}

fn emit_store_gprs(code: &mut BlockOfCode, gprs: &[u8], offset: usize) -> Result<(), String> {
    for (pair_index, pair) in gprs.chunks_exact(2).enumerate() {
        let pair_offset = offset + pair_index * 16;
        code.write_u32(inst::stp_x_offset_sp(pair[0], pair[1], pair_offset as i32))?;
    }
    if let Some(&reg) = gprs.chunks_exact(2).remainder().first() {
        let reg_offset = offset + (gprs.len() - 1) * 8;
        code.write_u32(inst::str_x_unsigned_sp(reg, reg_offset as u32))?;
    }
    Ok(())
}

fn emit_load_gprs(code: &mut BlockOfCode, gprs: &[u8], offset: usize) -> Result<(), String> {
    for (pair_index, pair) in gprs.chunks_exact(2).enumerate() {
        let pair_offset = offset + pair_index * 16;
        code.write_u32(inst::ldp_x_offset_sp(pair[0], pair[1], pair_offset as i32))?;
    }
    if let Some(&reg) = gprs.chunks_exact(2).remainder().first() {
        let reg_offset = offset + (gprs.len() - 1) * 8;
        code.write_u32(inst::ldr_x_unsigned_sp(reg, reg_offset as u32))?;
    }
    Ok(())
}

fn emit_store_fprs(code: &mut BlockOfCode, fprs: &[u8], offset: usize) -> Result<(), String> {
    for (pair_index, pair) in fprs.chunks_exact(2).enumerate() {
        let pair_offset = offset + pair_index * 32;
        code.write_u32(inst::stp_q_offset_sp(pair[0], pair[1], pair_offset as i32))?;
    }
    if let Some(&reg) = fprs.chunks_exact(2).remainder().first() {
        let reg_offset = offset + (fprs.len() - 1) * 16;
        code.write_u32(inst::str_q_unsigned_sp(reg, reg_offset as u32))?;
    }
    Ok(())
}

fn emit_load_fprs(code: &mut BlockOfCode, fprs: &[u8], offset: usize) -> Result<(), String> {
    for (pair_index, pair) in fprs.chunks_exact(2).enumerate() {
        let pair_offset = offset + pair_index * 32;
        code.write_u32(inst::ldp_q_offset_sp(pair[0], pair[1], pair_offset as i32))?;
    }
    if let Some(&reg) = fprs.chunks_exact(2).remainder().first() {
        let reg_offset = offset + (fprs.len() - 1) * 16;
        code.write_u32(inst::ldr_q_unsigned_sp(reg, reg_offset as u32))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm64_abi_register_sets_match_aapcs64() {
        assert_eq!(ABI_PARAMS, [0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(CALLEE_SAVE_GPRS, &[19, 20, 21, 22, 23, 24, 25, 26, 27, 28]);
        assert_eq!(FP, 29);
        assert_eq!(LR, 30);
        assert_eq!(STACK_ALIGNMENT, 16);
        assert_eq!(CALLEE_SAVE_LOW64_VEC_REGS, &[8, 9, 10, 11, 12, 13, 14, 15]);
        assert_eq!(XSTATE, 28);
        assert_eq!(XHALT, 27);
        assert_eq!(XTICKS, 26);
        assert_eq!(XFASTMEM, 25);
        assert_eq!(XPAGETABLE, 24);
        assert_eq!(XSCRATCH0, 16);
        assert_eq!(XSCRATCH1, 17);
        assert_eq!(XSCRATCH2, 30);
        assert_eq!(
            GPR_ORDER,
            &[19, 20, 21, 22, 23, 9, 10, 11, 12, 13, 14, 15, 0, 1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert_eq!(
            FPR_ORDER,
            &[
                8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28,
                29, 30, 31
            ]
        );
        assert_eq!(ABI_CALLEE_SAVE, 0x0000_ff00_7ff8_0000);
        assert_eq!(ABI_CALLER_SAVE, 0xffff_ffff_4000_ffff);
    }

    #[test]
    fn frame_info_matches_upstream_calculation() {
        let frame = calculate_frame_info(ABI_CALLEE_SAVE, 1184);
        assert_eq!(
            frame.gprs,
            vec![19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30]
        );
        assert_eq!(frame.fprs, vec![8, 9, 10, 11, 12, 13, 14, 15]);
        assert_eq!(frame.gprs_size, 96);
        assert_eq!(frame.fprs_size, 128);
        assert_eq!(frame.frame_size, 1184);
    }

    #[test]
    fn push_pop_registers_emit_upstream_order() {
        let mut code = BlockOfCode::with_size(4096).expect("code cache");
        emit_push_registers(&mut code, ABI_CALLEE_SAVE, 1184).unwrap();
        emit_pop_registers(&mut code, ABI_CALLEE_SAVE, 1184).unwrap();

        assert_eq!(code.code_size(), 96);
        let base = code.code_base_ptr();
        let read = |index: usize| unsafe { base.add(index * 4).cast::<u32>().read_unaligned() };
        assert_eq!(read(0), inst::sub_sp_imm(224));
        assert_eq!(read(1), inst::stp_x_offset_sp(19, 20, 0));
        assert_eq!(read(6), inst::stp_x_offset_sp(29, 30, 80));
        assert_eq!(read(7), inst::stp_q_offset_sp(8, 9, 96));
        assert_eq!(read(10), inst::stp_q_offset_sp(14, 15, 192));
        assert_eq!(read(11), inst::sub_sp_imm(1184));
        assert_eq!(read(12), inst::add_sp_imm(1184));
        assert_eq!(read(13), inst::ldp_x_offset_sp(19, 20, 0));
        assert_eq!(read(18), inst::ldp_x_offset_sp(29, 30, 80));
        assert_eq!(read(19), inst::ldp_q_offset_sp(8, 9, 96));
        assert_eq!(read(22), inst::ldp_q_offset_sp(14, 15, 192));
        assert_eq!(read(23), inst::add_sp_imm(224));
    }
}

/// The ABI registers as typed `rhazel` operands — upstream's `Xstate`,
/// `Xscratch0`, `Wscratch0`, … — for emitters on `rhazel::CodeGenerator`.
/// The `u8` constants above serve the encoders directly and go away once
/// every emitter has moved.
pub mod regs {
    use rhazel::{WReg, XReg};

    pub const XSTATE: XReg = XReg::new(super::XSTATE);
    pub const XHALT: XReg = XReg::new(super::XHALT);
    pub const XTICKS: XReg = XReg::new(super::XTICKS);
    pub const XFASTMEM: XReg = XReg::new(super::XFASTMEM);
    pub const XPAGETABLE: XReg = XReg::new(super::XPAGETABLE);
    pub const XSCRATCH0: XReg = XReg::new(super::XSCRATCH0);
    pub const XSCRATCH1: XReg = XReg::new(super::XSCRATCH1);
    pub const XSCRATCH2: XReg = XReg::new(super::XSCRATCH2);
    pub const WSCRATCH0: WReg = WReg::new(super::XSCRATCH0);
    pub const WSCRATCH1: WReg = WReg::new(super::XSCRATCH1);
    pub const WSCRATCH2: WReg = WReg::new(super::XSCRATCH2);
    pub const FP: XReg = XReg::new(super::FP);
    pub const LR: XReg = XReg::new(super::LR);
}

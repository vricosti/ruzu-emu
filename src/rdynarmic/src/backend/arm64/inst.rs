//! Small AArch64 instruction encoders used by the ARM64 backend bootstrap.
//!
//! The full backend will grow a structured emitter, but keeping these first
//! encoders here avoids scattering raw opcodes through dispatcher code.

use crate::ir::cond::Cond;

fn reg5(reg: u8) -> u32 {
    assert!(reg < 32, "AArch64 register out of range: {reg}");
    reg as u32
}

fn imm7_scaled(imm_bytes: i32, scale: i32) -> u32 {
    assert!(
        imm_bytes % scale == 0,
        "AArch64 pair offset must be scaled by {scale}: {imm_bytes}"
    );
    let imm = imm_bytes / scale;
    assert!(
        (-64..=63).contains(&imm),
        "AArch64 pair offset out of imm7 range: {imm_bytes}"
    );
    (imm as u32) & 0x7f
}

fn imm12_scaled(imm_bytes: u32, scale: u32) -> u32 {
    assert!(
        imm_bytes % scale == 0,
        "AArch64 unsigned offset must be scaled by {scale}: {imm_bytes}"
    );
    let imm = imm_bytes / scale;
    assert!(
        imm < 4096,
        "AArch64 unsigned offset out of imm12 range: {imm_bytes}"
    );
    imm
}

fn imm12_unscaled(imm: u32) -> u32 {
    assert!(imm < 4096, "AArch64 immediate out of imm12 range: {imm}");
    imm
}

fn imm9_unscaled(imm: i32) -> u32 {
    assert!(
        (-256..=255).contains(&imm),
        "AArch64 unscaled offset out of imm9 range: {imm}"
    );
    (imm as u32) & 0x1ff
}

fn logical_imm32(imm: u32) -> (u32, u32, u32) {
    match imm {
        0x1 => (0, 0, 0),
        0x2 => (0, 31, 0),
        0x3 => (0, 0, 1),
        0xff => (0, 0, 7),
        0x70 => (0, 28, 2),
        0x300 => (0, 24, 1),
        0xfc00 => (0, 22, 5),
        0x0101_0101 => (0, 0, 48),
        0x0800_0000 => (0, 5, 0),
        0x1000_0000 => (0, 4, 0),
        0x2000_0000 => (0, 3, 0),
        0x3000_0000 => (0, 4, 1),
        0x8080_8080 => (0, 1, 48),
        0x1111_1111 => (0, 0, 56),
        0xf000_0000 => (0, 4, 3),
        0xffff_0000 => (0, 16, 15),
        _ => panic!("unsupported AArch64 32-bit logical immediate: {imm:#x}"),
    }
}

fn logical_imm64(imm: u64) -> (u32, u32, u32) {
    match imm {
        0x1 => (1, 0, 0),
        0x3 => (1, 0, 1),
        0x4 => (1, 62, 0),
        0x7 => (1, 0, 2),
        0xf => (1, 0, 3),
        0xfff => (1, 0, 11),
        0x00ff_ffff_ffff_ffff => (1, 0, 55),
        0xffff_ffff_ffff_fffc => (1, 62, 61),
        0xffff_ffff_ffff_ffe0 => (1, 59, 58),
        0xffff_ffff_f800_0000 => (1, 37, 36),
        0xffff_ffff_f000_0000 => (1, 36, 35),
        _ => panic!("unsupported AArch64 64-bit logical immediate: {imm:#x}"),
    }
}

fn simd_size(size: u8) -> u32 {
    match size {
        8 => 0,
        16 => 1,
        32 => 2,
        64 => 3,
        _ => panic!("unsupported AArch64 SIMD element size: {size}"),
    }
}

fn simd_arrange(size: u8, q: bool) -> u32 {
    ((q as u32) << 30) | (simd_size(size) << 22)
}

fn simd_three_same(base: u32, rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    base | simd_arrange(size, q) | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

fn simd_two_same(base: u32, rd: u8, rn: u8, size: u8, q: bool) -> u32 {
    base | simd_arrange(size, q) | (reg5(rn) << 5) | reg5(rd)
}

fn simd_narrow(base: u32, rd: u8, rn: u8, source_size: u8) -> u32 {
    let imm = match source_size {
        16 => 0x01,
        32 => 0x41,
        64 => 0x81,
        _ => panic!("unsupported AArch64 SIMD narrow source size: {source_size}"),
    };
    base | (imm << 16) | (reg5(rn) << 5) | reg5(rd)
}

fn simd_shift_right(base: u32, rd: u8, rn: u8, size: u8, shift: u8, q: bool) -> u32 {
    assert!(shift <= size, "AArch64 SIMD right shift out of range");
    let imm = (size * 2) - shift;
    base | ((q as u32) << 30) | ((imm as u32) << 16) | (reg5(rn) << 5) | reg5(rd)
}

fn simd_shift_left(base: u32, rd: u8, rn: u8, size: u8, shift: u8, q: bool) -> u32 {
    assert!(shift < size, "AArch64 SIMD left shift out of range");
    let imm = size + shift;
    base | ((q as u32) << 30) | ((imm as u32) << 16) | (reg5(rn) << 5) | reg5(rd)
}

fn simd_imm5(size: u8, index: u8) -> u32 {
    let shift = match size {
        8 => 1,
        16 => 2,
        32 => 3,
        64 => 4,
        _ => panic!("unsupported AArch64 SIMD element size: {size}"),
    };
    assert!(
        index < 128 / size,
        "AArch64 SIMD element index out of range"
    );
    ((index as u32) << shift) | (1 << (shift - 1))
}

fn imm19(pc_offset_bytes: i32) -> u32 {
    assert!(
        pc_offset_bytes % 4 == 0,
        "AArch64 branch offset must be instruction-aligned: {pc_offset_bytes}"
    );
    let imm = pc_offset_bytes / 4;
    assert!(
        (-(1 << 18)..(1 << 18)).contains(&imm),
        "AArch64 branch offset out of imm19 range: {pc_offset_bytes}"
    );
    (imm as u32) & 0x7ffff
}

fn cond4(cond: Cond) -> u32 {
    cond as u32
}

fn imm26(pc_offset_bytes: isize) -> u32 {
    assert!(
        pc_offset_bytes % 4 == 0,
        "AArch64 branch offset must be instruction-aligned: {pc_offset_bytes}"
    );
    let imm = pc_offset_bytes / 4;
    assert!(
        (-(1 << 25)..(1 << 25)).contains(&imm),
        "AArch64 branch offset out of imm26 range: {pc_offset_bytes}"
    );
    (imm as u32) & 0x03ff_ffff
}

fn imm21_page(pc_page_offset_bytes: isize) -> u32 {
    assert!(
        pc_page_offset_bytes % 4096 == 0,
        "AArch64 ADRP offset must be page-aligned: {pc_page_offset_bytes}"
    );
    let imm = pc_page_offset_bytes / 4096;
    assert!(
        (-(1 << 20)..(1 << 20)).contains(&imm),
        "AArch64 ADRP offset out of imm21 range: {pc_page_offset_bytes}"
    );
    (imm as u32) & 0x1f_ffff
}

fn hw_from_shift(shift: u8) -> u32 {
    assert!(
        matches!(shift, 0 | 16 | 32 | 48),
        "MOV wide shift must be 0, 16, 32, or 48"
    );
    (shift / 16) as u32
}

/// `movz xD, #imm16, lsl #shift`.
pub fn movz_x(rd: u8, imm16: u16, shift: u8) -> u32 {
    0xd280_0000 | (hw_from_shift(shift) << 21) | ((imm16 as u32) << 5) | reg5(rd)
}

/// `movk xD, #imm16, lsl #shift`.
pub fn movk_x(rd: u8, imm16: u16, shift: u8) -> u32 {
    0xf280_0000 | (hw_from_shift(shift) << 21) | ((imm16 as u32) << 5) | reg5(rd)
}

/// `mov xD, xM`.
pub fn mov_x(rd: u8, rm: u8) -> u32 {
    0xaa00_03e0 | (reg5(rm) << 16) | reg5(rd)
}

/// `mov wD, wM`.
pub fn mov_w(rd: u8, rm: u8) -> u32 {
    0x2a00_03e0 | (reg5(rm) << 16) | reg5(rd)
}

/// `mov wD, #imm16`.
pub fn movz_w(rd: u8, imm16: u16, shift: u8) -> u32 {
    0x5280_0000 | (hw_from_shift(shift) << 21) | ((imm16 as u32) << 5) | reg5(rd)
}

/// `movk wD, #imm16, lsl #shift`.
pub fn movk_w(rd: u8, imm16: u16, shift: u8) -> u32 {
    assert!(matches!(shift, 0 | 16), "32-bit MOVK shift must be 0 or 16");
    0x7280_0000 | (hw_from_shift(shift) << 21) | ((imm16 as u32) << 5) | reg5(rd)
}

/// `sub sp, sp, #imm`.
pub fn sub_sp_imm(imm: u32) -> u32 {
    0xd100_03ff | (imm12_unscaled(imm) << 10)
}

/// `sub xD, xN, #imm`.
pub fn sub_x_imm(rd: u8, rn: u8, imm: u32) -> u32 {
    0xd100_0000 | (imm12_unscaled(imm) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `sub xD, xN, #imm{, lsl #12}`.
pub fn sub_x_imm_shift(rd: u8, rn: u8, imm12: u32, shift12: bool) -> u32 {
    0xd100_0000
        | ((shift12 as u32) << 22)
        | (imm12_unscaled(imm12) << 10)
        | (reg5(rn) << 5)
        | reg5(rd)
}

/// `subs xD, xN, #imm{, lsl #12}`.
pub fn subs_x_imm_shift(rd: u8, rn: u8, imm12: u32, shift12: bool) -> u32 {
    0xf100_0000
        | ((shift12 as u32) << 22)
        | (imm12_unscaled(imm12) << 10)
        | (reg5(rn) << 5)
        | reg5(rd)
}

/// `subs xD, xN, xM`.
pub fn subs_x_reg(rd: u8, rn: u8, rm: u8) -> u32 {
    0xeb00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `sub xD, xN, xM`.
pub fn sub_x_reg(rd: u8, rn: u8, rm: u8) -> u32 {
    0xcb00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `sub wD, wN, #imm`.
pub fn sub_w_imm(rd: u8, rn: u8, imm: u32) -> u32 {
    0x5100_0000 | (imm12_unscaled(imm) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `sub wD, wN, #imm{, lsl #12}`.
pub fn sub_w_imm_shift(rd: u8, rn: u8, imm12: u32, shift12: bool) -> u32 {
    0x5100_0000
        | ((shift12 as u32) << 22)
        | (imm12_unscaled(imm12) << 10)
        | (reg5(rn) << 5)
        | reg5(rd)
}

/// `subs wD, wN, #imm{, lsl #12}`.
pub fn subs_w_imm_shift(rd: u8, rn: u8, imm12: u32, shift12: bool) -> u32 {
    0x7100_0000
        | ((shift12 as u32) << 22)
        | (imm12_unscaled(imm12) << 10)
        | (reg5(rn) << 5)
        | reg5(rd)
}

/// `subs wD, wN, wM`.
pub fn subs_w_reg(rd: u8, rn: u8, rm: u8) -> u32 {
    0x6b00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `adc wD, wN, wM`.
pub fn adc_w(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1a00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `adc xD, xN, xM`.
pub fn adc_x(rd: u8, rn: u8, rm: u8) -> u32 {
    0x9a00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `adcs wD, wN, wM`.
pub fn adcs_w(rd: u8, rn: u8, rm: u8) -> u32 {
    0x3a00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `adcs xD, xN, xM`.
pub fn adcs_x(rd: u8, rn: u8, rm: u8) -> u32 {
    0xba00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `sbc wD, wN, wM`.
pub fn sbc_w(rd: u8, rn: u8, rm: u8) -> u32 {
    0x5a00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `sbc xD, xN, xM`.
pub fn sbc_x(rd: u8, rn: u8, rm: u8) -> u32 {
    0xda00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `sbcs wD, wN, wM`.
pub fn sbcs_w(rd: u8, rn: u8, rm: u8) -> u32 {
    0x7a00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `sbcs xD, xN, xM`.
pub fn sbcs_x(rd: u8, rn: u8, rm: u8) -> u32 {
    0xfa00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `cmp xN, #imm`.
pub fn cmp_x_imm(rn: u8, imm: u32) -> u32 {
    0xf100_001f | (imm12_unscaled(imm) << 10) | (reg5(rn) << 5)
}

/// `cmp wN, #imm`.
pub fn cmp_w_imm(rn: u8, imm: u32) -> u32 {
    0x7100_001f | (imm12_unscaled(imm) << 10) | (reg5(rn) << 5)
}

/// `cmp xN, xM`.
pub fn cmp_x_reg(rn: u8, rm: u8) -> u32 {
    0xeb00_001f | (reg5(rm) << 16) | (reg5(rn) << 5)
}

/// `cmp wN, wM`.
pub fn cmp_w_reg(rn: u8, rm: u8) -> u32 {
    0x6b00_001f | (reg5(rm) << 16) | (reg5(rn) << 5)
}

/// `add sp, sp, #imm`.
pub fn add_sp_imm(imm: u32) -> u32 {
    0x9100_03ff | (imm12_unscaled(imm) << 10)
}

/// `add xD, xN, #imm`.
pub fn add_x_imm(rd: u8, rn: u8, imm: u32) -> u32 {
    0x9100_0000 | (imm12_unscaled(imm) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `add xD, xN, #imm{, lsl #12}`.
pub fn add_x_imm_shift(rd: u8, rn: u8, imm12: u32, shift12: bool) -> u32 {
    0x9100_0000
        | ((shift12 as u32) << 22)
        | (imm12_unscaled(imm12) << 10)
        | (reg5(rn) << 5)
        | reg5(rd)
}

/// `adds xD, xN, #imm{, lsl #12}`.
pub fn adds_x_imm_shift(rd: u8, rn: u8, imm12: u32, shift12: bool) -> u32 {
    0xb100_0000
        | ((shift12 as u32) << 22)
        | (imm12_unscaled(imm12) << 10)
        | (reg5(rn) << 5)
        | reg5(rd)
}

/// `add wD, wN, #imm`.
pub fn add_w_imm(rd: u8, rn: u8, imm: u32) -> u32 {
    0x1100_0000 | (imm12_unscaled(imm) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `add wD, wN, #imm{, lsl #12}`.
pub fn add_w_imm_shift(rd: u8, rn: u8, imm12: u32, shift12: bool) -> u32 {
    0x1100_0000
        | ((shift12 as u32) << 22)
        | (imm12_unscaled(imm12) << 10)
        | (reg5(rn) << 5)
        | reg5(rd)
}

/// `adds wD, wN, #imm{, lsl #12}`.
pub fn adds_w_imm_shift(rd: u8, rn: u8, imm12: u32, shift12: bool) -> u32 {
    0x3100_0000
        | ((shift12 as u32) << 22)
        | (imm12_unscaled(imm12) << 10)
        | (reg5(rn) << 5)
        | reg5(rd)
}

/// `adds xD, xN, xM`.
pub fn adds_x_reg(rd: u8, rn: u8, rm: u8) -> u32 {
    0xab00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `add xD, xN|sp, xM`.
pub fn add_x_reg_sp(rd: u8, rn: u8, rm: u8) -> u32 {
    0x8b20_0000 | (reg5(rm) << 16) | (0b011 << 13) | (reg5(rn) << 5) | reg5(rd)
}

/// `add xD, xN, xM`.
pub fn add_x_reg(rd: u8, rn: u8, rm: u8) -> u32 {
    0x8b00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `add wD, wN, wM`.
pub fn add_w_reg(rd: u8, rn: u8, rm: u8) -> u32 {
    0x0b00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `add wD, wN, wM, lsr #shift`.
pub fn add_w_reg_lsr(rd: u8, rn: u8, rm: u8, shift: u8) -> u32 {
    assert!(shift < 32, "AArch64 ADD W LSR shift out of range: {shift}");
    0x0b40_0000 | (reg5(rm) << 16) | ((shift as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `adds wD, wN, wM`.
pub fn adds_w_reg(rd: u8, rn: u8, rm: u8) -> u32 {
    0x2b00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `tst wN, wM`.
pub fn tst_w_reg(rn: u8, rm: u8) -> u32 {
    0x6a00_001f | (reg5(rm) << 16) | (reg5(rn) << 5)
}

/// `tst xN, xM`.
pub fn tst_x_reg(rn: u8, rm: u8) -> u32 {
    0xea00_001f | (reg5(rm) << 16) | (reg5(rn) << 5)
}

/// `mvn wD, wM`.
pub fn mvn_w(rd: u8, rm: u8) -> u32 {
    0x2a20_03e0 | (reg5(rm) << 16) | reg5(rd)
}

/// `mvn xD, xM`.
pub fn mvn_x(rd: u8, rm: u8) -> u32 {
    0xaa20_03e0 | (reg5(rm) << 16) | reg5(rd)
}

/// `add xD, xN, wM, uxtw`.
pub fn add_x_reg_uxtw(rd: u8, rn: u8, rm: u8) -> u32 {
    0x8b20_4000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `ldr xT, label`.
pub fn ldr_x_lit(rt: u8, pc_offset_bytes: i32) -> u32 {
    0x5800_0000 | (imm19(pc_offset_bytes) << 5) | reg5(rt)
}

/// `adrp xD, label`.
pub fn adrp(rd: u8, pc_page_offset_bytes: isize) -> u32 {
    let imm = imm21_page(pc_page_offset_bytes);
    let immlo = imm & 0x3;
    let immhi = (imm >> 2) & 0x7ffff;
    0x9000_0000 | (immlo << 29) | (immhi << 5) | reg5(rd)
}

/// `br xN`.
pub fn br(rn: u8) -> u32 {
    0xd61f_0000 | (reg5(rn) << 5)
}

/// `b label`.
pub fn b_imm(pc_offset_bytes: isize) -> u32 {
    0x1400_0000 | imm26(pc_offset_bytes)
}

/// `bl label`.
pub fn bl_imm(pc_offset_bytes: isize) -> u32 {
    0x9400_0000 | imm26(pc_offset_bytes)
}

/// `b.cond label`.
pub fn b_cond(cond: Cond, pc_offset_bytes: i32) -> u32 {
    0x5400_0000 | (imm19(pc_offset_bytes) << 5) | cond4(cond)
}

/// `cbz wT, label`.
pub fn cbz_w(rt: u8, pc_offset_bytes: i32) -> u32 {
    0x3400_0000 | (imm19(pc_offset_bytes) << 5) | reg5(rt)
}

/// `cbz xT, label`.
pub fn cbz_x(rt: u8, pc_offset_bytes: i32) -> u32 {
    0xb400_0000 | (imm19(pc_offset_bytes) << 5) | reg5(rt)
}

/// `blr xN`.
pub fn blr(rn: u8) -> u32 {
    0xd63f_0000 | (reg5(rn) << 5)
}

/// `ldarb wT, [xN]`.
pub fn ldarb_w(rt: u8, rn: u8) -> u32 {
    0x08df_fc00 | (reg5(rn) << 5) | reg5(rt)
}

/// `ldarh wT, [xN]`.
pub fn ldarh_w(rt: u8, rn: u8) -> u32 {
    0x48df_fc00 | (reg5(rn) << 5) | reg5(rt)
}

/// `ldar wT, [xN]`.
pub fn ldar_w(rt: u8, rn: u8) -> u32 {
    0x88df_fc00 | (reg5(rn) << 5) | reg5(rt)
}

/// `ldar xT, [xN]`.
pub fn ldar_x(rt: u8, rn: u8) -> u32 {
    0xc8df_fc00 | (reg5(rn) << 5) | reg5(rt)
}

/// `ldaxr wT, [xN]`.
pub fn ldaxr_w(rt: u8, rn: u8) -> u32 {
    0x885f_fc00 | (reg5(rn) << 5) | reg5(rt)
}

/// `stlrb wT, [xN]`.
pub fn stlrb_w(rt: u8, rn: u8) -> u32 {
    0x089f_fc00 | (reg5(rn) << 5) | reg5(rt)
}

/// `stlrh wT, [xN]`.
pub fn stlrh_w(rt: u8, rn: u8) -> u32 {
    0x489f_fc00 | (reg5(rn) << 5) | reg5(rt)
}

/// `stlr wT, [xN]`.
pub fn stlr_w(rt: u8, rn: u8) -> u32 {
    0x889f_fc00 | (reg5(rn) << 5) | reg5(rt)
}

/// `stlr xT, [xN]`.
pub fn stlr_x(rt: u8, rn: u8) -> u32 {
    0xc89f_fc00 | (reg5(rn) << 5) | reg5(rt)
}

/// `stlxr wS, wT, [xN]`.
pub fn stlxr_w(rs: u8, rt: u8, rn: u8) -> u32 {
    0x8800_fc00 | (reg5(rs) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `stp wT1, wT2, [xN, #imm]`.
pub fn stp_w_offset(rt: u8, rt2: u8, rn: u8, imm_bytes: i32) -> u32 {
    0x2900_0000 | (imm7_scaled(imm_bytes, 4) << 15) | (reg5(rt2) << 10) | (reg5(rn) << 5) | reg5(rt)
}

/// `ldp wT1, wT2, [xN, #imm]`.
pub fn ldp_w_offset(rt: u8, rt2: u8, rn: u8, imm_bytes: i32) -> u32 {
    0x2940_0000 | (imm7_scaled(imm_bytes, 4) << 15) | (reg5(rt2) << 10) | (reg5(rn) << 5) | reg5(rt)
}

/// `str wT, [xN, #imm]`.
pub fn str_w_unsigned(rt: u8, rn: u8, imm_bytes: u32) -> u32 {
    0xb900_0000 | (imm12_scaled(imm_bytes, 4) << 10) | (reg5(rn) << 5) | reg5(rt)
}

/// `ldr wT, [xN, #imm]`.
pub fn ldr_w_unsigned(rt: u8, rn: u8, imm_bytes: u32) -> u32 {
    0xb940_0000 | (imm12_scaled(imm_bytes, 4) << 10) | (reg5(rn) << 5) | reg5(rt)
}

/// `ldrb wT, [xN, xM]`.
pub fn ldrb_w_reg_lsl(rt: u8, rn: u8, rm: u8) -> u32 {
    0x3860_6800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `ldrh wT, [xN, xM]`.
pub fn ldrh_w_reg_lsl(rt: u8, rn: u8, rm: u8) -> u32 {
    0x7860_6800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `ldr wT, [xN, xM]`.
pub fn ldr_w_reg_lsl(rt: u8, rn: u8, rm: u8) -> u32 {
    0xb860_6800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `ldr xT, [xN, xM]`.
pub fn ldr_x_reg_lsl(rt: u8, rn: u8, rm: u8) -> u32 {
    0xf860_6800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `ldr xT, [xN, xM, lsl #3]`.
pub fn ldr_x_reg_lsl3(rt: u8, rn: u8, rm: u8) -> u32 {
    0xf860_7800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `ldrb wT, [xN, wM, uxtw]`.
pub fn ldrb_w_reg_uxtw(rt: u8, rn: u8, rm: u8) -> u32 {
    0x3860_4800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `ldrh wT, [xN, wM, uxtw]`.
pub fn ldrh_w_reg_uxtw(rt: u8, rn: u8, rm: u8) -> u32 {
    0x7860_4800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `ldr wT, [xN, wM, uxtw]`.
pub fn ldr_w_reg_uxtw(rt: u8, rn: u8, rm: u8) -> u32 {
    0xb860_4800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `ldr xT, [xN, wM, uxtw]`.
pub fn ldr_x_reg_uxtw(rt: u8, rn: u8, rm: u8) -> u32 {
    0xf860_4800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `strb wT, [xN, xM]`.
pub fn strb_w_reg_lsl(rt: u8, rn: u8, rm: u8) -> u32 {
    0x3820_6800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `strh wT, [xN, xM]`.
pub fn strh_w_reg_lsl(rt: u8, rn: u8, rm: u8) -> u32 {
    0x7820_6800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `str wT, [xN, xM]`.
pub fn str_w_reg_lsl(rt: u8, rn: u8, rm: u8) -> u32 {
    0xb820_6800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `str xT, [xN, xM]`.
pub fn str_x_reg_lsl(rt: u8, rn: u8, rm: u8) -> u32 {
    0xf820_6800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `strb wT, [xN, wM, uxtw]`.
pub fn strb_w_reg_uxtw(rt: u8, rn: u8, rm: u8) -> u32 {
    0x3820_4800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `strh wT, [xN, wM, uxtw]`.
pub fn strh_w_reg_uxtw(rt: u8, rn: u8, rm: u8) -> u32 {
    0x7820_4800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `str wT, [xN, wM, uxtw]`.
pub fn str_w_reg_uxtw(rt: u8, rn: u8, rm: u8) -> u32 {
    0xb820_4800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `str xT, [xN, wM, uxtw]`.
pub fn str_x_reg_uxtw(rt: u8, rn: u8, rm: u8) -> u32 {
    0xf820_4800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `and wD, wN, #imm`.
pub fn and_w_imm(rd: u8, rn: u8, imm: u32) -> u32 {
    let (n, immr, imms) = logical_imm32(imm);
    0x1200_0000 | (n << 22) | (immr << 16) | (imms << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `orr wD, wN, #imm`.
pub fn orr_w_imm(rd: u8, rn: u8, imm: u32) -> u32 {
    let (n, immr, imms) = logical_imm32(imm);
    0x3200_0000 | (n << 22) | (immr << 16) | (imms << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `and wD, wN, wM`.
pub fn and_w_reg(rd: u8, rn: u8, rm: u8) -> u32 {
    0x0a00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `and xD, xN, #imm`.
pub fn and_x_imm(rd: u8, rn: u8, imm: u64) -> u32 {
    let (n, immr, imms) = logical_imm64(imm);
    0x9200_0000 | (n << 22) | (immr << 16) | (imms << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `and xD, xN, xM`.
pub fn and_x_reg(rd: u8, rn: u8, rm: u8) -> u32 {
    0x8a00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `ands wD, wN, wM`.
pub fn ands_w_reg(rd: u8, rn: u8, rm: u8) -> u32 {
    0x6a00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `ands xD, xN, xM`.
pub fn ands_x_reg(rd: u8, rn: u8, rm: u8) -> u32 {
    0xea00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `eor wD, wN, wM`.
pub fn eor_w_reg(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4a00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `eor xD, xN, xM`.
pub fn eor_x_reg(rd: u8, rn: u8, rm: u8) -> u32 {
    0xca00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `tst xN, #imm`.
pub fn tst_x_imm(rn: u8, imm: u64) -> u32 {
    let (n, immr, imms) = logical_imm64(imm);
    0xf200_001f | (n << 22) | (immr << 16) | (imms << 10) | (reg5(rn) << 5)
}

/// `ldrb wT, [xN, #imm]`.
pub fn ldrb_w_unsigned(rt: u8, rn: u8, imm_bytes: u32) -> u32 {
    0x3940_0000 | (imm12_unscaled(imm_bytes) << 10) | (reg5(rn) << 5) | reg5(rt)
}

/// `strb wT, [xN, #imm]`.
pub fn strb_w_unsigned(rt: u8, rn: u8, imm_bytes: u32) -> u32 {
    0x3900_0000 | (imm12_unscaled(imm_bytes) << 10) | (reg5(rn) << 5) | reg5(rt)
}

/// `orr wD, wN, wM`.
pub fn orr_w(rd: u8, rn: u8, rm: u8) -> u32 {
    0x2a00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `orr wD, wN, wM, lsl #shift`.
pub fn orr_w_lsl(rd: u8, rn: u8, rm: u8, shift: u8) -> u32 {
    assert!(shift < 32, "AArch64 ORR W shift out of range: {shift}");
    0x2a00_0000 | (reg5(rm) << 16) | ((shift as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `orr wD, wN, wM, lsr #shift`.
pub fn orr_w_lsr(rd: u8, rn: u8, rm: u8, shift: u8) -> u32 {
    assert!(shift < 32, "AArch64 ORR W shift out of range: {shift}");
    0x2a40_0000 | (reg5(rm) << 16) | ((shift as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `lsr wD, wN, #shift`.
pub fn lsr_w_imm(rd: u8, rn: u8, shift: u8) -> u32 {
    assert!(shift < 32, "AArch64 LSR W shift out of range: {shift}");
    0x5300_0000 | ((shift as u32) << 16) | (31 << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `lsr xD, xN, #shift`.
pub fn lsr_x_imm(rd: u8, rn: u8, shift: u8) -> u32 {
    assert!(shift < 64, "AArch64 LSR X shift out of range: {shift}");
    0xd340_0000 | ((shift as u32) << 16) | (63 << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `lsl wD, wN, #shift`.
pub fn lsl_w_imm(rd: u8, rn: u8, shift: u8) -> u32 {
    assert!(shift < 32, "AArch64 LSL W shift out of range: {shift}");
    let immr = (32 - shift as u32) & 0x1f;
    let imms = 31 - shift as u32;
    0x5300_0000 | (immr << 16) | (imms << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `asr wD, wN, #shift`.
pub fn asr_w_imm(rd: u8, rn: u8, shift: u8) -> u32 {
    assert!(shift < 32, "AArch64 ASR W shift out of range: {shift}");
    0x1300_7c00 | ((shift as u32) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `asr xD, xN, #shift`.
pub fn asr_x_imm(rd: u8, rn: u8, shift: u8) -> u32 {
    assert!(shift < 64, "AArch64 ASR X shift out of range: {shift}");
    0x9340_fc00 | ((shift as u32) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `lsl wD, wN, wM`.
pub fn lslv_w(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1ac0_2000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `lsr wD, wN, wM`.
pub fn lsrv_w(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1ac0_2400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `asr wD, wN, wM`.
pub fn asrv_w(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1ac0_2800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `ror wD, wN, wM`.
pub fn rorv_w(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1ac0_2c00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `extr wD, wN, wM, #lsb`.
pub fn extr_w(rd: u8, rn: u8, rm: u8, lsb: u8) -> u32 {
    assert!(lsb < 32, "AArch64 EXTR W lsb out of range: {lsb}");
    0x1380_0000 | ((lsb as u32) << 10) | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `extr xD, xN, xM, #lsb`.
pub fn extr_x(rd: u8, rn: u8, rm: u8, lsb: u8) -> u32 {
    assert!(lsb < 64, "AArch64 EXTR X lsb out of range: {lsb}");
    0x93c0_0000 | ((lsb as u32) << 10) | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `lsl xD, xN, xM`.
pub fn lslv_x(rd: u8, rn: u8, rm: u8) -> u32 {
    0x9ac0_2000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `lsr xD, xN, xM`.
pub fn lsrv_x(rd: u8, rn: u8, rm: u8) -> u32 {
    0x9ac0_2400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `asr xD, xN, xM`.
pub fn asrv_x(rd: u8, rn: u8, rm: u8) -> u32 {
    0x9ac0_2800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `ror xD, xN, xM`.
pub fn rorv_x(rd: u8, rn: u8, rm: u8) -> u32 {
    0x9ac0_2c00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `mul wD, wN, wM`.
pub fn mul_w(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1b00_7c00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `mul xD, xN, xM`.
pub fn mul_x(rd: u8, rn: u8, rm: u8) -> u32 {
    0x9b00_7c00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `smulh xD, xN, xM`.
pub fn smulh_x(rd: u8, rn: u8, rm: u8) -> u32 {
    0x9b40_7c00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `umulh xD, xN, xM`.
pub fn umulh_x(rd: u8, rn: u8, rm: u8) -> u32 {
    0x9bc0_7c00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `udiv wD, wN, wM`.
pub fn udiv_w(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1ac0_0800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `udiv xD, xN, xM`.
pub fn udiv_x(rd: u8, rn: u8, rm: u8) -> u32 {
    0x9ac0_0800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `sdiv wD, wN, wM`.
pub fn sdiv_w(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1ac0_0c00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `sdiv xD, xN, xM`.
pub fn sdiv_x(rd: u8, rn: u8, rm: u8) -> u32 {
    0x9ac0_0c00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `clz wD, wN`.
pub fn clz_w(rd: u8, rn: u8) -> u32 {
    0x5ac0_1000 | (reg5(rn) << 5) | reg5(rd)
}

/// `clz xD, xN`.
pub fn clz_x(rd: u8, rn: u8) -> u32 {
    0xdac0_1000 | (reg5(rn) << 5) | reg5(rd)
}

/// `ubfx wD, wN, #lsb, #width`.
pub fn ubfx_w(rd: u8, rn: u8, lsb: u8, width: u8) -> u32 {
    assert!(width > 0, "AArch64 UBFX width must be non-zero");
    assert!(lsb < 32, "AArch64 UBFX lsb out of range: {lsb}");
    assert!(
        (lsb as u16 + width as u16) <= 32,
        "AArch64 UBFX range out of bounds: lsb={lsb} width={width}"
    );
    let immr = lsb as u32;
    let imms = lsb as u32 + width as u32 - 1;
    0x5300_0000 | (immr << 16) | (imms << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `ubfx xD, xN, #lsb, #width`.
pub fn ubfx_x(rd: u8, rn: u8, lsb: u8, width: u8) -> u32 {
    assert!(width > 0, "AArch64 UBFX width must be non-zero");
    assert!(lsb < 64, "AArch64 UBFX lsb out of range: {lsb}");
    assert!(
        (lsb as u16 + width as u16) <= 64,
        "AArch64 UBFX range out of bounds: lsb={lsb} width={width}"
    );
    let immr = lsb as u32;
    let imms = lsb as u32 + width as u32 - 1;
    0xd340_0000 | (immr << 16) | (imms << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `sxtb wD, wN`.
pub fn sxtb_w(rd: u8, rn: u8) -> u32 {
    0x1300_1c00 | (reg5(rn) << 5) | reg5(rd)
}

/// `sxth wD, wN`.
pub fn sxth_w(rd: u8, rn: u8) -> u32 {
    0x1300_3c00 | (reg5(rn) << 5) | reg5(rd)
}

/// `sxtb xD, wN`.
pub fn sxtb_x(rd: u8, rn: u8) -> u32 {
    0x9340_1c00 | (reg5(rn) << 5) | reg5(rd)
}

/// `sxth xD, wN`.
pub fn sxth_x(rd: u8, rn: u8) -> u32 {
    0x9340_3c00 | (reg5(rn) << 5) | reg5(rd)
}

/// `sxtw xD, wN`.
pub fn sxtw_x(rd: u8, rn: u8) -> u32 {
    0x9340_7c00 | (reg5(rn) << 5) | reg5(rd)
}

/// `bfxil wD, wN, #lsb, #width`.
pub fn bfxil_w(rd: u8, rn: u8, lsb: u8, width: u8) -> u32 {
    assert!(width > 0, "AArch64 BFXIL width must be non-zero");
    assert!(lsb < 32, "AArch64 BFXIL lsb out of range: {lsb}");
    assert!(
        (lsb as u16 + width as u16) <= 32,
        "AArch64 BFXIL range out of bounds: lsb={lsb} width={width}"
    );
    let immr = lsb as u32;
    let imms = lsb as u32 + width as u32 - 1;
    0x3300_0000 | (immr << 16) | (imms << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `bfi xD, xN, #lsb, #width`.
pub fn bfi_x(rd: u8, rn: u8, lsb: u8, width: u8) -> u32 {
    assert!(width > 0, "AArch64 BFI width must be non-zero");
    assert!(lsb < 64, "AArch64 BFI lsb out of range: {lsb}");
    assert!(
        (lsb as u16 + width as u16) <= 64,
        "AArch64 BFI range out of bounds: lsb={lsb} width={width}"
    );
    let immr = ((64 - lsb as u32) & 0x3f) as u32;
    let imms = width as u32 - 1;
    0xb340_0000 | (immr << 16) | (imms << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `ubfiz wD, wN, #lsb, #width`.
pub fn ubfiz_w(rd: u8, rn: u8, lsb: u8, width: u8) -> u32 {
    assert!(width > 0, "AArch64 UBFIZ width must be non-zero");
    assert!(lsb < 32, "AArch64 UBFIZ lsb out of range: {lsb}");
    assert!(
        (lsb as u16 + width as u16) <= 32,
        "AArch64 UBFIZ range out of bounds: lsb={lsb} width={width}"
    );
    let immr = ((32 - lsb as u32) & 0x1f) as u32;
    let imms = width as u32 - 1;
    0x5300_0000 | (immr << 16) | (imms << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `sub wD, wN, wM`.
pub fn sub_w_reg(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4b00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `ands wD, wN, #imm`.
pub fn ands_w_imm(rd: u8, rn: u8, imm: u32) -> u32 {
    let (n, immr, imms) = logical_imm32(imm);
    0x7200_0000 | (n << 22) | (immr << 16) | (imms << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `csel wD, wN, wM, cond`.
pub fn csel_w(rd: u8, rn: u8, rm: u8, cond: Cond) -> u32 {
    0x1a80_0000 | (reg5(rm) << 16) | (cond4(cond) << 12) | (reg5(rn) << 5) | reg5(rd)
}

/// `csel xD, xN, xM, cond`.
pub fn csel_x(rd: u8, rn: u8, rm: u8, cond: Cond) -> u32 {
    0x9a80_0000 | (reg5(rm) << 16) | (cond4(cond) << 12) | (reg5(rn) << 5) | reg5(rd)
}

/// `neg wD, wN`.
pub fn neg_w(rd: u8, rn: u8) -> u32 {
    sub_w_reg(rd, 31, rn)
}

/// `cinc wD, wN, cond`.
pub fn cinc_w(rd: u8, rn: u8, cond: Cond) -> u32 {
    // CINC is an alias of CSINC with the inverse condition.
    0x1a80_0400 | (reg5(rn) << 16) | (cond4(cond.invert()) << 12) | (reg5(rn) << 5) | reg5(rd)
}

/// `bic wD, wN, wM`.
pub fn bic_w(rd: u8, rn: u8, rm: u8) -> u32 {
    0x0a20_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `bic xD, xN, xM`.
pub fn bic_x(rd: u8, rn: u8, rm: u8) -> u32 {
    0x8a20_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `bics wD, wN, wM`.
pub fn bics_w(rd: u8, rn: u8, rm: u8) -> u32 {
    0x6a20_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `bics xD, xN, xM`.
pub fn bics_x(rd: u8, rn: u8, rm: u8) -> u32 {
    0xea20_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `lsl xD, xN, #shift`.
pub fn lsl_x_imm(rd: u8, rn: u8, shift: u8) -> u32 {
    assert!(shift < 64, "AArch64 LSL shift out of range: {shift}");
    let immr = (64 - shift as u32) & 0x3f;
    let imms = 63 - shift as u32;
    0xd340_0000 | (immr << 16) | (imms << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `ror wD, wN, #shift`.
pub fn ror_w_imm(rd: u8, rn: u8, shift: u8) -> u32 {
    assert!(shift < 32, "AArch64 ROR W shift out of range: {shift}");
    0x1380_0000 | (reg5(rn) << 16) | ((shift as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `ror xD, xN, #shift`.
pub fn ror_x_imm(rd: u8, rn: u8, shift: u8) -> u32 {
    assert!(shift < 64, "AArch64 ROR X shift out of range: {shift}");
    0x93c0_0000 | (reg5(rn) << 16) | ((shift as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `orr xD, xN, xM`.
pub fn orr_x(rd: u8, rn: u8, rm: u8) -> u32 {
    0xaa00_0000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `mrs xT, FPSR`.
pub fn mrs_fpsr(rt: u8) -> u32 {
    0xd53b_4420 | reg5(rt)
}

/// `msr FPSR, xT`.
pub fn msr_fpsr(rt: u8) -> u32 {
    0xd51b_4420 | reg5(rt)
}

/// `mrs xT, FPCR`.
pub fn mrs_fpcr(rt: u8) -> u32 {
    0xd53b_4400 | reg5(rt)
}

/// `msr FPCR, xT`.
pub fn msr_fpcr(rt: u8) -> u32 {
    0xd51b_4400 | reg5(rt)
}

/// `str xT, [sp, #imm]`.
pub fn str_x_unsigned_sp(rt: u8, imm_bytes: u32) -> u32 {
    str_x_unsigned(rt, 31, imm_bytes)
}

/// `ldr xT, [sp, #imm]`.
pub fn ldr_x_unsigned_sp(rt: u8, imm_bytes: u32) -> u32 {
    ldr_x_unsigned(rt, 31, imm_bytes)
}

/// `str xT, [xN, #imm]`.
pub fn str_x_unsigned(rt: u8, rn: u8, imm_bytes: u32) -> u32 {
    0xf900_0000 | (imm12_scaled(imm_bytes, 8) << 10) | (reg5(rn) << 5) | reg5(rt)
}

/// `ldr xT, [xN, #imm]`.
pub fn ldr_x_unsigned(rt: u8, rn: u8, imm_bytes: u32) -> u32 {
    0xf940_0000 | (imm12_scaled(imm_bytes, 8) << 10) | (reg5(rn) << 5) | reg5(rt)
}

/// `stur xT, [xN, #imm]`.
pub fn stur_x(rt: u8, rn: u8, imm_bytes: i32) -> u32 {
    0xf800_0000 | (imm9_unscaled(imm_bytes) << 12) | (reg5(rn) << 5) | reg5(rt)
}

/// `str sT, [xN, #imm]`.
pub fn str_s_unsigned(rt: u8, rn: u8, imm_bytes: u32) -> u32 {
    0xbd00_0000 | (imm12_scaled(imm_bytes, 4) << 10) | (reg5(rn) << 5) | reg5(rt)
}

/// `ldr sT, [xN, #imm]`.
pub fn ldr_s_unsigned(rt: u8, rn: u8, imm_bytes: u32) -> u32 {
    0xbd40_0000 | (imm12_scaled(imm_bytes, 4) << 10) | (reg5(rn) << 5) | reg5(rt)
}

/// `str dT, [xN, #imm]`.
pub fn str_d_unsigned(rt: u8, rn: u8, imm_bytes: u32) -> u32 {
    0xfd00_0000 | (imm12_scaled(imm_bytes, 8) << 10) | (reg5(rn) << 5) | reg5(rt)
}

/// `ldr dT, [xN, #imm]`.
pub fn ldr_d_unsigned(rt: u8, rn: u8, imm_bytes: u32) -> u32 {
    0xfd40_0000 | (imm12_scaled(imm_bytes, 8) << 10) | (reg5(rn) << 5) | reg5(rt)
}

/// `ldur xT, [xN, #imm]`.
pub fn ldur_x(rt: u8, rn: u8, imm_bytes: i32) -> u32 {
    0xf840_0000 | (imm9_unscaled(imm_bytes) << 12) | (reg5(rn) << 5) | reg5(rt)
}

/// `str qT, [sp, #imm]`.
pub fn str_q_unsigned_sp(rt: u8, imm_bytes: u32) -> u32 {
    str_q_unsigned(rt, 31, imm_bytes)
}

/// `ldr qT, [sp, #imm]`.
pub fn ldr_q_unsigned_sp(rt: u8, imm_bytes: u32) -> u32 {
    ldr_q_unsigned(rt, 31, imm_bytes)
}

/// `str qT, [xN, #imm]`.
pub fn str_q_unsigned(rt: u8, rn: u8, imm_bytes: u32) -> u32 {
    0x3d80_0000 | (imm12_scaled(imm_bytes, 16) << 10) | (reg5(rn) << 5) | reg5(rt)
}

/// `ldr qT, [xN, #imm]`.
pub fn ldr_q_unsigned(rt: u8, rn: u8, imm_bytes: u32) -> u32 {
    0x3dc0_0000 | (imm12_scaled(imm_bytes, 16) << 10) | (reg5(rn) << 5) | reg5(rt)
}

/// `str qT, [xN, xM]`.
pub fn str_q_reg_lsl(rt: u8, rn: u8, rm: u8) -> u32 {
    0x3ca0_6800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `ldr qT, [xN, xM]`.
pub fn ldr_q_reg_lsl(rt: u8, rn: u8, rm: u8) -> u32 {
    0x3ce0_6800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `str qT, [xN, wM, uxtw]`.
pub fn str_q_reg_uxtw(rt: u8, rn: u8, rm: u8) -> u32 {
    0x3ca0_4800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `ldr qT, [xN, wM, uxtw]`.
pub fn ldr_q_reg_uxtw(rt: u8, rn: u8, rm: u8) -> u32 {
    0x3ce0_4800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rt)
}

/// `fmov xD, dN`.
pub fn fmov_x_from_d(rd: u8, rn: u8) -> u32 {
    0x9e66_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fmov dD, xN`.
pub fn fmov_d_from_x(rd: u8, rn: u8) -> u32 {
    0x9e67_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fmov dD, dN`.
pub fn fmov_d(rd: u8, rn: u8) -> u32 {
    0x1e60_4000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fmov sD, sN`.
pub fn fmov_s(rd: u8, rn: u8) -> u32 {
    0x1e20_4000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fmul sD, sN, sM`.
pub fn fmul_s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1e20_0800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmul dD, dN, dM`.
pub fn fmul_d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1e60_0800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fdiv sD, sN, sM`.
pub fn fdiv_s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1e20_1800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fdiv dD, dN, dM`.
pub fn fdiv_d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1e60_1800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmaxnm sD, sN, sM`.
pub fn fmaxnm_s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1e20_6800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmax sD, sN, sM`.
pub fn fmax_s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1e20_4800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmaxnm dD, dN, dM`.
pub fn fmaxnm_d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1e60_6800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmax dD, dN, dM`.
pub fn fmax_d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1e60_4800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fminnm sD, sN, sM`.
pub fn fminnm_s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1e20_7800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmin sD, sN, sM`.
pub fn fmin_s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1e20_5800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fminnm dD, dN, dM`.
pub fn fminnm_d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1e60_7800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmin dD, dN, dM`.
pub fn fmin_d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1e60_5800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fadd sD, sN, sM`.
pub fn fadd_s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1e20_2800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fadd dD, dN, dM`.
pub fn fadd_d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1e60_2800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fsub sD, sN, sM`.
pub fn fsub_s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1e20_3800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fsub dD, dN, dM`.
pub fn fsub_d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1e60_3800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmulx sD, sN, sM`.
pub fn fmulx_s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x5e20_dc00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmulx dD, dN, dM`.
pub fn fmulx_d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x5e60_dc00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `frecpe sD, sN`.
pub fn frecpe_s(rd: u8, rn: u8) -> u32 {
    0x5ea1_d800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frecpe dD, dN`.
pub fn frecpe_d(rd: u8, rn: u8) -> u32 {
    0x5ee1_d800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frecpx sD, sN`.
pub fn frecpx_s(rd: u8, rn: u8) -> u32 {
    0x5ea1_f800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frecpx dD, dN`.
pub fn frecpx_d(rd: u8, rn: u8) -> u32 {
    0x5ee1_f800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frecps sD, sN, sM`.
pub fn frecps_s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x5e20_fc00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `frecps dD, dN, dM`.
pub fn frecps_d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x5e60_fc00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `frsqrte sD, sN`.
pub fn frsqrte_s(rd: u8, rn: u8) -> u32 {
    0x7ea1_d800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frsqrte dD, dN`.
pub fn frsqrte_d(rd: u8, rn: u8) -> u32 {
    0x7ee1_d800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frsqrts sD, sN, sM`.
pub fn frsqrts_s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x5ea0_fc00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `frsqrts dD, dN, dM`.
pub fn frsqrts_d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x5ee0_fc00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fabs sD, sN`.
pub fn fabs_s(rd: u8, rn: u8) -> u32 {
    0x1e20_c000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fabs dD, dN`.
pub fn fabs_d(rd: u8, rn: u8) -> u32 {
    0x1e60_c000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fabs vD.4s, vN.4s`.
pub fn fabs_v4s(rd: u8, rn: u8) -> u32 {
    0x4ea0_f800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fabs vD.2d, vN.2d`.
pub fn fabs_v2d(rd: u8, rn: u8) -> u32 {
    0x4ee0_f800 | (reg5(rn) << 5) | reg5(rd)
}

/// `bic vD.8h, #0x80, lsl #8`.
pub fn bic_v8h_sign_bit(rd: u8) -> u32 {
    0x6f04_b400 | reg5(rd)
}

/// `frintn sD, sN`.
pub fn frintn_s(rd: u8, rn: u8) -> u32 {
    0x1e24_4000 | (reg5(rn) << 5) | reg5(rd)
}

/// `frintn dD, dN`.
pub fn frintn_d(rd: u8, rn: u8) -> u32 {
    0x1e64_4000 | (reg5(rn) << 5) | reg5(rd)
}

/// `frintp sD, sN`.
pub fn frintp_s(rd: u8, rn: u8) -> u32 {
    0x1e24_c000 | (reg5(rn) << 5) | reg5(rd)
}

/// `frintp dD, dN`.
pub fn frintp_d(rd: u8, rn: u8) -> u32 {
    0x1e64_c000 | (reg5(rn) << 5) | reg5(rd)
}

/// `frintm sD, sN`.
pub fn frintm_s(rd: u8, rn: u8) -> u32 {
    0x1e25_4000 | (reg5(rn) << 5) | reg5(rd)
}

/// `frintm dD, dN`.
pub fn frintm_d(rd: u8, rn: u8) -> u32 {
    0x1e65_4000 | (reg5(rn) << 5) | reg5(rd)
}

/// `frintz sD, sN`.
pub fn frintz_s(rd: u8, rn: u8) -> u32 {
    0x1e25_c000 | (reg5(rn) << 5) | reg5(rd)
}

/// `frintz dD, dN`.
pub fn frintz_d(rd: u8, rn: u8) -> u32 {
    0x1e65_c000 | (reg5(rn) << 5) | reg5(rd)
}

/// `frinta sD, sN`.
pub fn frinta_s(rd: u8, rn: u8) -> u32 {
    0x1e26_4000 | (reg5(rn) << 5) | reg5(rd)
}

/// `frinta dD, dN`.
pub fn frinta_d(rd: u8, rn: u8) -> u32 {
    0x1e66_4000 | (reg5(rn) << 5) | reg5(rd)
}

/// `frintx sD, sN`.
pub fn frintx_s(rd: u8, rn: u8) -> u32 {
    0x1e27_4000 | (reg5(rn) << 5) | reg5(rd)
}

/// `frintx dD, dN`.
pub fn frintx_d(rd: u8, rn: u8) -> u32 {
    0x1e67_4000 | (reg5(rn) << 5) | reg5(rd)
}

/// `frintn vD.4s, vN.4s`.
pub fn frintn_v4s(rd: u8, rn: u8) -> u32 {
    0x4e21_8800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frintn vD.2d, vN.2d`.
pub fn frintn_v2d(rd: u8, rn: u8) -> u32 {
    0x4e61_8800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frintp vD.4s, vN.4s`.
pub fn frintp_v4s(rd: u8, rn: u8) -> u32 {
    0x4ea1_8800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frintp vD.2d, vN.2d`.
pub fn frintp_v2d(rd: u8, rn: u8) -> u32 {
    0x4ee1_8800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frintm vD.4s, vN.4s`.
pub fn frintm_v4s(rd: u8, rn: u8) -> u32 {
    0x4e21_9800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frintm vD.2d, vN.2d`.
pub fn frintm_v2d(rd: u8, rn: u8) -> u32 {
    0x4e61_9800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frintz vD.4s, vN.4s`.
pub fn frintz_v4s(rd: u8, rn: u8) -> u32 {
    0x4ea1_9800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frintz vD.2d, vN.2d`.
pub fn frintz_v2d(rd: u8, rn: u8) -> u32 {
    0x4ee1_9800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frinta vD.4s, vN.4s`.
pub fn frinta_v4s(rd: u8, rn: u8) -> u32 {
    0x6e21_8800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frinta vD.2d, vN.2d`.
pub fn frinta_v2d(rd: u8, rn: u8) -> u32 {
    0x6e61_8800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frintx vD.4s, vN.4s`.
pub fn frintx_v4s(rd: u8, rn: u8) -> u32 {
    0x6e21_9800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frintx vD.2d, vN.2d`.
pub fn frintx_v2d(rd: u8, rn: u8) -> u32 {
    0x6e61_9800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fneg sD, sN`.
pub fn fneg_s(rd: u8, rn: u8) -> u32 {
    0x1e21_4000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fneg dD, dN`.
pub fn fneg_d(rd: u8, rn: u8) -> u32 {
    0x1e61_4000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fsqrt sD, sN`.
pub fn fsqrt_s(rd: u8, rn: u8) -> u32 {
    0x1e21_c000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fsqrt dD, dN`.
pub fn fsqrt_d(rd: u8, rn: u8) -> u32 {
    0x1e61_c000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fmadd sD, sN, sM, sA`.
pub fn fmadd_s(rd: u8, rn: u8, rm: u8, ra: u8) -> u32 {
    0x1f00_0000 | (reg5(rm) << 16) | (reg5(ra) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmadd dD, dN, dM, dA`.
pub fn fmadd_d(rd: u8, rn: u8, rm: u8, ra: u8) -> u32 {
    0x1f40_0000 | (reg5(rm) << 16) | (reg5(ra) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmsub sD, sN, sM, sA`.
pub fn fmsub_s(rd: u8, rn: u8, rm: u8, ra: u8) -> u32 {
    0x1f00_8000 | (reg5(rm) << 16) | (reg5(ra) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmsub dD, dN, dM, dA`.
pub fn fmsub_d(rd: u8, rn: u8, rm: u8, ra: u8) -> u32 {
    0x1f40_8000 | (reg5(rm) << 16) | (reg5(ra) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `fcmp sN, sM`.
pub fn fcmp_s(rn: u8, rm: u8) -> u32 {
    0x1e20_2000 | (reg5(rm) << 16) | (reg5(rn) << 5)
}

/// `fcmp dN, dM`.
pub fn fcmp_d(rn: u8, rm: u8) -> u32 {
    0x1e60_2000 | (reg5(rm) << 16) | (reg5(rn) << 5)
}

/// `fcmp sN, #0.0`.
pub fn fcmp_s_zero(rn: u8) -> u32 {
    0x1e20_2008 | (reg5(rn) << 5)
}

/// `fcmp dN, #0.0`.
pub fn fcmp_d_zero(rn: u8) -> u32 {
    0x1e60_2008 | (reg5(rn) << 5)
}

/// `fcmpe sN, sM`.
pub fn fcmpe_s(rn: u8, rm: u8) -> u32 {
    0x1e20_2010 | (reg5(rm) << 16) | (reg5(rn) << 5)
}

/// `fcmpe dN, dM`.
pub fn fcmpe_d(rn: u8, rm: u8) -> u32 {
    0x1e60_2010 | (reg5(rm) << 16) | (reg5(rn) << 5)
}

/// `fcmpe sN, #0.0`.
pub fn fcmpe_s_zero(rn: u8) -> u32 {
    0x1e20_2018 | (reg5(rn) << 5)
}

/// `fcmpe dN, #0.0`.
pub fn fcmpe_d_zero(rn: u8) -> u32 {
    0x1e60_2018 | (reg5(rn) << 5)
}

/// `fcvtzu wD, sN`.
pub fn fcvtzu_w_from_s(rd: u8, rn: u8) -> u32 {
    0x1e39_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzu xD, sN`.
pub fn fcvtzu_x_from_s(rd: u8, rn: u8) -> u32 {
    0x9e39_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzu wD, dN`.
pub fn fcvtzu_w_from_d(rd: u8, rn: u8) -> u32 {
    0x1e79_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzu xD, dN`.
pub fn fcvtzu_x_from_d(rd: u8, rn: u8) -> u32 {
    0x9e79_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtnu wD, sN`.
pub fn fcvtnu_w_from_s(rd: u8, rn: u8) -> u32 {
    0x1e21_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtnu xD, sN`.
pub fn fcvtnu_x_from_s(rd: u8, rn: u8) -> u32 {
    0x9e21_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtnu wD, dN`.
pub fn fcvtnu_w_from_d(rd: u8, rn: u8) -> u32 {
    0x1e61_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtnu xD, dN`.
pub fn fcvtnu_x_from_d(rd: u8, rn: u8) -> u32 {
    0x9e61_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtpu wD, sN`.
pub fn fcvtpu_w_from_s(rd: u8, rn: u8) -> u32 {
    0x1e29_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtpu xD, sN`.
pub fn fcvtpu_x_from_s(rd: u8, rn: u8) -> u32 {
    0x9e29_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtpu wD, dN`.
pub fn fcvtpu_w_from_d(rd: u8, rn: u8) -> u32 {
    0x1e69_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtpu xD, dN`.
pub fn fcvtpu_x_from_d(rd: u8, rn: u8) -> u32 {
    0x9e69_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtmu wD, sN`.
pub fn fcvtmu_w_from_s(rd: u8, rn: u8) -> u32 {
    0x1e31_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtmu xD, sN`.
pub fn fcvtmu_x_from_s(rd: u8, rn: u8) -> u32 {
    0x9e31_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtmu wD, dN`.
pub fn fcvtmu_w_from_d(rd: u8, rn: u8) -> u32 {
    0x1e71_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtmu xD, dN`.
pub fn fcvtmu_x_from_d(rd: u8, rn: u8) -> u32 {
    0x9e71_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtau wD, sN`.
pub fn fcvtau_w_from_s(rd: u8, rn: u8) -> u32 {
    0x1e25_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtau xD, sN`.
pub fn fcvtau_x_from_s(rd: u8, rn: u8) -> u32 {
    0x9e25_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtau wD, dN`.
pub fn fcvtau_w_from_d(rd: u8, rn: u8) -> u32 {
    0x1e65_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtau xD, dN`.
pub fn fcvtau_x_from_d(rd: u8, rn: u8) -> u32 {
    0x9e65_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzs wD, sN`.
pub fn fcvtzs_w_from_s(rd: u8, rn: u8) -> u32 {
    0x1e38_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzs xD, sN`.
pub fn fcvtzs_x_from_s(rd: u8, rn: u8) -> u32 {
    0x9e38_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzs wD, dN`.
pub fn fcvtzs_w_from_d(rd: u8, rn: u8) -> u32 {
    0x1e78_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzs xD, dN`.
pub fn fcvtzs_x_from_d(rd: u8, rn: u8) -> u32 {
    0x9e78_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtns wD, sN`.
pub fn fcvtns_w_from_s(rd: u8, rn: u8) -> u32 {
    0x1e20_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtns xD, sN`.
pub fn fcvtns_x_from_s(rd: u8, rn: u8) -> u32 {
    0x9e20_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtns wD, dN`.
pub fn fcvtns_w_from_d(rd: u8, rn: u8) -> u32 {
    0x1e60_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtns xD, dN`.
pub fn fcvtns_x_from_d(rd: u8, rn: u8) -> u32 {
    0x9e60_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtps wD, sN`.
pub fn fcvtps_w_from_s(rd: u8, rn: u8) -> u32 {
    0x1e28_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtps xD, sN`.
pub fn fcvtps_x_from_s(rd: u8, rn: u8) -> u32 {
    0x9e28_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtps wD, dN`.
pub fn fcvtps_w_from_d(rd: u8, rn: u8) -> u32 {
    0x1e68_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtps xD, dN`.
pub fn fcvtps_x_from_d(rd: u8, rn: u8) -> u32 {
    0x9e68_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtms wD, sN`.
pub fn fcvtms_w_from_s(rd: u8, rn: u8) -> u32 {
    0x1e30_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtms xD, sN`.
pub fn fcvtms_x_from_s(rd: u8, rn: u8) -> u32 {
    0x9e30_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtms wD, dN`.
pub fn fcvtms_w_from_d(rd: u8, rn: u8) -> u32 {
    0x1e70_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtms xD, dN`.
pub fn fcvtms_x_from_d(rd: u8, rn: u8) -> u32 {
    0x9e70_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtas wD, sN`.
pub fn fcvtas_w_from_s(rd: u8, rn: u8) -> u32 {
    0x1e24_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtas xD, sN`.
pub fn fcvtas_x_from_s(rd: u8, rn: u8) -> u32 {
    0x9e24_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtas wD, dN`.
pub fn fcvtas_w_from_d(rd: u8, rn: u8) -> u32 {
    0x1e64_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtas xD, dN`.
pub fn fcvtas_x_from_d(rd: u8, rn: u8) -> u32 {
    0x9e64_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzs xD, sN, #fbits`.
pub fn fcvtzs_x_from_s_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=64).contains(&fbits));
    0x9e18_0000 | ((64 - fbits as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzs xD, dN, #fbits`.
pub fn fcvtzs_x_from_d_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=64).contains(&fbits));
    0x9e58_0000 | ((64 - fbits as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzs wD, sN, #fbits`.
pub fn fcvtzs_w_from_s_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=32).contains(&fbits));
    0x1e18_0000 | ((64 - fbits as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzs wD, dN, #fbits`.
pub fn fcvtzs_w_from_d_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=32).contains(&fbits));
    0x1e58_0000 | ((64 - fbits as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzu xD, sN, #fbits`.
pub fn fcvtzu_x_from_s_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=64).contains(&fbits));
    0x9e19_0000 | ((64 - fbits as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzu xD, dN, #fbits`.
pub fn fcvtzu_x_from_d_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=64).contains(&fbits));
    0x9e59_0000 | ((64 - fbits as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzu wD, sN, #fbits`.
pub fn fcvtzu_w_from_s_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=32).contains(&fbits));
    0x1e19_0000 | ((64 - fbits as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzu wD, dN, #fbits`.
pub fn fcvtzu_w_from_d_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=32).contains(&fbits));
    0x1e59_0000 | ((64 - fbits as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvt dD, sN`.
pub fn fcvt_d_from_s(rd: u8, rn: u8) -> u32 {
    0x1e22_c000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvt sD, dN`.
pub fn fcvt_s_from_d(rd: u8, rn: u8) -> u32 {
    0x1e62_4000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvt dD, hN`.
pub fn fcvt_d_from_h(rd: u8, rn: u8) -> u32 {
    0x1ee2_c000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvt sD, hN`.
pub fn fcvt_s_from_h(rd: u8, rn: u8) -> u32 {
    0x1ee2_4000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvt hD, sN`.
pub fn fcvt_h_from_s(rd: u8, rn: u8) -> u32 {
    0x1e23_c000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvt hD, dN`.
pub fn fcvt_h_from_d(rd: u8, rn: u8) -> u32 {
    0x1e63_c000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtxn sD, dN`.
pub fn fcvtxn_s_from_d(rd: u8, rn: u8) -> u32 {
    0x7e61_6800 | (reg5(rn) << 5) | reg5(rd)
}

/// `ucvtf sD, wN`.
pub fn ucvtf_s_from_w(rd: u8, rn: u8) -> u32 {
    0x1e23_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `ucvtf sD, xN`.
pub fn ucvtf_s_from_x(rd: u8, rn: u8) -> u32 {
    0x9e23_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `ucvtf dD, wN`.
pub fn ucvtf_d_from_w(rd: u8, rn: u8) -> u32 {
    0x1e63_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `ucvtf dD, xN`.
pub fn ucvtf_d_from_x(rd: u8, rn: u8) -> u32 {
    0x9e63_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `scvtf sD, wN`.
pub fn scvtf_s_from_w(rd: u8, rn: u8) -> u32 {
    0x1e22_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `scvtf sD, xN`.
pub fn scvtf_s_from_x(rd: u8, rn: u8) -> u32 {
    0x9e22_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `scvtf dD, wN`.
pub fn scvtf_d_from_w(rd: u8, rn: u8) -> u32 {
    0x1e62_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `scvtf dD, xN`.
pub fn scvtf_d_from_x(rd: u8, rn: u8) -> u32 {
    0x9e62_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `scvtf sD, xN, #fbits`.
pub fn scvtf_s_from_x_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=64).contains(&fbits));
    0x9e02_0000 | ((64 - fbits as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `scvtf sD, wN, #fbits`.
pub fn scvtf_s_from_w_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=32).contains(&fbits));
    0x1e02_0000 | ((64 - fbits as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `scvtf dD, xN, #fbits`.
pub fn scvtf_d_from_x_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=64).contains(&fbits));
    0x9e42_0000 | ((64 - fbits as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `scvtf dD, wN, #fbits`.
pub fn scvtf_d_from_w_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=32).contains(&fbits));
    0x1e42_0000 | ((64 - fbits as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `ucvtf sD, xN, #fbits`.
pub fn ucvtf_s_from_x_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=64).contains(&fbits));
    0x9e03_0000 | ((64 - fbits as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `ucvtf sD, wN, #fbits`.
pub fn ucvtf_s_from_w_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=32).contains(&fbits));
    0x1e03_0000 | ((64 - fbits as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `ucvtf dD, xN, #fbits`.
pub fn ucvtf_d_from_x_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=64).contains(&fbits));
    0x9e43_0000 | ((64 - fbits as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `ucvtf dD, wN, #fbits`.
pub fn ucvtf_d_from_w_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=32).contains(&fbits));
    0x1e43_0000 | ((64 - fbits as u32) << 10) | (reg5(rn) << 5) | reg5(rd)
}

/// `scvtf vD.4s, vN.4s`.
pub fn scvtf_v4s(rd: u8, rn: u8) -> u32 {
    0x4e21_d800 | (reg5(rn) << 5) | reg5(rd)
}

/// `scvtf vD.2d, vN.2d`.
pub fn scvtf_v2d(rd: u8, rn: u8) -> u32 {
    0x4e61_d800 | (reg5(rn) << 5) | reg5(rd)
}

/// `ucvtf vD.4s, vN.4s`.
pub fn ucvtf_v4s(rd: u8, rn: u8) -> u32 {
    0x6e21_d800 | (reg5(rn) << 5) | reg5(rd)
}

/// `ucvtf vD.2d, vN.2d`.
pub fn ucvtf_v2d(rd: u8, rn: u8) -> u32 {
    0x6e61_d800 | (reg5(rn) << 5) | reg5(rd)
}

/// `scvtf vD.4s, vN.4s, #fbits`.
pub fn scvtf_v4s_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=32).contains(&fbits));
    0x4f00_e400 | (((64 - fbits as u32) & 0x7f) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `scvtf vD.2d, vN.2d, #fbits`.
pub fn scvtf_v2d_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=64).contains(&fbits));
    0x4f00_e400 | (((128 - fbits as u32) & 0x7f) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `ucvtf vD.4s, vN.4s, #fbits`.
pub fn ucvtf_v4s_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=32).contains(&fbits));
    0x6f00_e400 | (((64 - fbits as u32) & 0x7f) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `ucvtf vD.2d, vN.2d, #fbits`.
pub fn ucvtf_v2d_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=64).contains(&fbits));
    0x6f00_e400 | (((128 - fbits as u32) & 0x7f) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzs vD.4s, vN.4s`.
pub fn fcvtzs_v4s(rd: u8, rn: u8) -> u32 {
    0x4ea1_b800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzs vD.2d, vN.2d`.
pub fn fcvtzs_v2d(rd: u8, rn: u8) -> u32 {
    0x4ee1_b800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzu vD.4s, vN.4s`.
pub fn fcvtzu_v4s(rd: u8, rn: u8) -> u32 {
    0x6ea1_b800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzu vD.2d, vN.2d`.
pub fn fcvtzu_v2d(rd: u8, rn: u8) -> u32 {
    0x6ee1_b800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtns vD.4s, vN.4s`.
pub fn fcvtns_v4s(rd: u8, rn: u8) -> u32 {
    0x4e21_a800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtns vD.2d, vN.2d`.
pub fn fcvtns_v2d(rd: u8, rn: u8) -> u32 {
    0x4e61_a800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtps vD.4s, vN.4s`.
pub fn fcvtps_v4s(rd: u8, rn: u8) -> u32 {
    0x4ea1_a800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtps vD.2d, vN.2d`.
pub fn fcvtps_v2d(rd: u8, rn: u8) -> u32 {
    0x4ee1_a800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtms vD.4s, vN.4s`.
pub fn fcvtms_v4s(rd: u8, rn: u8) -> u32 {
    0x4e21_b800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtms vD.2d, vN.2d`.
pub fn fcvtms_v2d(rd: u8, rn: u8) -> u32 {
    0x4e61_b800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtas vD.4s, vN.4s`.
pub fn fcvtas_v4s(rd: u8, rn: u8) -> u32 {
    0x4e21_c800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtas vD.2d, vN.2d`.
pub fn fcvtas_v2d(rd: u8, rn: u8) -> u32 {
    0x4e61_c800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtnu vD.4s, vN.4s`.
pub fn fcvtnu_v4s(rd: u8, rn: u8) -> u32 {
    0x6e21_a800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtnu vD.2d, vN.2d`.
pub fn fcvtnu_v2d(rd: u8, rn: u8) -> u32 {
    0x6e61_a800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtpu vD.4s, vN.4s`.
pub fn fcvtpu_v4s(rd: u8, rn: u8) -> u32 {
    0x6ea1_a800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtpu vD.2d, vN.2d`.
pub fn fcvtpu_v2d(rd: u8, rn: u8) -> u32 {
    0x6ee1_a800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtmu vD.4s, vN.4s`.
pub fn fcvtmu_v4s(rd: u8, rn: u8) -> u32 {
    0x6e21_b800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtmu vD.2d, vN.2d`.
pub fn fcvtmu_v2d(rd: u8, rn: u8) -> u32 {
    0x6e61_b800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtau vD.4s, vN.4s`.
pub fn fcvtau_v4s(rd: u8, rn: u8) -> u32 {
    0x6e21_c800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtau vD.2d, vN.2d`.
pub fn fcvtau_v2d(rd: u8, rn: u8) -> u32 {
    0x6e61_c800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzs vD.4s, vN.4s, #fbits`.
pub fn fcvtzs_v4s_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=32).contains(&fbits));
    0x4f00_fc00 | (((64 - fbits as u32) & 0x7f) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzs vD.2d, vN.2d, #fbits`.
pub fn fcvtzs_v2d_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=64).contains(&fbits));
    0x4f00_fc00 | (((128 - fbits as u32) & 0x7f) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzu vD.4s, vN.4s, #fbits`.
pub fn fcvtzu_v4s_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=32).contains(&fbits));
    0x6f00_fc00 | (((64 - fbits as u32) & 0x7f) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtzu vD.2d, vN.2d, #fbits`.
pub fn fcvtzu_v2d_fixed(rd: u8, rn: u8, fbits: u8) -> u32 {
    assert!((1..=64).contains(&fbits));
    0x6f00_fc00 | (((128 - fbits as u32) & 0x7f) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmov vD.d[1], xN`.
pub fn fmov_v_d1_from_x(rd: u8, rn: u8) -> u32 {
    0x9eaf_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `fmov xD, vN.d[1]`.
pub fn fmov_x_from_v_d1(rd: u8, rn: u8) -> u32 {
    0x9eae_0000 | (reg5(rn) << 5) | reg5(rd)
}

/// `mov vD.d[1], vN.d[0]`.
pub fn mov_v_d1_from_v_d0(rd: u8, rn: u8) -> u32 {
    0x6e18_0400 | (reg5(rn) << 5) | reg5(rd)
}

/// `mov vD.16b, vN.16b`.
pub fn mov_v16b(rd: u8, rn: u8) -> u32 {
    0x4ea0_1c00 | (reg5(rn) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `movi dD, #0`.
pub fn movi_d_imm0(rd: u8) -> u32 {
    0x2f00_e400 | reg5(rd)
}

/// `aese vD.16b, vN.16b`.
pub fn aese_v16b(rd: u8, rn: u8) -> u32 {
    0x4e28_4800 | (reg5(rn) << 5) | reg5(rd)
}

/// `aesd vD.16b, vN.16b`.
pub fn aesd_v16b(rd: u8, rn: u8) -> u32 {
    0x4e28_5800 | (reg5(rn) << 5) | reg5(rd)
}

/// `aesmc vD.16b, vN.16b`.
pub fn aesmc_v16b(rd: u8, rn: u8) -> u32 {
    0x4e28_6800 | (reg5(rn) << 5) | reg5(rd)
}

/// `aesimc vD.16b, vN.16b`.
pub fn aesimc_v16b(rd: u8, rn: u8) -> u32 {
    0x4e28_7800 | (reg5(rn) << 5) | reg5(rd)
}

/// `crc32b Wd, Wn, Wm`.
pub fn crc32b_w(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1ac0_4000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `crc32h Wd, Wn, Wm`.
pub fn crc32h_w(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1ac0_4400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `crc32w Wd, Wn, Wm`.
pub fn crc32w_w(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1ac0_4800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `crc32x Wd, Wn, Xm`.
pub fn crc32x_x(rd: u8, rn: u8, rm: u8) -> u32 {
    0x9ac0_4c00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `crc32cb Wd, Wn, Wm`.
pub fn crc32cb_w(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1ac0_5000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `crc32ch Wd, Wn, Wm`.
pub fn crc32ch_w(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1ac0_5400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `crc32cw Wd, Wn, Wm`.
pub fn crc32cw_w(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1ac0_5800 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `crc32cx Wd, Wn, Xm`.
pub fn crc32cx_x(rd: u8, rn: u8, rm: u8) -> u32 {
    0x9ac0_5c00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `sha256h qD, qN, vM.4s`.
pub fn sha256h_q(rd: u8, rn: u8, rm: u8) -> u32 {
    0x5e00_4000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `sha256h2 qD, qN, vM.4s`.
pub fn sha256h2_q(rd: u8, rn: u8, rm: u8) -> u32 {
    0x5e00_5000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `sha256su0 vD.4s, vN.4s`.
pub fn sha256su0_v4s(rd: u8, rn: u8) -> u32 {
    0x5e28_2800 | (reg5(rn) << 5) | reg5(rd)
}

/// `sha256su1 vD.4s, vN.4s, vM.4s`.
pub fn sha256su1_v4s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x5e00_6000 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

fn movi_v_imm(rd: u8, imm: u8, q: bool) -> u32 {
    0x0f00_e400
        | ((q as u32) << 30)
        | (((imm as u32) & 0xe0) << 11)
        | (((imm as u32) & 0x1f) << 5)
        | reg5(rd)
}

/// `movi vD.8b, #imm`.
pub fn movi_v8b_imm(rd: u8, imm: u8) -> u32 {
    movi_v_imm(rd, imm, false)
}

/// `movi vD.16b, #imm`.
pub fn movi_v16b_imm(rd: u8, imm: u8) -> u32 {
    movi_v_imm(rd, imm, true)
}

/// `umov wD/xD, vN.<T>[index]`.
pub fn umov_from_v(rd: u8, rn: u8, size: u8, index: u8) -> u32 {
    let q = size == 64;
    0x0e00_3c00 | ((q as u32) << 30) | (simd_imm5(size, index) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `mov vD.<T>[index], wN/xN`.
pub fn mov_to_v_element(rd: u8, rn: u8, size: u8, index: u8) -> u32 {
    0x4e00_1c00 | (simd_imm5(size, index) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `dup vD.<T>, wN/xN`.
pub fn dup_v_from_reg(rd: u8, rn: u8, size: u8, q: bool) -> u32 {
    0x0e00_0c00 | ((q as u32) << 30) | (simd_imm5(size, 0) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `dup vD.<T>, vN.<T>[index]`.
pub fn dup_v_from_element(rd: u8, rn: u8, size: u8, index: u8, q: bool) -> u32 {
    0x0e00_0400 | ((q as u32) << 30) | (simd_imm5(size, index) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `add vD.<T>, vN.<T>, vM.<T>`.
pub fn add_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e20_8400, rd, rn, rm, size, q)
}

/// `sub vD.<T>, vN.<T>, vM.<T>`.
pub fn sub_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x2e20_8400, rd, rn, rm, size, q)
}

/// `mul vD.<T>, vN.<T>, vM.<T>`.
pub fn mul_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e20_9c00, rd, rn, rm, size, q)
}

/// `fadd vD.4s, vN.4s, vM.4s`.
pub fn fadd_v4s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4e20_d400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fadd vD.2d, vN.2d, vM.2d`.
pub fn fadd_v2d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4e60_d400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fsub vD.4s, vN.4s, vM.4s`.
pub fn fsub_v4s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4ea0_d400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fsub vD.2d, vN.2d, vM.2d`.
pub fn fsub_v2d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4ee0_d400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmul vD.4s, vN.4s, vM.4s`.
pub fn fmul_v4s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x6e20_dc00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmul vD.2d, vN.2d, vM.2d`.
pub fn fmul_v2d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x6e60_dc00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmax vD.4s, vN.4s, vM.4s`.
pub fn fmax_v4s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4e20_f400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmax vD.2d, vN.2d, vM.2d`.
pub fn fmax_v2d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4e60_f400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmaxnm vD.4s, vN.4s, vM.4s`.
pub fn fmaxnm_v4s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4e20_c400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmaxnm vD.2d, vN.2d, vM.2d`.
pub fn fmaxnm_v2d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4e60_c400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmin vD.4s, vN.4s, vM.4s`.
pub fn fmin_v4s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4ea0_f400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmin vD.2d, vN.2d, vM.2d`.
pub fn fmin_v2d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4ee0_f400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fminnm vD.4s, vN.4s, vM.4s`.
pub fn fminnm_v4s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4ea0_c400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fminnm vD.2d, vN.2d, vM.2d`.
pub fn fminnm_v2d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4ee0_c400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fcmeq vD.4s, vN.4s, vM.4s`.
pub fn fcmeq_v4s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4e20_e400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fcmeq vD.2d, vN.2d, vM.2d`.
pub fn fcmeq_v2d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4e60_e400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fcmgt vD.4s, vN.4s, vM.4s`.
pub fn fcmgt_v4s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x6ea0_e400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fcmgt vD.2d, vN.2d, vM.2d`.
pub fn fcmgt_v2d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x6ee0_e400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fcmge vD.4s, vN.4s, vM.4s`.
pub fn fcmge_v4s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x6e20_e400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fcmge vD.2d, vN.2d, vM.2d`.
pub fn fcmge_v2d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x6e60_e400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmla vD.4s, vN.4s, vM.4s`.
pub fn fmla_v4s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4e20_cc00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmla vD.2d, vN.2d, vM.2d`.
pub fn fmla_v2d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4e60_cc00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmulx vD.4s, vN.4s, vM.4s`.
pub fn fmulx_v4s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4e20_dc00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fmulx vD.2d, vN.2d, vM.2d`.
pub fn fmulx_v2d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4e60_dc00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtl vD.4s, vN.4h`.
pub fn fcvtl_v4s_from_v4h(rd: u8, rn: u8) -> u32 {
    0x0e21_7800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fcvtn vD.4h, vN.4s`.
pub fn fcvtn_v4h_from_v4s(rd: u8, rn: u8) -> u32 {
    0x0e21_6800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fdiv vD.4s, vN.4s, vM.4s`.
pub fn fdiv_v4s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x6e20_fc00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fdiv vD.2d, vN.2d, vM.2d`.
pub fn fdiv_v2d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x6e60_fc00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `fneg vD.4s, vN.4s`.
pub fn fneg_v4s(rd: u8, rn: u8) -> u32 {
    0x6ea0_f800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fneg vD.2d, vN.2d`.
pub fn fneg_v2d(rd: u8, rn: u8) -> u32 {
    0x6ee0_f800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fsqrt vD.4s, vN.4s`.
pub fn fsqrt_v4s(rd: u8, rn: u8) -> u32 {
    0x6ea1_f800 | (reg5(rn) << 5) | reg5(rd)
}

/// `fsqrt vD.2d, vN.2d`.
pub fn fsqrt_v2d(rd: u8, rn: u8) -> u32 {
    0x6ee1_f800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frecpe vD.4s, vN.4s`.
pub fn frecpe_v4s(rd: u8, rn: u8) -> u32 {
    0x4ea1_d800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frecpe vD.2d, vN.2d`.
pub fn frecpe_v2d(rd: u8, rn: u8) -> u32 {
    0x4ee1_d800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frsqrte vD.4s, vN.4s`.
pub fn frsqrte_v4s(rd: u8, rn: u8) -> u32 {
    0x6ea1_d800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frsqrte vD.2d, vN.2d`.
pub fn frsqrte_v2d(rd: u8, rn: u8) -> u32 {
    0x6ee1_d800 | (reg5(rn) << 5) | reg5(rd)
}

/// `faddp vD.4s, vN.4s, vM.4s`.
pub fn faddp_v4s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x6e20_d400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `faddp vD.2d, vN.2d, vM.2d`.
pub fn faddp_v2d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x6e60_d400 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `faddp dD, vN.2d`.
pub fn faddp_d_from_v2d(rd: u8, rn: u8) -> u32 {
    0x7e70_d800 | (reg5(rn) << 5) | reg5(rd)
}

/// `frecps vD.4s, vN.4s, vM.4s`.
pub fn frecps_v4s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4e20_fc00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `frecps vD.2d, vN.2d, vM.2d`.
pub fn frecps_v2d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4e60_fc00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `frsqrts vD.4s, vN.4s, vM.4s`.
pub fn frsqrts_v4s(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4ea0_fc00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `frsqrts vD.2d, vN.2d, vM.2d`.
pub fn frsqrts_v2d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4ee0_fc00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `smull vD.<wide T>, vN.<narrow T>, vM.<narrow T>`.
pub fn smull_v(rd: u8, rn: u8, rm: u8, size: u8) -> u32 {
    simd_three_same(0x0e20_c000, rd, rn, rm, size, false)
}

/// `umull vD.<wide T>, vN.<narrow T>, vM.<narrow T>`.
pub fn umull_v(rd: u8, rn: u8, rm: u8, size: u8) -> u32 {
    simd_three_same(0x2e20_c000, rd, rn, rm, size, false)
}

/// `pmull vD.<wide T>, vN.<narrow T>, vM.<narrow T>`.
pub fn pmull_v(rd: u8, rn: u8, rm: u8, size: u8) -> u32 {
    simd_three_same(0x0e20_e000, rd, rn, rm, size, false)
}

/// `sqdmull vD.<wide T>, vN.<narrow T>, vM.<narrow T>`.
pub fn sqdmull_v(rd: u8, rn: u8, rm: u8, size: u8) -> u32 {
    simd_three_same(0x0e20_d000, rd, rn, rm, size, false)
}

/// `and vD.16b, vN.16b, vM.16b`.
pub fn and_v16b(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4e20_1c00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `and vD.8b, vN.8b, vM.8b`.
pub fn and_v8b(rd: u8, rn: u8, rm: u8) -> u32 {
    0x0e20_1c00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `bic vD.16b, vN.16b, vM.16b`.
pub fn bic_v16b(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4e60_1c00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `eor vD.16b, vN.16b, vM.16b`.
pub fn eor_v16b(rd: u8, rn: u8, rm: u8) -> u32 {
    0x6e20_1c00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `eor vD.8b, vN.8b, vM.8b`.
pub fn eor_v8b(rd: u8, rn: u8, rm: u8) -> u32 {
    0x2e20_1c00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `bsl vD.8b, vN.8b, vM.8b`.
pub fn bsl_v8b(rd: u8, rn: u8, rm: u8) -> u32 {
    0x2e60_1c00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `orr vD.16b, vN.16b, vM.16b`.
pub fn orr_v16b(rd: u8, rn: u8, rm: u8) -> u32 {
    0x4ea0_1c00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `orr vD.8b, vN.8b, vM.8b`.
pub fn orr_v8b(rd: u8, rn: u8, rm: u8) -> u32 {
    0x0ea0_1c00 | (reg5(rm) << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `cmeq vD.<T>, vN.<T>, vM.<T>`.
pub fn cmeq_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x2e20_8c00, rd, rn, rm, size, q)
}

/// `cmgt vD.<T>, vN.<T>, vM.<T>`.
pub fn cmgt_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e20_3400, rd, rn, rm, size, q)
}

/// `cmge vD.<T>, vN.<T>, vM.<T>`.
pub fn cmge_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e20_3c00, rd, rn, rm, size, q)
}

/// `cmge vD.<T>, vN.<T>, #0`.
pub fn cmge_v_zero(rd: u8, rn: u8, size: u8, q: bool) -> u32 {
    simd_two_same(0x2e20_8800, rd, rn, size, q)
}

/// `cmeq vD.<T>, vN.<T>, #0`.
pub fn cmeq_v_zero(rd: u8, rn: u8, size: u8, q: bool) -> u32 {
    simd_two_same(0x0e20_9800, rd, rn, size, q)
}

/// `cmhi vD.<T>, vN.<T>, vM.<T>`.
pub fn cmhi_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x2e20_3400, rd, rn, rm, size, q)
}

/// `cmhs vD.<T>, vN.<T>, vM.<T>`.
pub fn cmhs_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x2e20_3c00, rd, rn, rm, size, q)
}

/// `smax vD.<T>, vN.<T>, vM.<T>`.
pub fn smax_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e20_6400, rd, rn, rm, size, q)
}

/// `umax vD.<T>, vN.<T>, vM.<T>`.
pub fn umax_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x2e20_6400, rd, rn, rm, size, q)
}

/// `smin vD.<T>, vN.<T>, vM.<T>`.
pub fn smin_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e20_6c00, rd, rn, rm, size, q)
}

/// `umin vD.<T>, vN.<T>, vM.<T>`.
pub fn umin_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x2e20_6c00, rd, rn, rm, size, q)
}

/// `addp vD.<T>, vN.<T>, vM.<T>`.
pub fn addp_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e20_bc00, rd, rn, rm, size, q)
}

/// `addv <Bd/Hd/Sd>, vN.<16B/8H/4S>`.
pub fn addv_from_v(rd: u8, rn: u8, size: u8) -> u32 {
    0x4e31_b800 | (simd_size(size) << 22) | (reg5(rn) << 5) | reg5(rd)
}

/// `uaddlv <Hd/Sd/Dd>, vN.<8B/4H/4S>` (or the corresponding 128-bit source).
pub fn uaddlv_from_v(rd: u8, rn: u8, size: u8, q: bool) -> u32 {
    0x2e30_3800 | simd_arrange(size, q) | (reg5(rn) << 5) | reg5(rd)
}

/// `addp Dd, vN.2D`.
pub fn addp_d_from_v2d(rd: u8, rn: u8) -> u32 {
    0x5ef1_b800 | (reg5(rn) << 5) | reg5(rd)
}

/// `smaxp vD.<T>, vN.<T>, vM.<T>`.
pub fn smaxp_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e20_a400, rd, rn, rm, size, q)
}

/// `umaxp vD.<T>, vN.<T>, vM.<T>`.
pub fn umaxp_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x2e20_a400, rd, rn, rm, size, q)
}

/// `sminp vD.<T>, vN.<T>, vM.<T>`.
pub fn sminp_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e20_ac00, rd, rn, rm, size, q)
}

/// `uminp vD.<T>, vN.<T>, vM.<T>`.
pub fn uminp_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x2e20_ac00, rd, rn, rm, size, q)
}

/// `sqadd vD.<T>, vN.<T>, vM.<T>`.
pub fn sqadd_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e20_0c00, rd, rn, rm, size, q)
}

/// `sqsub vD.<T>, vN.<T>, vM.<T>`.
pub fn sqsub_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e20_2c00, rd, rn, rm, size, q)
}

/// `uqadd vD.<T>, vN.<T>, vM.<T>`.
pub fn uqadd_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x2e20_0c00, rd, rn, rm, size, q)
}

/// `uqsub vD.<T>, vN.<T>, vM.<T>`.
pub fn uqsub_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x2e20_2c00, rd, rn, rm, size, q)
}

/// `shadd vD.<T>, vN.<T>, vM.<T>`.
pub fn shadd_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e20_0400, rd, rn, rm, size, q)
}

/// `uhadd vD.<T>, vN.<T>, vM.<T>`.
pub fn uhadd_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x2e20_0400, rd, rn, rm, size, q)
}

/// `shsub vD.<T>, vN.<T>, vM.<T>`.
pub fn shsub_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e20_2400, rd, rn, rm, size, q)
}

/// `uhsub vD.<T>, vN.<T>, vM.<T>`.
pub fn uhsub_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x2e20_2400, rd, rn, rm, size, q)
}

/// `srhadd vD.<T>, vN.<T>, vM.<T>`.
pub fn srhadd_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e20_1400, rd, rn, rm, size, q)
}

/// `urhadd vD.<T>, vN.<T>, vM.<T>`.
pub fn urhadd_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x2e20_1400, rd, rn, rm, size, q)
}

/// `sshl vD.<T>, vN.<T>, vM.<T>`.
pub fn sshl_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e20_4400, rd, rn, rm, size, q)
}

/// `ushl vD.<T>, vN.<T>, vM.<T>`.
pub fn ushl_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x2e20_4400, rd, rn, rm, size, q)
}

/// `srshl vD.<T>, vN.<T>, vM.<T>`.
pub fn srshl_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e20_5400, rd, rn, rm, size, q)
}

/// `urshl vD.<T>, vN.<T>, vM.<T>`.
pub fn urshl_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x2e20_5400, rd, rn, rm, size, q)
}

/// `sabd vD.<T>, vN.<T>, vM.<T>`.
pub fn sabd_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e20_7400, rd, rn, rm, size, q)
}

/// `uabd vD.<T>, vN.<T>, vM.<T>`.
pub fn uabd_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x2e20_7400, rd, rn, rm, size, q)
}

/// `pmul vD.16b, vN.16b, vM.16b`.
pub fn pmul_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x2e20_9c00, rd, rn, rm, size, q)
}

/// `sqdmulh vD.<T>, vN.<T>, vM.<T>`.
pub fn sqdmulh_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e20_b400, rd, rn, rm, size, q)
}

/// `sqrdmulh vD.<T>, vN.<T>, vM.<T>`.
pub fn sqrdmulh_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x2e20_b400, rd, rn, rm, size, q)
}

/// `sqshl vD.<T>, vN.<T>, vM.<T>`.
pub fn sqshl_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e20_4c00, rd, rn, rm, size, q)
}

/// `uqshl vD.<T>, vN.<T>, vM.<T>`.
pub fn uqshl_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x2e20_4c00, rd, rn, rm, size, q)
}

/// `zip1 vD.<T>, vN.<T>, vM.<T>`.
pub fn zip1_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e00_3800, rd, rn, rm, size, q)
}

/// `zip2 vD.<T>, vN.<T>, vM.<T>`.
pub fn zip2_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e00_7800, rd, rn, rm, size, q)
}

/// `uzp1 vD.<T>, vN.<T>, vM.<T>`.
pub fn uzp1_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e00_1800, rd, rn, rm, size, q)
}

/// `uzp2 vD.<T>, vN.<T>, vM.<T>`.
pub fn uzp2_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e00_5800, rd, rn, rm, size, q)
}

/// `trn1 vD.<T>, vN.<T>, vM.<T>`.
pub fn trn1_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e00_2800, rd, rn, rm, size, q)
}

/// `trn2 vD.<T>, vN.<T>, vM.<T>`.
pub fn trn2_v(rd: u8, rn: u8, rm: u8, size: u8, q: bool) -> u32 {
    simd_three_same(0x0e00_6800, rd, rn, rm, size, q)
}

/// `abs vD.<T>, vN.<T>`.
pub fn abs_v(rd: u8, rn: u8, size: u8, q: bool) -> u32 {
    simd_two_same(0x0e20_b800, rd, rn, size, q)
}

/// `neg vD.<T>, vN.<T>`.
pub fn neg_v(rd: u8, rn: u8, size: u8, q: bool) -> u32 {
    simd_two_same(0x2e20_b800, rd, rn, size, q)
}

/// `sqabs vD.<T>, vN.<T>`.
pub fn sqabs_v(rd: u8, rn: u8, size: u8, q: bool) -> u32 {
    simd_two_same(0x0e20_7800, rd, rn, size, q)
}

/// `sqneg vD.<T>, vN.<T>`.
pub fn sqneg_v(rd: u8, rn: u8, size: u8, q: bool) -> u32 {
    simd_two_same(0x2e20_7800, rd, rn, size, q)
}

/// `suqadd vD.<T>, vN.<T>`.
pub fn suqadd_v(rd: u8, rn: u8, size: u8, q: bool) -> u32 {
    simd_two_same(0x0e20_3800, rd, rn, size, q)
}

/// `usqadd vD.<T>, vN.<T>`.
pub fn usqadd_v(rd: u8, rn: u8, size: u8, q: bool) -> u32 {
    simd_two_same(0x2e20_3800, rd, rn, size, q)
}

/// `saddlp vD.<wide T>, vN.<narrow T>`.
pub fn saddlp_v(rd: u8, rn: u8, size: u8) -> u32 {
    simd_two_same(0x0e20_2800, rd, rn, size, true)
}

/// `uaddlp vD.<wide T>, vN.<narrow T>`.
pub fn uaddlp_v(rd: u8, rn: u8, size: u8) -> u32 {
    simd_two_same(0x2e20_2800, rd, rn, size, true)
}

/// `mvn vD.16b, vN.16b`.
pub fn not_v16b(rd: u8, rn: u8) -> u32 {
    0x6e20_5800 | (reg5(rn) << 5) | reg5(rd)
}

/// `clz vD.<T>, vN.<T>`.
pub fn clz_v(rd: u8, rn: u8, size: u8, q: bool) -> u32 {
    simd_two_same(0x2e20_4800, rd, rn, size, q)
}

/// `cnt vD.16b, vN.16b`.
pub fn cnt_v16b(rd: u8, rn: u8) -> u32 {
    0x4e20_5800 | (reg5(rn) << 5) | reg5(rd)
}

/// `rbit vD.16b, vN.16b`.
pub fn rbit_v16b(rd: u8, rn: u8) -> u32 {
    0x6e60_5800 | (reg5(rn) << 5) | reg5(rd)
}

/// `urecpe vD.4s, vN.4s`.
pub fn urecpe_v4s(rd: u8, rn: u8) -> u32 {
    0x4ea1_c800 | (reg5(rn) << 5) | reg5(rd)
}

/// `ursqrte vD.4s, vN.4s`.
pub fn ursqrte_v4s(rd: u8, rn: u8) -> u32 {
    0x6ea1_c800 | (reg5(rn) << 5) | reg5(rd)
}

/// `rev16 vD.16b, vN.16b`.
pub fn rev16_v16b(rd: u8, rn: u8) -> u32 {
    0x4e20_1800 | (reg5(rn) << 5) | reg5(rd)
}

/// `rev32 vD.<T>, vN.<T>`.
pub fn rev32_v(rd: u8, rn: u8, size: u8, q: bool) -> u32 {
    match size {
        8 => 0x2e20_0800 | ((q as u32) << 30) | (reg5(rn) << 5) | reg5(rd),
        16 => 0x2e60_0800 | ((q as u32) << 30) | (reg5(rn) << 5) | reg5(rd),
        _ => panic!("unsupported REV32 element size: {size}"),
    }
}

/// `rev64 vD.<T>, vN.<T>`.
pub fn rev64_v(rd: u8, rn: u8, size: u8, q: bool) -> u32 {
    match size {
        8 => 0x0e20_0800 | ((q as u32) << 30) | (reg5(rn) << 5) | reg5(rd),
        16 => 0x0e60_0800 | ((q as u32) << 30) | (reg5(rn) << 5) | reg5(rd),
        32 => 0x0ea0_0800 | ((q as u32) << 30) | (reg5(rn) << 5) | reg5(rd),
        _ => panic!("unsupported REV64 element size: {size}"),
    }
}

/// `rev wD, wN`.
pub fn rev_w(rd: u8, rn: u8) -> u32 {
    0x5ac0_0800 | (reg5(rn) << 5) | reg5(rd)
}

/// `rev xD, xN`.
pub fn rev_x(rd: u8, rn: u8) -> u32 {
    0xdac0_0c00 | (reg5(rn) << 5) | reg5(rd)
}

/// `rev16 wD, wN`.
pub fn rev16_w(rd: u8, rn: u8) -> u32 {
    0x5ac0_0400 | (reg5(rn) << 5) | reg5(rd)
}

/// `sshr vD.<T>, vN.<T>, #shift`.
pub fn sshr_v(rd: u8, rn: u8, size: u8, shift: u8, q: bool) -> u32 {
    simd_shift_right(0x0f00_0400, rd, rn, size, shift, q)
}

/// `ushr vD.<T>, vN.<T>, #shift`.
pub fn ushr_v(rd: u8, rn: u8, size: u8, shift: u8, q: bool) -> u32 {
    simd_shift_right(0x2f00_0400, rd, rn, size, shift, q)
}

/// `shl vD.<T>, vN.<T>, #shift`.
pub fn shl_v(rd: u8, rn: u8, size: u8, shift: u8, q: bool) -> u32 {
    simd_shift_left(0x0f00_5400, rd, rn, size, shift, q)
}

/// `sqshlu vD.<T>, vN.<T>, #shift`.
pub fn sqshlu_v(rd: u8, rn: u8, size: u8, shift: u8, q: bool) -> u32 {
    simd_shift_left(0x2f00_6400, rd, rn, size, shift, q)
}

/// `ext vD.16b, vN.16b, vM.16b, #index`.
pub fn ext_v16b(rd: u8, rn: u8, rm: u8, index: u8, q: bool) -> u32 {
    let limit = if q { 16 } else { 8 };
    assert!(index < limit, "AArch64 EXT index out of range");
    0x2e00_0000
        | ((q as u32) << 30)
        | ((reg5(rm)) << 16)
        | ((index as u32) << 11)
        | (reg5(rn) << 5)
        | reg5(rd)
}

/// `tbl vD.8b/16b, {vN.16b..}, vM.8b/16b`.
pub fn tbl_v(rd: u8, rn: u8, rm: u8, list_len: u8, q: bool) -> u32 {
    assert!((1..=4).contains(&list_len), "TBL list length out of range");
    0x0e00_0000
        | ((q as u32) << 30)
        | (((list_len - 1) as u32) << 13)
        | (reg5(rm) << 16)
        | (reg5(rn) << 5)
        | reg5(rd)
}

/// `tbx vD.8b/16b, {vN.16b..}, vM.8b/16b`.
pub fn tbx_v(rd: u8, rn: u8, rm: u8, list_len: u8, q: bool) -> u32 {
    tbl_v(rd, rn, rm, list_len, q) | 0x1000
}

/// `uxtl vD.<wide T>, vN.<narrow T>`.
pub fn uxtl_v(rd: u8, rn: u8, size: u8) -> u32 {
    let immh = match size {
        8 => 0x08,
        16 => 0x10,
        32 => 0x20,
        _ => panic!("unsupported UXTL source element size: {size}"),
    };
    0x2f00_a400 | (immh << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `sxtl vD.<wide T>, vN.<narrow T>`.
pub fn sxtl_v(rd: u8, rn: u8, size: u8) -> u32 {
    let immh = match size {
        8 => 0x08,
        16 => 0x10,
        32 => 0x20,
        _ => panic!("unsupported SXTL source element size: {size}"),
    };
    0x0f00_a400 | (immh << 16) | (reg5(rn) << 5) | reg5(rd)
}

/// `xtn vD.<narrow T>, vN.<wide T>`.
pub fn xtn_v(rd: u8, rn: u8, source_size: u8) -> u32 {
    simd_narrow(0x0e20_2800, rd, rn, source_size)
}

/// `shrn vD.<narrow T>, vN.<wide T>, #shift`.
pub fn shrn_v(rd: u8, rn: u8, source_size: u8, shift: u8) -> u32 {
    assert!(
        (1..=source_size / 2).contains(&shift),
        "AArch64 SHRN shift out of range"
    );
    simd_shift_right(0x0f00_8400, rd, rn, source_size, shift, false)
}

/// `sqxtn vD.<narrow T>, vN.<wide T>`.
pub fn sqxtn_v(rd: u8, rn: u8, source_size: u8) -> u32 {
    simd_narrow(0x0e20_4800, rd, rn, source_size)
}

/// `sqxtun vD.<narrow T>, vN.<wide T>`.
pub fn sqxtun_v(rd: u8, rn: u8, source_size: u8) -> u32 {
    simd_narrow(0x2e20_2800, rd, rn, source_size)
}

/// `uqxtn vD.<narrow T>, vN.<wide T>`.
pub fn uqxtn_v(rd: u8, rn: u8, source_size: u8) -> u32 {
    simd_narrow(0x2e20_4800, rd, rn, source_size)
}

/// `mrs xT, NZCV`.
pub fn mrs_nzcv(rt: u8) -> u32 {
    0xd53b_4200 | reg5(rt)
}

/// `msr NZCV, xT`.
pub fn msr_nzcv(rt: u8) -> u32 {
    0xd51b_4200 | reg5(rt)
}

/// `cbnz wT, label`.
///
/// `pc_offset_bytes` is the byte distance from this instruction to the target
/// label as used by the small emitters in this module.
pub fn cbnz_w(rt: u8, pc_offset_bytes: i32) -> u32 {
    0x3500_0000 | (imm19(pc_offset_bytes) << 5) | reg5(rt)
}

/// `cbnz xT, label`.
///
/// `pc_offset_bytes` is the byte distance from this instruction to the target
/// label as used by the small emitters in this module.
pub fn cbnz_x(rt: u8, pc_offset_bytes: i32) -> u32 {
    0xb500_0000 | (imm19(pc_offset_bytes) << 5) | reg5(rt)
}

/// `dmb ish`.
pub fn dmb_ish() -> u32 {
    0xd503_3bbf
}

/// `dsb sy`.
pub fn dsb_sy() -> u32 {
    0xd503_3f9f
}

/// `dmb sy`.
pub fn dmb_sy() -> u32 {
    0xd503_3fbf
}

/// `stp xT, xT2, [sp, #imm]!`.
pub fn stp_x_pre_sp(rt: u8, rt2: u8, imm_bytes: i32) -> u32 {
    0xa980_0000 | (imm7_scaled(imm_bytes, 8) << 15) | (reg5(rt2) << 10) | (31 << 5) | reg5(rt)
}

/// `ldp xT, xT2, [sp], #imm`.
pub fn ldp_x_post_sp(rt: u8, rt2: u8, imm_bytes: i32) -> u32 {
    0xa8c0_0000 | (imm7_scaled(imm_bytes, 8) << 15) | (reg5(rt2) << 10) | (31 << 5) | reg5(rt)
}

/// `stp xT, xT2, [sp, #imm]`.
pub fn stp_x_offset_sp(rt: u8, rt2: u8, imm_bytes: i32) -> u32 {
    0xa900_0000 | (imm7_scaled(imm_bytes, 8) << 15) | (reg5(rt2) << 10) | (31 << 5) | reg5(rt)
}

/// `ldp xT, xT2, [sp, #imm]`.
pub fn ldp_x_offset_sp(rt: u8, rt2: u8, imm_bytes: i32) -> u32 {
    0xa940_0000 | (imm7_scaled(imm_bytes, 8) << 15) | (reg5(rt2) << 10) | (31 << 5) | reg5(rt)
}

/// `stp xT, xT2, [xN, #imm]`.
pub fn stp_x_offset(rt: u8, rt2: u8, rn: u8, imm_bytes: i32) -> u32 {
    0xa900_0000 | (imm7_scaled(imm_bytes, 8) << 15) | (reg5(rt2) << 10) | (reg5(rn) << 5) | reg5(rt)
}

/// `ldp xT, xT2, [xN, #imm]`.
pub fn ldp_x_offset(rt: u8, rt2: u8, rn: u8, imm_bytes: i32) -> u32 {
    0xa940_0000 | (imm7_scaled(imm_bytes, 8) << 15) | (reg5(rt2) << 10) | (reg5(rn) << 5) | reg5(rt)
}

/// `stp qT, qT2, [sp, #imm]`.
pub fn stp_q_offset_sp(rt: u8, rt2: u8, imm_bytes: i32) -> u32 {
    0xad00_0000 | (imm7_scaled(imm_bytes, 16) << 15) | (reg5(rt2) << 10) | (31 << 5) | reg5(rt)
}

/// `ldp qT, qT2, [sp, #imm]`.
pub fn ldp_q_offset_sp(rt: u8, rt2: u8, imm_bytes: i32) -> u32 {
    0xad40_0000 | (imm7_scaled(imm_bytes, 16) << 15) | (reg5(rt2) << 10) | (31 << 5) | reg5(rt)
}

/// `stp x29, x30, [sp, #-16]!`.
pub fn stp_fp_lr_pre_16() -> u32 {
    stp_x_pre_sp(29, 30, -16)
}

/// `ldp x29, x30, [sp], #16`.
pub fn ldp_fp_lr_post_16() -> u32 {
    ldp_x_post_sp(29, 30, 16)
}

/// `ret xN`.
pub fn ret(rn: u8) -> u32 {
    0xd65f_0000 | (reg5(rn) << 5)
}

/// `ret`.
pub fn ret_lr() -> u32 {
    ret(30)
}

/// `nop`.
pub fn nop() -> u32 {
    0xd503_201f
}

/// `brk #imm16`.
pub fn brk(imm16: u16) -> u32 {
    0xd420_0000 | ((imm16 as u32) << 5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_known_arm64_words() {
        assert_eq!(nop(), 0xd503_201f);
        assert_eq!(brk(0), 0xd420_0000);
        assert_eq!(ret_lr(), 0xd65f_03c0);
        assert_eq!(mov_x(28, 1), 0xaa01_03fc);
        assert_eq!(mov_w(28, 1), 0x2a01_03fc);
        assert_eq!(movz_w(16, 1, 0), 0x5280_0030);
        assert_eq!(movk_w(16, 0x1234, 16), 0x72a2_4690);
        assert_eq!(movz_x(0, 0x1234, 0), 0xd282_4680);
        assert_eq!(movk_x(0, 0xabcd, 16), 0xf2b5_79a0);
        assert_eq!(aese_v16b(16, 17), 0x4e28_4a30);
        assert_eq!(aesd_v16b(16, 17), 0x4e28_5a30);
        assert_eq!(aesmc_v16b(16, 17), 0x4e28_6a30);
        assert_eq!(aesimc_v16b(16, 17), 0x4e28_7a30);
        assert_eq!(crc32b_w(16, 17, 18), 0x1ad2_4230);
        assert_eq!(crc32h_w(16, 17, 18), 0x1ad2_4630);
        assert_eq!(crc32w_w(16, 17, 18), 0x1ad2_4a30);
        assert_eq!(crc32x_x(16, 17, 18), 0x9ad2_4e30);
        assert_eq!(crc32cb_w(16, 17, 18), 0x1ad2_5230);
        assert_eq!(crc32ch_w(16, 17, 18), 0x1ad2_5630);
        assert_eq!(crc32cw_w(16, 17, 18), 0x1ad2_5a30);
        assert_eq!(crc32cx_x(16, 17, 18), 0x9ad2_5e30);
        assert_eq!(sha256h_q(16, 17, 18), 0x5e12_4230);
        assert_eq!(sha256h2_q(16, 17, 18), 0x5e12_5230);
        assert_eq!(sha256su0_v4s(16, 17), 0x5e28_2a30);
        assert_eq!(sha256su1_v4s(16, 17, 18), 0x5e12_6230);
        assert_eq!(sub_sp_imm(224), 0xd103_83ff);
        assert_eq!(sub_sp_imm(1184), 0xd112_83ff);
        assert_eq!(sub_x_imm(26, 26, 7), 0xd100_1f5a);
        assert_eq!(sub_x_imm_shift(26, 26, 7, false), sub_x_imm(26, 26, 7));
        assert_eq!(
            sub_x_imm_shift(26, 26, 7, true),
            sub_x_imm(26, 26, 7) | (1 << 22)
        );
        assert_eq!(sub_x_reg(1, 1, 26), 0xcb1a_0021);
        assert_eq!(subs_x_imm_shift(31, 26, 0, false), cmp_x_imm(26, 0));
        assert_eq!(subs_x_reg(16, 17, 18), 0xeb12_0230);
        assert_eq!(sub_w_imm(30, 30, 16), 0x5100_43de);
        assert_eq!(sub_w_imm_shift(30, 30, 16, false), sub_w_imm(30, 30, 16));
        assert_eq!(
            sub_w_imm_shift(30, 30, 16, true),
            sub_w_imm(30, 30, 16) | (1 << 22)
        );
        assert_eq!(subs_w_imm_shift(31, 26, 0, false), cmp_w_imm(26, 0));
        assert_eq!(subs_w_reg(16, 17, 18), 0x6b12_0230);
        assert_eq!(adc_w(1, 2, 3), 0x1a03_0041);
        assert_eq!(adc_x(1, 2, 3), 0x9a03_0041);
        assert_eq!(adcs_w(1, 2, 3), 0x3a03_0041);
        assert_eq!(adcs_x(1, 2, 3), 0xba03_0041);
        assert_eq!(sbc_w(1, 2, 3), 0x5a03_0041);
        assert_eq!(sbc_x(1, 2, 3), 0xda03_0041);
        assert_eq!(sbcs_w(1, 2, 3), 0x7a03_0041);
        assert_eq!(sbcs_x(1, 2, 3), 0xfa03_0041);
        assert_eq!(cmp_x_imm(26, 0), 0xf100_035f);
        assert_eq!(cmp_x_reg(0, 16), 0xeb10_001f);
        assert_eq!(cmp_w_reg(16, 17), 0x6b11_021f);
        assert_eq!(add_sp_imm(1184), 0x9112_83ff);
        assert_eq!(add_x_imm(17, 17, 0x123), 0x9104_8e31);
        assert_eq!(
            add_x_imm_shift(17, 17, 0x123, false),
            add_x_imm(17, 17, 0x123)
        );
        assert_eq!(
            add_x_imm_shift(17, 17, 0x123, true),
            add_x_imm(17, 17, 0x123) | (1 << 22)
        );
        assert_eq!(adds_x_imm_shift(31, 17, 0x123, false), 0xb104_8e3f);
        assert_eq!(add_w_imm(30, 30, 16), 0x1100_43de);
        assert_eq!(add_w_imm_shift(30, 30, 16, false), add_w_imm(30, 30, 16));
        assert_eq!(
            add_w_imm_shift(30, 30, 16, true),
            add_w_imm(30, 30, 16) | (1 << 22)
        );
        assert_eq!(adds_w_imm_shift(31, 17, 0x123, false), 0x3104_8e3f);
        assert_eq!(adds_x_reg(16, 17, 18), 0xab12_0230);
        assert_eq!(add_x_reg_sp(2, 31, 30), 0x8b3e_63e2);
        assert_eq!(add_x_reg(16, 17, 18), 0x8b12_0230);
        assert_eq!(add_w_reg(16, 17, 18), 0x0b12_0230);
        assert_eq!(adds_w_reg(16, 17, 18), 0x2b12_0230);
        assert_eq!(tst_w_reg(17, 18), 0x6a12_023f);
        assert_eq!(tst_x_reg(17, 18), 0xea12_023f);
        assert_eq!(mvn_w(16, 17), 0x2a31_03f0);
        assert_eq!(mvn_x(16, 17), 0xaa31_03f0);
        assert_eq!(ubfiz_w(1, 2, 29, 1), 0x5303_0041);
        assert_eq!(neg_w(1, 2), 0x4b02_03e1);
        assert_eq!(rev_w(1, 2), 0x5ac0_0841);
        assert_eq!(rev_x(1, 2), 0xdac0_0c41);
        assert_eq!(rev16_w(1, 2), 0x5ac0_0441);
        assert_eq!(add_x_reg_uxtw(16, 17, 18), 0x8b32_4230);
        assert_eq!(ldr_x_lit(0, 16), 0x5800_0080);
        assert_eq!(adrp(17, 0), 0x9000_0011);
        assert_eq!(adrp(17, 4096), 0xb000_0011);
        assert_eq!(adrp(17, -4096), 0xf0ff_fff1);
        assert_eq!(br(16), 0xd61f_0200);
        assert_eq!(b_imm(8), 0x1400_0002);
        assert_eq!(bl_imm(8), 0x9400_0002);
        assert_eq!(b_imm(-4), 0x17ff_ffff);
        assert_eq!(bl_imm(-4), 0x97ff_ffff);
        assert_eq!(b_cond(Cond::LE, 8), 0x5400_004d);
        assert_eq!(cbz_w(16, 8), 0x3400_0050);
        assert_eq!(cbz_x(16, 8), 0xb400_0050);
        assert_eq!(blr(17), 0xd63f_0220);
        assert_eq!(ldarb_w(16, 17), 0x08df_fe30);
        assert_eq!(ldarh_w(16, 17), 0x48df_fe30);
        assert_eq!(ldar_w(16, 27), 0x88df_ff70);
        assert_eq!(ldar_x(16, 27), 0xc8df_ff70);
        assert_eq!(ldaxr_w(16, 27), 0x885f_ff70);
        assert_eq!(stlrb_w(16, 17), 0x089f_fe30);
        assert_eq!(stlrh_w(16, 17), 0x489f_fe30);
        assert_eq!(stlr_w(16, 17), 0x889f_fe30);
        assert_eq!(stlr_x(16, 17), 0xc89f_fe30);
        assert_eq!(stlxr_w(17, 16, 27), 0x8811_ff70);
        assert_eq!(stp_w_offset(16, 17, 28, 0), 0x2900_4790);
        assert_eq!(stp_w_offset(16, 17, 28, 4), 0x2900_c790);
        assert_eq!(ldp_w_offset(16, 17, 28, 16), 0x2942_4790);
        assert_eq!(str_w_unsigned(31, 27, 0), 0xb900_037f);
        assert_eq!(str_w_unsigned(30, 31, 1152), 0xb904_83fe);
        assert_eq!(ldr_w_unsigned(16, 28, 12), 0xb940_0f90);
        assert_eq!(ldr_w_unsigned(30, 31, 1152), 0xb944_83fe);
        assert_eq!(ldrb_w_reg_lsl(16, 17, 18), 0x3872_6a30);
        assert_eq!(ldrh_w_reg_lsl(16, 17, 18), 0x7872_6a30);
        assert_eq!(ldr_w_reg_lsl(16, 17, 18), 0xb872_6a30);
        assert_eq!(ldr_x_reg_lsl(16, 17, 18), 0xf872_6a30);
        assert_eq!(ldr_x_reg_lsl3(16, 27, 16), 0xf870_7b70);
        assert_eq!(ldrb_w_reg_uxtw(16, 17, 18), 0x3872_4a30);
        assert_eq!(ldrh_w_reg_uxtw(16, 17, 18), 0x7872_4a30);
        assert_eq!(ldr_w_reg_uxtw(16, 17, 18), 0xb872_4a30);
        assert_eq!(ldr_x_reg_uxtw(16, 17, 18), 0xf872_4a30);
        assert_eq!(strb_w_reg_lsl(16, 17, 18), 0x3832_6a30);
        assert_eq!(strh_w_reg_lsl(16, 17, 18), 0x7832_6a30);
        assert_eq!(str_w_reg_lsl(16, 17, 18), 0xb832_6a30);
        assert_eq!(str_x_reg_lsl(16, 17, 18), 0xf832_6a30);
        assert_eq!(strb_w_reg_uxtw(16, 17, 18), 0x3832_4a30);
        assert_eq!(strh_w_reg_uxtw(16, 17, 18), 0x7832_4a30);
        assert_eq!(str_w_reg_uxtw(16, 17, 18), 0xb832_4a30);
        assert_eq!(str_x_reg_uxtw(16, 17, 18), 0xf832_4a30);
        assert_eq!(and_w_imm(30, 30, 0x70), 0x121c_0bde);
        assert_eq!(and_w_imm(16, 17, 0xf000_0000), 0x1204_0e30);
        assert_eq!(and_w_imm(16, 17, 0x0800_0000), 0x1205_0230);
        assert_eq!(and_w_imm(16, 17, 0x2000_0000), 0x1203_0230);
        assert_eq!(and_w_imm(16, 16, 0x3000_0000), 0x1204_0610);
        assert_eq!(and_w_imm(16, 16, 0x1000_0000), 0x1204_0210);
        assert_eq!(and_w_imm(16, 16, 0x0101_0101), 0x1200_c210);
        assert_eq!(orr_w_imm(16, 16, 0x2000_0000), 0x3203_0210);
        assert_eq!(and_w_reg(0, 0, 16), 0x0a10_0000);
        assert_eq!(and_w_reg(16, 17, 18), 0x0a12_0230);
        assert_eq!(and_x_imm(1, 1, 0x00ff_ffff_ffff_ffff), 0x9240_dc21);
        assert_eq!(and_x_reg(16, 17, 18), 0x8a12_0230);
        assert_eq!(ands_w_reg(16, 17, 18), 0x6a12_0230);
        assert_eq!(ands_x_reg(16, 17, 18), 0xea12_0230);
        assert_eq!(eor_w_reg(16, 17, 18), 0x4a12_0230);
        assert_eq!(eor_x_reg(16, 17, 18), 0xca12_0230);
        assert_eq!(tst_x_imm(16, 0x4), 0xf27e_021f);
        assert_eq!(tst_x_imm(16, 0xffff_ffff_f800_0000), 0xf265_921f);
        assert_eq!(tst_x_imm(16, 0xffff_ffff_f000_0000), 0xf264_8e1f);
        assert_eq!(ldrb_w_unsigned(16, 31, 1172), 0x3952_53f0);
        assert_eq!(strb_w_unsigned(16, 28, 800), 0x390c_8390);
        assert_eq!(orr_w(16, 16, 17), 0x2a11_0210);
        assert_eq!(orr_w_lsl(16, 16, 17, 27), 0x2a11_6e10);
        assert_eq!(orr_w_lsr(0, 0, 16, 12), 0x2a50_3000);
        assert_eq!(orr_x(16, 17, 18), 0xaa12_0230);
        assert_eq!(cmp_w_imm(16, 32), 0x7100_821f);
        assert_eq!(lsr_w_imm(16, 17, 16), 0x5310_7e30);
        assert_eq!(lsr_x_imm(16, 0, 12), 0xd34c_fc10);
        assert_eq!(lsl_w_imm(17, 16, 8), 0x5318_5e11);
        assert_eq!(asr_w_imm(16, 17, 5), 0x1305_7e30);
        assert_eq!(asr_x_imm(16, 17, 5), 0x9345_fe30);
        assert_eq!(lslv_w(16, 17, 18), 0x1ad2_2230);
        assert_eq!(lsrv_w(16, 17, 18), 0x1ad2_2630);
        assert_eq!(asrv_w(16, 17, 18), 0x1ad2_2a30);
        assert_eq!(rorv_w(16, 17, 18), 0x1ad2_2e30);
        assert_eq!(lslv_x(16, 17, 18), 0x9ad2_2230);
        assert_eq!(lsrv_x(16, 17, 18), 0x9ad2_2630);
        assert_eq!(asrv_x(16, 17, 18), 0x9ad2_2a30);
        assert_eq!(rorv_x(16, 17, 18), 0x9ad2_2e30);
        assert_eq!(extr_w(0, 1, 2, 1), 0x1382_0420);
        assert_eq!(extr_x(0, 1, 2, 1), 0x93c2_0420);
        assert_eq!(ror_w_imm(16, 17, 5), 0x1391_1630);
        assert_eq!(ror_x_imm(16, 17, 5), 0x93d1_1630);
        assert_eq!(mul_w(16, 16, 17), 0x1b11_7e10);
        assert_eq!(mul_x(16, 16, 17), 0x9b11_7e10);
        assert_eq!(smulh_x(16, 16, 17), 0x9b51_7e10);
        assert_eq!(umulh_x(16, 16, 17), 0x9bd1_7e10);
        assert_eq!(udiv_w(16, 17, 18), 0x1ad2_0a30);
        assert_eq!(udiv_x(16, 17, 18), 0x9ad2_0a30);
        assert_eq!(sdiv_w(16, 17, 18), 0x1ad2_0e30);
        assert_eq!(sdiv_x(16, 17, 18), 0x9ad2_0e30);
        assert_eq!(clz_w(16, 17), 0x5ac0_1230);
        assert_eq!(clz_x(16, 17), 0xdac0_1230);
        assert_eq!(ubfx_w(16, 0, 16, 4), 0x5310_4c10);
        assert_eq!(ubfx_x(16, 0, 12, 28), 0xd34c_9c10);
        assert_eq!(sxtb_w(16, 17), 0x1300_1e30);
        assert_eq!(sxth_w(16, 17), 0x1300_3e30);
        assert_eq!(sxtb_x(16, 17), 0x9340_1e30);
        assert_eq!(sxth_x(16, 17), 0x9340_3e30);
        assert_eq!(sxtw_x(16, 17), 0x9340_7e30);
        assert_eq!(bfxil_w(16, 0, 5, 1), 0x3305_1410);
        assert_eq!(bfi_x(16, 17, 32, 32), 0xb360_7e30);
        assert_eq!(bic_w(16, 17, 18), 0x0a32_0230);
        assert_eq!(bic_x(16, 17, 18), 0x8a32_0230);
        assert_eq!(bics_w(16, 17, 18), 0x6a32_0230);
        assert_eq!(bics_x(16, 17, 18), 0xea32_0230);
        assert_eq!(sub_w_reg(16, 17, 16), 0x4b10_0230);
        assert_eq!(and_w_imm(16, 16, 0x2), 0x121f_0210);
        assert_eq!(and_w_imm(16, 16, 0x3), 0x1200_0610);
        assert_eq!(and_w_imm(16, 17, 0xff), 0x1200_1e30);
        assert_eq!(and_w_imm(16, 16, 0xfc00), 0x1216_1610);
        assert_eq!(and_w_imm(17, 17, 0x300), 0x1218_0631);
        assert_eq!(and_w_imm(17, 17, 0xffff_0000), 0x1210_3e31);
        assert_eq!(and_w_imm(16, 16, 0x8080_8080), 0x1201_c210);
        assert_eq!(and_w_imm(16, 16, 0x1111_1111), 0x1200_e210);
        assert_eq!(ands_w_imm(16, 0, 1), 0x7200_0010);
        assert_eq!(csel_w(17, 16, 17, Cond::NE), 0x1a91_1211);
        assert_eq!(csel_x(17, 16, 17, Cond::NE), 0x9a91_1211);
        assert_eq!(cinc_w(16, 16, Cond::NE), 0x1a90_0610);
        assert_eq!(cinc_w(16, 31, Cond::VS), 0x1a9f_77f0);
        assert_eq!(bic_w(17, 0, 17), 0x0a31_0011);
        assert_eq!(lsl_x_imm(0, 0, 37), 0xd35b_6800);
        assert_eq!(orr_x(0, 0, 1), 0xaa01_0000);
        assert_eq!(mrs_fpsr(17), 0xd53b_4431);
        assert_eq!(msr_fpsr(31), 0xd51b_443f);
        assert_eq!(mrs_nzcv(15), 0xd53b_420f);
        assert_eq!(msr_nzcv(16), 0xd51b_4210);
        assert_eq!(str_x_unsigned_sp(19, 80), 0xf900_2bf3);
        assert_eq!(ldr_x_unsigned_sp(19, 80), 0xf940_2bf3);
        assert_eq!(str_q_unsigned_sp(8, 96), 0x3d80_1be8);
        assert_eq!(ldr_q_unsigned_sp(8, 96), 0x3dc0_1be8);
        assert_eq!(str_x_unsigned(5, 31, 128), 0xf900_43e5);
        assert_eq!(ldr_x_unsigned(5, 31, 128), 0xf940_43e5);
        assert_eq!(str_q_unsigned(6, 31, 144), 0x3d80_27e6);
        assert_eq!(ldr_q_unsigned(6, 31, 144), 0x3dc0_27e6);
        assert_eq!(str_q_reg_lsl(8, 17, 18), 0x3cb2_6a28);
        assert_eq!(ldr_q_reg_lsl(8, 17, 18), 0x3cf2_6a28);
        assert_eq!(str_q_reg_uxtw(8, 17, 18), 0x3cb2_4a28);
        assert_eq!(ldr_q_reg_uxtw(8, 17, 18), 0x3cf2_4a28);
        assert_eq!(str_x_unsigned(7, 20, 16), 0xf900_0a87);
        assert_eq!(ldr_x_unsigned(7, 20, 16), 0xf940_0a87);
        assert_eq!(stur_x(16, 28, 84), 0xf805_4390);
        assert_eq!(str_s_unsigned(6, 21, 20), 0xbd00_16a6);
        assert_eq!(ldr_s_unsigned(6, 21, 20), 0xbd40_16a6);
        assert_eq!(str_d_unsigned(7, 20, 16), 0xfd00_0a87);
        assert_eq!(ldr_d_unsigned(7, 20, 16), 0xfd40_0a87);
        assert_eq!(ldur_x(0, 28, 84), 0xf845_4380);
        assert_eq!(str_q_unsigned(8, 21, 32), 0x3d80_0aa8);
        assert_eq!(ldr_q_unsigned(8, 21, 32), 0x3dc0_0aa8);
        assert_eq!(fmov_x_from_d(9, 10), 0x9e66_0149);
        assert_eq!(fmov_d_from_x(11, 12), 0x9e67_018b);
        assert_eq!(fmov_d(16, 17), 0x1e60_4230);
        assert_eq!(fmov_s(16, 17), 0x1e20_4230);
        assert_eq!(fmul_s(0, 1, 2), 0x1e22_0820);
        assert_eq!(fmul_d(3, 4, 5), 0x1e65_0883);
        assert_eq!(fdiv_s(0, 1, 2), 0x1e22_1820);
        assert_eq!(fdiv_d(3, 4, 5), 0x1e65_1883);
        assert_eq!(fmaxnm_s(0, 1, 2), 0x1e22_6820);
        assert_eq!(fmaxnm_s(3, 4, 5), 0x1e25_6883);
        assert_eq!(fmaxnm_d(6, 7, 8), 0x1e68_68e6);
        assert_eq!(fmaxnm_d(9, 10, 11), 0x1e6b_6949);
        assert_eq!(fminnm_s(12, 13, 14), 0x1e2e_79ac);
        assert_eq!(fminnm_s(15, 16, 17), 0x1e31_7a0f);
        assert_eq!(fminnm_d(18, 19, 20), 0x1e74_7a72);
        assert_eq!(fminnm_d(21, 22, 23), 0x1e77_7ad5);
        assert_eq!(fadd_s(0, 1, 2), 0x1e22_2820);
        assert_eq!(fadd_d(3, 4, 5), 0x1e65_2883);
        assert_eq!(fsub_s(0, 1, 2), 0x1e22_3820);
        assert_eq!(fsub_d(3, 4, 5), 0x1e65_3883);
        assert_eq!(add_w_reg_lsr(3, 5, 7, 16), 0x0b47_40a3);
        assert_eq!(fmulx_s(3, 5, 7), 0x5e27_dca3);
        assert_eq!(fmulx_d(3, 5, 7), 0x5e67_dca3);
        assert_eq!(frecpe_s(3, 5), 0x5ea1_d8a3);
        assert_eq!(frecpe_d(3, 5), 0x5ee1_d8a3);
        assert_eq!(frecpx_s(3, 5), 0x5ea1_f8a3);
        assert_eq!(frecpx_d(3, 5), 0x5ee1_f8a3);
        assert_eq!(frecps_s(3, 5, 7), 0x5e27_fca3);
        assert_eq!(frecps_d(3, 5, 7), 0x5e67_fca3);
        assert_eq!(frsqrte_s(3, 5), 0x7ea1_d8a3);
        assert_eq!(frsqrte_d(3, 5), 0x7ee1_d8a3);
        assert_eq!(frsqrts_s(3, 5, 7), 0x5ea7_fca3);
        assert_eq!(frsqrts_d(3, 5, 7), 0x5ee7_fca3);
        assert_eq!(fabs_s(0, 1), 0x1e20_c020);
        assert_eq!(fabs_d(2, 3), 0x1e60_c062);
        assert_eq!(fabs_v4s(0, 1), 0x4ea0_f820);
        assert_eq!(fabs_v2d(2, 3), 0x4ee0_f862);
        assert_eq!(bic_v8h_sign_bit(4), 0x6f04_b404);
        assert_eq!(fmax_s(0, 1, 2), 0x1e22_4820);
        assert_eq!(fmax_d(3, 4, 5), 0x1e65_4883);
        assert_eq!(fmin_s(6, 7, 8), 0x1e28_58e6);
        assert_eq!(fmin_d(9, 10, 11), 0x1e6b_5949);
        assert_eq!(frintn_s(0, 1), 0x1e24_4020);
        assert_eq!(frintn_d(2, 3), 0x1e64_4062);
        assert_eq!(frintp_s(4, 5), 0x1e24_c0a4);
        assert_eq!(frintp_d(6, 7), 0x1e64_c0e6);
        assert_eq!(frintm_s(8, 9), 0x1e25_4128);
        assert_eq!(frintm_d(10, 11), 0x1e65_416a);
        assert_eq!(frintz_s(12, 13), 0x1e25_c1ac);
        assert_eq!(frintz_d(14, 15), 0x1e65_c1ee);
        assert_eq!(frinta_s(16, 17), 0x1e26_4230);
        assert_eq!(frinta_d(18, 19), 0x1e66_4272);
        assert_eq!(frintx_s(20, 21), 0x1e27_42b4);
        assert_eq!(frintx_d(22, 23), 0x1e67_42f6);
        assert_eq!(frintn_v4s(0, 1), 0x4e21_8820);
        assert_eq!(frintn_v2d(2, 3), 0x4e61_8862);
        assert_eq!(frintp_v4s(4, 5), 0x4ea1_88a4);
        assert_eq!(frintp_v2d(6, 7), 0x4ee1_88e6);
        assert_eq!(frintm_v4s(8, 9), 0x4e21_9928);
        assert_eq!(frintm_v2d(10, 11), 0x4e61_996a);
        assert_eq!(frintz_v4s(12, 13), 0x4ea1_99ac);
        assert_eq!(frintz_v2d(14, 15), 0x4ee1_99ee);
        assert_eq!(frinta_v4s(16, 17), 0x6e21_8a30);
        assert_eq!(frinta_v2d(18, 19), 0x6e61_8a72);
        assert_eq!(frintx_v4s(20, 21), 0x6e21_9ab4);
        assert_eq!(frintx_v2d(22, 23), 0x6e61_9af6);
        assert_eq!(fneg_s(0, 1), 0x1e21_4020);
        assert_eq!(fneg_d(2, 3), 0x1e61_4062);
        assert_eq!(fsqrt_s(0, 1), 0x1e21_c020);
        assert_eq!(fsqrt_s(2, 3), 0x1e21_c062);
        assert_eq!(fsqrt_d(4, 5), 0x1e61_c0a4);
        assert_eq!(fsqrt_d(6, 7), 0x1e61_c0e6);
        assert_eq!(fmadd_s(0, 1, 2, 3), 0x1f02_0c20);
        assert_eq!(fmadd_d(4, 5, 6, 7), 0x1f46_1ca4);
        assert_eq!(fmsub_s(0, 1, 2, 3), 0x1f02_8c20);
        assert_eq!(fmsub_d(4, 5, 6, 7), 0x1f46_9ca4);
        assert_eq!(fcmp_s(6, 7), 0x1e27_20c0);
        assert_eq!(fcmp_d(8, 9), 0x1e69_2100);
        assert_eq!(fcmp_s_zero(10), 0x1e20_2148);
        assert_eq!(fcmp_d_zero(11), 0x1e60_2168);
        assert_eq!(fcmpe_s(12, 13), 0x1e2d_2190);
        assert_eq!(fcmpe_d(14, 15), 0x1e6f_21d0);
        assert_eq!(fcmpe_s_zero(16), 0x1e20_2218);
        assert_eq!(fcmpe_d_zero(17), 0x1e60_2238);
        assert_eq!(fcvtzu_w_from_s(0, 1), 0x1e39_0020);
        assert_eq!(fcvtzu_w_from_d(2, 3), 0x1e79_0062);
        assert_eq!(fcvtzu_x_from_s(0, 1), 0x9e39_0020);
        assert_eq!(fcvtzu_x_from_d(2, 3), 0x9e79_0062);
        assert_eq!(fcvtnu_w_from_s(12, 13), 0x1e21_01ac);
        assert_eq!(fcvtnu_x_from_s(0, 1), 0x9e21_0020);
        assert_eq!(fcvtnu_x_from_d(2, 3), 0x9e61_0062);
        assert_eq!(fcvtpu_w_from_d(14, 15), 0x1e69_01ee);
        assert_eq!(fcvtpu_x_from_s(4, 5), 0x9e29_00a4);
        assert_eq!(fcvtpu_x_from_d(6, 7), 0x9e69_00e6);
        assert_eq!(fcvtmu_w_from_s(16, 17), 0x1e31_0230);
        assert_eq!(fcvtmu_x_from_s(8, 9), 0x9e31_0128);
        assert_eq!(fcvtmu_x_from_d(10, 11), 0x9e71_016a);
        assert_eq!(fcvtau_w_from_d(18, 19), 0x1e65_0272);
        assert_eq!(fcvtau_x_from_s(16, 17), 0x9e25_0230);
        assert_eq!(fcvtau_x_from_d(18, 19), 0x9e65_0272);
        assert_eq!(fcvtns_w_from_s(0, 1), 0x1e20_0020);
        assert_eq!(fcvtns_w_from_d(2, 3), 0x1e60_0062);
        assert_eq!(fcvtns_x_from_s(20, 21), 0x9e20_02b4);
        assert_eq!(fcvtns_x_from_d(22, 23), 0x9e60_02f6);
        assert_eq!(fcvtps_w_from_s(4, 5), 0x1e28_00a4);
        assert_eq!(fcvtps_w_from_d(6, 7), 0x1e68_00e6);
        assert_eq!(fcvtps_x_from_s(24, 25), 0x9e28_0338);
        assert_eq!(fcvtps_x_from_d(26, 27), 0x9e68_037a);
        assert_eq!(fcvtms_w_from_s(8, 9), 0x1e30_0128);
        assert_eq!(fcvtms_w_from_d(10, 11), 0x1e70_016a);
        assert_eq!(fcvtms_x_from_s(28, 29), 0x9e30_03bc);
        assert_eq!(fcvtms_x_from_d(30, 31), 0x9e70_03fe);
        assert_eq!(fcvtzs_w_from_s(12, 13), 0x1e38_01ac);
        assert_eq!(fcvtzs_w_from_d(14, 15), 0x1e78_01ee);
        assert_eq!(fcvtzs_x_from_s(0, 1), 0x9e38_0020);
        assert_eq!(fcvtzs_x_from_d(2, 3), 0x9e78_0062);
        assert_eq!(fcvtas_w_from_s(16, 17), 0x1e24_0230);
        assert_eq!(fcvtas_w_from_d(18, 19), 0x1e64_0272);
        assert_eq!(fcvtas_x_from_s(4, 5), 0x9e24_00a4);
        assert_eq!(fcvtas_x_from_d(6, 7), 0x9e64_00e6);
        assert_eq!(fcvtzu_x_from_d_fixed(8, 9, 1), 0x9e59_fd28);
        assert_eq!(fcvtzs_x_from_s_fixed(10, 11, 7), 0x9e18_e56a);
        assert_eq!(fcvt_d_from_s(0, 1), 0x1e22_c020);
        assert_eq!(fcvt_d_from_s(2, 3), 0x1e22_c062);
        assert_eq!(fcvt_s_from_d(4, 5), 0x1e62_40a4);
        assert_eq!(fcvt_s_from_d(6, 7), 0x1e62_40e6);
        assert_eq!(fcvt_d_from_h(3, 5), 0x1ee2_c0a3);
        assert_eq!(fcvt_s_from_h(3, 5), 0x1ee2_40a3);
        assert_eq!(fcvt_h_from_s(3, 5), 0x1e23_c0a3);
        assert_eq!(fcvt_h_from_d(3, 5), 0x1e63_c0a3);
        assert_eq!(fcvtxn_s_from_d(8, 9), 0x7e61_6928);
        assert_eq!(ucvtf_s_from_w(0, 1), 0x1e23_0020);
        assert_eq!(ucvtf_s_from_x(0, 1), 0x9e23_0020);
        assert_eq!(ucvtf_d_from_w(2, 3), 0x1e63_0062);
        assert_eq!(ucvtf_d_from_x(2, 3), 0x9e63_0062);
        assert_eq!(scvtf_s_from_w(0, 1), 0x1e22_0020);
        assert_eq!(scvtf_s_from_x(0, 1), 0x9e22_0020);
        assert_eq!(scvtf_s_from_w(2, 3), 0x1e22_0062);
        assert_eq!(scvtf_d_from_w(4, 5), 0x1e62_00a4);
        assert_eq!(scvtf_d_from_x(4, 5), 0x9e62_00a4);
        assert_eq!(scvtf_d_from_w(6, 7), 0x1e62_00e6);
        assert_eq!(ucvtf_d_from_x_fixed(0, 1, 1), 0x9e43_fc20);
        assert_eq!(ucvtf_d_from_x_fixed(2, 3, 7), 0x9e43_e462);
        assert_eq!(scvtf_s_from_x_fixed(4, 5, 1), 0x9e02_fca4);
        assert_eq!(scvtf_s_from_x_fixed(6, 7, 7), 0x9e02_e4e6);
        assert_eq!(scvtf_s_from_w_fixed(4, 5, 3), 0x1e02_f4a4);
        assert_eq!(scvtf_d_from_w_fixed(6, 7, 3), 0x1e42_f4e6);
        assert_eq!(ucvtf_s_from_w_fixed(8, 9, 3), 0x1e03_f528);
        assert_eq!(ucvtf_d_from_w_fixed(10, 11, 3), 0x1e43_f56a);
        assert_eq!(fcvtl_v4s_from_v4h(3, 5), 0x0e21_78a3);
        assert_eq!(fcvtn_v4h_from_v4s(3, 5), 0x0e21_68a3);
        assert_eq!(fmulx_v4s(3, 5, 7), 0x4e27_dca3);
        assert_eq!(fmulx_v2d(3, 5, 7), 0x4e67_dca3);
        assert_eq!(scvtf_v4s(0, 1), 0x4e21_d820);
        assert_eq!(scvtf_v2d(2, 3), 0x4e61_d862);
        assert_eq!(ucvtf_v4s(4, 5), 0x6e21_d8a4);
        assert_eq!(ucvtf_v2d(6, 7), 0x6e61_d8e6);
        assert_eq!(scvtf_v4s_fixed(8, 9, 1), 0x4f3f_e528);
        assert_eq!(scvtf_v2d_fixed(10, 11, 1), 0x4f7f_e56a);
        assert_eq!(ucvtf_v4s_fixed(12, 13, 7), 0x6f39_e5ac);
        assert_eq!(ucvtf_v2d_fixed(14, 15, 7), 0x6f79_e5ee);
        assert_eq!(fcvtzs_v4s(0, 1), 0x4ea1_b820);
        assert_eq!(fcvtzs_v2d(2, 3), 0x4ee1_b862);
        assert_eq!(fcvtzu_v4s(4, 5), 0x6ea1_b8a4);
        assert_eq!(fcvtzu_v2d(6, 7), 0x6ee1_b8e6);
        assert_eq!(fcvtns_v4s(8, 9), 0x4e21_a928);
        assert_eq!(fcvtns_v2d(10, 11), 0x4e61_a96a);
        assert_eq!(fcvtps_v4s(12, 13), 0x4ea1_a9ac);
        assert_eq!(fcvtps_v2d(14, 15), 0x4ee1_a9ee);
        assert_eq!(fcvtms_v4s(16, 17), 0x4e21_ba30);
        assert_eq!(fcvtms_v2d(18, 19), 0x4e61_ba72);
        assert_eq!(fcvtas_v4s(20, 21), 0x4e21_cab4);
        assert_eq!(fcvtas_v2d(22, 23), 0x4e61_caf6);
        assert_eq!(fcvtnu_v4s(24, 25), 0x6e21_ab38);
        assert_eq!(fcvtnu_v2d(26, 27), 0x6e61_ab7a);
        assert_eq!(fcvtpu_v4s(28, 29), 0x6ea1_abbc);
        assert_eq!(fcvtpu_v2d(30, 31), 0x6ee1_abfe);
        assert_eq!(fcvtmu_v4s(0, 1), 0x6e21_b820);
        assert_eq!(fcvtmu_v2d(2, 3), 0x6e61_b862);
        assert_eq!(fcvtau_v4s(4, 5), 0x6e21_c8a4);
        assert_eq!(fcvtau_v2d(6, 7), 0x6e61_c8e6);
        assert_eq!(fcvtzs_v4s_fixed(8, 9, 3), 0x4f3d_fd28);
        assert_eq!(fcvtzs_v2d_fixed(10, 11, 3), 0x4f7d_fd6a);
        assert_eq!(fcvtzu_v4s_fixed(12, 13, 3), 0x6f3d_fdac);
        assert_eq!(fcvtzu_v2d_fixed(14, 15, 3), 0x6f7d_fdee);
        assert_eq!(fmov_v_d1_from_x(0, 1), 0x9eaf_0020);
        assert_eq!(fmov_x_from_v_d1(2, 0), 0x9eae_0002);
        assert_eq!(fmov_v_d1_from_x(31, 30), 0x9eaf_03df);
        assert_eq!(fmov_x_from_v_d1(30, 31), 0x9eae_03fe);
        assert_eq!(mov_v_d1_from_v_d0(0, 1), 0x6e18_0420);
        assert_eq!(mov_v_d1_from_v_d0(2, 3), 0x6e18_0462);
        assert_eq!(mov_v16b(13, 14), 0x4eae_1dcd);
        assert_eq!(movi_d_imm0(16), 0x2f00_e410);
        assert_eq!(movi_v8b_imm(2, 0x0f), 0x0f00_e5e2);
        assert_eq!(movi_v8b_imm(2, 0xf0), 0x0f07_e602);
        assert_eq!(movi_v16b_imm(2, 0x08), 0x4f00_e502);
        assert_eq!(movi_v16b_imm(2, 0x18), 0x4f00_e702);
        assert_eq!(umov_from_v(16, 17, 8, 7), 0x0e0f_3e30);
        assert_eq!(umov_from_v(16, 17, 16, 7), 0x0e1e_3e30);
        assert_eq!(umov_from_v(16, 17, 32, 3), 0x0e1c_3e30);
        assert_eq!(umov_from_v(16, 17, 64, 1), 0x4e18_3e30);
        assert_eq!(mov_to_v_element(16, 17, 8, 7), 0x4e0f_1e30);
        assert_eq!(mov_to_v_element(16, 17, 16, 7), 0x4e1e_1e30);
        assert_eq!(mov_to_v_element(16, 17, 32, 3), 0x4e1c_1e30);
        assert_eq!(mov_to_v_element(16, 17, 64, 1), 0x4e18_1e30);
        assert_eq!(dup_v_from_reg(16, 17, 8, true), 0x4e01_0e30);
        assert_eq!(dup_v_from_reg(16, 17, 16, true), 0x4e02_0e30);
        assert_eq!(dup_v_from_reg(16, 17, 32, true), 0x4e04_0e30);
        assert_eq!(dup_v_from_reg(16, 17, 64, true), 0x4e08_0e30);
        assert_eq!(dup_v_from_reg(16, 17, 8, false), 0x0e01_0e30);
        assert_eq!(dup_v_from_reg(16, 17, 16, false), 0x0e02_0e30);
        assert_eq!(dup_v_from_reg(16, 17, 32, false), 0x0e04_0e30);
        assert_eq!(dup_v_from_element(16, 17, 8, 7, true), 0x4e0f_0630);
        assert_eq!(dup_v_from_element(16, 17, 16, 7, true), 0x4e1e_0630);
        assert_eq!(dup_v_from_element(16, 17, 32, 3, true), 0x4e1c_0630);
        assert_eq!(dup_v_from_element(16, 17, 64, 1, true), 0x4e18_0630);
        assert_eq!(dup_v_from_element(16, 17, 8, 7, false), 0x0e0f_0630);
        assert_eq!(dup_v_from_element(16, 17, 16, 7, false), 0x0e1e_0630);
        assert_eq!(dup_v_from_element(16, 17, 32, 3, false), 0x0e1c_0630);
        assert_eq!(add_v(16, 17, 18, 8, true), 0x4e32_8630);
        assert_eq!(add_v(16, 17, 18, 16, true), 0x4e72_8630);
        assert_eq!(add_v(16, 17, 18, 32, true), 0x4eb2_8630);
        assert_eq!(add_v(16, 17, 18, 64, true), 0x4ef2_8630);
        assert_eq!(sub_v(16, 17, 18, 8, true), 0x6e32_8630);
        assert_eq!(mul_v(16, 17, 18, 8, true), 0x4e32_9e30);
        assert_eq!(fadd_v4s(0, 1, 2), 0x4e22_d420);
        assert_eq!(fadd_v2d(3, 4, 5), 0x4e65_d483);
        assert_eq!(fsub_v4s(0, 1, 2), 0x4ea2_d420);
        assert_eq!(fsub_v2d(3, 4, 5), 0x4ee5_d483);
        assert_eq!(fmul_v4s(0, 1, 2), 0x6e22_dc20);
        assert_eq!(fmul_v2d(3, 4, 5), 0x6e65_dc83);
        assert_eq!(fmax_v4s(0, 1, 2), 0x4e22_f420);
        assert_eq!(fmax_v2d(3, 4, 5), 0x4e65_f483);
        assert_eq!(fmaxnm_v4s(6, 7, 8), 0x4e28_c4e6);
        assert_eq!(fmaxnm_v2d(9, 10, 11), 0x4e6b_c549);
        assert_eq!(fmin_v4s(12, 13, 14), 0x4eae_f5ac);
        assert_eq!(fmin_v2d(15, 16, 17), 0x4ef1_f60f);
        assert_eq!(fminnm_v4s(18, 19, 20), 0x4eb4_c672);
        assert_eq!(fminnm_v2d(21, 22, 23), 0x4ef7_c6d5);
        assert_eq!(fcmeq_v4s(0, 1, 2), 0x4e22_e420);
        assert_eq!(fcmeq_v2d(3, 4, 5), 0x4e65_e483);
        assert_eq!(fcmgt_v4s(6, 7, 8), 0x6ea8_e4e6);
        assert_eq!(fcmgt_v2d(9, 10, 11), 0x6eeb_e549);
        assert_eq!(fcmge_v4s(12, 13, 14), 0x6e2e_e5ac);
        assert_eq!(fcmge_v2d(15, 16, 17), 0x6e71_e60f);
        assert_eq!(fmla_v4s(0, 1, 2), 0x4e22_cc20);
        assert_eq!(fmla_v2d(3, 4, 5), 0x4e65_cc83);
        assert_eq!(fdiv_v4s(0, 1, 2), 0x6e22_fc20);
        assert_eq!(fdiv_v2d(3, 4, 5), 0x6e65_fc83);
        assert_eq!(fneg_v4s(0, 1), 0x6ea0_f820);
        assert_eq!(fneg_v2d(2, 3), 0x6ee0_f862);
        assert_eq!(fsqrt_v4s(0, 1), 0x6ea1_f820);
        assert_eq!(fsqrt_v2d(2, 3), 0x6ee1_f862);
        assert_eq!(frecpe_v4s(31, 6), 0x4ea1_d8df);
        assert_eq!(frecpe_v2d(31, 6), 0x4ee1_d8df);
        assert_eq!(frsqrte_v4s(31, 6), 0x6ea1_d8df);
        assert_eq!(frsqrte_v2d(31, 6), 0x6ee1_d8df);
        assert_eq!(faddp_v4s(0, 1, 2), 0x6e22_d420);
        assert_eq!(faddp_v2d(3, 4, 5), 0x6e65_d483);
        assert_eq!(faddp_v4s(6, 7, 8), 0x6e28_d4e6);
        assert_eq!(faddp_d_from_v2d(9, 10), 0x7e70_d949);
        assert_eq!(frecps_v4s(0, 1, 2), 0x4e22_fc20);
        assert_eq!(frecps_v2d(3, 4, 5), 0x4e65_fc83);
        assert_eq!(frsqrts_v4s(6, 7, 8), 0x4ea8_fce6);
        assert_eq!(frsqrts_v2d(9, 10, 11), 0x4eeb_fd49);
        assert_eq!(smull_v(16, 17, 18, 8), 0x0e32_c230);
        assert_eq!(smull_v(16, 17, 18, 16), 0x0e72_c230);
        assert_eq!(smull_v(16, 17, 18, 32), 0x0eb2_c230);
        assert_eq!(umull_v(16, 17, 18, 8), 0x2e32_c230);
        assert_eq!(pmull_v(16, 17, 18, 8), 0x0e32_e230);
        assert_eq!(pmull_v(16, 17, 18, 64), 0x0ef2_e230);
        assert_eq!(sqdmull_v(16, 17, 18, 16), 0x0e72_d230);
        assert_eq!(and_v16b(16, 17, 18), 0x4e32_1e30);
        assert_eq!(and_v8b(9, 10, 11), 0x0e2b_1d49);
        assert_eq!(bic_v16b(16, 17, 18), 0x4e72_1e30);
        assert_eq!(eor_v16b(16, 17, 18), 0x6e32_1e30);
        assert_eq!(eor_v8b(9, 10, 11), 0x2e2b_1d49);
        assert_eq!(bsl_v8b(9, 10, 11), 0x2e6b_1d49);
        assert_eq!(orr_v16b(16, 17, 18), 0x4eb2_1e30);
        assert_eq!(orr_v8b(2, 3, 2), 0x0ea2_1c62);
        assert_eq!(cmeq_v(16, 17, 18, 8, true), 0x6e32_8e30);
        assert_eq!(cmgt_v(16, 17, 18, 8, true), 0x4e32_3630);
        assert_eq!(cmhi_v(16, 17, 18, 8, true), 0x6e32_3630);
        assert_eq!(cmge_v(16, 17, 18, 8, true), 0x4e32_3e30);
        assert_eq!(cmge_v_zero(3, 4, 8, false), 0x2e20_8883);
        assert_eq!(cmge_v_zero(3, 4, 16, false), 0x2e60_8883);
        assert_eq!(cmeq_v_zero(3, 4, 8, false), 0x0e20_9883);
        assert_eq!(cmeq_v_zero(3, 4, 16, false), 0x0e60_9883);
        assert_eq!(cmhs_v(16, 17, 18, 8, true), 0x6e32_3e30);
        assert_eq!(smax_v(16, 17, 18, 8, true), 0x4e32_6630);
        assert_eq!(umax_v(16, 17, 18, 8, true), 0x6e32_6630);
        assert_eq!(smin_v(16, 17, 18, 8, true), 0x4e32_6e30);
        assert_eq!(umin_v(16, 17, 18, 8, true), 0x6e32_6e30);
        assert_eq!(addp_v(16, 17, 18, 8, true), 0x4e32_be30);
        assert_eq!(addp_v(16, 17, 18, 8, false), 0x0e32_be30);
        assert_eq!(addv_from_v(0, 1, 8), 0x4e31_b820);
        assert_eq!(addv_from_v(2, 3, 16), 0x4e71_b862);
        assert_eq!(addv_from_v(4, 5, 32), 0x4eb1_b8a4);
        assert_eq!(uaddlv_from_v(7, 8, 8, false), 0x2e30_3907);
        assert_eq!(uaddlv_from_v(7, 8, 16, false), 0x2e70_3907);
        assert_eq!(uaddlv_from_v(7, 8, 32, true), 0x6eb0_3907);
        assert_eq!(addp_d_from_v2d(6, 7), 0x5ef1_b8e6);
        assert_eq!(smaxp_v(16, 17, 18, 8, true), 0x4e32_a630);
        assert_eq!(umaxp_v(16, 17, 18, 8, true), 0x6e32_a630);
        assert_eq!(sminp_v(16, 17, 18, 8, true), 0x4e32_ae30);
        assert_eq!(uminp_v(16, 17, 18, 8, true), 0x6e32_ae30);
        assert_eq!(sqadd_v(16, 17, 18, 8, true), 0x4e32_0e30);
        assert_eq!(sqsub_v(16, 17, 18, 8, true), 0x4e32_2e30);
        assert_eq!(uqadd_v(16, 17, 18, 8, true), 0x6e32_0e30);
        assert_eq!(uqsub_v(16, 17, 18, 8, true), 0x6e32_2e30);
        assert_eq!(shadd_v(16, 17, 18, 8, true), 0x4e32_0630);
        assert_eq!(uhadd_v(16, 17, 18, 8, true), 0x6e32_0630);
        assert_eq!(shsub_v(16, 17, 18, 8, true), 0x4e32_2630);
        assert_eq!(uhsub_v(16, 17, 18, 8, true), 0x6e32_2630);
        assert_eq!(srhadd_v(16, 17, 18, 8, true), 0x4e32_1630);
        assert_eq!(urhadd_v(16, 17, 18, 8, true), 0x6e32_1630);
        assert_eq!(sshl_v(16, 17, 18, 8, true), 0x4e32_4630);
        assert_eq!(ushl_v(16, 17, 18, 8, true), 0x6e32_4630);
        assert_eq!(srshl_v(16, 17, 18, 8, true), 0x4e32_5630);
        assert_eq!(urshl_v(16, 17, 18, 8, true), 0x6e32_5630);
        assert_eq!(sabd_v(16, 17, 18, 8, true), 0x4e32_7630);
        assert_eq!(uabd_v(16, 17, 18, 8, true), 0x6e32_7630);
        assert_eq!(pmul_v(16, 17, 18, 8, true), 0x6e32_9e30);
        assert_eq!(sqdmulh_v(16, 17, 18, 16, true), 0x4e72_b630);
        assert_eq!(sqrdmulh_v(16, 17, 18, 16, true), 0x6e72_b630);
        assert_eq!(shrn_v(5, 6, 16, 8), 0x0f08_84c5);
        assert_eq!(shrn_v(5, 6, 32, 16), 0x0f10_84c5);
        assert_eq!(shrn_v(5, 6, 64, 32), 0x0f20_84c5);
        assert_eq!(sqshl_v(16, 17, 18, 8, true), 0x4e32_4e30);
        assert_eq!(uqshl_v(16, 17, 18, 8, true), 0x6e32_4e30);
        assert_eq!(zip1_v(16, 17, 18, 8, true), 0x4e12_3a30);
        assert_eq!(zip2_v(16, 17, 18, 8, true), 0x4e12_7a30);
        assert_eq!(uzp1_v(16, 17, 18, 8, true), 0x4e12_1a30);
        assert_eq!(uzp2_v(16, 17, 18, 8, true), 0x4e12_5a30);
        assert_eq!(trn1_v(16, 17, 18, 8, true), 0x4e12_2a30);
        assert_eq!(trn2_v(16, 17, 18, 8, true), 0x4e12_6a30);
        assert_eq!(abs_v(16, 17, 8, true), 0x4e20_ba30);
        assert_eq!(neg_v(16, 17, 8, true), 0x6e20_ba30);
        assert_eq!(sqabs_v(16, 17, 8, true), 0x4e20_7a30);
        assert_eq!(sqneg_v(16, 17, 8, true), 0x6e20_7a30);
        assert_eq!(suqadd_v(16, 17, 8, true), 0x4e20_3a30);
        assert_eq!(usqadd_v(16, 17, 8, true), 0x6e20_3a30);
        assert_eq!(saddlp_v(16, 17, 8), 0x4e20_2a30);
        assert_eq!(uaddlp_v(16, 17, 8), 0x6e20_2a30);
        assert_eq!(not_v16b(16, 17), 0x6e20_5a30);
        assert_eq!(mrs_fpcr(9), 0xd53b_4409);
        assert_eq!(msr_fpcr(10), 0xd51b_440a);
        assert_eq!(mrs_fpsr(11), 0xd53b_442b);
        assert_eq!(msr_fpsr(12), 0xd51b_442c);
        assert_eq!(clz_v(16, 17, 8, true), 0x6e20_4a30);
        assert_eq!(cnt_v16b(16, 17), 0x4e20_5a30);
        assert_eq!(rbit_v16b(16, 17), 0x6e60_5a30);
        assert_eq!(urecpe_v4s(16, 17), 0x4ea1_ca30);
        assert_eq!(ursqrte_v4s(16, 17), 0x6ea1_ca30);
        assert_eq!(rev16_v16b(16, 17), 0x4e20_1a30);
        assert_eq!(rev32_v(16, 17, 8, false), 0x2e20_0a30);
        assert_eq!(rev32_v(16, 17, 8, true), 0x6e20_0a30);
        assert_eq!(rev32_v(16, 17, 16, false), 0x2e60_0a30);
        assert_eq!(rev32_v(16, 17, 16, true), 0x6e60_0a30);
        assert_eq!(rev64_v(16, 17, 8, false), 0x0e20_0a30);
        assert_eq!(rev64_v(16, 17, 8, true), 0x4e20_0a30);
        assert_eq!(rev64_v(16, 17, 16, false), 0x0e60_0a30);
        assert_eq!(rev64_v(16, 17, 16, true), 0x4e60_0a30);
        assert_eq!(rev64_v(16, 17, 32, false), 0x0ea0_0a30);
        assert_eq!(rev64_v(16, 17, 32, true), 0x4ea0_0a30);
        assert_eq!(sshr_v(16, 17, 8, 3, true), 0x4f0d_0630);
        assert_eq!(ushr_v(16, 17, 8, 3, true), 0x6f0d_0630);
        assert_eq!(shl_v(16, 17, 8, 3, true), 0x4f0b_5630);
        assert_eq!(sqshlu_v(16, 17, 8, 3, true), 0x6f0b_6630);
        assert_eq!(sqshlu_v(16, 17, 16, 3, true), 0x6f13_6630);
        assert_eq!(ext_v16b(16, 17, 18, 5, true), 0x6e12_2a30);
        assert_eq!(tbl_v(16, 17, 18, 1, true), 0x4e12_0230);
        assert_eq!(tbx_v(16, 17, 18, 1, true), 0x4e12_1230);
        assert_eq!(tbl_v(16, 17, 19, 2, true), 0x4e13_2230);
        assert_eq!(tbx_v(16, 17, 19, 2, true), 0x4e13_3230);
        assert_eq!(tbl_v(16, 17, 20, 3, true), 0x4e14_4230);
        assert_eq!(tbx_v(16, 17, 20, 3, true), 0x4e14_5230);
        assert_eq!(tbl_v(16, 17, 21, 4, true), 0x4e15_6230);
        assert_eq!(tbx_v(16, 17, 21, 4, true), 0x4e15_7230);
        assert_eq!(tbl_v(16, 17, 18, 1, false), 0x0e12_0230);
        assert_eq!(tbx_v(16, 17, 18, 1, false), 0x0e12_1230);
        assert_eq!(uxtl_v(16, 17, 8), 0x2f08_a630);
        assert_eq!(uxtl_v(16, 17, 16), 0x2f10_a630);
        assert_eq!(uxtl_v(16, 17, 32), 0x2f20_a630);
        assert_eq!(sxtl_v(16, 17, 8), 0x0f08_a630);
        assert_eq!(sxtl_v(16, 17, 16), 0x0f10_a630);
        assert_eq!(sxtl_v(16, 17, 32), 0x0f20_a630);
        assert_eq!(xtn_v(16, 17, 16), 0x0e21_2a30);
        assert_eq!(xtn_v(16, 17, 32), 0x0e61_2a30);
        assert_eq!(xtn_v(16, 17, 64), 0x0ea1_2a30);
        assert_eq!(sqxtn_v(16, 17, 16), 0x0e21_4a30);
        assert_eq!(sqxtun_v(16, 17, 16), 0x2e21_2a30);
        assert_eq!(uqxtn_v(16, 17, 16), 0x2e21_4a30);
        assert_eq!(cbnz_w(16, 8), 0x3500_0050);
        assert_eq!(cbnz_x(16, 8), 0xb500_0050);
        assert_eq!(dmb_ish(), 0xd503_3bbf);
        assert_eq!(dsb_sy(), 0xd503_3f9f);
        assert_eq!(dmb_sy(), 0xd503_3fbf);
        assert_eq!(stp_x_pre_sp(27, 28, -16), 0xa9bf_73fb);
        assert_eq!(ldp_x_post_sp(27, 28, 16), 0xa8c1_73fb);
        assert_eq!(stp_x_offset_sp(19, 20, 0), 0xa900_53f3);
        assert_eq!(stp_x_offset_sp(29, 30, 80), 0xa905_7bfd);
        assert_eq!(ldp_x_offset_sp(19, 20, 0), 0xa940_53f3);
        assert_eq!(ldp_x_offset_sp(29, 30, 80), 0xa945_7bfd);
        assert_eq!(stp_x_offset(16, 17, 30, 0), 0xa900_47d0);
        assert_eq!(ldp_x_offset(16, 17, 2, 0), 0xa940_4450);
        assert_eq!(stp_q_offset_sp(8, 9, 96), 0xad03_27e8);
        assert_eq!(stp_q_offset_sp(14, 15, 192), 0xad06_3fee);
        assert_eq!(ldp_q_offset_sp(8, 9, 96), 0xad43_27e8);
        assert_eq!(ldp_q_offset_sp(14, 15, 192), 0xad46_3fee);
        assert_eq!(stp_fp_lr_pre_16(), 0xa9bf_7bfd);
        assert_eq!(ldp_fp_lr_post_16(), 0xa8c1_7bfd);
    }

    #[test]
    #[should_panic(expected = "AArch64 register out of range")]
    fn rejects_invalid_register() {
        let _ = ret(32);
    }

    #[test]
    #[should_panic(expected = "MOV wide shift")]
    fn rejects_invalid_mov_shift() {
        let _ = movz_x(0, 1, 8);
    }
}

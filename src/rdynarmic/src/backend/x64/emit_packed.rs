use rxbyak::Reg;

use crate::backend::x64::emit_context::EmitContext;
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::ir::inst::Inst;
use crate::ir::value::InstRef;

// ---------------------------------------------------------------------------
// Helper: emit packed SSE operation via GPR→XMM→GPR round-trip
// Packed ops work on 32-bit GPR values treated as packed 4×u8 or 2×u16.
// We move to XMM, do the SSE op, then move back.
// ---------------------------------------------------------------------------

fn emit_packed_sse_binary(
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    op: fn(&mut rxbyak::CodeAssembler, Reg, Reg) -> rxbyak::Result<()>,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a_gpr = ra.use_gpr(&mut args[0]);
    let b_gpr = ra.use_gpr(&mut args[1]);

    let a_xmm = ra.scratch_xmm();
    let b_xmm = ra.scratch_xmm();

    // Move 32-bit GPR values into XMM low 32 bits
    ra.asm.movd(a_xmm, a_gpr.cvt32().unwrap()).unwrap();
    ra.asm.movd(b_xmm, b_gpr.cvt32().unwrap()).unwrap();

    // Perform the packed SSE operation
    op(&mut *ra.asm, a_xmm, b_xmm).unwrap();

    // Move result back to GPR
    let result = ra.scratch_gpr();
    ra.asm.movd(result.cvt32().unwrap(), a_xmm).unwrap();

    ra.release(a_xmm);
    ra.release(b_xmm);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// Helper: emit host_call fallback for packed ops without native SSE equivalent
// ---------------------------------------------------------------------------

fn emit_packed_host_call(ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst, func: usize) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let (first, rest) = args.split_at_mut(1);
    ra.host_call(
        Some(inst_ref),
        &mut [Some(&mut first[0]), Some(&mut rest[0]), None, None],
    );
    ra.asm.mov(rxbyak::RAX, func as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();
}

// ---------------------------------------------------------------------------
// Packed add (native SSE)
// ---------------------------------------------------------------------------

pub fn emit_packed_add_u8(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_packed_sse_binary(ra, inst_ref, inst, rxbyak::CodeAssembler::paddb);
}
pub fn emit_packed_add_s8(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_packed_sse_binary(ra, inst_ref, inst, rxbyak::CodeAssembler::paddb);
}
pub fn emit_packed_add_u16(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_packed_sse_binary(ra, inst_ref, inst, rxbyak::CodeAssembler::paddw);
}
pub fn emit_packed_add_s16(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_packed_sse_binary(ra, inst_ref, inst, rxbyak::CodeAssembler::paddw);
}

// ---------------------------------------------------------------------------
// Packed sub (native SSE)
// ---------------------------------------------------------------------------

pub fn emit_packed_sub_u8(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_packed_sse_binary(ra, inst_ref, inst, rxbyak::CodeAssembler::psubb);
}
pub fn emit_packed_sub_s8(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_packed_sse_binary(ra, inst_ref, inst, rxbyak::CodeAssembler::psubb);
}
pub fn emit_packed_sub_u16(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_packed_sse_binary(ra, inst_ref, inst, rxbyak::CodeAssembler::psubw);
}
pub fn emit_packed_sub_s16(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_packed_sse_binary(ra, inst_ref, inst, rxbyak::CodeAssembler::psubw);
}

// ---------------------------------------------------------------------------
// Packed saturated add (native SSE)
// ---------------------------------------------------------------------------

pub fn emit_packed_saturated_add_u8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_sse_binary(ra, inst_ref, inst, rxbyak::CodeAssembler::paddusb);
}
pub fn emit_packed_saturated_add_s8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_sse_binary(ra, inst_ref, inst, rxbyak::CodeAssembler::paddsb);
}
pub fn emit_packed_saturated_add_u16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_sse_binary(ra, inst_ref, inst, rxbyak::CodeAssembler::paddusw);
}
pub fn emit_packed_saturated_add_s16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_sse_binary(ra, inst_ref, inst, rxbyak::CodeAssembler::paddsw);
}

// ---------------------------------------------------------------------------
// Packed saturated sub (native SSE)
// ---------------------------------------------------------------------------

pub fn emit_packed_saturated_sub_u8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_sse_binary(ra, inst_ref, inst, rxbyak::CodeAssembler::psubusb);
}
pub fn emit_packed_saturated_sub_s8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_sse_binary(ra, inst_ref, inst, rxbyak::CodeAssembler::psubsb);
}
pub fn emit_packed_saturated_sub_u16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_sse_binary(ra, inst_ref, inst, rxbyak::CodeAssembler::psubusw);
}
pub fn emit_packed_saturated_sub_s16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_sse_binary(ra, inst_ref, inst, rxbyak::CodeAssembler::psubsw);
}

// ---------------------------------------------------------------------------
// PackedAbsDiffSumU8 (native SSE: psadbw)
// ---------------------------------------------------------------------------

pub fn emit_packed_abs_diff_sum_s8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_sse_binary(ra, inst_ref, inst, rxbyak::CodeAssembler::psadbw);
}

// ---------------------------------------------------------------------------
// PackedSelect: blend two values using a mask
// Args: (mask: U32, a: U32, b: U32) → (mask & a) | (~mask & b)
// ---------------------------------------------------------------------------

pub fn emit_packed_select(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let xmm_count = args.iter().take(3).filter(|arg| arg.is_in_xmm(ra)).count();

    if xmm_count >= 2 {
        let ge = ra.use_scratch_xmm(&mut args[0]);
        let to = ra.use_xmm(&mut args[1]);
        let from = ra.use_scratch_xmm(&mut args[2]);
        ra.asm.pand(from, ge).unwrap();
        ra.asm.pandn(ge, to).unwrap();
        ra.asm.por(from, ge).unwrap();
        ra.define_value(inst_ref, from);
    } else if ctx.has_host_feature(HostFeature::BMI1) {
        let ge = ra.use_gpr(&mut args[0]).cvt32().unwrap();
        let to = ra.use_scratch_gpr(&mut args[1]).cvt32().unwrap();
        let from = ra.use_scratch_gpr(&mut args[2]).cvt32().unwrap();
        ra.asm.and_(from, ge).unwrap();
        ra.asm.andn(to, ge, to).unwrap();
        ra.asm.or_(from, to).unwrap();
        ra.define_value(inst_ref, Reg::gpr64(from.get_idx()));
    } else {
        let ge = ra.use_scratch_gpr(&mut args[0]).cvt32().unwrap();
        let to = ra.use_gpr(&mut args[1]).cvt32().unwrap();
        let from = ra.use_scratch_gpr(&mut args[2]).cvt32().unwrap();
        ra.asm.and_(from, ge).unwrap();
        ra.asm.not_(ge).unwrap();
        ra.asm.and_(ge, to).unwrap();
        ra.asm.or_(from, ge).unwrap();
        ra.define_value(inst_ref, Reg::gpr64(from.get_idx()));
    }
}

// ---------------------------------------------------------------------------
// Halving add/sub and AddSub/SubAdd — host_call fallback (no direct SSE)
// ---------------------------------------------------------------------------

// Rust implementations for packed halving/addsub ops
extern "C" fn packed_halving_add_u8(a: u32, b: u32) -> u32 {
    let mut result = 0u32;
    for i in 0..4 {
        let va = (a >> (i * 8)) & 0xFF;
        let vb = (b >> (i * 8)) & 0xFF;
        result |= ((va + vb) >> 1) << (i * 8);
    }
    result
}

extern "C" fn packed_halving_add_s8(a: u32, b: u32) -> u32 {
    let mut result = 0u32;
    for i in 0..4 {
        let va = ((a >> (i * 8)) & 0xFF) as i8;
        let vb = ((b >> (i * 8)) & 0xFF) as i8;
        let sum = ((va as i16 + vb as i16) >> 1) as u8;
        result |= (sum as u32) << (i * 8);
    }
    result
}

extern "C" fn packed_halving_add_u16(a: u32, b: u32) -> u32 {
    let lo_a = a & 0xFFFF;
    let hi_a = a >> 16;
    let lo_b = b & 0xFFFF;
    let hi_b = b >> 16;
    let lo = (lo_a + lo_b) >> 1;
    let hi = (hi_a + hi_b) >> 1;
    (hi << 16) | lo
}

extern "C" fn packed_halving_add_s16(a: u32, b: u32) -> u32 {
    let lo_a = (a & 0xFFFF) as i16;
    let hi_a = (a >> 16) as i16;
    let lo_b = (b & 0xFFFF) as i16;
    let hi_b = (b >> 16) as i16;
    let lo = ((lo_a as i32 + lo_b as i32) >> 1) as u16;
    let hi = ((hi_a as i32 + hi_b as i32) >> 1) as u16;
    ((hi as u32) << 16) | lo as u32
}

extern "C" fn packed_halving_sub_u8(a: u32, b: u32) -> u32 {
    let mut result = 0u32;
    for i in 0..4 {
        let va = (a >> (i * 8)) & 0xFF;
        let vb = (b >> (i * 8)) & 0xFF;
        result |= (va.wrapping_sub(vb) >> 1) << (i * 8);
    }
    result
}

extern "C" fn packed_halving_sub_s8(a: u32, b: u32) -> u32 {
    let mut result = 0u32;
    for i in 0..4 {
        let va = ((a >> (i * 8)) & 0xFF) as i8;
        let vb = ((b >> (i * 8)) & 0xFF) as i8;
        let diff = ((va as i16 - vb as i16) >> 1) as u8;
        result |= (diff as u32) << (i * 8);
    }
    result
}

extern "C" fn packed_halving_sub_u16(a: u32, b: u32) -> u32 {
    let lo_a = a & 0xFFFF;
    let hi_a = a >> 16;
    let lo_b = b & 0xFFFF;
    let hi_b = b >> 16;
    let lo = lo_a.wrapping_sub(lo_b) >> 1;
    let hi = hi_a.wrapping_sub(hi_b) >> 1;
    (hi << 16) | lo
}

extern "C" fn packed_halving_sub_s16(a: u32, b: u32) -> u32 {
    let lo_a = (a & 0xFFFF) as i16;
    let hi_a = (a >> 16) as i16;
    let lo_b = (b & 0xFFFF) as i16;
    let hi_b = (b >> 16) as i16;
    let lo = ((lo_a as i32 - lo_b as i32) >> 1) as u16;
    let hi = ((hi_a as i32 - hi_b as i32) >> 1) as u16;
    ((hi as u32) << 16) | lo as u32
}

extern "C" fn packed_add_sub_u16(a: u32, b: u32) -> u32 {
    let lo_a = a & 0xFFFF;
    let hi_a = a >> 16;
    let lo_b = b & 0xFFFF;
    let hi_b = b >> 16;
    let lo = lo_a.wrapping_sub(lo_b) & 0xFFFF;
    let hi = hi_a.wrapping_add(hi_b) & 0xFFFF;
    (hi << 16) | lo
}

extern "C" fn packed_add_sub_s16(a: u32, b: u32) -> u32 {
    let lo_a = (a & 0xFFFF) as i16;
    let hi_a = (a >> 16) as i16;
    let lo_b = (b & 0xFFFF) as i16;
    let hi_b = (b >> 16) as i16;
    let lo = lo_a.wrapping_sub(lo_b) as u16;
    let hi = hi_a.wrapping_add(hi_b) as u16;
    ((hi as u32) << 16) | lo as u32
}

extern "C" fn packed_sub_add_u16(a: u32, b: u32) -> u32 {
    let lo_a = a & 0xFFFF;
    let hi_a = a >> 16;
    let lo_b = b & 0xFFFF;
    let hi_b = b >> 16;
    let lo = lo_a.wrapping_add(lo_b) & 0xFFFF;
    let hi = hi_a.wrapping_sub(hi_b) & 0xFFFF;
    (hi << 16) | lo
}

extern "C" fn packed_sub_add_s16(a: u32, b: u32) -> u32 {
    let lo_a = (a & 0xFFFF) as i16;
    let hi_a = (a >> 16) as i16;
    let lo_b = (b & 0xFFFF) as i16;
    let hi_b = (b >> 16) as i16;
    let lo = lo_a.wrapping_add(lo_b) as u16;
    let hi = hi_a.wrapping_sub(hi_b) as u16;
    ((hi as u32) << 16) | lo as u32
}

extern "C" fn packed_halving_add_sub_u16(a: u32, b: u32) -> u32 {
    let lo_a = a & 0xFFFF;
    let hi_a = a >> 16;
    let lo_b = b & 0xFFFF;
    let hi_b = b >> 16;
    let lo = lo_a.wrapping_sub(lo_b) >> 1;
    let hi = (hi_a + hi_b) >> 1;
    ((hi & 0xFFFF) << 16) | (lo & 0xFFFF)
}

extern "C" fn packed_halving_add_sub_s16(a: u32, b: u32) -> u32 {
    let lo_a = (a & 0xFFFF) as i16;
    let hi_a = (a >> 16) as i16;
    let lo_b = (b & 0xFFFF) as i16;
    let hi_b = (b >> 16) as i16;
    let lo = ((lo_a as i32 - lo_b as i32) >> 1) as u16;
    let hi = ((hi_a as i32 + hi_b as i32) >> 1) as u16;
    ((hi as u32) << 16) | lo as u32
}

extern "C" fn packed_halving_sub_add_u16(a: u32, b: u32) -> u32 {
    let lo_a = a & 0xFFFF;
    let hi_a = a >> 16;
    let lo_b = b & 0xFFFF;
    let hi_b = b >> 16;
    let lo = (lo_a + lo_b) >> 1;
    let hi = hi_a.wrapping_sub(hi_b) >> 1;
    ((hi & 0xFFFF) << 16) | (lo & 0xFFFF)
}

extern "C" fn packed_halving_sub_add_s16(a: u32, b: u32) -> u32 {
    let lo_a = (a & 0xFFFF) as i16;
    let hi_a = (a >> 16) as i16;
    let lo_b = (b & 0xFFFF) as i16;
    let hi_b = (b >> 16) as i16;
    let lo = ((lo_a as i32 + lo_b as i32) >> 1) as u16;
    let hi = ((hi_a as i32 - hi_b as i32) >> 1) as u16;
    ((hi as u32) << 16) | lo as u32
}

// Halving add
pub fn emit_packed_halving_add_u8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_host_call(ra, inst_ref, inst, packed_halving_add_u8 as usize);
}
pub fn emit_packed_halving_add_s8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_host_call(ra, inst_ref, inst, packed_halving_add_s8 as usize);
}
pub fn emit_packed_halving_add_u16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_host_call(ra, inst_ref, inst, packed_halving_add_u16 as usize);
}
pub fn emit_packed_halving_add_s16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_host_call(ra, inst_ref, inst, packed_halving_add_s16 as usize);
}

// Halving sub
pub fn emit_packed_halving_sub_u8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_host_call(ra, inst_ref, inst, packed_halving_sub_u8 as usize);
}
pub fn emit_packed_halving_sub_s8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_host_call(ra, inst_ref, inst, packed_halving_sub_s8 as usize);
}
pub fn emit_packed_halving_sub_u16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_host_call(ra, inst_ref, inst, packed_halving_sub_u16 as usize);
}
pub fn emit_packed_halving_sub_s16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_host_call(ra, inst_ref, inst, packed_halving_sub_s16 as usize);
}

// AddSub / SubAdd
pub fn emit_packed_add_sub_u16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_host_call(ra, inst_ref, inst, packed_add_sub_u16 as usize);
}
pub fn emit_packed_add_sub_s16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_host_call(ra, inst_ref, inst, packed_add_sub_s16 as usize);
}
pub fn emit_packed_sub_add_u16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_host_call(ra, inst_ref, inst, packed_sub_add_u16 as usize);
}
pub fn emit_packed_sub_add_s16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_host_call(ra, inst_ref, inst, packed_sub_add_s16 as usize);
}

// Halving AddSub / SubAdd
pub fn emit_packed_halving_add_sub_u16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_host_call(ra, inst_ref, inst, packed_halving_add_sub_u16 as usize);
}
pub fn emit_packed_halving_add_sub_s16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_host_call(ra, inst_ref, inst, packed_halving_add_sub_s16 as usize);
}
pub fn emit_packed_halving_sub_add_u16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_host_call(ra, inst_ref, inst, packed_halving_sub_add_u16 as usize);
}
pub fn emit_packed_halving_sub_add_s16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_packed_host_call(ra, inst_ref, inst, packed_halving_sub_add_s16 as usize);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packed_fn_signatures() {
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_packed_add_u8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_packed_sub_u16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_packed_saturated_add_u8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_packed_saturated_sub_s16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_packed_abs_diff_sum_s8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_packed_select;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_packed_halving_add_u8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_packed_add_sub_u16;
    }

    #[test]
    fn test_packed_halving_add_u8_impl() {
        // 4 bytes: [10, 20, 30, 40] + [2, 4, 6, 8] = [6, 12, 18, 24]
        let a = 10u32 | (20 << 8) | (30 << 16) | (40 << 24);
        let b = 2u32 | (4 << 8) | (6 << 16) | (8 << 24);
        let result = packed_halving_add_u8(a, b);
        assert_eq!(result & 0xFF, 6);
        assert_eq!((result >> 8) & 0xFF, 12);
        assert_eq!((result >> 16) & 0xFF, 18);
        assert_eq!((result >> 24) & 0xFF, 24);
    }

    #[test]
    fn test_packed_add_sub_u16_impl() {
        // hi = add, lo = sub
        let a = 0x0010_0020u32; // hi=16, lo=32
        let b = 0x0005_0003u32; // hi=5, lo=3
        let result = packed_add_sub_u16(a, b);
        assert_eq!(result & 0xFFFF, 29); // 32 - 3
        assert_eq!(result >> 16, 21); // 16 + 5
    }
}

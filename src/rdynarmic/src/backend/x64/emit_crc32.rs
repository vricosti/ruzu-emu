use crate::backend::x64::abi;
use crate::backend::x64::emit_context::EmitContext;
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::common::crypto::crc32;
use crate::ir::inst::Inst;
use crate::ir::value::InstRef;

// ---------------------------------------------------------------------------
// CRC32 Castagnoli (native x86 crc32 instruction)
// ---------------------------------------------------------------------------

fn emit_crc32_fallback(
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
    function: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let (first, rest) = args.split_at_mut(1);
    ra.host_call(
        Some(inst_ref),
        &mut [Some(&mut first[0]), Some(&mut rest[0]), None, None],
    );
    ra.asm
        .mov(
            abi::ABI_PARAMS[2].to_reg64().cvt32().unwrap(),
            (bitsize / 8) as i32,
        )
        .unwrap();
    ra.asm.mov(rxbyak::RAX, function as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();
}

fn emit_crc32_castagnoli(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    if !ctx.has_host_feature(HostFeature::SSE42) {
        emit_crc32_fallback(
            ra,
            inst_ref,
            inst,
            bitsize,
            crc32::compute_crc32_castagnoli as *const () as usize,
        );
        return;
    }

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    let data = ra.use_gpr(&mut args[1]);

    match bitsize {
        8 => {
            ra.asm
                .crc32(result.cvt32().unwrap(), data.cvt8().unwrap())
                .unwrap();
        }
        16 => {
            ra.asm
                .crc32(result.cvt32().unwrap(), data.cvt16().unwrap())
                .unwrap();
        }
        32 => {
            ra.asm
                .crc32(result.cvt32().unwrap(), data.cvt32().unwrap())
                .unwrap();
        }
        64 => {
            ra.asm.crc32(result, data).unwrap();
        }
        _ => unreachable!(),
    }

    ra.define_value(inst_ref, result);
}

pub fn emit_crc32_castagnoli8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_crc32_castagnoli(ctx, ra, inst_ref, inst, 8);
}

pub fn emit_crc32_castagnoli16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_crc32_castagnoli(ctx, ra, inst_ref, inst, 16);
}

pub fn emit_crc32_castagnoli32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_crc32_castagnoli(ctx, ra, inst_ref, inst, 32);
}

pub fn emit_crc32_castagnoli64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_crc32_castagnoli(ctx, ra, inst_ref, inst, 64);
}

// ---------------------------------------------------------------------------
// CRC32 ISO
// ---------------------------------------------------------------------------

fn emit_crc32_iso(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    if !ctx.has_host_feature(HostFeature::PCLMULQDQ) {
        emit_crc32_fallback(
            ra,
            inst_ref,
            inst,
            bitsize,
            crc32::compute_crc32_iso as *const () as usize,
        );
        return;
    }

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let crc = ra.use_scratch_gpr(&mut args[0]);
    let value = if bitsize < 32 {
        ra.use_scratch_gpr(&mut args[1])
    } else {
        ra.use_gpr(&mut args[1])
    };
    let xmm_value = ra.scratch_xmm();
    let xmm_const = ra.scratch_xmm();
    let constant = ra
        .constant_pool
        .as_mut()
        .expect("constant pool required")
        .get_constant(0xB4E5_B025_F701_1641, 0x0000_0001_DB71_0641);
    ra.asm
        .movdqa(xmm_const, rxbyak::xmmword_ptr(constant))
        .unwrap();

    match bitsize {
        8 | 16 => {
            let value_sized = if bitsize == 8 {
                value.cvt8().unwrap()
            } else {
                value.cvt16().unwrap()
            };
            ra.asm.movzx(value.cvt32().unwrap(), value_sized).unwrap();
            ra.asm
                .xor_(value.cvt32().unwrap(), crc.cvt32().unwrap())
                .unwrap();
            let xmm_tmp = ra.scratch_xmm();
            ra.asm.movd(xmm_tmp, value.cvt32().unwrap()).unwrap();
            ra.asm.pslldq(xmm_tmp, ((64 - bitsize) / 8) as u8).unwrap();

            if ctx.has_host_feature(HostFeature::AVX) {
                ra.asm
                    .vpclmulqdq(xmm_value, xmm_tmp, xmm_const, 0x00)
                    .unwrap();
            } else {
                ra.asm.movdqa(xmm_value, xmm_tmp).unwrap();
                ra.asm.pclmulqdq(xmm_value, xmm_const, 0x00).unwrap();
            }
            ra.asm.pclmulqdq(xmm_value, xmm_const, 0x10).unwrap();
            ra.asm.pxor(xmm_value, xmm_tmp).unwrap();
            ra.asm.pextrd(crc.cvt32().unwrap(), xmm_value, 2).unwrap();
            ra.release(xmm_tmp);
        }
        32 => {
            ra.asm
                .xor_(crc.cvt32().unwrap(), value.cvt32().unwrap())
                .unwrap();
            ra.asm.shl(crc, 32).unwrap();
            ra.asm.movq(xmm_value, crc).unwrap();
            ra.asm.pclmulqdq(xmm_value, xmm_const, 0x00).unwrap();
            ra.asm.pclmulqdq(xmm_value, xmm_const, 0x10).unwrap();
            ra.asm.pextrd(crc.cvt32().unwrap(), xmm_value, 2).unwrap();
        }
        64 => {
            // Zero-extend the original 32-bit CRC before the 64-bit XOR.
            ra.asm
                .mov(crc.cvt32().unwrap(), crc.cvt32().unwrap())
                .unwrap();
            ra.asm.xor_(crc, value).unwrap();
            ra.asm.movq(xmm_value, crc).unwrap();
            ra.asm.pclmulqdq(xmm_value, xmm_const, 0x00).unwrap();
            ra.asm.pclmulqdq(xmm_value, xmm_const, 0x10).unwrap();
            ra.asm.pextrd(crc.cvt32().unwrap(), xmm_value, 2).unwrap();
        }
        _ => unreachable!(),
    }

    ra.release(xmm_value);
    ra.release(xmm_const);
    ra.define_value(inst_ref, crc);
}

pub fn emit_crc32_iso8(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_crc32_iso(ctx, ra, inst_ref, inst, 8);
}

pub fn emit_crc32_iso16(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_crc32_iso(ctx, ra, inst_ref, inst, 16);
}

pub fn emit_crc32_iso32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_crc32_iso(ctx, ra, inst_ref, inst, 32);
}

pub fn emit_crc32_iso64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_crc32_iso(ctx, ra, inst_ref, inst, 64);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crc32_fn_signatures() {
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_crc32_castagnoli8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_crc32_castagnoli64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_crc32_iso8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_crc32_iso64;
    }
}

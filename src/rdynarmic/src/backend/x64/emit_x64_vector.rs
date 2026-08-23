//! Port of Eden Dynarmic `backend/x64/emit_x64_vector.cpp`.

use crate::backend::x64::emit_context::EmitContext;
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::ir::inst::Inst;
use crate::ir::value::InstRef;

pub fn emit_vector_broadcast_element_lower8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let index = args[1].get_immediate_u8();
    assert!(index < 16);
    if index > 0 {
        ra.asm.psrldq(result, index).unwrap();
    }
    if ctx.has_host_feature(HostFeature::AVX2) {
        ra.asm.vpbroadcastb(result, result).unwrap();
        ra.asm.vmovq(result, result).unwrap();
    } else if ctx.has_host_feature(HostFeature::SSSE3) {
        let temporary = ra.scratch_xmm();
        ra.asm.pxor(temporary, temporary).unwrap();
        ra.asm.pshufb(result, temporary).unwrap();
        ra.asm.movq(result, result).unwrap();
        ra.release(temporary);
    } else {
        ra.asm.punpcklbw(result, result).unwrap();
        ra.asm.pshuflw(result, result, 0).unwrap();
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_broadcast_element_lower16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let index = args[1].get_immediate_u8();
    assert!(index < 8);
    if index > 0 {
        ra.asm.psrldq(result, index * 2).unwrap();
    }
    ra.asm.pshuflw(result, result, 0).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_broadcast_element_lower32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let index = args[1].get_immediate_u8();
    assert!(index < 4);
    if index > 0 {
        ra.asm.psrldq(result, index * 4).unwrap();
    }
    ra.asm.pshuflw(result, result, 0b01_00_01_00).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_broadcast_element8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let index = args[1].get_immediate_u8();
    assert!(index < 16);
    if index > 0 {
        ra.asm.psrldq(result, index).unwrap();
    }
    if ctx.has_host_feature(HostFeature::AVX2) {
        ra.asm.vpbroadcastb(result, result).unwrap();
    } else if ctx.has_host_feature(HostFeature::SSSE3) {
        let temporary = ra.scratch_xmm();
        ra.asm.pxor(temporary, temporary).unwrap();
        ra.asm.pshufb(result, temporary).unwrap();
        ra.release(temporary);
    } else {
        ra.asm.punpcklbw(result, result).unwrap();
        ra.asm.pshuflw(result, result, 0).unwrap();
        ra.asm.punpcklqdq(result, result).unwrap();
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_broadcast_element16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let index = args[1].get_immediate_u8();
    assert!(index < 8);
    if index == 0 && ctx.has_host_feature(HostFeature::AVX2) {
        ra.asm.vpbroadcastw(result, result).unwrap();
    } else if index < 4 {
        ra.asm.pshuflw(result, result, index * 0x55).unwrap();
        ra.asm.punpcklqdq(result, result).unwrap();
    } else {
        ra.asm.pshufhw(result, result, (index - 4) * 0x55).unwrap();
        ra.asm.punpckhqdq(result, result).unwrap();
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_broadcast_element32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let index = args[1].get_immediate_u8();
    assert!(index < 4);
    ra.asm.pshufd(result, result, index * 0x55).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_broadcast_element64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let index = args[1].get_immediate_u8();
    assert!(index < 2);
    if ctx.has_host_feature(HostFeature::AVX) {
        ra.asm.vpermilpd_imm(result, result, index * 3).unwrap();
    } else if index == 0 {
        ra.asm.punpcklqdq(result, result).unwrap();
    } else {
        ra.asm.punpckhqdq(result, result).unwrap();
    }
    ra.define_value(inst_ref, result);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::x64::callback::Callback;
    use crate::backend::x64::emit_context::{EmitCallbacks, EmitConfig};
    use crate::ir::location::LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::value::Value;
    use rxbyak::CodeAssembler;

    struct NoopCallback;

    impl Callback for NoopCallback {
        fn emit_call(
            &self,
            _code: &mut CodeAssembler,
            _setup: &dyn Fn(&mut CodeAssembler, &[rxbyak::Reg]) -> rxbyak::Result<()>,
        ) -> rxbyak::Result<()> {
            unreachable!("callback emission is not used in this unit test");
        }

        fn emit_call_with_return_pointer(
            &self,
            _code: &mut CodeAssembler,
            _setup: &dyn Fn(&mut CodeAssembler, rxbyak::Reg, &[rxbyak::Reg]) -> rxbyak::Result<()>,
        ) -> rxbyak::Result<()> {
            unreachable!("callback emission is not used in this unit test");
        }
    }

    fn dummy_emit_config() -> EmitConfig {
        fn cb() -> Box<dyn Callback> {
            Box::new(NoopCallback)
        }

        EmitConfig {
            callbacks: EmitCallbacks {
                memory_read_8: cb(),
                memory_read_16: cb(),
                memory_read_32: cb(),
                memory_read_64: cb(),
                memory_read_128: cb(),
                memory_write_8: cb(),
                memory_write_16: cb(),
                memory_write_32: cb(),
                memory_write_64: cb(),
                memory_write_128: cb(),
                call_supervisor: cb(),
                interpreter_fallback: cb(),
                exception_raised: cb(),
                data_cache_operation: cb(),
                instruction_cache_operation: cb(),
                instruction_synchronization_barrier: cb(),
                add_ticks: cb(),
                get_ticks_remaining: cb(),
                exclusive_clear: cb(),
                exclusive_read_8: cb(),
                exclusive_read_16: cb(),
                exclusive_read_32: cb(),
                exclusive_read_64: cb(),
                exclusive_read_128: cb(),
                get_cntpct: cb(),
                exclusive_write_8: cb(),
                exclusive_write_16: cb(),
                exclusive_write_32: cb(),
                exclusive_write_64: cb(),
                exclusive_write_128: cb(),
            },
            raw_exclusive_write_callbacks: None,
            enable_cycle_counting: false,
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
            global_monitor: None,
            cntfrq_el0: 600_000_000,
        }
    }

    fn emit_with_features(opcode: Opcode, index: u8, features: HostFeature) -> Vec<u8> {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let mut ra = RegAlloc::new_default(&mut asm, vec![(1, 128), (0, 128)]);
        let source = ra.scratch_xmm();
        ra.define_value(InstRef(0), source);
        ra.end_of_alloc_scope();

        let config = dummy_emit_config();
        let mut ctx = EmitContext::new(LocationDescriptor::new(0), &config);
        ctx.host_features = features;
        let inst = Inst::new(opcode, &[Value::Inst(InstRef(0)), Value::ImmU8(index)]);
        match opcode {
            Opcode::VectorBroadcastElementLower8 => {
                emit_vector_broadcast_element_lower8(&ctx, &mut ra, InstRef(1), &inst)
            }
            Opcode::VectorBroadcastElementLower16 => {
                emit_vector_broadcast_element_lower16(&ctx, &mut ra, InstRef(1), &inst)
            }
            Opcode::VectorBroadcastElementLower32 => {
                emit_vector_broadcast_element_lower32(&ctx, &mut ra, InstRef(1), &inst)
            }
            Opcode::VectorBroadcastElement8 => {
                emit_vector_broadcast_element8(&ctx, &mut ra, InstRef(1), &inst)
            }
            Opcode::VectorBroadcastElement16 => {
                emit_vector_broadcast_element16(&ctx, &mut ra, InstRef(1), &inst)
            }
            Opcode::VectorBroadcastElement32 => {
                emit_vector_broadcast_element32(&ctx, &mut ra, InstRef(1), &inst)
            }
            Opcode::VectorBroadcastElement64 => {
                emit_vector_broadcast_element64(&ctx, &mut ra, InstRef(1), &inst)
            }
            _ => unreachable!("test helper only covers selected broadcast-element opcodes"),
        }
        ra.end_of_alloc_scope();
        ra.asm.code().to_vec()
    }

    #[test]
    fn function_signatures_match_the_seven_upstream_emitters() {
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_broadcast_element_lower8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_broadcast_element_lower16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_broadcast_element_lower32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_broadcast_element8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_broadcast_element16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_broadcast_element32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_broadcast_element64;
    }

    #[test]
    fn lower8_avx2_uses_vpbroadcastb_then_vmovq() {
        let code = emit_with_features(Opcode::VectorBroadcastElementLower8, 0, HostFeature::AVX2);
        assert!(
            code.windows(4)
                .any(|bytes| bytes[0] == 0xc4 && bytes[3] == 0x78),
            "{code:02x?}"
        );
        assert!(
            code.windows(4)
                .any(|bytes| bytes[0] == 0xc5 && bytes[2] == 0x7e),
            "{code:02x?}"
        );
    }

    #[test]
    fn lower8_selects_edens_ssse3_and_sse2_fallbacks() {
        let ssse3 = emit_with_features(Opcode::VectorBroadcastElementLower8, 1, HostFeature::SSSE3);
        assert!(ssse3
            .windows(4)
            .any(|bytes| bytes[..4] == [0x66, 0x0f, 0x38, 0x00]));
        assert!(ssse3
            .windows(3)
            .any(|bytes| bytes[..3] == [0xf3, 0x0f, 0x7e]));

        let sse2 = emit_with_features(
            Opcode::VectorBroadcastElementLower8,
            1,
            HostFeature::empty(),
        );
        assert!(sse2
            .windows(3)
            .any(|bytes| bytes[..3] == [0x66, 0x0f, 0x60]));
        assert!(sse2
            .windows(3)
            .any(|bytes| bytes[..3] == [0xf2, 0x0f, 0x70]));
        assert!(!sse2
            .windows(3)
            .any(|bytes| bytes[..3] == [0x66, 0x0f, 0x6c]));
    }

    #[test]
    fn full16_selects_edens_avx2_and_upper_half_fallbacks() {
        let avx2 = emit_with_features(Opcode::VectorBroadcastElement16, 0, HostFeature::AVX2);
        assert!(avx2
            .windows(4)
            .any(|bytes| bytes[0] == 0xc4 && bytes[3] == 0x79));

        let upper = emit_with_features(Opcode::VectorBroadcastElement16, 5, HostFeature::empty());
        assert!(upper
            .windows(3)
            .any(|bytes| bytes[..3] == [0xf3, 0x0f, 0x70]));
        assert!(upper
            .windows(3)
            .any(|bytes| bytes[..3] == [0x66, 0x0f, 0x6d]));
    }

    #[test]
    fn lower32_shifts_selected_lane_then_uses_upstream_shuffle() {
        let code = emit_with_features(
            Opcode::VectorBroadcastElementLower32,
            2,
            HostFeature::empty(),
        );
        assert!(code.windows(5).any(|bytes| bytes[0] == 0x66
            && bytes[1] == 0x0f
            && bytes[2] == 0x73
            && bytes[4] == 8));
        assert_eq!(code.last(), Some(&0b01_00_01_00));
    }

    #[test]
    fn full32_replicates_the_selected_lane_in_the_shuffle_control() {
        let code = emit_with_features(Opcode::VectorBroadcastElement32, 3, HostFeature::empty());
        assert_eq!(code.last(), Some(&0xff));
    }

    #[test]
    fn full64_avx_uses_the_immediate_vpermilpd_form() {
        let code = emit_with_features(Opcode::VectorBroadcastElement64, 1, HostFeature::AVX);
        assert!(
            code.windows(6)
                .any(|bytes| bytes[0] == 0xc4 && bytes[3] == 0x05 && bytes[5] == 3),
            "{code:02x?}"
        );
    }
}

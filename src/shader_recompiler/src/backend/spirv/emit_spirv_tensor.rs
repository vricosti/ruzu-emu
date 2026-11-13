// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of Fritchii backend/spirv/emit_spirv_tensor.cpp (software warp collective).
use super::spirv_emit_context::SpirvEmitContext;
use crate::ir::{Inst, Opcode, Program};
use rspirv::spirv::{self, Word};

pub fn define_software_matrix_functions(ctx: &mut SpirvEmitContext, program: &Program) {
    let mut needed = [false; 2];
    for block in &program.blocks {
        for inst in block.iter() {
            if inst.opcode != Opcode::TensorMulAdd || inst.flags == 0 {
                continue;
            }
            if !matches!(inst.flags, 8 | 16) {
                std::panic::panic_any(crate::exception::LogicError::new("Invalid software HMMA depth"));
            }
            needed[usize::from(inst.flags == 16)] = true;
        }
    }
    for (variant, needed) in needed.into_iter().enumerate() {
        if !needed {
            continue;
        }
        if !ctx.profile.supports_subgroup_stage(program.stage)
            || ctx.profile.has_broken_spirv_subgroup_shuffle
        {
            std::panic::panic_any(crate::exception::NotImplementedException::new(
                "Software HMMA requires subgroup shuffle support",
            ));
        }
        let u = ctx.u32_type;
        let f = ctx.f32_type;
        let boolean = ctx.bool_type;
        let u2 = ctx.u32_vec2_type;
        let u4 = ctx.u32_vec4_type;
        let f2 = ctx.f32_vec2_type;
        let ext = ctx.glsl_ext;
        let lane_id = ctx.subgroup_local_invocation_id;
        let b = &mut ctx.builder;
        b.capability(spirv::Capability::GroupNonUniform);
        b.capability(spirv::Capability::GroupNonUniformShuffle);
        // Mechanical equivalents of the upstream local lambdas.
        macro_rules! imm {
            ($v:expr) => {
                b.constant_bit32(u, $v)
            };
        }
        macro_rules! op { ($name:ident, $ty:expr, $($arg:expr),+) => {{
            let args = ($($arg,)+);
            op!(@call $name, $ty, args; $($arg),+)
        }};
        (@call $name:ident, $ty:expr, $args:ident; $a:expr) => { b.$name($ty, None, $args.0).unwrap() };
        (@call $name:ident, $ty:expr, $args:ident; $a:expr, $c:expr) => { b.$name($ty, None, $args.0, $args.1).unwrap() };
        (@call $name:ident, $ty:expr, $args:ident; $a:expr, $c:expr, $d:expr) => { b.$name($ty, None, $args.0, $args.1, $args.2).unwrap() }; }
        macro_rules! extract {
            ($ty:expr, $v:expr, $i:expr) => {
                b.composite_extract($ty, None, $v, vec![$i]).unwrap()
            };
        }
        macro_rules! load {
            ($ty:expr, $p:expr) => {
                b.load($ty, None, $p, None, []).unwrap()
            };
        }
        macro_rules! gl { ($ty:expr, $name:ident, $($v:expr),+) => {{
            let args = vec![$(rspirv::dr::Operand::IdRef($v)),+];
            b.ext_inst($ty, None, ext, spirv::GLOp::$name as u32, args).unwrap()
        }}; }
        let function_type = b.type_function(u2, vec![u4, u2, u2]);
        let function = b
            .begin_function(u2, None, spirv::FunctionControl::DONT_INLINE, function_type)
            .unwrap();
        ctx.software_matrix_functions[variant] = function;
        let matrix_a = b.function_parameter(u4).unwrap();
        let matrix_b = b.function_parameter(u2).unwrap();
        let matrix_c = b.function_parameter(u2).unwrap();
        b.begin_block(None).unwrap();
        let up = b.type_pointer(None, spirv::StorageClass::Function, u);
        let fp = b.type_pointer(None, spirv::StorageClass::Function, f);
        let counter = b.variable(up, None, spirv::StorageClass::Function, None);
        let accumulators = std::array::from_fn::<_, 4, _>(|_| {
            b.variable(fp, None, spirv::StorageClass::Function, None)
        });
        let mut inputs = [0; 8];
        for i in 0..4 {
            inputs[i] = extract!(u, matrix_a, i as u32);
        }
        for i in 0..2 {
            inputs[4 + i] = extract!(u, matrix_b, i as u32);
            inputs[6 + i] = extract!(u, matrix_c, i as u32);
        }
        for row in 0..2 {
            let c = gl!(f2, UnpackHalf2x16, inputs[6 + row]);
            for col in 0..2 {
                let v = extract!(f, c, col as u32);
                b.store(accumulators[row * 2 + col], v, None, []).unwrap();
            }
        }
        let zero = imm!(0);
        b.store(counter, zero, None, []).unwrap();
        let lane = load!(u, lane_id);
        let warp_base = op!(bitwise_and, u, lane, imm!(!31u32));
        let row_base = op!(bitwise_and, u, lane, imm!(28));
        let col_bits = op!(bitwise_and, u, lane, imm!(3));
        let col_base = op!(shift_left_logical, u, col_bits, imm!(3));
        let header = b.id();
        let body = b.id();
        let next = b.id();
        let end = b.id();
        b.branch(header).unwrap();
        b.begin_block(Some(header)).unwrap();
        let pair = load!(u, counter);
        let more = op!(
            u_less_than,
            boolean,
            pair,
            imm!(if variant == 1 { 8 } else { 4 })
        );
        b.loop_merge(end, next, spirv::LoopControl::DONT_UNROLL, [])
            .unwrap();
        b.branch_conditional(more, body, end, []).unwrap();
        b.begin_block(Some(body)).unwrap();
        let upper = op!(u_greater_than_equal, boolean, pair, imm!(4));
        let component = op!(bitwise_and, u, pair, imm!(3));
        let a_offset = op!(i_add, u, row_base, component);
        let a_lane = op!(i_add, u, warp_base, a_offset);
        let b_offset = op!(i_add, u, col_base, component);
        let b_lane = op!(i_add, u, warp_base, b_offset);
        let bv = op!(select, u, upper, inputs[5], inputs[4]);
        let mut bs = [0; 2];
        for col in 0..2 {
            let target = op!(i_add, u, b_lane, imm!(col as u32 * 4));
            let shuffled = op!(group_non_uniform_shuffle, u, imm!(3), bv, target);
            bs[col] = gl!(f2, UnpackHalf2x16, shuffled);
        }
        let round_now = op!(i_equal, boolean, component, imm!(3));
        for row in 0..2 {
            let av = op!(select, u, upper, inputs[row + 2], inputs[row]);
            let shuffled = op!(group_non_uniform_shuffle, u, imm!(3), av, a_lane);
            let halves = gl!(f2, UnpackHalf2x16, shuffled);
            for col in 0..2 {
                let mut value = load!(f, accumulators[row * 2 + col]);
                for half in 0..2 {
                    let a = extract!(f, halves, half);
                    let rhs = extract!(f, bs[col], half);
                    value = gl!(f, Fma, a, rhs, value);
                    b.decorate(value, spirv::Decoration::NoContraction, []);
                }
                b.store(accumulators[row * 2 + col], value, None, [])
                    .unwrap();
            }
        }
        let round_block = b.id();
        let round_merge = b.id();
        b.selection_merge(round_merge, spirv::SelectionControl::DONT_FLATTEN)
            .unwrap();
        b.branch_conditional(round_now, round_block, round_merge, [])
            .unwrap();
        b.begin_block(Some(round_block)).unwrap();
        for accumulator in accumulators {
            let value = load!(f, accumulator);
            let bits = op!(bitcast, u, value);
            let magnitude = op!(bitwise_and, u, bits, imm!(0x7fffffff));
            let shifted = op!(shift_right_logical, u, magnitude, imm!(13));
            let tie = op!(bitwise_and, u, shifted, imm!(1));
            let biased = op!(i_add, u, magnitude, imm!(0xfff));
            let rounded_sum = op!(i_add, u, biased, tie);
            let rounded = op!(bitwise_and, u, rounded_sum, imm!(!0x1fffu32));
            let overflow = op!(u_greater_than_equal, boolean, rounded, imm!(0x47800000));
            let normal = op!(select, u, overflow, imm!(0x7f800000), rounded);
            let magnitude_f = op!(bitcast, f, magnitude);
            let scale = b.constant_bit32(f, 16777216.0f32.to_bits());
            let scaled = op!(f_mul, f, magnitude_f, scale);
            let even = gl!(f, RoundEven, scaled);
            let unscale = b.constant_bit32(f, (1.0f32 / 16777216.0).to_bits());
            let subnormal_f = op!(f_mul, f, even, unscale);
            let subnormal = op!(bitcast, u, subnormal_f);
            let is_normal = op!(u_greater_than_equal, boolean, magnitude, imm!(0x38800000));
            let finite = op!(select, u, is_normal, normal, subnormal);
            let non_finite = op!(u_greater_than_equal, boolean, magnitude, imm!(0x7f800000));
            let result = op!(select, u, non_finite, magnitude, finite);
            let sign = op!(bitwise_and, u, bits, imm!(0x80000000));
            let signed = op!(bitwise_or, u, result, sign);
            let rounded_value = op!(bitcast, f, signed);
            b.store(accumulator, rounded_value, None, []).unwrap();
        }
        b.branch(round_merge).unwrap();
        b.begin_block(Some(round_merge)).unwrap();
        b.branch(next).unwrap();
        b.begin_block(Some(next)).unwrap();
        let increment = op!(i_add, u, pair, imm!(1));
        b.store(counter, increment, None, []).unwrap();
        b.branch(header).unwrap();
        b.begin_block(Some(end)).unwrap();
        let mut packed = [0; 2];
        for row in 0..2 {
            let x = load!(f, accumulators[row * 2]);
            let y = load!(f, accumulators[row * 2 + 1]);
            let vector = b.composite_construct(f2, None, vec![x, y]).unwrap();
            packed[row] = gl!(u, PackHalf2x16, vector);
        }
        let result = b.composite_construct(u2, None, packed).unwrap();
        b.ret_value(result).unwrap();
        b.end_function().unwrap();
    }
}

pub fn emit_tensor_mul_add(
    ctx: &mut SpirvEmitContext,
    inst: &Inst,
    a: Word,
    b: Word,
    c: Word,
) -> Word {
    if !matches!(inst.flags, 8 | 16) {
        std::panic::panic_any(crate::exception::NotImplementedException::new("Unsupported matrix multiply depth"));
    }
    ctx.builder
        .function_call(
            ctx.u32_vec2_type,
            None,
            ctx.software_matrix_functions[usize::from(inst.flags == 16)],
            vec![a, b, c],
        )
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{emitter::Emitter, types::ShaderStage, SyntaxNode, Value};
    use crate::{runtime_info::RuntimeInfo, Profile};

    fn program() -> Program {
        let mut p = Program::new(ShaderStage::Compute);
        let block = p.add_block();
        let mut e = Emitter::new(&mut p, block);
        let z = Value::ImmU32(0);
        let a = e.composite_construct_u32x4(z, z, z, z);
        let b = e.composite_construct_u32x2(z, z);
        for depth in [8, 16, 8] {
            e.tensor_mul_add(a, b, b, depth).unwrap();
        }
        p.syntax_list = vec![SyntaxNode::Block(block), SyntaxNode::Return];
        crate::ir_opt::collect_shader_info_pass::collect_shader_info_pass(&mut p);
        p
    }

    #[test]
    fn software_matrix_helpers_share_bounded_loops_and_preserve_rounding() {
        let p = program();
        assert!(
            p.info.uses_subgroup_shuffles
                && p.info.uses_subgroup_invocation_id
                && p.info.uses_fp32_denorms_preserve
        );
        let profile = Profile {
            supported_spirv: 0x00010300,
            supported_subgroup_stages: 1 << ShaderStage::Compute as u32,
            ..Profile::default()
        };
        let mut ctx = SpirvEmitContext::new(&p, &profile, &RuntimeInfo::default());
        ctx.emit_program(&p);
        assert_ne!(ctx.software_matrix_functions[0], 0);
        assert_ne!(ctx.software_matrix_functions[1], 0);
        let module = ctx.builder.module_ref();
        for id in ctx.software_matrix_functions {
            let helper = module
                .functions
                .iter()
                .find(|f| f.def.as_ref().unwrap().result_id == Some(id))
                .unwrap();
            assert!(helper.def.as_ref().unwrap().operands.contains(
                &rspirv::dr::Operand::FunctionControl(spirv::FunctionControl::DONT_INLINE)
            ));
            let instructions: Vec<_> = helper.blocks.iter().flat_map(|b| &b.instructions).collect();
            assert_eq!(
                instructions
                    .iter()
                    .filter(|i| i.class.opcode == spirv::Op::LoopMerge)
                    .count(),
                1
            );
            assert_eq!(
                instructions
                    .iter()
                    .filter(|i| i.class.opcode == spirv::Op::GroupNonUniformShuffle)
                    .count(),
                4
            );
            assert!(instructions
                .iter()
                .any(|i| i.class.opcode == spirv::Op::LoopMerge
                    && i.operands.contains(&rspirv::dr::Operand::LoopControl(
                        spirv::LoopControl::DONT_UNROLL
                    ))));
            assert!(instructions
                .iter()
                .any(|i| i.class.opcode == spirv::Op::ExtInst
                    && i.operands
                        .contains(&rspirv::dr::Operand::LiteralExtInstInteger(
                            spirv::GLOp::RoundEven as u32
                        ))));
        }
        assert_eq!(
            module
                .functions
                .iter()
                .flat_map(|f| &f.blocks)
                .flat_map(|b| &b.instructions)
                .filter(|i| i.class.opcode == spirv::Op::FunctionCall)
                .count(),
            3
        );
        // Optional external validation artifact, emitted only by this unit test.
        if let Some(path) = std::env::var_os("RUZU_TEST_MATRIX_SPIRV") {
            let bytes: Vec<_> = ctx
                .finalize()
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect();
            std::fs::write(path, bytes).unwrap();
        }
    }

    #[test]
    fn software_matrix_rejects_unsupported_subgroup_profiles() {
        for broken in [false, true] {
            let p = program();
            let profile = Profile {
                supported_subgroup_stages: if broken { u32::MAX } else { 0 },
                has_broken_spirv_subgroup_shuffle: broken,
                ..Profile::default()
            };
            let mut ctx = SpirvEmitContext::new(&p, &profile, &RuntimeInfo::default());
            assert!(std::panic::catch_unwind(
                std::panic::AssertUnwindSafe(|| ctx.emit_program(&p))
            )
            .is_err());
        }
    }
}

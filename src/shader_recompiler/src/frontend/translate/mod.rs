// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/shader_recompiler/frontend/maxwell/translate/impl/
//!
//! Maxwell → IR translator.
//!
//! The `TranslatorVisitor` decodes each Maxwell instruction and emits
//! corresponding IR instructions via the `Emitter`.
//!
//! Each submodule corresponds 1:1 to an upstream `impl/*.cpp` file.

// Instruction translation modules (1:1 with upstream impl/*.cpp files)
use crate::ir::program::ShaderInfoExt;
pub mod atomic_operations_global_memory;
pub mod atomic_operations_shared_memory;
pub mod attribute_memory_to_physical;
pub mod barrier_operations;
pub mod bitfield_extract;
pub mod bitfield_insert;
pub mod branch_indirect;
pub mod common_encoding;
pub mod common_funcs;
pub mod condition_code_set;
pub mod double_add;
pub mod double_compare_and_set;
pub mod double_fused_multiply_add;
pub mod double_min_max;
pub mod double_multiply;
pub mod double_set_predicate;
pub mod exit_program;
pub mod find_leading_one;
pub mod floating_point_add;
pub mod floating_point_compare;
pub mod floating_point_compare_and_set;
pub mod floating_point_conversion_floating_point;
pub mod floating_point_conversion_integer;
pub mod floating_point_fused_multiply_add;
pub mod floating_point_min_max;
pub mod floating_point_multi_function;
pub mod floating_point_multiply;
pub mod floating_point_range_reduction;
pub mod floating_point_set_predicate;
pub mod floating_point_swizzled_add;
pub mod half_floating_point_add;
pub mod half_floating_point_fused_multiply_add;
pub mod half_floating_point_helper;
pub mod half_floating_point_multiply;
pub mod half_floating_point_set;
pub mod half_floating_point_set_predicate;
pub mod integer_add;
pub mod integer_add_three_input;
pub mod integer_compare;
pub mod integer_compare_and_set;
pub mod integer_floating_point_conversion;
pub mod integer_funnel_shift;
pub mod integer_minimum_maximum;
pub mod integer_popcount;
pub mod integer_scaled_add;
pub mod integer_set_predicate;
pub mod integer_shift_left;
pub mod integer_shift_right;
pub mod integer_short_multiply_add;
pub mod integer_to_integer_conversion;
pub mod internal_stage_buffer_entry_read;
pub mod load_constant;
pub mod load_effective_address;
pub mod load_store_attribute;
pub mod load_store_local_shared;
pub mod load_store_memory;
pub mod logic_operation;
pub mod logic_operation_three_input;
pub mod move_predicate_to_register;
pub mod move_register;
pub mod move_register_to_predicate;
pub mod move_special_register;
pub mod not_implemented;
pub mod output_geometry;
pub mod pixel_load;
pub mod predicate_set_predicate;
pub mod predicate_set_register;
pub mod select_source_with_predicate;
pub mod surface_atomic_operations;
pub mod surface_load_store;
pub mod texture_fetch;
pub mod texture_fetch_swizzled;
pub mod texture_gather;
pub mod texture_gather_swizzled;
pub mod texture_gradient;
pub mod texture_load;
pub mod texture_load_swizzled;
pub mod texture_mipmap_level;
pub mod texture_query;
pub mod video_helper;
pub mod video_minimum_maximum;
pub mod video_multiply_add;
pub mod video_set_predicate;
pub mod vote;
pub mod warp_shuffle;

use crate::environment::Environment;
use crate::frontend::maxwell_opcodes::{MaxwellOpcode, SrcType};
use crate::ir::emitter::Emitter;
use crate::ir::program::Program;
use crate::ir::types::ShaderStage;
use crate::ir::value::{Reg, Value};
use crate::program_header::ProgramHeader;

/// Maxwell instruction bit field extraction helpers.
pub fn field(insn: u64, start: u32, len: u32) -> u32 {
    ((insn >> start) & ((1u64 << len) - 1)) as u32
}

pub fn bit(insn: u64, pos: u32) -> bool {
    (insn >> pos) & 1 != 0
}

pub fn sfield(insn: u64, start: u32, len: u32) -> i32 {
    let val = field(insn, start, len);
    let sign_bit = 1u32 << (len - 1);
    if val & sign_bit != 0 {
        (val | !((1u32 << len) - 1)) as i32
    } else {
        val as i32
    }
}

/// The translator visitor: holds state during translation of a single shader.
///
/// Corresponds to the `TranslatorVisitor` class in upstream `impl.h` / `impl.cpp`.
pub struct TranslatorVisitor<'a> {
    pub ir: Emitter<'a>,
    pub stage: ShaderStage,
    pub sph: Option<ProgramHeader>,
    /// Upstream `TranslatorVisitor::env` owner. Runtime translation always
    /// supplies this; reduced instruction tests may omit it.
    pub env: Option<&'a dyn Environment>,
}

impl<'a> TranslatorVisitor<'a> {
    pub fn new(program: &'a mut Program, block: u32) -> Self {
        Self::new_with_sph(program, block, None)
    }

    pub fn new_with_sph(program: &'a mut Program, block: u32, sph: Option<ProgramHeader>) -> Self {
        let stage = program.stage;
        Self {
            ir: Emitter::new(program, block),
            stage,
            sph,
            env: None,
        }
    }

    /// Construct the runtime visitor with the same environment ownership as
    /// upstream `TranslatorVisitor(Environment&, IR::Block&)`.
    pub fn new_with_env(program: &'a mut Program, block: u32, env: &'a dyn Environment) -> Self {
        let stage = env.shader_stage();
        Self {
            ir: Emitter::new(program, block),
            stage,
            sph: Some(env.sph().clone()),
            env: Some(env),
        }
    }

    /// Get a register value as U32.
    pub fn x(&mut self, reg_idx: u32) -> Value {
        let reg = Reg(reg_idx as u8);
        if reg.is_zero() {
            Value::ImmU32(0)
        } else {
            self.ir.get_reg(reg)
        }
    }

    /// Get an unsigned 64-bit integer from an aligned register pair.
    ///
    /// Corresponds to upstream `TranslatorVisitor::L(IR::Reg reg)`.
    pub fn l(&mut self, reg_idx: u32) -> Value {
        if reg_idx & 1 != 0 {
            panic!("Unaligned source register {}", reg_idx);
        }
        let lo = self.x(reg_idx);
        let hi = self.x(reg_idx + 1);
        let pair = self.ir.composite_construct_u32x2(lo, hi);
        self.ir.pack_uint_2x32(pair)
    }

    /// Get a register value interpreted as F32.
    pub fn f(&mut self, reg_idx: u32) -> Value {
        let u = self.x(reg_idx);
        self.ir.bit_cast_f32_u32(u)
    }

    /// Set a register to a U32 value.
    pub fn set_x(&mut self, reg_idx: u32, value: Value) {
        let reg = Reg(reg_idx as u8);
        if !reg.is_zero() {
            self.ir.set_reg(reg, value);
        }
    }

    /// Store an unsigned 64-bit integer into an aligned register pair.
    ///
    /// Corresponds to upstream `TranslatorVisitor::L(IR::Reg, IR::U64)`.
    pub fn set_l(&mut self, reg_idx: u32, value: Value) {
        if reg_idx & 1 != 0 {
            panic!("Unaligned destination register {}", reg_idx);
        }
        let pair = self.ir.unpack_uint_2x32(value);
        let lo = self.ir.composite_extract_u32x2_idx(pair.clone(), 0);
        let hi = self.ir.composite_extract_u32x2_idx(pair, 1);
        self.set_x(reg_idx, lo);
        self.set_x(reg_idx + 1, hi);
    }

    /// Set a register to an F32 value (via bitcast).
    pub fn set_f(&mut self, reg_idx: u32, value: Value) {
        let bits = self.ir.bit_cast_u32_f32(value);
        self.set_x(reg_idx, bits);
    }

    /// Decode src_b operand based on opcode variant (register, cbuf, immediate).
    pub fn decode_src_b(&mut self, insn: u64, opcode: MaxwellOpcode) -> Value {
        match opcode.src_type() {
            SrcType::Register => {
                let reg_idx = field(insn, 20, 8);
                self.x(reg_idx)
            }
            SrcType::ConstantBuffer => {
                let cb_index = field(insn, 34, 5);
                let cb_offset = field(insn, 20, 14) << 2;
                let binding = Value::ImmU32(cb_index);
                let offset = Value::ImmU32(cb_offset);
                self.ir.program.info.register_cbuf(cb_index);
                self.ir.get_cbuf_u32(binding, offset)
            }
            SrcType::Immediate => {
                // Maxwell encodes the sign separately in bit 56. This is
                // upstream `TranslatorVisitor::GetImm20`, not a signed
                // 19-bit field.
                self.get_imm20(insn)
            }
        }
    }

    /// Decode src_b as F32 (for floating-point instructions).
    pub fn decode_src_b_f32(&mut self, insn: u64, opcode: MaxwellOpcode) -> Value {
        match opcode.src_type() {
            SrcType::Register => {
                let reg_idx = field(insn, 20, 8);
                self.f(reg_idx)
            }
            SrcType::ConstantBuffer => {
                let cb_index = field(insn, 34, 5);
                let cb_offset = field(insn, 20, 14) << 2;
                let binding = Value::ImmU32(cb_index);
                let offset = Value::ImmU32(cb_offset);
                self.ir.program.info.register_cbuf(cb_index);
                self.ir.get_cbuf_f32(binding, offset)
            }
            SrcType::Immediate => self.get_float_imm20(insn),
        }
    }

    /// Decode the 32-bit immediate for 32I-type instructions.
    pub fn decode_imm32(&self, insn: u64) -> u32 {
        field(insn, 20, 32)
    }

    /// Decode the destination register index from bits [7:0].
    pub fn dst_reg(&self, insn: u64) -> u32 {
        field(insn, 0, 8)
    }

    /// Decode src_a register index from bits [15:8].
    pub fn src_a_reg(&self, insn: u64) -> u32 {
        field(insn, 8, 8)
    }

    /// Decode predicate register for result from bits [47:44].
    pub fn dst_pred(&self, insn: u64) -> u32 {
        field(insn, 44, 3)
    }

    /// Decode predicate register for secondary result from bits [3:1].
    pub fn dst_pred2(&self, insn: u64) -> u32 {
        field(insn, 1, 3)
    }

    /// Get a double-precision (F64) value from a register pair at reg_idx and reg_idx+1.
    ///
    /// Corresponds to `TranslatorVisitor::D(IR::Reg reg)` upstream.
    pub fn d(&mut self, reg_idx: u32) -> Value {
        if reg_idx & 1 != 0 {
            panic!("Unaligned source register {}", reg_idx);
        }
        let lo = self.x(reg_idx);
        let hi = self.x(reg_idx + 1);
        let vec = self.ir.composite_construct_u32x2(lo, hi);
        self.ir.pack_double_2x32(vec)
    }

    /// Store a double-precision (F64) value into a register pair.
    ///
    /// Corresponds to `TranslatorVisitor::D(IR::Reg dest, const IR::F64& value)` upstream.
    pub fn set_d(&mut self, reg_idx: u32, value: Value) {
        if reg_idx & 1 != 0 {
            panic!("Unaligned destination register {}", reg_idx);
        }
        let unpacked = self.ir.unpack_double_2x32(value);
        let lo = self.ir.composite_extract_u32x2_idx(unpacked.clone(), 0);
        let hi = self.ir.composite_extract_u32x2_idx(unpacked, 1);
        self.set_x(reg_idx, lo);
        self.set_x(reg_idx + 1, hi);
    }

    /// Get a reg from bits [8:15] as U32 (GetReg8 upstream).
    pub fn get_reg8(&mut self, insn: u64) -> Value {
        let idx = field(insn, 8, 8);
        self.x(idx)
    }

    /// Get a reg from bits [20:27] as U32 (GetReg20 upstream).
    pub fn get_reg20(&mut self, insn: u64) -> Value {
        let idx = field(insn, 20, 8);
        self.x(idx)
    }

    /// Get a reg from bits [39:46] as U32 (GetReg39 upstream).
    pub fn get_reg39(&mut self, insn: u64) -> Value {
        let idx = field(insn, 39, 8);
        self.x(idx)
    }

    /// Get a double from register pair at bits [20:27].
    pub fn get_double_reg20(&mut self, insn: u64) -> Value {
        let idx = field(insn, 20, 8);
        self.d(idx)
    }

    /// Get a double from register pair at bits [39:46].
    pub fn get_double_reg39(&mut self, insn: u64) -> Value {
        let idx = field(insn, 39, 8);
        self.d(idx)
    }

    /// Get a U32 from a constant buffer (GetCbuf upstream).
    pub fn get_cbuf(&mut self, insn: u64) -> Value {
        let cb_index = field(insn, 34, 5);
        let cb_offset = field(insn, 20, 14) << 2;
        if cb_index >= 18 {
            panic!("Out of bounds constant buffer binding {cb_index}");
        }
        let binding = Value::ImmU32(cb_index);
        let offset = Value::ImmU32(cb_offset);
        self.ir.program.info.register_cbuf(cb_index);
        self.ir.get_cbuf_u32(binding, offset)
    }

    /// Get an aligned packed U64 from a constant buffer.
    ///
    /// Corresponds to upstream `TranslatorVisitor::GetPackedCbuf`.
    pub fn get_packed_cbuf(&mut self, insn: u64) -> Value {
        if bit(insn, 20) {
            panic!("Unaligned packed constant buffer read");
        }
        let cb_index = field(insn, 34, 5);
        let cb_offset = field(insn, 20, 14) << 2;
        if cb_index >= 18 {
            panic!("Out of bounds constant buffer binding {cb_index}");
        }
        self.ir.program.info.register_cbuf(cb_index);
        let binding = Value::ImmU32(cb_index);
        let lo = self
            .ir
            .get_cbuf_u32(binding.clone(), Value::ImmU32(cb_offset));
        let hi = self.ir.get_cbuf_u32(binding, Value::ImmU32(cb_offset + 4));
        let pair = self.ir.composite_construct_u32x2(lo, hi);
        self.ir.pack_uint_2x32(pair)
    }

    /// Get an F32 from a constant buffer.
    pub fn get_float_cbuf(&mut self, insn: u64) -> Value {
        let cb_index = field(insn, 34, 5);
        let cb_offset = field(insn, 20, 14) << 2;
        if cb_index >= 18 {
            panic!("Out of bounds constant buffer binding {cb_index}");
        }
        let binding = Value::ImmU32(cb_index);
        let offset = Value::ImmU32(cb_offset);
        self.ir.program.info.register_cbuf(cb_index);
        self.ir.get_cbuf_f32(binding, offset)
    }

    /// Get an F64 from a constant buffer (two 32-bit reads packed into F64).
    pub fn get_double_cbuf(&mut self, insn: u64) -> Value {
        let cb_index = field(insn, 34, 5);
        let cb_offset = field(insn, 20, 14) << 2;
        if cb_index >= 18 {
            panic!("Out of bounds constant buffer binding {cb_index}");
        }
        let unaligned = bit(insn, 20);
        let binding = Value::ImmU32(cb_index);
        self.ir.program.info.register_cbuf(cb_index);
        let upper_offset = if unaligned {
            cb_offset | 4
        } else {
            (cb_offset & !7) | 4
        };
        let hi = self.ir.get_cbuf_u32(binding, Value::ImmU32(upper_offset));
        let lo = if unaligned {
            Value::ImmU32(0)
        } else {
            self.ir
                .get_cbuf_u32(Value::ImmU32(cb_index), Value::ImmU32(cb_offset))
        };
        let vec = self.ir.composite_construct_u32x2(lo, hi);
        self.ir.pack_double_2x32(vec)
    }

    /// Get a sign-extended 20-bit immediate as U32 (GetImm20 upstream).
    pub fn get_imm20(&self, insn: u64) -> Value {
        let value = field(insn, 20, 19);
        let value = if bit(insn, 56) {
            value.wrapping_add((-(1i32 << 19)) as u32)
        } else {
            value
        };
        Value::ImmU32(value)
    }

    /// Get the I2F packed 64-bit immediate.
    ///
    /// Corresponds to upstream `TranslatorVisitor::GetPackedImm20`.
    pub fn get_packed_imm20(&self, insn: u64) -> Value {
        let Value::ImmU32(value) = self.get_imm20(insn) else {
            unreachable!("GetImm20 always returns an immediate")
        };
        Value::ImmU64((value as u64) << 32)
    }

    /// Get a 20-bit immediate as F32 (sign bit at bit 56, mantissa at [20:38]).
    pub fn get_float_imm20(&self, insn: u64) -> Value {
        let imm = field(insn, 20, 19) << 12;
        let sign = if bit(insn, 56) { 1u32 << 31 } else { 0 };
        Value::ImmF32(f32::from_bits(imm | sign))
    }

    /// Get a 20-bit double immediate as an IEEE-754 bit pattern.
    pub fn get_double_imm20(&self, insn: u64) -> Value {
        let value = (field(insn, 20, 19) as u64) << 44;
        let sign = if bit(insn, 56) { 1u64 << 63 } else { 0 };
        Value::ImmF64(f64::from_bits(value | sign))
    }

    /// Get a register value from bits [8:15] as F32 (GetFloatReg8 upstream).
    pub fn get_float_reg8(&mut self, insn: u64) -> Value {
        let idx = field(insn, 8, 8);
        self.f(idx)
    }

    /// Get a register value from bits [20:27] as F32 (GetFloatReg20 upstream).
    pub fn get_float_reg20(&mut self, insn: u64) -> Value {
        let idx = field(insn, 20, 8);
        self.f(idx)
    }

    /// Get a register value from bits [39:46] as F32 (GetFloatReg39 upstream).
    pub fn get_float_reg39(&mut self, insn: u64) -> Value {
        let idx = field(insn, 39, 8);
        self.f(idx)
    }

    /// Translate a single Maxwell instruction word.
    ///
    /// Corresponds to the dispatch table in upstream `impl.cpp`.
    pub fn translate_instruction(&mut self, insn: u64) {
        let opcode = super::decode::decode(insn);

        match opcode {
            // FP32 arithmetic — floating_point_add.cpp
            MaxwellOpcode::FADD_reg | MaxwellOpcode::FADD_cbuf | MaxwellOpcode::FADD_imm => {
                self::floating_point_add::fadd(self, insn, opcode);
            }
            MaxwellOpcode::FADD32I => {
                self::floating_point_add::fadd32i(self, insn);
            }

            // floating_point_multiply.cpp
            MaxwellOpcode::FMUL_reg | MaxwellOpcode::FMUL_cbuf | MaxwellOpcode::FMUL_imm => {
                self::floating_point_multiply::fmul(self, insn, opcode);
            }
            MaxwellOpcode::FMUL32I => {
                self::floating_point_multiply::fmul32i(self, insn);
            }

            // double_add.cpp
            MaxwellOpcode::DADD_reg => self::double_add::dadd_reg(self, insn),
            MaxwellOpcode::DADD_cbuf => self::double_add::dadd_cbuf(self, insn),
            MaxwellOpcode::DADD_imm => self::double_add::dadd_imm(self, insn),

            // double_multiply.cpp
            MaxwellOpcode::DMUL_reg => self::double_multiply::dmul_reg(self, insn),
            MaxwellOpcode::DMUL_cbuf => self::double_multiply::dmul_cbuf(self, insn),
            MaxwellOpcode::DMUL_imm => self::double_multiply::dmul_imm(self, insn),

            // floating_point_fused_multiply_add.cpp
            MaxwellOpcode::FFMA_reg
            | MaxwellOpcode::FFMA_rc
            | MaxwellOpcode::FFMA_cr
            | MaxwellOpcode::FFMA_imm => {
                self::floating_point_fused_multiply_add::ffma(self, insn, opcode);
            }
            MaxwellOpcode::FFMA32I => {
                self::floating_point_fused_multiply_add::ffma32i(self, insn);
            }

            // double_fused_multiply_add.cpp
            MaxwellOpcode::DFMA_reg => self::double_fused_multiply_add::dfma_reg(self, insn),
            MaxwellOpcode::DFMA_rc => self::double_fused_multiply_add::dfma_rc(self, insn),
            MaxwellOpcode::DFMA_cr => self::double_fused_multiply_add::dfma_cr(self, insn),
            MaxwellOpcode::DFMA_imm => self::double_fused_multiply_add::dfma_imm(self, insn),

            // floating_point_min_max.cpp
            MaxwellOpcode::FMNMX_reg | MaxwellOpcode::FMNMX_cbuf | MaxwellOpcode::FMNMX_imm => {
                self::floating_point_min_max::fmnmx(self, insn, opcode);
            }

            // double_min_max.cpp
            MaxwellOpcode::DMNMX_reg => self::double_min_max::dmnmx_reg(self, insn),
            MaxwellOpcode::DMNMX_cbuf => self::double_min_max::dmnmx_cbuf(self, insn),
            MaxwellOpcode::DMNMX_imm => self::double_min_max::dmnmx_imm(self, insn),

            // floating_point_swizzled_add.cpp
            MaxwellOpcode::FSWZADD => self::floating_point_swizzled_add::fswzadd(self, insn),

            // floating_point_multi_function.cpp
            MaxwellOpcode::MUFU => {
                self::floating_point_multi_function::mufu(self, insn);
            }

            // half_floating_point_add.cpp
            MaxwellOpcode::HADD2_reg => {
                self::half_floating_point_add::hadd2_reg(self, insn);
            }
            MaxwellOpcode::HADD2_cbuf => {
                self::half_floating_point_add::hadd2_cbuf(self, insn);
            }
            MaxwellOpcode::HADD2_imm => {
                self::half_floating_point_add::hadd2_imm(self, insn);
            }
            MaxwellOpcode::HADD2_32I => {
                self::half_floating_point_add::hadd2_32i(self, insn);
            }

            // half_floating_point_fused_multiply_add.cpp
            MaxwellOpcode::HFMA2_reg => {
                self::half_floating_point_fused_multiply_add::hfma2_reg(self, insn);
            }
            MaxwellOpcode::HFMA2_rc => {
                self::half_floating_point_fused_multiply_add::hfma2_rc(self, insn);
            }
            MaxwellOpcode::HFMA2_cr => {
                self::half_floating_point_fused_multiply_add::hfma2_cr(self, insn);
            }
            MaxwellOpcode::HFMA2_imm => {
                self::half_floating_point_fused_multiply_add::hfma2_imm(self, insn);
            }
            MaxwellOpcode::HFMA2_32I => {
                self::half_floating_point_fused_multiply_add::hfma2_32i(self, insn);
            }

            // half_floating_point_multiply.cpp
            MaxwellOpcode::HMUL2_reg => {
                self::half_floating_point_multiply::hmul2_reg(self, insn);
            }
            MaxwellOpcode::HMUL2_cbuf => {
                self::half_floating_point_multiply::hmul2_cbuf(self, insn);
            }
            MaxwellOpcode::HMUL2_imm => {
                self::half_floating_point_multiply::hmul2_imm(self, insn);
            }
            MaxwellOpcode::HMUL2_32I => {
                self::half_floating_point_multiply::hmul2_32i(self, insn);
            }

            // half_floating_point_set.cpp
            MaxwellOpcode::HSET2_reg => {
                self::half_floating_point_set::hset2_reg(self, insn);
            }
            MaxwellOpcode::HSET2_cbuf => {
                self::half_floating_point_set::hset2_cbuf(self, insn);
            }
            MaxwellOpcode::HSET2_imm => {
                self::half_floating_point_set::hset2_imm(self, insn);
            }

            // half_floating_point_set_predicate.cpp
            MaxwellOpcode::HSETP2_reg => {
                self::half_floating_point_set_predicate::hsetp2_reg(self, insn);
            }
            MaxwellOpcode::HSETP2_cbuf => {
                self::half_floating_point_set_predicate::hsetp2_cbuf(self, insn);
            }
            MaxwellOpcode::HSETP2_imm => {
                self::half_floating_point_set_predicate::hsetp2_imm(self, insn);
            }

            // floating_point_range_reduction.cpp
            MaxwellOpcode::RRO_reg | MaxwellOpcode::RRO_cbuf => {
                self::floating_point_range_reduction::rro(self, insn, opcode);
            }
            MaxwellOpcode::RRO_imm => {
                self::floating_point_range_reduction::rro(self, insn, opcode);
            }

            // integer_add.cpp
            MaxwellOpcode::IADD_reg | MaxwellOpcode::IADD_cbuf | MaxwellOpcode::IADD_imm => {
                self::integer_add::iadd(self, insn, opcode);
            }
            MaxwellOpcode::IADD32I => {
                self::integer_add::iadd32i(self, insn);
            }

            // integer_add_three_input.cpp
            MaxwellOpcode::IADD3_reg | MaxwellOpcode::IADD3_cbuf | MaxwellOpcode::IADD3_imm => {
                self::integer_add_three_input::iadd3(self, insn, opcode);
            }

            // not_implemented.cpp
            MaxwellOpcode::IMAD_reg => self.translate_imad_reg(insn),
            MaxwellOpcode::IMAD_rc => self.translate_imad_rc(insn),
            MaxwellOpcode::IMAD_cr => self.translate_imad_cr(insn),
            MaxwellOpcode::IMAD_imm => self.translate_imad_imm(insn),
            MaxwellOpcode::IMAD32I => self.translate_imad32i(insn),

            // integer_short_multiply_add.cpp
            MaxwellOpcode::XMAD_reg
            | MaxwellOpcode::XMAD_rc
            | MaxwellOpcode::XMAD_cr
            | MaxwellOpcode::XMAD_imm => {
                self::integer_short_multiply_add::xmad(self, insn, opcode);
            }

            // integer_scaled_add.cpp
            MaxwellOpcode::ISCADD_reg | MaxwellOpcode::ISCADD_cbuf | MaxwellOpcode::ISCADD_imm => {
                self::integer_scaled_add::iscadd(self, insn, opcode);
            }
            MaxwellOpcode::ISCADD32I => {
                self::integer_scaled_add::iscadd32i(self, insn);
            }

            // integer_minimum_maximum.cpp
            MaxwellOpcode::IMNMX_reg | MaxwellOpcode::IMNMX_cbuf | MaxwellOpcode::IMNMX_imm => {
                self::integer_minimum_maximum::imnmx(self, insn, opcode);
            }

            // floating_point_set_predicate.cpp
            MaxwellOpcode::FSETP_reg | MaxwellOpcode::FSETP_cbuf | MaxwellOpcode::FSETP_imm => {
                self::floating_point_set_predicate::fsetp(self, insn, opcode);
            }

            // integer_set_predicate.cpp
            MaxwellOpcode::ISETP_reg | MaxwellOpcode::ISETP_cbuf | MaxwellOpcode::ISETP_imm => {
                self::integer_set_predicate::isetp(self, insn, opcode);
            }

            // floating_point_compare_and_set.cpp
            MaxwellOpcode::FSET_reg | MaxwellOpcode::FSET_cbuf | MaxwellOpcode::FSET_imm => {
                self::floating_point_compare_and_set::fset(self, insn, opcode);
            }

            // floating_point_compare.cpp
            MaxwellOpcode::FCMP_reg => self::floating_point_compare::fcmp_reg(self, insn),
            MaxwellOpcode::FCMP_rc => self::floating_point_compare::fcmp_rc(self, insn),
            MaxwellOpcode::FCMP_cr => self::floating_point_compare::fcmp_cr(self, insn),
            MaxwellOpcode::FCMP_imm => self::floating_point_compare::fcmp_imm(self, insn),

            // integer_compare_and_set.cpp
            MaxwellOpcode::ISET_reg | MaxwellOpcode::ISET_cbuf | MaxwellOpcode::ISET_imm => {
                self::integer_compare_and_set::iset(self, insn, opcode);
            }

            // integer_compare.cpp
            MaxwellOpcode::ICMP_reg => self::integer_compare::icmp_reg(self, insn),
            MaxwellOpcode::ICMP_rc => self::integer_compare::icmp_rc(self, insn),
            MaxwellOpcode::ICMP_cr => self::integer_compare::icmp_cr(self, insn),
            MaxwellOpcode::ICMP_imm => self::integer_compare::icmp_imm(self, insn),

            // double_compare_and_set.cpp
            MaxwellOpcode::DSET_reg => self::double_compare_and_set::dset_reg(self, insn),
            MaxwellOpcode::DSET_cbuf => self::double_compare_and_set::dset_cbuf(self, insn),
            MaxwellOpcode::DSET_imm => self::double_compare_and_set::dset_imm(self, insn),

            // double_set_predicate.cpp
            MaxwellOpcode::DSETP_reg => self::double_set_predicate::dsetp_reg(self, insn),
            MaxwellOpcode::DSETP_cbuf => self::double_set_predicate::dsetp_cbuf(self, insn),
            MaxwellOpcode::DSETP_imm => self::double_set_predicate::dsetp_imm(self, insn),

            // floating_point_conversion_integer.cpp
            MaxwellOpcode::F2I_reg | MaxwellOpcode::F2I_cbuf | MaxwellOpcode::F2I_imm => {
                self::floating_point_conversion_integer::f2i(self, insn, opcode);
            }

            // integer_floating_point_conversion.cpp
            MaxwellOpcode::I2F_reg | MaxwellOpcode::I2F_cbuf | MaxwellOpcode::I2F_imm => {
                self::integer_floating_point_conversion::i2f(self, insn, opcode);
            }

            // floating_point_conversion_floating_point.cpp
            MaxwellOpcode::F2F_reg => {
                self::floating_point_conversion_floating_point::f2f_reg(self, insn);
            }
            MaxwellOpcode::F2F_cbuf => {
                self::floating_point_conversion_floating_point::f2f_cbuf(self, insn);
            }
            MaxwellOpcode::F2F_imm => {
                self::floating_point_conversion_floating_point::f2f_imm(self, insn);
            }

            // integer_to_integer_conversion.cpp
            MaxwellOpcode::I2I_reg | MaxwellOpcode::I2I_cbuf | MaxwellOpcode::I2I_imm => {
                self::integer_to_integer_conversion::i2i(self, insn, opcode);
            }

            // logic_operation.cpp
            MaxwellOpcode::LOP_reg | MaxwellOpcode::LOP_cbuf | MaxwellOpcode::LOP_imm => {
                self::logic_operation::lop(self, insn, opcode);
            }
            MaxwellOpcode::LOP32I => {
                self::logic_operation::lop32i(self, insn);
            }

            // logic_operation_three_input.cpp
            MaxwellOpcode::LOP3_reg | MaxwellOpcode::LOP3_cbuf | MaxwellOpcode::LOP3_imm => {
                self::logic_operation_three_input::lop3(self, insn, opcode);
            }

            // integer_shift_left.cpp
            MaxwellOpcode::SHL_reg | MaxwellOpcode::SHL_cbuf | MaxwellOpcode::SHL_imm => {
                self::integer_shift_left::shl(self, insn, opcode);
            }

            // integer_shift_right.cpp
            MaxwellOpcode::SHR_reg | MaxwellOpcode::SHR_cbuf | MaxwellOpcode::SHR_imm => {
                self::integer_shift_right::shr(self, insn, opcode);
            }

            // integer_funnel_shift.cpp
            MaxwellOpcode::SHF_l_reg => self::integer_funnel_shift::shf_l_reg(self, insn),
            MaxwellOpcode::SHF_l_imm => self::integer_funnel_shift::shf_l_imm(self, insn),
            MaxwellOpcode::SHF_r_reg => self::integer_funnel_shift::shf_r_reg(self, insn),
            MaxwellOpcode::SHF_r_imm => self::integer_funnel_shift::shf_r_imm(self, insn),

            // bitfield_extract.cpp
            MaxwellOpcode::BFE_reg | MaxwellOpcode::BFE_cbuf | MaxwellOpcode::BFE_imm => {
                self::bitfield_extract::bfe(self, insn, opcode);
            }

            // bitfield_insert.cpp
            MaxwellOpcode::BFI_reg
            | MaxwellOpcode::BFI_rc
            | MaxwellOpcode::BFI_cr
            | MaxwellOpcode::BFI_imm => {
                self::bitfield_insert::bfi(self, insn, opcode);
            }

            // integer_popcount.cpp
            MaxwellOpcode::POPC_reg | MaxwellOpcode::POPC_cbuf | MaxwellOpcode::POPC_imm => {
                self::integer_popcount::popc(self, insn, opcode);
            }

            // find_leading_one.cpp
            MaxwellOpcode::FLO_reg | MaxwellOpcode::FLO_cbuf | MaxwellOpcode::FLO_imm => {
                self::find_leading_one::flo(self, insn, opcode);
            }

            // move_register.cpp
            MaxwellOpcode::MOV_reg | MaxwellOpcode::MOV_cbuf | MaxwellOpcode::MOV_imm => {
                self::move_register::mov(self, insn, opcode);
            }
            MaxwellOpcode::MOV32I => {
                self::move_register::mov32i(self, insn);
            }

            // select_source_with_predicate.cpp
            MaxwellOpcode::SEL_reg | MaxwellOpcode::SEL_cbuf | MaxwellOpcode::SEL_imm => {
                self::select_source_with_predicate::sel(self, insn, opcode);
            }

            // move_special_register.cpp
            MaxwellOpcode::S2R => {
                self::move_special_register::s2r(self, insn);
            }

            // move_predicate_to_register.cpp
            MaxwellOpcode::P2R_reg => self::move_predicate_to_register::p2r_reg(self, insn),
            MaxwellOpcode::P2R_cbuf => self::move_predicate_to_register::p2r_cbuf(self, insn),
            MaxwellOpcode::P2R_imm => self::move_predicate_to_register::p2r_imm(self, insn),

            // move_register_to_predicate.cpp
            MaxwellOpcode::R2P_reg => self::move_register_to_predicate::r2p_reg(self, insn),
            MaxwellOpcode::R2P_cbuf => self::move_register_to_predicate::r2p_cbuf(self, insn),
            MaxwellOpcode::R2P_imm => self::move_register_to_predicate::r2p_imm(self, insn),

            // attribute_memory_to_physical.cpp
            MaxwellOpcode::AL2P => self::attribute_memory_to_physical::al2p(self, insn),

            // internal_stage_buffer_entry_read.cpp
            MaxwellOpcode::ISBERD => self.translate_isberd(insn),

            // pixel_load.cpp
            MaxwellOpcode::PIXLD => self.translate_pixld(insn),

            // vote.cpp / warp_shuffle.cpp
            MaxwellOpcode::VOTE => self.translate_vote(insn),
            MaxwellOpcode::VOTE_vtg => self.translate_vote_vtg(insn),
            MaxwellOpcode::SHFL => self::warp_shuffle::shfl(self, insn),

            // predicate_set_predicate.cpp
            MaxwellOpcode::PSETP => {
                self::predicate_set_predicate::psetp(self, insn);
            }

            // predicate_set_register.cpp
            MaxwellOpcode::PSET => {
                self::predicate_set_register::pset(self, insn);
            }

            // condition_code_set.cpp
            MaxwellOpcode::CSET => {
                self::condition_code_set::cset(self, insn);
            }
            MaxwellOpcode::CSETP => {
                self::condition_code_set::csetp(self, insn);
            }

            // load_effective_address.cpp
            MaxwellOpcode::LEA_hi_reg => {
                self::load_effective_address::lea_hi_reg(self, insn);
            }
            MaxwellOpcode::LEA_hi_cbuf => {
                self::load_effective_address::lea_hi_cbuf(self, insn);
            }
            MaxwellOpcode::LEA_lo_reg => {
                self::load_effective_address::lea_lo_reg(self, insn);
            }
            MaxwellOpcode::LEA_lo_cbuf => {
                self::load_effective_address::lea_lo_cbuf(self, insn);
            }
            MaxwellOpcode::LEA_lo_imm => {
                self::load_effective_address::lea_lo_imm(self, insn);
            }

            // load_store_memory.cpp
            MaxwellOpcode::LDG => {
                self::load_store_memory::ldg(self, insn);
            }
            MaxwellOpcode::STG => {
                self::load_store_memory::stg(self, insn);
            }

            MaxwellOpcode::LDS => self::load_store_local_shared::lds(self, insn),
            MaxwellOpcode::STS => self::load_store_local_shared::sts(self, insn),

            // atomic_operations_global_memory.cpp
            MaxwellOpcode::ATOM => {
                self::atomic_operations_global_memory::atom(self, insn);
            }
            MaxwellOpcode::RED => {
                self::atomic_operations_global_memory::red(self, insn);
            }
            MaxwellOpcode::ATOM_cas => {
                self.translate_atom_cas(insn);
            }

            // atomic_operations_shared_memory.cpp
            MaxwellOpcode::ATOMS => {
                self::atomic_operations_shared_memory::atoms(self, insn);
            }
            MaxwellOpcode::ATOMS_cas => {
                self.translate_atoms_cas(insn);
            }

            // surface_load_store.cpp / surface_atomic_operations.cpp
            MaxwellOpcode::SULD => self::surface_load_store::suld(self, insn),
            MaxwellOpcode::SUST => self::surface_load_store::sust(self, insn),
            MaxwellOpcode::SUATOM => self::surface_atomic_operations::suatom(self, insn),
            MaxwellOpcode::SURED => self::surface_atomic_operations::sured(self, insn),

            // load_constant.cpp
            MaxwellOpcode::LDC => {
                self::load_constant::ldc(self, insn);
            }

            // load_store_local_shared.cpp
            MaxwellOpcode::LDL => {
                self::load_store_local_shared::ldl(self, insn);
            }
            MaxwellOpcode::STL => {
                self::load_store_local_shared::stl(self, insn);
            }

            // texture_fetch.cpp
            MaxwellOpcode::TEX => self::texture_fetch::tex(self, insn, opcode),
            MaxwellOpcode::TEX_b => self::texture_fetch::tex_b(self, insn, opcode),

            // texture_fetch_swizzled.cpp
            MaxwellOpcode::TEXS => {
                self::texture_fetch_swizzled::texs(self, insn);
            }

            // texture_load.cpp
            MaxwellOpcode::TLD => self::texture_load::tld(self, insn, opcode),
            MaxwellOpcode::TLD_b => self::texture_load::tld_b(self, insn, opcode),

            // texture_load_swizzled.cpp
            MaxwellOpcode::TLDS => {
                self::texture_load_swizzled::tlds(self, insn);
            }

            // texture_gather.cpp
            MaxwellOpcode::TLD4 => {
                self::texture_gather::tld4(self, insn, opcode);
            }
            MaxwellOpcode::TLD4_b => {
                self::texture_gather::tld4_b(self, insn, opcode);
            }
            MaxwellOpcode::TLD4S => self::texture_gather_swizzled::tld4s(self, insn),

            // texture_gradient.cpp
            MaxwellOpcode::TXD => self::texture_gradient::txd(self, insn),
            MaxwellOpcode::TXD_b => self::texture_gradient::txd_b(self, insn),

            // texture_mipmap_level.cpp
            MaxwellOpcode::TMML => self::texture_mipmap_level::tmml(self, insn),
            MaxwellOpcode::TMML_b => self::texture_mipmap_level::tmml_b(self, insn),

            // texture_query.cpp
            MaxwellOpcode::TXQ => self::texture_query::txq(self, insn, opcode),
            MaxwellOpcode::TXQ_b => self::texture_query::txq_b(self, insn, opcode),

            // load_store_attribute.cpp
            MaxwellOpcode::ALD => {
                self::load_store_attribute::ald(self, insn);
            }
            MaxwellOpcode::AST => {
                self::load_store_attribute::ast(self, insn);
            }
            MaxwellOpcode::IPA => {
                self::load_store_attribute::ipa(self, insn);
            }

            // output_geometry.cpp
            MaxwellOpcode::OUT_reg => self::output_geometry::out_reg(self, insn),
            MaxwellOpcode::OUT_cbuf => self::output_geometry::out_cbuf(self, insn),
            MaxwellOpcode::OUT_imm => self::output_geometry::out_imm(self, insn),

            // video_minimum_maximum.cpp / video_multiply_add.cpp /
            // video_set_predicate.cpp
            MaxwellOpcode::VMNMX => self::video_minimum_maximum::vmnmx(self, insn),
            MaxwellOpcode::VMAD => self::video_multiply_add::vmad(self, insn),
            MaxwellOpcode::VSETP => self::video_set_predicate::vsetp(self, insn),

            // barrier_operations.cpp
            MaxwellOpcode::BAR => self::barrier_operations::bar(self, insn),

            // branch_indirect.cpp
            MaxwellOpcode::BRX => self::branch_indirect::brx(self, insn),
            MaxwellOpcode::JMX => self::branch_indirect::jmx(self, insn),

            // not_implemented.cpp: upstream rejects these opcodes explicitly.
            MaxwellOpcode::B2R => self.translate_b2r(insn),
            MaxwellOpcode::BPT => self.translate_bpt(insn),
            MaxwellOpcode::CCTL => self.translate_cctl(insn),
            MaxwellOpcode::CCTLL => self.translate_cctll(insn),
            MaxwellOpcode::CCTLT => self.translate_cctlt(insn),
            MaxwellOpcode::CS2R => self.translate_cs2r(insn),
            MaxwellOpcode::FCHK_reg => self.translate_fchk_reg(insn),
            MaxwellOpcode::FCHK_cbuf => self.translate_fchk_cbuf(insn),
            MaxwellOpcode::FCHK_imm => self.translate_fchk_imm(insn),
            MaxwellOpcode::GETCRSPTR => self.translate_getcrsptr(insn),
            MaxwellOpcode::GETLMEMBASE => self.translate_getlmembase(insn),
            MaxwellOpcode::IDE => self.translate_ide(insn),
            MaxwellOpcode::IDP_reg => self.translate_idp_reg(insn),
            MaxwellOpcode::IDP_imm => self.translate_idp_imm(insn),
            MaxwellOpcode::IMADSP_reg => self.translate_imadsp_reg(insn),
            MaxwellOpcode::IMADSP_rc => self.translate_imadsp_rc(insn),
            MaxwellOpcode::IMADSP_cr => self.translate_imadsp_cr(insn),
            MaxwellOpcode::IMADSP_imm => self.translate_imadsp_imm(insn),
            MaxwellOpcode::IMUL_reg => self.translate_imul_reg(insn),
            MaxwellOpcode::IMUL_cbuf => self.translate_imul_cbuf(insn),
            MaxwellOpcode::IMUL_imm => self.translate_imul_imm(insn),
            MaxwellOpcode::IMUL32I => self.translate_imul32i(insn),
            MaxwellOpcode::JCAL => self.translate_jcal(insn),
            MaxwellOpcode::JMP => self.translate_jmp(insn),
            MaxwellOpcode::LD => self.translate_ld(insn),
            MaxwellOpcode::LEPC => self.translate_lepc(insn),
            MaxwellOpcode::LONGJMP => self.translate_longjmp(insn),
            MaxwellOpcode::PLONGJMP => self.translate_plongjmp(insn),
            MaxwellOpcode::PRET => self.translate_pret(insn),
            MaxwellOpcode::PRMT_reg => self.translate_prmt_reg(insn),
            MaxwellOpcode::PRMT_rc => self.translate_prmt_rc(insn),
            MaxwellOpcode::PRMT_cr => self.translate_prmt_cr(insn),
            MaxwellOpcode::PRMT_imm => self.translate_prmt_imm(insn),
            MaxwellOpcode::R2B => self.translate_r2b(insn),
            MaxwellOpcode::RTT => self.translate_rtt(insn),
            MaxwellOpcode::SETCRSPTR => self.translate_setcrsptr(insn),
            MaxwellOpcode::SETLMEMBASE => self.translate_setlmembase(insn),
            MaxwellOpcode::ST => self.translate_st(insn),
            MaxwellOpcode::STP => self.translate_stp(insn),
            MaxwellOpcode::SUATOM_cas => self.translate_suatom_cas(insn),
            MaxwellOpcode::TXA => self.translate_txa(insn),
            MaxwellOpcode::VABSDIFF => self.translate_vabsdiff(insn),
            MaxwellOpcode::VABSDIFF4 => self.translate_vabsdiff4(insn),
            MaxwellOpcode::VADD => self.translate_vadd(insn),
            MaxwellOpcode::VSET => self.translate_vset(insn),
            MaxwellOpcode::VSHL => self.translate_vshl(insn),
            MaxwellOpcode::VSHR => self.translate_vshr(insn),

            // not_implemented.cpp: upstream deliberately logs and continues.
            MaxwellOpcode::RAM => self.translate_ram(insn),
            MaxwellOpcode::SAM => self.translate_sam(insn),

            // Control flow. The CFG builder owns block/successor structure, but
            // EXIT still has stage-specific side effects upstream: fragment
            // shaders write their color/depth/sample outputs in
            // `TranslatorVisitor::EXIT()` before control returns.
            MaxwellOpcode::EXIT => {
                self.translate_exit(insn);
            }

            // Control-flow structure is owned by the CFG builder. These calls
            // preserve upstream behavior if a control instruction reaches the
            // instruction translator directly.
            MaxwellOpcode::BRA => self.translate_bra(insn),
            MaxwellOpcode::BRK => self.translate_brk(insn),
            MaxwellOpcode::CONT => self.translate_cont(insn),
            MaxwellOpcode::SYNC => self.translate_sync(insn),
            MaxwellOpcode::PEXIT => self.translate_pexit(insn),
            MaxwellOpcode::RET => self.translate_ret(insn),
            MaxwellOpcode::SSY => self.translate_ssy(insn),
            MaxwellOpcode::PBK => self.translate_pbk(insn),
            MaxwellOpcode::PCNT => self.translate_pcnt(insn),
            MaxwellOpcode::CAL => self.translate_cal(insn),

            MaxwellOpcode::NOP => self.translate_nop(insn),
            MaxwellOpcode::DEPBAR => self::barrier_operations::depbar(self),

            // Kill. Upstream `TranslatorVisitor::KIL()` is a no-op; the
            // structured control-flow pass owns demote/discard insertion.
            MaxwellOpcode::KIL => self.translate_kil(insn),

            // barrier_operations.cpp
            MaxwellOpcode::MEMBAR => self::barrier_operations::membar(self, insn),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::basic_block::Block;
    use crate::ir::opcodes::Opcode;
    use crate::ir::program::Program;
    use crate::ir::types::ShaderStage;

    #[test]
    fn kil_translate_is_noop_like_upstream() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let mut tv = TranslatorVisitor::new(&mut program, 0);

        tv.translate_instruction(0xe330_0000_0000_0000);

        let opcodes: Vec<_> = tv.ir.program.blocks[0]
            .iter()
            .map(|inst| inst.opcode)
            .collect();
        assert!(!opcodes.contains(&Opcode::DemoteToHelperInvocation));
        assert!(!tv.ir.program.info.uses_demote_to_helper_invocation);
    }

    #[test]
    fn invalid_encoding_falls_back_to_nop_like_upstream() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        let mut tv = TranslatorVisitor::new(&mut program, 0);

        tv.translate_instruction(0);

        assert!(tv.ir.program.blocks[0].is_empty());
    }

    #[test]
    fn regression_i2i_cc_feeds_csetp_through_ssa() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        {
            let mut tv = TranslatorVisitor::new(&mut program, 0);
            tv.translate_instruction(0x5CE0_8000_0027_0AFF);
            tv.translate_instruction(0x50A0_0380_0007_0D1F);
        }

        let before_ssa: Vec<_> = program.blocks[0].iter().map(|inst| inst.opcode).collect();
        assert!(before_ssa.contains(&Opcode::SetZFlag));
        assert!(before_ssa.contains(&Opcode::SetSFlag));
        assert!(before_ssa.contains(&Opcode::GetZFlag));
        assert!(before_ssa.contains(&Opcode::GetSFlag));
        assert!(before_ssa.contains(&Opcode::SetPred));

        crate::ir_opt::ssa_rewrite_pass::ssa_rewrite_pass(&mut program);
        crate::ir_opt::dead_code_elimination_pass::dead_code_elimination_pass(&mut program);
        let after_ssa: Vec<_> = program.blocks[0].iter().map(|inst| inst.opcode).collect();
        for flag_op in [
            Opcode::SetZFlag,
            Opcode::SetSFlag,
            Opcode::SetCFlag,
            Opcode::SetOFlag,
            Opcode::GetZFlag,
            Opcode::GetSFlag,
            Opcode::GetCFlag,
            Opcode::GetOFlag,
        ] {
            assert!(!after_ssa.contains(&flag_op));
        }
    }

    #[test]
    fn regression_flare_rro_reg_is_dispatched() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        let mut tv = TranslatorVisitor::new(&mut program, 0);

        tv.translate_instruction(0x5C90_0000_0037_0003);

        let opcodes: Vec<_> = tv.ir.program.blocks[0]
            .iter()
            .map(|inst| inst.opcode)
            .collect();
        assert!(opcodes.contains(&Opcode::GetRegister));
        assert!(opcodes.contains(&Opcode::SetRegister));
    }

    #[test]
    fn hmul2_reg_is_dispatched_to_half_multiply() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let mut tv = TranslatorVisitor::new(&mut program, 0);

        tv.translate_instruction(0x5D0B_0000_1037_0403);

        let opcodes: Vec<_> = tv.ir.program.blocks[0]
            .iter()
            .map(|inst| inst.opcode)
            .collect();
        assert!(opcodes.contains(&Opcode::FPMul32));
        assert!(opcodes.contains(&Opcode::SetRegister));
    }

    #[test]
    fn instruction_translation_leaves_execution_predication_to_cfg() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        let mut tv = TranslatorVisitor::new(&mut program, 0);

        // RRO with execution predicate P0. Upstream Translate invokes the
        // instruction implementation directly; CFG::AnalyzeCondInst owns P0.
        tv.translate_instruction(0x5C90_0000_0030_0003);

        let opcodes: Vec<_> = tv.ir.program.blocks[0]
            .iter()
            .map(|inst| inst.opcode)
            .collect();
        assert!(opcodes.contains(&Opcode::SetRegister));
        assert!(!opcodes.contains(&Opcode::SelectU32));
    }

    #[test]
    fn regression_attract_xmad_cbcc_ports_half_and_psl_semantics() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        let mut tv = TranslatorVisitor::new(&mut program, 0);

        tv.translate_instruction(0x5B30_0798_00C7_0D1D);

        let opcodes: Vec<_> = tv.ir.program.blocks[0]
            .iter()
            .map(|inst| inst.opcode)
            .collect();
        assert!(
            opcodes
                .iter()
                .filter(|&&op| op == Opcode::BitFieldUExtract)
                .count()
                >= 2
        );
        assert!(opcodes.contains(&Opcode::ShiftLeftLogical32));
        assert!(opcodes.contains(&Opcode::IMul32));
        assert!(opcodes.iter().filter(|&&op| op == Opcode::IAdd32).count() >= 2);
        assert!(opcodes.contains(&Opcode::SetRegister));
    }

    #[test]
    fn unaligned_double_cbuf_zeroes_the_lower_word() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        let mut tv = TranslatorVisitor::new(&mut program, 0);

        let _ = tv.get_double_cbuf(1u64 << 20);

        let instructions: Vec<_> = tv.ir.program.blocks[0].iter().collect();
        assert_eq!(
            instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::GetCbufU32)
                .count(),
            1
        );
        let construct = instructions
            .iter()
            .find(|inst| inst.opcode == Opcode::CompositeConstructU32x2)
            .expect("GetDoubleCbuf must construct a pair");
        assert_eq!(construct.args[0], Value::ImmU32(0));
    }

    #[test]
    fn tex_b_dispatch_uses_bindless_field_layout() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let mut tv = TranslatorVisitor::new(&mut program, 0);

        // Bit 58 is set while bit 40 is clear. TEX would interpret this as
        // unsupported LC, whereas TEX_b reads LC from bit 40 like upstream.
        tv.translate_instruction(0xDEBA_0000_A0E7_0807);

        let opcodes: Vec<_> = tv.ir.program.blocks[0]
            .iter()
            .map(|inst| inst.opcode)
            .collect();
        assert!(opcodes.iter().any(|opcode| matches!(
            opcode,
            Opcode::BindlessImageSampleImplicitLod
                | Opcode::BindlessImageSampleExplicitLod
                | Opcode::BindlessImageSampleDrefImplicitLod
                | Opcode::BindlessImageSampleDrefExplicitLod
        )));
        assert!(!opcodes.iter().any(|opcode| matches!(
            opcode,
            Opcode::BoundImageSampleImplicitLod
                | Opcode::BoundImageSampleExplicitLod
                | Opcode::BoundImageSampleDrefImplicitLod
                | Opcode::BoundImageSampleDrefExplicitLod
        )));
    }

    #[test]
    fn tld_b_dispatch_emits_bindless_fetch() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let mut tv = TranslatorVisitor::new(&mut program, 0);
        let insn =
            0xDD00_0000_0000_0000 | 1 | (8u64 << 8) | (20u64 << 20) | (2u64 << 28) | (1u64 << 31);

        tv.translate_instruction(insn);

        let opcodes: Vec<_> = tv.ir.program.blocks[0]
            .iter()
            .map(|inst| inst.opcode)
            .collect();
        assert!(opcodes.contains(&Opcode::BindlessImageFetch));
        assert!(!opcodes.contains(&Opcode::BoundImageFetch));
    }

    #[test]
    fn txq_b_dispatch_emits_bindless_query() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let mut tv = TranslatorVisitor::new(&mut program, 0);
        let insn = 0xDF50_0000_0000_0000 | 4 | (8u64 << 8) | (1u64 << 22) | (8u64 << 31);

        tv.translate_instruction(insn);

        let opcodes: Vec<_> = tv.ir.program.blocks[0]
            .iter()
            .map(|inst| inst.opcode)
            .collect();
        assert!(opcodes.contains(&Opcode::BindlessImageQueryDimensions));
        assert!(!opcodes.contains(&Opcode::BoundImageQueryDimensions));
    }

    #[test]
    fn invalid_opcode_uses_upstream_nop_fallback() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        let mut tv = TranslatorVisitor::new(&mut program, 0);
        tv.translate_instruction(0);

        assert!(tv.ir.program.blocks[0].is_empty());
    }
}

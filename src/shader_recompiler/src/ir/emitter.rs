// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! IR Emitter — builder interface for constructing IR instructions.
//!
//! Matches zuyu's `IREmitter` from `ir_emitter.h`. Provides typed methods for
//! emitting IR instructions with correct argument types.

use super::instruction::Inst;
use super::opcodes::Opcode;
use super::program::Program;
use super::types::FpControl;
use super::value::{Attribute, InstRef, Patch, Pred, Reg, Value};
use super::{Condition, FlowTest};

/// IR builder that emits instructions into a specific block.
pub struct Emitter<'a> {
    pub program: &'a mut Program,
    pub block: u32,
}

impl<'a> Emitter<'a> {
    /// Create a new emitter targeting the given block.
    pub fn new(program: &'a mut Program, block: u32) -> Self {
        Self { program, block }
    }

    /// Change the target block.
    pub fn set_block(&mut self, block: u32) {
        self.block = block;
    }

    /// Emit a raw instruction and return a reference to its result.
    fn emit(&mut self, inst: Inst) -> Value {
        let inst_idx = self.program.block_mut(self.block).append_inst(inst);
        Value::Inst(InstRef {
            block: self.block,
            inst: inst_idx,
        })
    }

    fn emit_pseudo_from_op(&mut self, pseudo_op: Opcode, op: Value) -> Value {
        let pseudo = self.emit(Inst::new(pseudo_op, vec![op]));
        if let (Value::Inst(parent), Value::Inst(pseudo_ref)) = (op, pseudo) {
            self.program
                .block_mut(parent.block)
                .inst_mut(parent.inst)
                .set_associated_pseudo(pseudo_op, pseudo_ref);
        }
        pseudo
    }

    /// Emit a void instruction (no result value used).
    fn emit_void(&mut self, inst: Inst) {
        self.program.block_mut(self.block).append_inst(inst);
    }

    // ── Immediates ────────────────────────────────────────────────────

    pub fn imm_u1(&self, v: bool) -> Value {
        Value::ImmU1(v)
    }

    pub fn imm_u32(&self, v: u32) -> Value {
        Value::ImmU32(v)
    }

    pub fn imm_f32(&self, v: f32) -> Value {
        Value::ImmF32(v)
    }

    pub fn imm_u64(&self, v: u64) -> Value {
        Value::ImmU64(v)
    }

    pub fn imm_f64(&self, v: f64) -> Value {
        Value::ImmF64(v)
    }

    // ── Control / Meta ────────────────────────────────────────────────

    pub fn phi(&mut self) -> Value {
        self.emit(Inst::phi())
    }

    pub fn identity(&mut self, value: Value) -> Value {
        self.emit(Inst::new(Opcode::Identity, vec![value]))
    }

    pub fn void_inst(&mut self) {
        self.emit_void(Inst::new(Opcode::Void, vec![]));
    }

    pub fn condition_ref(&mut self, cond: Value) -> Value {
        self.emit(Inst::new(Opcode::ConditionRef, vec![cond]))
    }

    pub fn reference(&mut self, value: Value) {
        self.emit_void(Inst::new(Opcode::Reference, vec![value]));
    }

    pub fn prologue(&mut self) {
        self.emit_void(Inst::new(Opcode::Prologue, vec![]));
    }

    pub fn epilogue(&mut self) {
        self.emit_void(Inst::new(Opcode::Epilogue, vec![]));
    }

    pub fn demote_to_helper_invocation(&mut self) {
        self.emit_void(Inst::new(Opcode::DemoteToHelperInvocation, vec![]));
    }

    // ── Barriers ──────────────────────────────────────────────────────

    pub fn barrier(&mut self) {
        self.emit_void(Inst::new(Opcode::Barrier, vec![]));
    }

    pub fn workgroup_memory_barrier(&mut self) {
        self.emit_void(Inst::new(Opcode::WorkgroupMemoryBarrier, vec![]));
    }

    pub fn device_memory_barrier(&mut self) {
        self.emit_void(Inst::new(Opcode::DeviceMemoryBarrier, vec![]));
    }

    // ── Register / Predicate access ───────────────────────────────────

    pub fn get_reg(&mut self, reg: Reg) -> Value {
        self.emit(Inst::new(Opcode::GetRegister, vec![Value::Reg(reg)]))
    }

    pub fn set_reg(&mut self, reg: Reg, value: Value) {
        self.emit_void(Inst::new(Opcode::SetRegister, vec![Value::Reg(reg), value]));
    }

    pub fn get_pred(&mut self, pred: Pred, is_negated: bool) -> Value {
        if pred.is_true() {
            return Value::ImmU1(!is_negated);
        }
        let raw = self.emit(Inst::new(Opcode::GetPred, vec![Value::Pred(pred)]));
        if is_negated {
            self.logical_not(raw)
        } else {
            raw
        }
    }

    pub fn set_pred(&mut self, pred: Pred, value: Value) {
        if !pred.is_true() {
            self.emit_void(Inst::new(Opcode::SetPred, vec![Value::Pred(pred), value]));
        }
    }

    pub fn get_z_flag(&mut self) -> Value {
        self.emit(Inst::new(Opcode::GetZFlag, vec![]))
    }

    pub fn get_s_flag(&mut self) -> Value {
        self.emit(Inst::new(Opcode::GetSFlag, vec![]))
    }

    pub fn get_c_flag(&mut self) -> Value {
        self.emit(Inst::new(Opcode::GetCFlag, vec![]))
    }

    pub fn get_o_flag(&mut self) -> Value {
        self.emit(Inst::new(Opcode::GetOFlag, vec![]))
    }

    pub fn set_z_flag(&mut self, value: Value) {
        self.emit_void(Inst::new(Opcode::SetZFlag, vec![value]));
    }

    pub fn set_s_flag(&mut self, value: Value) {
        self.emit_void(Inst::new(Opcode::SetSFlag, vec![value]));
    }

    pub fn set_c_flag(&mut self, value: Value) {
        self.emit_void(Inst::new(Opcode::SetCFlag, vec![value]));
    }

    pub fn set_o_flag(&mut self, value: Value) {
        self.emit_void(Inst::new(Opcode::SetOFlag, vec![value]));
    }

    /// Upstream `IREmitter::GetFlowTestResult`.
    pub fn get_flow_test_result(&mut self, test: FlowTest) -> Value {
        match test {
            FlowTest::F => self.imm_u1(false),
            FlowTest::LT => {
                let s = self.get_s_flag();
                let z = self.get_z_flag();
                let not_z = self.logical_not(z);
                let s_and_not_z = self.logical_and(s, not_z);
                let o = self.get_o_flag();
                self.logical_xor(s_and_not_z, o)
            }
            FlowTest::EQ => {
                let s = self.get_s_flag();
                let not_s = self.logical_not(s);
                let z = self.get_z_flag();
                self.logical_and(not_s, z)
            }
            FlowTest::LE => {
                let s = self.get_s_flag();
                let z = self.get_z_flag();
                let o = self.get_o_flag();
                let z_or_o = self.logical_or(z, o);
                self.logical_xor(s, z_or_o)
            }
            FlowTest::GT => {
                let s = self.get_s_flag();
                let not_s = self.logical_not(s);
                let o = self.get_o_flag();
                let not_s_xor_o = self.logical_xor(not_s, o);
                let z = self.get_z_flag();
                let not_z = self.logical_not(z);
                self.logical_and(not_s_xor_o, not_z)
            }
            FlowTest::NE => {
                let z = self.get_z_flag();
                self.logical_not(z)
            }
            FlowTest::GE => {
                let s = self.get_s_flag();
                let o = self.get_o_flag();
                let xor = self.logical_xor(s, o);
                self.logical_not(xor)
            }
            FlowTest::NUM => {
                let s = self.get_s_flag();
                let not_s = self.logical_not(s);
                let z = self.get_z_flag();
                let not_z = self.logical_not(z);
                self.logical_or(not_s, not_z)
            }
            FlowTest::NaN => {
                let s = self.get_s_flag();
                let z = self.get_z_flag();
                self.logical_and(s, z)
            }
            FlowTest::LTU => {
                let s = self.get_s_flag();
                let o = self.get_o_flag();
                self.logical_xor(s, o)
            }
            FlowTest::EQU => self.get_z_flag(),
            FlowTest::LEU => {
                let s = self.get_s_flag();
                let o = self.get_o_flag();
                let xor = self.logical_xor(s, o);
                let z = self.get_z_flag();
                self.logical_or(xor, z)
            }
            FlowTest::GTU => {
                let s = self.get_s_flag();
                let not_s = self.logical_not(s);
                let z = self.get_z_flag();
                let o = self.get_o_flag();
                let z_or_o = self.logical_or(z, o);
                self.logical_xor(not_s, z_or_o)
            }
            FlowTest::NEU => {
                let s = self.get_s_flag();
                let z = self.get_z_flag();
                let not_z = self.logical_not(z);
                self.logical_or(s, not_z)
            }
            FlowTest::GEU => {
                let s = self.get_s_flag();
                let not_s = self.logical_not(s);
                let z = self.get_z_flag();
                let not_s_or_z = self.logical_or(not_s, z);
                let o = self.get_o_flag();
                self.logical_xor(not_s_or_z, o)
            }
            FlowTest::T => self.imm_u1(true),
            FlowTest::OFF => {
                let o = self.get_o_flag();
                self.logical_not(o)
            }
            FlowTest::LO => {
                let c = self.get_c_flag();
                self.logical_not(c)
            }
            FlowTest::SFF => {
                let s = self.get_s_flag();
                self.logical_not(s)
            }
            FlowTest::LS => {
                let z = self.get_z_flag();
                let c = self.get_c_flag();
                let not_c = self.logical_not(c);
                self.logical_or(z, not_c)
            }
            FlowTest::HI => {
                let c = self.get_c_flag();
                let z = self.get_z_flag();
                let not_z = self.logical_not(z);
                self.logical_and(c, not_z)
            }
            FlowTest::SFT => self.get_s_flag(),
            FlowTest::HS => self.get_c_flag(),
            FlowTest::OFT => self.get_o_flag(),
            FlowTest::RLE => {
                let s = self.get_s_flag();
                let z = self.get_z_flag();
                self.logical_or(s, z)
            }
            FlowTest::RGT => {
                let s = self.get_s_flag();
                let not_s = self.logical_not(s);
                let z = self.get_z_flag();
                let not_z = self.logical_not(z);
                self.logical_and(not_s, not_z)
            }
            FlowTest::FCSM_TR => {
                log::warn!("(STUBBED) FCSM_TR flow test");
                self.imm_u1(false)
            }
            FlowTest::CSM_TA
            | FlowTest::CSM_TR
            | FlowTest::CSM_MX
            | FlowTest::FCSM_TA
            | FlowTest::FCSM_MX => panic!("Unimplemented flow test {test}"),
        }
    }

    /// Upstream `IREmitter::Condition`.
    pub fn condition(&mut self, cond: Condition) -> Value {
        let flow_test = cond.get_flow_test();
        let (pred, is_negated) = cond.get_pred();
        let predicate = self.get_pred(Pred(pred as u8), is_negated);
        if flow_test == FlowTest::T {
            return predicate;
        }
        let flow = self.get_flow_test_result(flow_test);
        self.logical_and(predicate, flow)
    }

    pub fn get_goto_variable(&mut self, index: u32) -> Value {
        self.emit(Inst::new(
            Opcode::GetGotoVariable,
            vec![Value::ImmU32(index)],
        ))
    }

    pub fn set_goto_variable(&mut self, index: u32, value: Value) {
        self.emit_void(Inst::new(
            Opcode::SetGotoVariable,
            vec![Value::ImmU32(index), value],
        ));
    }

    // ── Constant buffer ───────────────────────────────────────────────

    pub fn get_cbuf_u32(&mut self, binding: Value, offset: Value) -> Value {
        self.emit(Inst::new(Opcode::GetCbufU32, vec![binding, offset]))
    }

    pub fn get_cbuf_u32x2(&mut self, binding: Value, offset: Value) -> Value {
        self.emit(Inst::new(Opcode::GetCbufU32x2, vec![binding, offset]))
    }

    pub fn get_cbuf_f32(&mut self, binding: Value, offset: Value) -> Value {
        self.emit(Inst::new(Opcode::GetCbufF32, vec![binding, offset]))
    }

    pub fn get_cbuf_s16(&mut self, binding: Value, offset: Value) -> Value {
        self.emit(Inst::new(Opcode::GetCbufS16, vec![binding, offset]))
    }

    pub fn get_cbuf_u16(&mut self, binding: Value, offset: Value) -> Value {
        self.emit(Inst::new(Opcode::GetCbufU16, vec![binding, offset]))
    }

    pub fn get_cbuf_u8(&mut self, binding: Value, offset: Value) -> Value {
        self.emit(Inst::new(Opcode::GetCbufU8, vec![binding, offset]))
    }

    pub fn get_cbuf_s8(&mut self, binding: Value, offset: Value) -> Value {
        self.emit(Inst::new(Opcode::GetCbufS8, vec![binding, offset]))
    }

    // ── Attributes ────────────────────────────────────────────────────

    pub fn get_attribute(&mut self, attr: Attribute, vertex: Value) -> Value {
        self.emit(Inst::new(
            Opcode::GetAttribute,
            vec![Value::Attribute(attr), vertex],
        ))
    }

    pub fn get_attribute_u32(&mut self, attr: Attribute, vertex: Value) -> Value {
        self.emit(Inst::new(
            Opcode::GetAttributeU32,
            vec![Value::Attribute(attr), vertex],
        ))
    }

    pub fn set_attribute(&mut self, attr: Attribute, value: Value, vertex: Value) {
        self.emit_void(Inst::new(
            Opcode::SetAttribute,
            vec![Value::Attribute(attr), value, vertex],
        ));
    }

    pub fn get_attribute_indexed(&mut self, offset: Value, vertex: Value) -> Value {
        self.emit(Inst::new(Opcode::GetAttributeIndexed, vec![offset, vertex]))
    }

    pub fn set_attribute_indexed(&mut self, offset: Value, value: Value, vertex: Value) {
        self.emit_void(Inst::new(
            Opcode::SetAttributeIndexed,
            vec![offset, value, vertex],
        ));
    }

    pub fn get_patch(&mut self, patch: Patch) -> Value {
        self.emit(Inst::new(Opcode::GetPatch, vec![Value::Patch(patch)]))
    }

    pub fn set_patch(&mut self, patch: Patch, value: Value) {
        self.emit_void(Inst::new(
            Opcode::SetPatch,
            vec![Value::Patch(patch), value],
        ));
    }

    pub fn set_frag_color(&mut self, rt_index: Value, component: Value, value: Value) {
        self.emit_void(Inst::new(
            Opcode::SetFragColor,
            vec![rt_index, component, value],
        ));
    }

    pub fn set_sample_mask(&mut self, value: Value) {
        self.emit_void(Inst::new(Opcode::SetSampleMask, vec![value]));
    }

    pub fn set_frag_depth(&mut self, value: Value) {
        self.emit_void(Inst::new(Opcode::SetFragDepth, vec![value]));
    }

    // ── Condition flags ───────────────────────────────────────────────

    pub fn get_zero_from_op(&mut self, op: Value) -> Value {
        self.emit_pseudo_from_op(Opcode::GetZeroFromOp, op)
    }

    pub fn get_sign_from_op(&mut self, op: Value) -> Value {
        self.emit_pseudo_from_op(Opcode::GetSignFromOp, op)
    }

    pub fn get_carry_from_op(&mut self, op: Value) -> Value {
        self.emit_pseudo_from_op(Opcode::GetCarryFromOp, op)
    }

    pub fn get_overflow_from_op(&mut self, op: Value) -> Value {
        self.emit_pseudo_from_op(Opcode::GetOverflowFromOp, op)
    }

    pub fn get_sparse_from_op(&mut self, op: Value) -> Value {
        self.emit_pseudo_from_op(Opcode::GetSparseFromOp, op)
    }

    pub fn get_in_bounds_from_op(&mut self, op: Value) -> Value {
        self.emit_pseudo_from_op(Opcode::GetInBoundsFromOp, op)
    }

    // ── System values ─────────────────────────────────────────────────

    pub fn workgroup_id(&mut self) -> Value {
        self.emit(Inst::new(Opcode::WorkgroupId, vec![]))
    }

    pub fn local_invocation_id(&mut self) -> Value {
        self.emit(Inst::new(Opcode::LocalInvocationId, vec![]))
    }

    pub fn invocation_id(&mut self) -> Value {
        self.emit(Inst::new(Opcode::InvocationId, vec![]))
    }

    pub fn invocation_info(&mut self) -> Value {
        self.emit(Inst::new(Opcode::InvocationInfo, vec![]))
    }

    pub fn is_helper_invocation(&mut self) -> Value {
        self.emit(Inst::new(Opcode::IsHelperInvocation, vec![]))
    }

    /// Port of upstream `IREmitter::SR_WScaleFactorXY`.
    pub fn sr_w_scale_factor_xy(&mut self) -> Value {
        self.emit(Inst::new(Opcode::SRWScaleFactorXY, vec![]))
    }

    /// Port of upstream `IREmitter::SR_WScaleFactorZ`.
    pub fn sr_w_scale_factor_z(&mut self) -> Value {
        self.emit(Inst::new(Opcode::SRWScaleFactorZ, vec![]))
    }

    // ── Undefined ─────────────────────────────────────────────────────

    pub fn undef_u1(&mut self) -> Value {
        self.emit(Inst::new(Opcode::UndefU1, vec![]))
    }

    pub fn undef_u32(&mut self) -> Value {
        self.emit(Inst::new(Opcode::UndefU32, vec![]))
    }

    pub fn undef_f32(&mut self) -> Value {
        self.emit(Inst::new(Opcode::UndefU32, vec![]))
    }

    // ── FP32 arithmetic ───────────────────────────────────────────────

    pub fn fp_abs_32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPAbs32, vec![a]))
    }

    pub fn fp_neg_32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPNeg32, vec![a]))
    }

    pub fn fp_add_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPAdd32, vec![a, b]))
    }

    pub fn fp_add_32_with_control(&mut self, a: Value, b: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPAdd32,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_sub_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPSub32, vec![a, b]))
    }

    pub fn fp_mul_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPMul32, vec![a, b]))
    }

    pub fn fp_mul_32_with_control(&mut self, a: Value, b: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPMul32,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_div_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPDiv32, vec![a, b]))
    }

    pub fn fp_fma_32(&mut self, a: Value, b: Value, c: Value) -> Value {
        self.emit(Inst::new(Opcode::FPFma32, vec![a, b, c]))
    }

    pub fn fp_fma_32_with_control(
        &mut self,
        a: Value,
        b: Value,
        c: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPFma32,
            vec![a, b, c],
            control.to_u32(),
        ))
    }

    pub fn fp_min_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPMin32, vec![a, b]))
    }

    pub fn fp_min_32_with_control(&mut self, a: Value, b: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPMin32,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_max_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPMax32, vec![a, b]))
    }

    pub fn fp_max_32_with_control(&mut self, a: Value, b: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPMax32,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_saturate_32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPSaturate32, vec![a]))
    }

    pub fn fp_clamp_32(&mut self, value: Value, min: Value, max: Value) -> Value {
        self.emit(Inst::new(Opcode::FPClamp32, vec![value, min, max]))
    }

    pub fn fp_clamp_16(&mut self, value: Value, min: Value, max: Value) -> Value {
        self.emit(Inst::new(Opcode::FPClamp16, vec![value, min, max]))
    }

    pub fn fp_round_even_32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPRoundEven32, vec![a]))
    }

    pub fn fp_round_even_32_with_control(&mut self, a: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPRoundEven32,
            vec![a],
            control.to_u32(),
        ))
    }

    pub fn fp_floor_32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPFloor32, vec![a]))
    }

    pub fn fp_floor_32_with_control(&mut self, a: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPFloor32,
            vec![a],
            control.to_u32(),
        ))
    }

    pub fn fp_ceil_32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPCeil32, vec![a]))
    }

    pub fn fp_ceil_32_with_control(&mut self, a: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPCeil32,
            vec![a],
            control.to_u32(),
        ))
    }

    pub fn fp_trunc_32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPTrunc32, vec![a]))
    }

    pub fn fp_trunc_32_with_control(&mut self, a: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPTrunc32,
            vec![a],
            control.to_u32(),
        ))
    }

    pub fn fp_recip_32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPRecip32, vec![a]))
    }

    pub fn fp_recip_sqrt_32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPRecipSqrt32, vec![a]))
    }

    pub fn fp_sqrt_32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPSqrt32, vec![a]))
    }

    pub fn fp_sin(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPSin, vec![a]))
    }

    pub fn fp_cos(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPCos, vec![a]))
    }

    pub fn fp_exp2(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPExp2, vec![a]))
    }

    pub fn fp_log2(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPLog2, vec![a]))
    }

    // ── FP32 comparison ───────────────────────────────────────────────

    pub fn fp_ord_equal_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPOrdEqual32, vec![a, b]))
    }

    pub fn fp_ord_equal_32_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPOrdEqual32,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_ord_not_equal_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPOrdNotEqual32, vec![a, b]))
    }

    pub fn fp_ord_not_equal_32_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPOrdNotEqual32,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_ord_less_than_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPOrdLessThan32, vec![a, b]))
    }

    pub fn fp_ord_less_than_32_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPOrdLessThan32,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_ord_greater_than_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPOrdGreaterThan32, vec![a, b]))
    }

    pub fn fp_ord_greater_than_32_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPOrdGreaterThan32,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_ord_less_than_equal_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPOrdLessThanEqual32, vec![a, b]))
    }

    pub fn fp_ord_less_than_equal_32_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPOrdLessThanEqual32,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_ord_greater_than_equal_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPOrdGreaterThanEqual32, vec![a, b]))
    }

    pub fn fp_ord_greater_than_equal_32_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPOrdGreaterThanEqual32,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_unord_equal_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPUnordEqual32, vec![a, b]))
    }

    pub fn fp_unord_equal_32_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPUnordEqual32,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_unord_not_equal_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPUnordNotEqual32, vec![a, b]))
    }

    pub fn fp_unord_not_equal_32_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPUnordNotEqual32,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_unord_less_than_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPUnordLessThan32, vec![a, b]))
    }

    pub fn fp_unord_less_than_32_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPUnordLessThan32,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_unord_greater_than_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPUnordGreaterThan32, vec![a, b]))
    }

    pub fn fp_unord_greater_than_32_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPUnordGreaterThan32,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_unord_less_than_equal_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPUnordLessThanEqual32, vec![a, b]))
    }

    pub fn fp_unord_less_than_equal_32_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPUnordLessThanEqual32,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_unord_greater_than_equal_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPUnordGreaterThanEqual32, vec![a, b]))
    }

    pub fn fp_unord_greater_than_equal_32_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPUnordGreaterThanEqual32,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_is_nan_32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPIsNan32, vec![a]))
    }

    // ── FP64 arithmetic ───────────────────────────────────────────────

    pub fn fp_add_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPAdd64, vec![a, b]))
    }

    pub fn fp_mul_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPMul64, vec![a, b]))
    }

    pub fn fp_mul_64_with_control(&mut self, a: Value, b: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPMul64,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_fma_64(&mut self, a: Value, b: Value, c: Value) -> Value {
        self.emit(Inst::new(Opcode::FPFma64, vec![a, b, c]))
    }

    pub fn fp_fma_64_with_control(
        &mut self,
        a: Value,
        b: Value,
        c: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPFma64,
            vec![a, b, c],
            control.to_u32(),
        ))
    }

    pub fn fp_min_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPMin64, vec![a, b]))
    }

    pub fn fp_max_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPMax64, vec![a, b]))
    }

    pub fn fp_neg_64(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPNeg64, vec![a]))
    }

    pub fn fp_abs_64(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPAbs64, vec![a]))
    }

    pub fn fp_add_64_with_control(&mut self, a: Value, b: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPAdd64,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_saturate_64(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPSaturate64, vec![a]))
    }

    pub fn fp_clamp_64(&mut self, value: Value, min: Value, max: Value) -> Value {
        self.emit(Inst::new(Opcode::FPClamp64, vec![value, min, max]))
    }

    /// Port of upstream `IREmitter::LaneId()` — subgroup invocation index.
    pub fn lane_id(&mut self) -> Value {
        self.emit(Inst::new(Opcode::LaneId, vec![]))
    }

    // ── Global-memory atomic helpers (port of `IREmitter::GlobalAtomic*`) ──

    fn global_atomic_integer(
        &mut self,
        opcode_32: Opcode,
        opcode_64: Opcode,
        offset: Value,
        value: Value,
        is_64_bit: bool,
    ) -> Value {
        let opcode = if is_64_bit { opcode_64 } else { opcode_32 };
        self.emit(Inst::new(opcode, vec![offset, value]))
    }

    pub fn global_atomic_iadd(&mut self, offset: Value, value: Value, is_64_bit: bool) -> Value {
        self.global_atomic_integer(
            Opcode::GlobalAtomicIAdd32,
            Opcode::GlobalAtomicIAdd64,
            offset,
            value,
            is_64_bit,
        )
    }

    pub fn global_atomic_imin(
        &mut self,
        offset: Value,
        value: Value,
        is_signed: bool,
        is_64_bit: bool,
    ) -> Value {
        let (opcode_32, opcode_64) = if is_signed {
            (Opcode::GlobalAtomicSMin32, Opcode::GlobalAtomicSMin64)
        } else {
            (Opcode::GlobalAtomicUMin32, Opcode::GlobalAtomicUMin64)
        };
        self.global_atomic_integer(opcode_32, opcode_64, offset, value, is_64_bit)
    }

    pub fn global_atomic_imax(
        &mut self,
        offset: Value,
        value: Value,
        is_signed: bool,
        is_64_bit: bool,
    ) -> Value {
        let (opcode_32, opcode_64) = if is_signed {
            (Opcode::GlobalAtomicSMax32, Opcode::GlobalAtomicSMax64)
        } else {
            (Opcode::GlobalAtomicUMax32, Opcode::GlobalAtomicUMax64)
        };
        self.global_atomic_integer(opcode_32, opcode_64, offset, value, is_64_bit)
    }

    pub fn global_atomic_inc_32(&mut self, offset: Value, value: Value) -> Value {
        self.emit(Inst::new(Opcode::GlobalAtomicInc32, vec![offset, value]))
    }

    pub fn global_atomic_dec_32(&mut self, offset: Value, value: Value) -> Value {
        self.emit(Inst::new(Opcode::GlobalAtomicDec32, vec![offset, value]))
    }

    pub fn global_atomic_and(&mut self, offset: Value, value: Value, is_64_bit: bool) -> Value {
        self.global_atomic_integer(
            Opcode::GlobalAtomicAnd32,
            Opcode::GlobalAtomicAnd64,
            offset,
            value,
            is_64_bit,
        )
    }

    pub fn global_atomic_or(&mut self, offset: Value, value: Value, is_64_bit: bool) -> Value {
        self.global_atomic_integer(
            Opcode::GlobalAtomicOr32,
            Opcode::GlobalAtomicOr64,
            offset,
            value,
            is_64_bit,
        )
    }

    pub fn global_atomic_xor(&mut self, offset: Value, value: Value, is_64_bit: bool) -> Value {
        self.global_atomic_integer(
            Opcode::GlobalAtomicXor32,
            Opcode::GlobalAtomicXor64,
            offset,
            value,
            is_64_bit,
        )
    }

    pub fn global_atomic_exchange(
        &mut self,
        offset: Value,
        value: Value,
        is_64_bit: bool,
    ) -> Value {
        self.global_atomic_integer(
            Opcode::GlobalAtomicExchange32,
            Opcode::GlobalAtomicExchange64,
            offset,
            value,
            is_64_bit,
        )
    }

    pub fn global_atomic_f32_add(
        &mut self,
        offset: Value,
        value: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::GlobalAtomicAddF32,
            vec![offset, value],
            control.to_u32(),
        ))
    }

    pub fn global_atomic_f16x2_add(
        &mut self,
        offset: Value,
        value: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::GlobalAtomicAddF16x2,
            vec![offset, value],
            control.to_u32(),
        ))
    }

    pub fn global_atomic_f16x2_min(
        &mut self,
        offset: Value,
        value: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::GlobalAtomicMinF16x2,
            vec![offset, value],
            control.to_u32(),
        ))
    }

    pub fn global_atomic_f16x2_max(
        &mut self,
        offset: Value,
        value: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::GlobalAtomicMaxF16x2,
            vec![offset, value],
            control.to_u32(),
        ))
    }

    // ── Shared-memory atomic helpers (port of `IREmitter::SharedAtomic*`) ──

    pub fn shared_atomic_iadd_32(&mut self, offset: Value, value: Value) -> Value {
        self.emit(Inst::new(Opcode::SharedAtomicIAdd32, vec![offset, value]))
    }
    pub fn shared_atomic_imin_32(&mut self, offset: Value, value: Value, is_signed: bool) -> Value {
        let op = if is_signed {
            Opcode::SharedAtomicSMin32
        } else {
            Opcode::SharedAtomicUMin32
        };
        self.emit(Inst::new(op, vec![offset, value]))
    }
    pub fn shared_atomic_imax_32(&mut self, offset: Value, value: Value, is_signed: bool) -> Value {
        let op = if is_signed {
            Opcode::SharedAtomicSMax32
        } else {
            Opcode::SharedAtomicUMax32
        };
        self.emit(Inst::new(op, vec![offset, value]))
    }
    pub fn shared_atomic_inc_32(&mut self, offset: Value, value: Value) -> Value {
        self.emit(Inst::new(Opcode::SharedAtomicInc32, vec![offset, value]))
    }
    pub fn shared_atomic_dec_32(&mut self, offset: Value, value: Value) -> Value {
        self.emit(Inst::new(Opcode::SharedAtomicDec32, vec![offset, value]))
    }
    pub fn shared_atomic_and_32(&mut self, offset: Value, value: Value) -> Value {
        self.emit(Inst::new(Opcode::SharedAtomicAnd32, vec![offset, value]))
    }
    pub fn shared_atomic_or_32(&mut self, offset: Value, value: Value) -> Value {
        self.emit(Inst::new(Opcode::SharedAtomicOr32, vec![offset, value]))
    }
    pub fn shared_atomic_xor_32(&mut self, offset: Value, value: Value) -> Value {
        self.emit(Inst::new(Opcode::SharedAtomicXor32, vec![offset, value]))
    }
    pub fn shared_atomic_exchange_32(&mut self, offset: Value, value: Value) -> Value {
        self.emit(Inst::new(
            Opcode::SharedAtomicExchange32,
            vec![offset, value],
        ))
    }
    pub fn shared_atomic_exchange_64(&mut self, offset: Value, value: Value) -> Value {
        self.emit(Inst::new(
            Opcode::SharedAtomicExchange64,
            vec![offset, value],
        ))
    }

    pub fn fp_round_even_64(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPRoundEven64, vec![a]))
    }

    pub fn fp_round_even_64_with_control(&mut self, a: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPRoundEven64,
            vec![a],
            control.to_u32(),
        ))
    }

    pub fn fp_floor_64(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPFloor64, vec![a]))
    }

    pub fn fp_floor_64_with_control(&mut self, a: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPFloor64,
            vec![a],
            control.to_u32(),
        ))
    }

    pub fn fp_ceil_64(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPCeil64, vec![a]))
    }

    pub fn fp_ceil_64_with_control(&mut self, a: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPCeil64,
            vec![a],
            control.to_u32(),
        ))
    }

    pub fn fp_trunc_64(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPTrunc64, vec![a]))
    }

    pub fn fp_trunc_64_with_control(&mut self, a: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPTrunc64,
            vec![a],
            control.to_u32(),
        ))
    }

    // ── Integer arithmetic ────────────────────────────────────────────

    pub fn iadd_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::IAdd32, vec![a, b]))
    }

    pub fn iadd_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::IAdd64, vec![a, b]))
    }

    pub fn isub_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::ISub32, vec![a, b]))
    }

    pub fn imul_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::IMul32, vec![a, b]))
    }

    pub fn ineg_32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::INeg32, vec![a]))
    }

    pub fn iabs_32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::IAbs32, vec![a]))
    }

    pub fn iabs_64(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::IAbs64, vec![a]))
    }

    pub fn shift_left_logical_32(&mut self, base: Value, shift: Value) -> Value {
        self.emit(Inst::new(Opcode::ShiftLeftLogical32, vec![base, shift]))
    }

    pub fn shift_right_logical_32(&mut self, base: Value, shift: Value) -> Value {
        self.emit(Inst::new(Opcode::ShiftRightLogical32, vec![base, shift]))
    }

    pub fn shift_right_arithmetic_32(&mut self, base: Value, shift: Value) -> Value {
        self.emit(Inst::new(Opcode::ShiftRightArithmetic32, vec![base, shift]))
    }

    pub fn bitwise_and_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::BitwiseAnd32, vec![a, b]))
    }

    pub fn bitwise_or_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::BitwiseOr32, vec![a, b]))
    }

    pub fn bitwise_xor_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::BitwiseXor32, vec![a, b]))
    }

    pub fn bitwise_not_32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::BitwiseNot32, vec![a]))
    }

    pub fn bit_field_insert(
        &mut self,
        base: Value,
        insert: Value,
        offset: Value,
        count: Value,
    ) -> Value {
        self.emit(Inst::new(
            Opcode::BitFieldInsert,
            vec![base, insert, offset, count],
        ))
    }

    pub fn bit_field_s_extract(&mut self, base: Value, offset: Value, count: Value) -> Value {
        self.emit(Inst::new(
            Opcode::BitFieldSExtract,
            vec![base, offset, count],
        ))
    }

    pub fn bit_field_u_extract(&mut self, base: Value, offset: Value, count: Value) -> Value {
        self.emit(Inst::new(
            Opcode::BitFieldUExtract,
            vec![base, offset, count],
        ))
    }

    pub fn bit_reverse_32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::BitReverse32, vec![a]))
    }

    pub fn s_clamp_32(&mut self, value: Value, min: Value, max: Value) -> Value {
        self.emit(Inst::new(Opcode::SClamp32, vec![value, min, max]))
    }

    pub fn u_clamp_32(&mut self, value: Value, min: Value, max: Value) -> Value {
        self.emit(Inst::new(Opcode::UClamp32, vec![value, min, max]))
    }

    pub fn bit_count_32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::BitCount32, vec![a]))
    }

    pub fn find_s_msb_32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FindSMsb32, vec![a]))
    }

    pub fn find_u_msb_32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FindUMsb32, vec![a]))
    }

    pub fn s_min_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::SMin32, vec![a, b]))
    }

    pub fn u_min_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::UMin32, vec![a, b]))
    }

    pub fn s_max_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::SMax32, vec![a, b]))
    }

    pub fn u_max_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::UMax32, vec![a, b]))
    }

    // ── Integer comparison ────────────────────────────────────────────

    pub fn i_equal(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::IEqual, vec![a, b]))
    }

    pub fn i_not_equal(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::INotEqual, vec![a, b]))
    }

    pub fn s_less_than(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::SLessThan, vec![a, b]))
    }

    pub fn u_less_than(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::ULessThan, vec![a, b]))
    }

    pub fn s_less_than_equal(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::SLessThanEqual, vec![a, b]))
    }

    pub fn u_less_than_equal(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::ULessThanEqual, vec![a, b]))
    }

    pub fn s_greater_than(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::SGreaterThan, vec![a, b]))
    }

    pub fn u_greater_than(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::UGreaterThan, vec![a, b]))
    }

    pub fn s_greater_than_equal(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::SGreaterThanEqual, vec![a, b]))
    }

    pub fn u_greater_than_equal(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::UGreaterThanEqual, vec![a, b]))
    }

    // ── Logic ─────────────────────────────────────────────────────────

    pub fn logical_or(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::LogicalOr, vec![a, b]))
    }

    pub fn logical_and(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::LogicalAnd, vec![a, b]))
    }

    pub fn logical_xor(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::LogicalXor, vec![a, b]))
    }

    pub fn logical_not(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::LogicalNot, vec![a]))
    }

    // ── Select ────────────────────────────────────────────────────────

    pub fn select_u32(&mut self, cond: Value, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::SelectU32, vec![cond, a, b]))
    }

    pub fn select_u64(&mut self, cond: Value, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::SelectU64, vec![cond, a, b]))
    }

    pub fn select_f16(&mut self, cond: Value, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::SelectF16, vec![cond, a, b]))
    }

    pub fn select_f32(&mut self, cond: Value, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::SelectF32, vec![cond, a, b]))
    }

    pub fn select_f64(&mut self, cond: Value, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::SelectF64, vec![cond, a, b]))
    }

    pub fn select_u1(&mut self, cond: Value, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::SelectU1, vec![cond, a, b]))
    }

    // ── Bitcast ───────────────────────────────────────────────────────

    pub fn bit_cast_u32_f32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::BitCastU32F32, vec![a]))
    }

    pub fn bit_cast_f32_u32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::BitCastF32U32, vec![a]))
    }

    pub fn bit_cast_u64_f64(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::BitCastU64F64, vec![a]))
    }

    pub fn bit_cast_f64_u64(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::BitCastF64U64, vec![a]))
    }

    pub fn pack_half_2x16(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::PackHalf2x16, vec![a]))
    }

    pub fn unpack_half_2x16(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::UnpackHalf2x16, vec![a]))
    }

    // ── Conversion ────────────────────────────────────────────────────

    pub fn convert_f32_from_s32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::ConvertF32S32, vec![a]))
    }

    pub fn convert_f32_from_u32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::ConvertF32U32, vec![a]))
    }

    /// Port of upstream `IREmitter::ConvertSToF`.
    pub fn convert_s_to_f(
        &mut self,
        dest_bits: u32,
        src_bits: u32,
        value: Value,
        control: FpControl,
    ) -> Value {
        let opcode = match (dest_bits, src_bits) {
            (16, 8) => Opcode::ConvertF16S8,
            (16, 16) => Opcode::ConvertF16S16,
            (16, 32) => Opcode::ConvertF16S32,
            (16, 64) => Opcode::ConvertF16S64,
            (32, 8) => Opcode::ConvertF32S8,
            (32, 16) => Opcode::ConvertF32S16,
            (32, 32) => Opcode::ConvertF32S32,
            (32, 64) => Opcode::ConvertF32S64,
            (64, 8) => Opcode::ConvertF64S8,
            (64, 16) => Opcode::ConvertF64S16,
            (64, 32) => Opcode::ConvertF64S32,
            (64, 64) => Opcode::ConvertF64S64,
            _ => panic!(
                "Invalid signed-integer-to-float bit size combination dst={} src={}",
                dest_bits, src_bits
            ),
        };
        self.emit(Inst::with_flags(opcode, vec![value], control.to_u32()))
    }

    /// Port of upstream `IREmitter::ConvertUToF`.
    pub fn convert_u_to_f(
        &mut self,
        dest_bits: u32,
        src_bits: u32,
        value: Value,
        control: FpControl,
    ) -> Value {
        let opcode = match (dest_bits, src_bits) {
            (16, 8) => Opcode::ConvertF16U8,
            (16, 16) => Opcode::ConvertF16U16,
            (16, 32) => Opcode::ConvertF16U32,
            (16, 64) => Opcode::ConvertF16U64,
            (32, 8) => Opcode::ConvertF32U8,
            (32, 16) => Opcode::ConvertF32U16,
            (32, 32) => Opcode::ConvertF32U32,
            (32, 64) => Opcode::ConvertF32U64,
            (64, 8) => Opcode::ConvertF64U8,
            (64, 16) => Opcode::ConvertF64U16,
            (64, 32) => Opcode::ConvertF64U32,
            (64, 64) => Opcode::ConvertF64U64,
            _ => panic!(
                "Invalid unsigned-integer-to-float bit size combination dst={} src={}",
                dest_bits, src_bits
            ),
        };
        self.emit(Inst::with_flags(opcode, vec![value], control.to_u32()))
    }

    /// Port of upstream `IREmitter::ConvertIToF`.
    pub fn convert_i_to_f(
        &mut self,
        dest_bits: u32,
        src_bits: u32,
        is_signed: bool,
        value: Value,
        control: FpControl,
    ) -> Value {
        if is_signed {
            self.convert_s_to_f(dest_bits, src_bits, value, control)
        } else {
            self.convert_u_to_f(dest_bits, src_bits, value, control)
        }
    }

    pub fn convert_s32_from_f32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::ConvertS32F32, vec![a]))
    }

    pub fn convert_u32_from_f32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::ConvertU32F32, vec![a]))
    }

    /// Port of upstream `IREmitter::ConvertFToI`.
    pub fn convert_f_to_i(
        &mut self,
        dest_bits: u32,
        src_bits: u32,
        is_signed: bool,
        value: Value,
    ) -> Value {
        let opcode = match (dest_bits, src_bits, is_signed) {
            (16, 16, true) => Opcode::ConvertS16F16,
            (16, 32, true) => Opcode::ConvertS16F32,
            (16, 64, true) => Opcode::ConvertS16F64,
            (32, 16, true) => Opcode::ConvertS32F16,
            (32, 32, true) => Opcode::ConvertS32F32,
            (32, 64, true) => Opcode::ConvertS32F64,
            (64, 16, true) => Opcode::ConvertS64F16,
            (64, 32, true) => Opcode::ConvertS64F32,
            (64, 64, true) => Opcode::ConvertS64F64,
            (16, 16, false) => Opcode::ConvertU16F16,
            (16, 32, false) => Opcode::ConvertU16F32,
            (16, 64, false) => Opcode::ConvertU16F64,
            (32, 16, false) => Opcode::ConvertU32F16,
            (32, 32, false) => Opcode::ConvertU32F32,
            (32, 64, false) => Opcode::ConvertU32F64,
            (64, 16, false) => Opcode::ConvertU64F16,
            (64, 32, false) => Opcode::ConvertU64F32,
            (64, 64, false) => Opcode::ConvertU64F64,
            _ => panic!(
                "Invalid float-to-integer bit size combination dst={} src={}",
                dest_bits, src_bits
            ),
        };
        self.emit(Inst::new(opcode, vec![value]))
    }

    pub fn convert_f16_from_f32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::ConvertF16F32, vec![a]))
    }

    pub fn convert_f16_from_f32_with_control(&mut self, a: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::ConvertF16F32,
            vec![a],
            control.to_u32(),
        ))
    }

    pub fn convert_f32_from_f16(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::ConvertF32F16, vec![a]))
    }

    pub fn convert_f32_from_f16_with_control(&mut self, a: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::ConvertF32F16,
            vec![a],
            control.to_u32(),
        ))
    }

    pub fn convert_f64_from_f32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::ConvertF64F32, vec![a]))
    }

    pub fn convert_f64_from_f32_with_control(&mut self, a: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::ConvertF64F32,
            vec![a],
            control.to_u32(),
        ))
    }

    pub fn convert_f32_from_f64(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::ConvertF32F64, vec![a]))
    }

    pub fn convert_f32_from_f64_with_control(&mut self, a: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::ConvertF32F64,
            vec![a],
            control.to_u32(),
        ))
    }

    /// Zero-extend a 32-bit unsigned integer to 64 bits.
    ///
    /// Counterpart of upstream `IREmitter::UConvert(64, U32)`.
    pub fn uconvert_u64_from_u32(&mut self, value: Value) -> Value {
        self.emit(Inst::new(Opcode::ConvertU64U32, vec![value]))
    }

    /// Polymorphic float conversion. Port of upstream
    /// `IREmitter::FPConvert(size_t result_bitsize, const F16F32F64& value, FpControl control)`.
    ///
    /// Same-width inputs return `src` unchanged. F16↔F64 panics, matching
    /// upstream's `throw LogicError("Illegal conversion from F64 to F16")`.
    pub fn fp_convert(
        &mut self,
        target_bits: u32,
        src: Value,
        src_bits: u32,
        control: FpControl,
    ) -> Value {
        match (target_bits, src_bits) {
            (16, 16) | (32, 32) | (64, 64) => src,
            (16, 32) => self.convert_f16_from_f32_with_control(src, control),
            (32, 16) => self.convert_f32_from_f16_with_control(src, control),
            (32, 64) => self.convert_f32_from_f64_with_control(src, control),
            (64, 32) => self.convert_f64_from_f32_with_control(src, control),
            (16, 64) => panic!("Illegal conversion from F64 to F16"),
            (64, 16) => panic!("Illegal conversion from F16 to F64"),
            _ => panic!(
                "Conversion from {} to {} bits not implemented",
                src_bits, target_bits
            ),
        }
    }

    // ── Composite (vector) ────────────────────────────────────────────

    pub fn composite_construct_u32x2(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::CompositeConstructU32x2, vec![a, b]))
    }

    pub fn composite_construct_u32x3(&mut self, a: Value, b: Value, c: Value) -> Value {
        self.emit(Inst::new(Opcode::CompositeConstructU32x3, vec![a, b, c]))
    }

    pub fn composite_construct_u32x4(&mut self, a: Value, b: Value, c: Value, d: Value) -> Value {
        self.emit(Inst::new(Opcode::CompositeConstructU32x4, vec![a, b, c, d]))
    }

    pub fn composite_extract_u32x2(&mut self, vector: Value, index: Value) -> Value {
        self.emit(Inst::new(
            Opcode::CompositeExtractU32x2,
            vec![vector, index],
        ))
    }

    pub fn composite_extract_u32x3(&mut self, vector: Value, index: Value) -> Value {
        self.emit(Inst::new(
            Opcode::CompositeExtractU32x3,
            vec![vector, index],
        ))
    }

    pub fn composite_extract_u32x4(&mut self, vector: Value, index: Value) -> Value {
        self.emit(Inst::new(
            Opcode::CompositeExtractU32x4,
            vec![vector, index],
        ))
    }

    pub fn composite_construct_f32x2(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::CompositeConstructF32x2, vec![a, b]))
    }

    pub fn composite_construct_f32x3(&mut self, a: Value, b: Value, c: Value) -> Value {
        self.emit(Inst::new(Opcode::CompositeConstructF32x3, vec![a, b, c]))
    }

    pub fn composite_construct_f32x4(&mut self, a: Value, b: Value, c: Value, d: Value) -> Value {
        self.emit(Inst::new(Opcode::CompositeConstructF32x4, vec![a, b, c, d]))
    }

    pub fn composite_extract_f32x4(&mut self, vector: Value, index: Value) -> Value {
        self.emit(Inst::new(
            Opcode::CompositeExtractF32x4,
            vec![vector, index],
        ))
    }

    // ── Memory ────────────────────────────────────────────────────────

    pub fn load_global_u8(&mut self, addr: Value) -> Value {
        self.emit(Inst::new(Opcode::LoadGlobalU8, vec![addr]))
    }

    pub fn load_global_s8(&mut self, addr: Value) -> Value {
        self.emit(Inst::new(Opcode::LoadGlobalS8, vec![addr]))
    }

    pub fn load_global_u16(&mut self, addr: Value) -> Value {
        self.emit(Inst::new(Opcode::LoadGlobalU16, vec![addr]))
    }

    pub fn load_global_s16(&mut self, addr: Value) -> Value {
        self.emit(Inst::new(Opcode::LoadGlobalS16, vec![addr]))
    }

    pub fn load_global_32(&mut self, addr: Value) -> Value {
        self.emit(Inst::new(Opcode::LoadGlobal32, vec![addr]))
    }

    pub fn load_global_64(&mut self, addr: Value) -> Value {
        self.emit(Inst::new(Opcode::LoadGlobal64, vec![addr]))
    }

    pub fn load_global_128(&mut self, addr: Value) -> Value {
        self.emit(Inst::new(Opcode::LoadGlobal128, vec![addr]))
    }

    pub fn write_global_32(&mut self, addr: Value, value: Value) {
        self.emit_void(Inst::new(Opcode::WriteGlobal32, vec![addr, value]));
    }

    pub fn write_global_u8(&mut self, addr: Value, value: Value) {
        self.emit_void(Inst::new(Opcode::WriteGlobalU8, vec![addr, value]));
    }

    pub fn write_global_s8(&mut self, addr: Value, value: Value) {
        self.emit_void(Inst::new(Opcode::WriteGlobalS8, vec![addr, value]));
    }

    pub fn write_global_u16(&mut self, addr: Value, value: Value) {
        self.emit_void(Inst::new(Opcode::WriteGlobalU16, vec![addr, value]));
    }

    pub fn write_global_s16(&mut self, addr: Value, value: Value) {
        self.emit_void(Inst::new(Opcode::WriteGlobalS16, vec![addr, value]));
    }

    pub fn write_global_64(&mut self, addr: Value, value: Value) {
        self.emit_void(Inst::new(Opcode::WriteGlobal64, vec![addr, value]));
    }

    pub fn write_global_128(&mut self, addr: Value, value: Value) {
        self.emit_void(Inst::new(Opcode::WriteGlobal128, vec![addr, value]));
    }

    pub fn load_local(&mut self, offset: Value) -> Value {
        self.emit(Inst::new(Opcode::LoadLocal, vec![offset]))
    }

    pub fn write_local(&mut self, offset: Value, value: Value) {
        self.emit_void(Inst::new(Opcode::WriteLocal, vec![offset, value]));
    }

    pub fn load_storage_32(&mut self, binding: Value, offset: Value) -> Value {
        self.emit(Inst::new(Opcode::LoadStorage32, vec![binding, offset]))
    }

    pub fn write_storage_32(&mut self, binding: Value, offset: Value, value: Value) {
        self.emit_void(Inst::new(
            Opcode::WriteStorage32,
            vec![binding, offset, value],
        ));
    }

    // ── Texture ───────────────────────────────────────────────────────

    pub fn image_sample_implicit_lod_full(
        &mut self,
        handle: Value,
        coords: Value,
        bias_lc: Value,
        offset: Value,
        info: u32,
    ) -> Value {
        let opcode = if handle.is_immediate() {
            Opcode::BoundImageSampleImplicitLod
        } else {
            Opcode::BindlessImageSampleImplicitLod
        };
        self.emit(Inst::with_flags(
            opcode,
            vec![handle, coords, bias_lc, offset],
            info,
        ))
    }

    pub fn image_sample_implicit_lod(&mut self, handle: Value, coords: Value, info: u32) -> Value {
        self.image_sample_implicit_lod_full(handle, coords, Value::Void, Value::Void, info)
    }

    pub fn image_sample_explicit_lod_full(
        &mut self,
        handle: Value,
        coords: Value,
        lod: Value,
        offset: Value,
        info: u32,
    ) -> Value {
        let opcode = if handle.is_immediate() {
            Opcode::BoundImageSampleExplicitLod
        } else {
            Opcode::BindlessImageSampleExplicitLod
        };
        self.emit(Inst::with_flags(
            opcode,
            vec![handle, coords, lod, offset],
            info,
        ))
    }

    pub fn image_sample_explicit_lod(
        &mut self,
        handle: Value,
        coords: Value,
        lod: Value,
        info: u32,
    ) -> Value {
        self.image_sample_explicit_lod_full(handle, coords, lod, Value::Void, info)
    }

    pub fn image_sample_dref_implicit_lod_full(
        &mut self,
        handle: Value,
        coords: Value,
        dref: Value,
        bias_lc: Value,
        offset: Value,
        info: u32,
    ) -> Value {
        let opcode = if handle.is_immediate() {
            Opcode::BoundImageSampleDrefImplicitLod
        } else {
            Opcode::BindlessImageSampleDrefImplicitLod
        };
        self.emit(Inst::with_flags(
            opcode,
            vec![handle, coords, dref, bias_lc, offset],
            info,
        ))
    }

    pub fn image_sample_dref_implicit_lod(
        &mut self,
        handle: Value,
        coords: Value,
        dref: Value,
        info: u32,
    ) -> Value {
        self.image_sample_dref_implicit_lod_full(
            handle,
            coords,
            dref,
            Value::Void,
            Value::Void,
            info,
        )
    }

    pub fn image_sample_dref_explicit_lod_full(
        &mut self,
        handle: Value,
        coords: Value,
        dref: Value,
        lod: Value,
        offset: Value,
        info: u32,
    ) -> Value {
        let opcode = if handle.is_immediate() {
            Opcode::BoundImageSampleDrefExplicitLod
        } else {
            Opcode::BindlessImageSampleDrefExplicitLod
        };
        self.emit(Inst::with_flags(
            opcode,
            vec![handle, coords, dref, lod, offset],
            info,
        ))
    }

    pub fn image_sample_dref_explicit_lod(
        &mut self,
        handle: Value,
        coords: Value,
        dref: Value,
        lod: Value,
        info: u32,
    ) -> Value {
        self.image_sample_dref_explicit_lod_full(handle, coords, dref, lod, Value::Void, info)
    }

    pub fn image_fetch_full(
        &mut self,
        handle: Value,
        coords: Value,
        offset: Value,
        lod: Value,
        multisampling: Value,
        info: u32,
    ) -> Value {
        let opcode = if handle.is_immediate() {
            Opcode::BoundImageFetch
        } else {
            Opcode::BindlessImageFetch
        };
        self.emit(Inst::with_flags(
            opcode,
            vec![handle, coords, offset, lod, multisampling],
            info,
        ))
    }

    pub fn image_fetch(&mut self, handle: Value, coords: Value, lod: Value, info: u32) -> Value {
        self.image_fetch_full(handle, coords, Value::Void, lod, Value::Void, info)
    }

    pub fn image_query_dimensions_full(
        &mut self,
        handle: Value,
        lod: Value,
        skip_mips: Value,
        info: u32,
    ) -> Value {
        let opcode = if handle.is_immediate() {
            Opcode::BoundImageQueryDimensions
        } else {
            Opcode::BindlessImageQueryDimensions
        };
        self.emit(Inst::with_flags(opcode, vec![handle, lod, skip_mips], info))
    }

    pub fn image_query_dimensions(&mut self, handle: Value, lod: Value, info: u32) -> Value {
        self.image_query_dimensions_full(handle, lod, Value::ImmU1(false), info)
    }

    pub fn image_query_dimension_full(
        &mut self,
        handle: Value,
        lod: Value,
        skip_mips: Value,
        info: u32,
    ) -> Value {
        self.image_query_dimensions_full(handle, lod, skip_mips, info)
    }

    pub fn image_query_dimension(&mut self, handle: Value, lod: Value, info: u32) -> Value {
        self.image_query_dimensions(handle, lod, info)
    }

    pub fn image_gather_full(
        &mut self,
        handle: Value,
        coords: Value,
        offset: Value,
        offset2: Value,
        info: u32,
    ) -> Value {
        let opcode = if handle.is_immediate() {
            Opcode::BoundImageGather
        } else {
            Opcode::BindlessImageGather
        };
        self.emit(Inst::with_flags(
            opcode,
            vec![handle, coords, offset, offset2],
            info,
        ))
    }

    pub fn image_gather(&mut self, handle: Value, coords: Value, info: u32) -> Value {
        self.image_gather_full(handle, coords, Value::Void, Value::Void, info)
    }

    pub fn image_gather_dref_full(
        &mut self,
        handle: Value,
        coords: Value,
        offset: Value,
        offset2: Value,
        dref: Value,
        info: u32,
    ) -> Value {
        let opcode = if handle.is_immediate() {
            Opcode::BoundImageGatherDref
        } else {
            Opcode::BindlessImageGatherDref
        };
        self.emit(Inst::with_flags(
            opcode,
            vec![handle, coords, offset, offset2, dref],
            info,
        ))
    }

    pub fn image_gather_dref(
        &mut self,
        handle: Value,
        coords: Value,
        dref: Value,
        info: u32,
    ) -> Value {
        self.image_gather_dref_full(handle, coords, Value::Void, Value::Void, dref, info)
    }

    /// Port of `IREmitter::ImageQueryLod`.
    pub fn image_query_lod(&mut self, handle: Value, coords: Value, info: u32) -> Value {
        self.emit(Inst::with_flags(
            Opcode::ImageQueryLod,
            vec![handle, coords],
            info,
        ))
    }

    /// Port of `IREmitter::ImageGradient`.
    pub fn image_gradient_full(
        &mut self,
        handle: Value,
        coords: Value,
        derivatives: Value,
        offset: Value,
        lod_clamp: Value,
        info: u32,
    ) -> Value {
        let opcode = if handle.is_immediate() {
            Opcode::BoundImageGradient
        } else {
            Opcode::BindlessImageGradient
        };
        self.emit(Inst::with_flags(
            opcode,
            vec![handle, coords, derivatives, offset, lod_clamp],
            info,
        ))
    }

    pub fn image_gradient(
        &mut self,
        handle: Value,
        coords: Value,
        derivatives: Value,
        info: u32,
    ) -> Value {
        self.image_gradient_full(handle, coords, derivatives, Value::Void, Value::Void, info)
    }

    /// Port of `IREmitter::ImageRead`.
    pub fn image_read(&mut self, handle: Value, coords: Value, info: u32) -> Value {
        let opcode = if handle.is_immediate() {
            Opcode::BoundImageRead
        } else {
            Opcode::BindlessImageRead
        };
        self.emit(Inst::with_flags(opcode, vec![handle, coords], info))
    }

    /// Port of `IREmitter::ImageWrite`.
    pub fn image_write(&mut self, handle: Value, coords: Value, color: Value, info: u32) {
        let opcode = if handle.is_immediate() {
            Opcode::BoundImageWrite
        } else {
            Opcode::BindlessImageWrite
        };
        let _ = self.emit(Inst::with_flags(opcode, vec![handle, coords, color], info));
    }

    fn image_atomic(
        &mut self,
        bound_opcode: Opcode,
        bindless_opcode: Opcode,
        handle: Value,
        coords: Value,
        value: Value,
        info: u32,
    ) -> Value {
        let opcode = if handle.is_immediate() {
            bound_opcode
        } else {
            bindless_opcode
        };
        self.emit(Inst::with_flags(opcode, vec![handle, coords, value], info))
    }

    pub fn image_atomic_iadd_32(
        &mut self,
        handle: Value,
        coords: Value,
        value: Value,
        info: u32,
    ) -> Value {
        self.image_atomic(
            Opcode::BoundImageAtomicIAdd32,
            Opcode::BindlessImageAtomicIAdd32,
            handle,
            coords,
            value,
            info,
        )
    }

    pub fn image_atomic_smin_32(
        &mut self,
        handle: Value,
        coords: Value,
        value: Value,
        info: u32,
    ) -> Value {
        self.image_atomic(
            Opcode::BoundImageAtomicSMin32,
            Opcode::BindlessImageAtomicSMin32,
            handle,
            coords,
            value,
            info,
        )
    }

    pub fn image_atomic_umin_32(
        &mut self,
        handle: Value,
        coords: Value,
        value: Value,
        info: u32,
    ) -> Value {
        self.image_atomic(
            Opcode::BoundImageAtomicUMin32,
            Opcode::BindlessImageAtomicUMin32,
            handle,
            coords,
            value,
            info,
        )
    }

    pub fn image_atomic_imin_32(
        &mut self,
        handle: Value,
        coords: Value,
        value: Value,
        is_signed: bool,
        info: u32,
    ) -> Value {
        if is_signed {
            self.image_atomic_smin_32(handle, coords, value, info)
        } else {
            self.image_atomic_umin_32(handle, coords, value, info)
        }
    }

    pub fn image_atomic_smax_32(
        &mut self,
        handle: Value,
        coords: Value,
        value: Value,
        info: u32,
    ) -> Value {
        self.image_atomic(
            Opcode::BoundImageAtomicSMax32,
            Opcode::BindlessImageAtomicSMax32,
            handle,
            coords,
            value,
            info,
        )
    }

    pub fn image_atomic_umax_32(
        &mut self,
        handle: Value,
        coords: Value,
        value: Value,
        info: u32,
    ) -> Value {
        self.image_atomic(
            Opcode::BoundImageAtomicUMax32,
            Opcode::BindlessImageAtomicUMax32,
            handle,
            coords,
            value,
            info,
        )
    }

    pub fn image_atomic_imax_32(
        &mut self,
        handle: Value,
        coords: Value,
        value: Value,
        is_signed: bool,
        info: u32,
    ) -> Value {
        if is_signed {
            self.image_atomic_smax_32(handle, coords, value, info)
        } else {
            self.image_atomic_umax_32(handle, coords, value, info)
        }
    }

    pub fn image_atomic_inc_32(
        &mut self,
        handle: Value,
        coords: Value,
        value: Value,
        info: u32,
    ) -> Value {
        self.image_atomic(
            Opcode::BoundImageAtomicInc32,
            Opcode::BindlessImageAtomicInc32,
            handle,
            coords,
            value,
            info,
        )
    }

    pub fn image_atomic_dec_32(
        &mut self,
        handle: Value,
        coords: Value,
        value: Value,
        info: u32,
    ) -> Value {
        self.image_atomic(
            Opcode::BoundImageAtomicDec32,
            Opcode::BindlessImageAtomicDec32,
            handle,
            coords,
            value,
            info,
        )
    }

    pub fn image_atomic_and_32(
        &mut self,
        handle: Value,
        coords: Value,
        value: Value,
        info: u32,
    ) -> Value {
        self.image_atomic(
            Opcode::BoundImageAtomicAnd32,
            Opcode::BindlessImageAtomicAnd32,
            handle,
            coords,
            value,
            info,
        )
    }

    pub fn image_atomic_or_32(
        &mut self,
        handle: Value,
        coords: Value,
        value: Value,
        info: u32,
    ) -> Value {
        self.image_atomic(
            Opcode::BoundImageAtomicOr32,
            Opcode::BindlessImageAtomicOr32,
            handle,
            coords,
            value,
            info,
        )
    }

    pub fn image_atomic_xor_32(
        &mut self,
        handle: Value,
        coords: Value,
        value: Value,
        info: u32,
    ) -> Value {
        self.image_atomic(
            Opcode::BoundImageAtomicXor32,
            Opcode::BindlessImageAtomicXor32,
            handle,
            coords,
            value,
            info,
        )
    }

    pub fn image_atomic_exchange_32(
        &mut self,
        handle: Value,
        coords: Value,
        value: Value,
        info: u32,
    ) -> Value {
        self.image_atomic(
            Opcode::BoundImageAtomicExchange32,
            Opcode::BindlessImageAtomicExchange32,
            handle,
            coords,
            value,
            info,
        )
    }

    // ── Warp / Subgroup ───────────────────────────────────────────────

    pub fn vote_all(&mut self, pred: Value) -> Value {
        self.emit(Inst::new(Opcode::VoteAll, vec![pred]))
    }

    pub fn vote_any(&mut self, pred: Value) -> Value {
        self.emit(Inst::new(Opcode::VoteAny, vec![pred]))
    }

    pub fn vote_equal(&mut self, pred: Value) -> Value {
        self.emit(Inst::new(Opcode::VoteEqual, vec![pred]))
    }

    pub fn subgroup_ballot(&mut self, pred: Value) -> Value {
        self.emit(Inst::new(Opcode::SubgroupBallot, vec![pred]))
    }

    pub fn shuffle_up(
        &mut self,
        value: Value,
        index: Value,
        clamp: Value,
        seg_mask: Value,
    ) -> Value {
        self.emit(Inst::new(
            Opcode::ShuffleUp,
            vec![value, index, clamp, seg_mask],
        ))
    }

    pub fn shuffle_down(
        &mut self,
        value: Value,
        index: Value,
        clamp: Value,
        seg_mask: Value,
    ) -> Value {
        self.emit(Inst::new(
            Opcode::ShuffleDown,
            vec![value, index, clamp, seg_mask],
        ))
    }

    pub fn shuffle_butterfly(
        &mut self,
        value: Value,
        index: Value,
        clamp: Value,
        seg_mask: Value,
    ) -> Value {
        self.emit(Inst::new(
            Opcode::ShuffleButterfly,
            vec![value, index, clamp, seg_mask],
        ))
    }

    pub fn shuffle_index(
        &mut self,
        value: Value,
        index: Value,
        clamp: Value,
        seg_mask: Value,
    ) -> Value {
        self.emit(Inst::new(
            Opcode::ShuffleIndex,
            vec![value, index, clamp, seg_mask],
        ))
    }

    pub fn subgroup_eq_mask(&mut self) -> Value {
        self.emit(Inst::new(Opcode::SubgroupEqMask, vec![]))
    }

    pub fn subgroup_lt_mask(&mut self) -> Value {
        self.emit(Inst::new(Opcode::SubgroupLtMask, vec![]))
    }

    pub fn subgroup_le_mask(&mut self) -> Value {
        self.emit(Inst::new(Opcode::SubgroupLeMask, vec![]))
    }

    pub fn subgroup_gt_mask(&mut self) -> Value {
        self.emit(Inst::new(Opcode::SubgroupGtMask, vec![]))
    }

    pub fn subgroup_ge_mask(&mut self) -> Value {
        self.emit(Inst::new(Opcode::SubgroupGeMask, vec![]))
    }

    pub fn dpdx_fine(&mut self, value: Value) -> Value {
        self.emit(Inst::new(Opcode::DPdxFine, vec![value]))
    }

    pub fn dpdy_fine(&mut self, value: Value) -> Value {
        self.emit(Inst::new(Opcode::DPdyFine, vec![value]))
    }

    pub fn dpdx_coarse(&mut self, value: Value) -> Value {
        self.emit(Inst::new(Opcode::DPdxCoarse, vec![value]))
    }

    pub fn dpdy_coarse(&mut self, value: Value) -> Value {
        self.emit(Inst::new(Opcode::DPdyCoarse, vec![value]))
    }

    pub fn y_direction(&mut self) -> Value {
        self.emit(Inst::new(Opcode::YDirection, vec![]))
    }

    // ── Geometry ──────────────────────────────────────────────────────

    pub fn emit_vertex(&mut self, stream: Value) -> Value {
        self.emit(Inst::new(Opcode::EmitVertex, vec![stream]))
    }

    pub fn end_primitive(&mut self, stream: Value) -> Value {
        self.emit(Inst::new(Opcode::EndPrimitive, vec![stream]))
    }

    // ── System values ────────────────────────────────────────────────

    pub fn sample_id(&mut self) -> Value {
        self.emit(Inst::new(Opcode::SampleId, vec![]))
    }

    // ── Helper: apply abs/neg modifiers ───────────────────────────────

    /// Apply floating-point absolute value and negation modifiers.
    pub fn fp_abs_neg_32(&mut self, value: Value, abs: bool, neg: bool) -> Value {
        let mut result = value;
        if abs {
            result = self.fp_abs_32(result);
        }
        if neg {
            result = self.fp_neg_32(result);
        }
        result
    }

    // ── FP64 comparison ───────────────────────────────────────────────

    pub fn fp_ord_equal_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPOrdEqual64, vec![a, b]))
    }

    pub fn fp_ord_not_equal_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPOrdNotEqual64, vec![a, b]))
    }

    pub fn fp_ord_less_than_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPOrdLessThan64, vec![a, b]))
    }

    pub fn fp_ord_greater_than_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPOrdGreaterThan64, vec![a, b]))
    }

    pub fn fp_ord_less_than_equal_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPOrdLessThanEqual64, vec![a, b]))
    }

    pub fn fp_ord_greater_than_equal_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPOrdGreaterThanEqual64, vec![a, b]))
    }

    pub fn fp_unord_equal_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPUnordEqual64, vec![a, b]))
    }

    pub fn fp_unord_not_equal_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPUnordNotEqual64, vec![a, b]))
    }

    pub fn fp_unord_less_than_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPUnordLessThan64, vec![a, b]))
    }

    pub fn fp_unord_greater_than_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPUnordGreaterThan64, vec![a, b]))
    }

    pub fn fp_unord_less_than_equal_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPUnordLessThanEqual64, vec![a, b]))
    }

    pub fn fp_unord_greater_than_equal_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPUnordGreaterThanEqual64, vec![a, b]))
    }

    pub fn fp_is_nan_64(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPIsNan64, vec![a]))
    }

    pub fn fp_abs_neg_64(&mut self, value: Value, abs: bool, neg: bool) -> Value {
        let mut result = value;
        if abs {
            result = self.fp_abs_64(result);
        }
        if neg {
            result = self.fp_neg_64(result);
        }
        result
    }

    // ── FP32 swizzled add ─────────────────────────────────────────────

    pub fn fp_swizzle_add(&mut self, a: Value, b: Value, swizzle: Value) -> Value {
        self.emit(Inst::new(Opcode::FSwizzleAdd, vec![a, b, swizzle]))
    }

    pub fn fp_swizzle_add_with_control(
        &mut self,
        a: Value,
        b: Value,
        swizzle: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FSwizzleAdd,
            vec![a, b, swizzle],
            control.to_u32(),
        ))
    }

    // ── FP16 arithmetic ───────────────────────────────────────────────

    pub fn fp_abs_16(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPAbs16, vec![a]))
    }

    pub fn fp_neg_16(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPNeg16, vec![a]))
    }

    pub fn fp_add_16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPAdd16, vec![a, b]))
    }

    pub fn fp_add_16_with_control(&mut self, a: Value, b: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPAdd16,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_round_even_16(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPRoundEven16, vec![a]))
    }

    pub fn fp_round_even_16_with_control(&mut self, a: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPRoundEven16,
            vec![a],
            control.to_u32(),
        ))
    }

    pub fn fp_floor_16(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPFloor16, vec![a]))
    }

    pub fn fp_floor_16_with_control(&mut self, a: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPFloor16,
            vec![a],
            control.to_u32(),
        ))
    }

    pub fn fp_ceil_16(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPCeil16, vec![a]))
    }

    pub fn fp_ceil_16_with_control(&mut self, a: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPCeil16,
            vec![a],
            control.to_u32(),
        ))
    }

    pub fn fp_trunc_16(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPTrunc16, vec![a]))
    }

    pub fn fp_trunc_16_with_control(&mut self, a: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPTrunc16,
            vec![a],
            control.to_u32(),
        ))
    }

    pub fn fp_mul_16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPMul16, vec![a, b]))
    }

    pub fn fp_mul_16_with_control(&mut self, a: Value, b: Value, control: FpControl) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPMul16,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_fma_16(&mut self, a: Value, b: Value, c: Value) -> Value {
        self.emit(Inst::new(Opcode::FPFma16, vec![a, b, c]))
    }

    pub fn fp_fma_16_with_control(
        &mut self,
        a: Value,
        b: Value,
        c: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPFma16,
            vec![a, b, c],
            control.to_u32(),
        ))
    }

    pub fn fp_min_16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPMin16, vec![a, b]))
    }

    pub fn fp_max_16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPMax16, vec![a, b]))
    }

    pub fn fp_saturate_16(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPSaturate16, vec![a]))
    }

    pub fn fp_abs_neg_16(&mut self, value: Value, abs: bool, neg: bool) -> Value {
        let mut result = value;
        if abs {
            result = self.fp_abs_16(result);
        }
        if neg {
            result = self.fp_neg_16(result);
        }
        result
    }

    // ── FP16 comparison ───────────────────────────────────────────────

    pub fn fp_ord_equal_16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPOrdEqual16, vec![a, b]))
    }

    pub fn fp_ord_equal_16_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPOrdEqual16,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_ord_not_equal_16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPOrdNotEqual16, vec![a, b]))
    }

    pub fn fp_ord_not_equal_16_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPOrdNotEqual16,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_ord_less_than_16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPOrdLessThan16, vec![a, b]))
    }

    pub fn fp_ord_less_than_16_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPOrdLessThan16,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_ord_greater_than_16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPOrdGreaterThan16, vec![a, b]))
    }

    pub fn fp_ord_greater_than_16_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPOrdGreaterThan16,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_ord_less_than_equal_16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPOrdLessThanEqual16, vec![a, b]))
    }

    pub fn fp_ord_less_than_equal_16_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPOrdLessThanEqual16,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_ord_greater_than_equal_16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPOrdGreaterThanEqual16, vec![a, b]))
    }

    pub fn fp_ord_greater_than_equal_16_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPOrdGreaterThanEqual16,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_unord_equal_16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPUnordEqual16, vec![a, b]))
    }

    pub fn fp_unord_equal_16_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPUnordEqual16,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_unord_not_equal_16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPUnordNotEqual16, vec![a, b]))
    }

    pub fn fp_unord_not_equal_16_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPUnordNotEqual16,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_unord_less_than_16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPUnordLessThan16, vec![a, b]))
    }

    pub fn fp_unord_less_than_16_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPUnordLessThan16,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_unord_greater_than_16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPUnordGreaterThan16, vec![a, b]))
    }

    pub fn fp_unord_greater_than_16_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPUnordGreaterThan16,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_unord_less_than_equal_16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPUnordLessThanEqual16, vec![a, b]))
    }

    pub fn fp_unord_less_than_equal_16_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPUnordLessThanEqual16,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_unord_greater_than_equal_16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::FPUnordGreaterThanEqual16, vec![a, b]))
    }

    pub fn fp_unord_greater_than_equal_16_with_control(
        &mut self,
        a: Value,
        b: Value,
        control: FpControl,
    ) -> Value {
        self.emit(Inst::with_flags(
            Opcode::FPUnordGreaterThanEqual16,
            vec![a, b],
            control.to_u32(),
        ))
    }

    pub fn fp_is_nan_16(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::FPIsNan16, vec![a]))
    }

    // ── F16x2 composite operations ────────────────────────────────────

    pub fn unpack_float_2x16(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::UnpackFloat2x16, vec![a]))
    }

    pub fn pack_float_2x16(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::PackFloat2x16, vec![a]))
    }

    pub fn composite_construct_f16x2(&mut self, a: Value, b: Value) -> Value {
        self.emit(Inst::new(Opcode::CompositeConstructF16x2, vec![a, b]))
    }

    pub fn composite_extract_f16x2(&mut self, vec: Value, index: u32) -> Value {
        self.emit(Inst::new(
            Opcode::CompositeExtractF16x2,
            vec![vec, Value::ImmU32(index)],
        ))
    }

    pub fn composite_insert_f16x2(&mut self, vec: Value, val: Value, index: u32) -> Value {
        self.emit(Inst::new(
            Opcode::CompositeInsertF16x2,
            vec![vec, val, Value::ImmU32(index)],
        ))
    }

    // ── 64-bit integer packing ────────────────────────────────────────

    pub fn pack_uint_2x32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::PackUint2x32, vec![a]))
    }

    pub fn unpack_uint_2x32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::UnpackUint2x32, vec![a]))
    }

    pub fn composite_extract_u32x2_idx(&mut self, vec: Value, index: u32) -> Value {
        self.emit(Inst::new(
            Opcode::CompositeExtractU32x2,
            vec![vec, Value::ImmU32(index)],
        ))
    }

    // ── 64-bit shift ──────────────────────────────────────────────────

    pub fn shift_left_logical_64(&mut self, base: Value, shift: Value) -> Value {
        self.emit(Inst::new(Opcode::ShiftLeftLogical64, vec![base, shift]))
    }

    pub fn shift_right_logical_64(&mut self, base: Value, shift: Value) -> Value {
        self.emit(Inst::new(Opcode::ShiftRightLogical64, vec![base, shift]))
    }

    pub fn shift_right_arithmetic_64(&mut self, base: Value, shift: Value) -> Value {
        self.emit(Inst::new(Opcode::ShiftRightArithmetic64, vec![base, shift]))
    }

    // ── Integer negate 64 ────────────────────────────────────────────

    pub fn ineg_64(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::INeg64, vec![a]))
    }

    // ── Shared memory ────────────────────────────────────────────────

    pub fn load_shared_u32(&mut self, offset: Value) -> Value {
        self.emit(Inst::new(Opcode::LoadSharedU32, vec![offset]))
    }

    pub fn load_shared_u8(&mut self, offset: Value) -> Value {
        self.emit(Inst::new(Opcode::LoadSharedU8, vec![offset]))
    }

    pub fn load_shared_s8(&mut self, offset: Value) -> Value {
        self.emit(Inst::new(Opcode::LoadSharedS8, vec![offset]))
    }

    pub fn load_shared_u16(&mut self, offset: Value) -> Value {
        self.emit(Inst::new(Opcode::LoadSharedU16, vec![offset]))
    }

    pub fn load_shared_s16(&mut self, offset: Value) -> Value {
        self.emit(Inst::new(Opcode::LoadSharedS16, vec![offset]))
    }

    pub fn load_shared_u64(&mut self, offset: Value) -> Value {
        self.emit(Inst::new(Opcode::LoadSharedU64, vec![offset]))
    }

    pub fn load_shared_u128(&mut self, offset: Value) -> Value {
        self.emit(Inst::new(Opcode::LoadSharedU128, vec![offset]))
    }

    pub fn write_shared_u8(&mut self, offset: Value, value: Value) {
        self.emit_void(Inst::new(Opcode::WriteSharedU8, vec![offset, value]));
    }

    pub fn write_shared_u16(&mut self, offset: Value, value: Value) {
        self.emit_void(Inst::new(Opcode::WriteSharedU16, vec![offset, value]));
    }

    pub fn write_shared_u32(&mut self, offset: Value, value: Value) {
        self.emit_void(Inst::new(Opcode::WriteSharedU32, vec![offset, value]));
    }

    pub fn write_shared_u64(&mut self, offset: Value, value: Value) {
        self.emit_void(Inst::new(Opcode::WriteSharedU64, vec![offset, value]));
    }

    pub fn write_shared_u128(&mut self, offset: Value, value: Value) {
        self.emit_void(Inst::new(Opcode::WriteSharedU128, vec![offset, value]));
    }

    // ── Double (F64) packing from register pair ────────────────────────

    pub fn pack_double_2x32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::PackDouble2x32, vec![a]))
    }

    pub fn unpack_double_2x32(&mut self, a: Value) -> Value {
        self.emit(Inst::new(Opcode::UnpackDouble2x32, vec![a]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::ShaderStage;

    #[test]
    fn get_sparse_from_op_associates_pseudo_with_parent() {
        let mut program = Program::new(ShaderStage::Fragment);
        let block = program.add_block();
        let mut emitter = Emitter::new(&mut program, block);

        let sample = emitter.image_sample_implicit_lod(Value::ImmU32(0), Value::ImmF32(0.0), 0);
        let sparse = emitter.get_sparse_from_op(sample);

        let Value::Inst(sample_ref) = sample else {
            panic!("sample should be an instruction value");
        };
        let Value::Inst(sparse_ref) = sparse else {
            panic!("sparse should be an instruction value");
        };
        assert_eq!(
            emitter
                .program
                .block(sample_ref.block)
                .inst(sample_ref.inst)
                .get_associated_pseudo(Opcode::GetSparseFromOp),
            Some(sparse_ref)
        );
    }

    #[test]
    fn shuffle_preserves_clamp_and_associates_in_bounds_pseudo() {
        let mut program = Program::new(ShaderStage::Compute);
        let block = program.add_block();
        let mut emitter = Emitter::new(&mut program, block);

        let shuffle = emitter.shuffle_index(
            Value::ImmU32(1),
            Value::ImmU32(2),
            Value::ImmU32(3),
            Value::ImmU32(4),
        );
        let in_bounds = emitter.get_in_bounds_from_op(shuffle);

        let Value::Inst(shuffle_ref) = shuffle else {
            panic!("shuffle should be an instruction value");
        };
        let Value::Inst(in_bounds_ref) = in_bounds else {
            panic!("in-bounds result should be an instruction value");
        };
        let shuffle = emitter
            .program
            .block(shuffle_ref.block)
            .inst(shuffle_ref.inst);
        assert_eq!(
            shuffle.args,
            vec![
                Value::ImmU32(1),
                Value::ImmU32(2),
                Value::ImmU32(3),
                Value::ImmU32(4),
            ]
        );
        assert_eq!(
            shuffle.get_associated_pseudo(Opcode::GetInBoundsFromOp),
            Some(in_bounds_ref)
        );
    }

    #[test]
    fn pt_reads_are_immediate_and_writes_are_discarded() {
        let mut program = Program::new(ShaderStage::VertexB);
        let block = program.add_block();
        let mut emitter = Emitter::new(&mut program, block);

        assert_eq!(emitter.get_pred(Pred::PT, false), Value::ImmU1(true));
        assert_eq!(emitter.get_pred(Pred::PT, true), Value::ImmU1(false));
        emitter.set_pred(Pred::PT, Value::ImmU1(false));

        assert!(emitter.program.block(block).is_empty());
    }

    #[test]
    fn condition_combines_predicate_with_flow_test_like_upstream() {
        use crate::ir::condition::{Condition, IrPred};

        let mut program = Program::new(ShaderStage::Fragment);
        let block = program.add_block();
        let mut emitter = Emitter::new(&mut program, block);

        let result = emitter.condition(Condition::new(FlowTest::NEU, IrPred::P0, false));
        let Value::Inst(result_ref) = result else {
            panic!("condition should produce an instruction");
        };
        assert_eq!(
            emitter.program.block(block).inst(result_ref.inst).opcode,
            Opcode::LogicalAnd
        );
        for opcode in [
            Opcode::GetPred,
            Opcode::GetSFlag,
            Opcode::GetZFlag,
            Opcode::LogicalNot,
            Opcode::LogicalOr,
        ] {
            assert!(
                emitter
                    .program
                    .block(block)
                    .iter()
                    .any(|inst| inst.opcode == opcode),
                "missing {opcode:?}"
            );
        }
    }
}

// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Maxwell shader execution engine.
//!
//! Provides `ShaderExecContext` which can run vertex and fragment shader programs
//! by interpreting decoded Maxwell instructions. Register file is 256 × 32-bit
//! (R255 = RZ, reads zero, writes discarded). Predicate file is 8 booleans
//! (P7 = PT, always true).

use std::collections::HashMap;

use super::decoder::*;
use super::ShaderProgram;

/// Constant buffer binding used during shader execution.
#[derive(Debug, Clone)]
pub struct CbBinding {
    pub address: u64,
    pub size: u32,
}

/// Texture sample result.
#[derive(Debug, Clone, Copy)]
pub struct TexSample {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Default for TexSample {
    fn default() -> Self {
        Self {
            r: 1.0,
            g: 0.0,
            b: 1.0,
            a: 1.0,
        }
    }
}

/// Shader execution context.
pub struct ShaderExecContext<'a> {
    /// 256 general-purpose 32-bit registers. R255 (RZ) reads as 0.
    pub regs: [u32; 256],
    /// 8 predicate registers. P7 (PT) is always true.
    pub preds: [bool; 8],
    /// Program counter (index into instructions vec).
    pub pc: usize,
    /// Whether the shader has exited.
    pub exited: bool,
    /// Whether the fragment was killed (discard).
    pub killed: bool,
    /// Remaining instruction budget.
    pub budget: u32,
    /// Output attribute writes (key = byte offset, value = f32).
    pub output_attrs: HashMap<u32, f32>,
    /// GPU memory read callback.
    pub read_gpu: &'a dyn Fn(u64, &mut [u8]),
    /// GPU memory write callback (for STG instruction).
    pub write_gpu: Option<&'a dyn Fn(u64, &[u8])>,
    /// Constant buffer bindings (indexed by cbuf slot).
    pub cb_bindings: Vec<CbBinding>,
    /// Input vertex attributes (indexed by hw attribute index).
    pub input_attribs: Vec<[f32; 4]>,
    /// Input fragment varyings (indexed by generic attribute index).
    pub input_varyings: Vec<[f32; 4]>,
    /// Texture sampling callback: (tex_index, u, v) -> TexSample.
    pub sample_texture: &'a dyn Fn(u32, f32, f32) -> TexSample,
    /// Vertex ID for S2R.
    pub vertex_id: u32,
    /// Instance ID for S2R.
    pub instance_id: u32,
}

impl<'a> ShaderExecContext<'a> {
    /// Create a new execution context for a shader.
    pub fn new(
        read_gpu: &'a dyn Fn(u64, &mut [u8]),
        sample_texture: &'a dyn Fn(u32, f32, f32) -> TexSample,
    ) -> Self {
        let mut preds = [false; 8];
        preds[7] = true; // PT = always true
        Self {
            regs: [0u32; 256],
            preds,
            pc: 0,
            exited: false,
            killed: false,
            budget: 65536,
            output_attrs: HashMap::new(),
            read_gpu,
            write_gpu: None,
            cb_bindings: Vec::new(),
            input_attribs: Vec::new(),
            input_varyings: Vec::new(),
            sample_texture,
            vertex_id: 0,
            instance_id: 0,
        }
    }

    /// Read a register as u32. R255 (RZ) always returns 0.
    #[inline]
    fn read_reg(&self, idx: u8) -> u32 {
        if idx == 255 {
            0
        } else {
            self.regs[idx as usize]
        }
    }

    /// Read a register as f32.
    #[inline]
    fn read_reg_f32(&self, idx: u8) -> f32 {
        f32::from_bits(self.read_reg(idx))
    }

    /// Write a register. Writes to R255 are discarded.
    #[inline]
    fn write_reg(&mut self, idx: u8, val: u32) {
        if idx != 255 {
            self.regs[idx as usize] = val;
        }
    }

    /// Write a register as f32.
    #[inline]
    fn write_reg_f32(&mut self, idx: u8, val: f32) {
        self.write_reg(idx, val.to_bits());
    }

    /// Read a predicate register. P7 (PT) always returns true.
    #[inline]
    fn read_pred(&self, idx: u8) -> bool {
        if idx >= 7 {
            true
        } else {
            self.preds[idx as usize]
        }
    }

    /// Write a predicate register. Writes to P7 are discarded.
    #[inline]
    fn write_pred(&mut self, idx: u8, val: bool) {
        if idx < 7 {
            self.preds[idx as usize] = val;
        }
    }

    /// Resolve a SrcB operand to a u32 value.
    fn resolve_src_b(&self, src: &SrcB) -> u32 {
        match src {
            SrcB::Reg(r) => self.read_reg(*r),
            SrcB::Cbuf { index, offset } => self.read_cbuf(*index, *offset as i32),
            SrcB::Imm20(val) => *val as u32,
        }
    }

    /// Resolve a SrcB operand to a f32 value.
    fn resolve_src_b_f32(&self, src: &SrcB) -> f32 {
        match src {
            SrcB::Reg(r) => self.read_reg_f32(*r),
            SrcB::Cbuf { index, offset } => f32::from_bits(self.read_cbuf(*index, *offset as i32)),
            SrcB::Imm20(val) => {
                // For FP instructions, the 20-bit immediate is interpreted as the
                // high 20 bits of a 32-bit float (low 12 bits = 0).
                let bits = ((*val as u32) & 0x000F_FFFF) << 12;
                f32::from_bits(bits)
            }
        }
    }

    /// Resolve a SrcC operand to a u32.
    fn resolve_src_c(&self, src: &SrcC) -> u32 {
        match src {
            SrcC::Reg(r) => self.read_reg(*r),
            SrcC::Cbuf { index, offset } => self.read_cbuf(*index, *offset as i32),
        }
    }

    /// Read a 32-bit word from a constant buffer.
    fn read_cbuf(&self, index: u8, offset: i32) -> u32 {
        let idx = index as usize;
        if idx >= self.cb_bindings.len() {
            return 0;
        }
        let cb = &self.cb_bindings[idx];
        if cb.address == 0 {
            return 0;
        }
        let byte_offset = offset as u64;
        if byte_offset + 4 > cb.size as u64 {
            return 0;
        }
        let mut buf = [0u8; 4];
        (self.read_gpu)(cb.address + byte_offset, &mut buf);
        u32::from_le_bytes(buf)
    }

    /// Saturate a float to [0.0, 1.0].
    #[inline]
    fn saturate(val: f32) -> f32 {
        val.clamp(0.0, 1.0)
    }

    /// Apply abs/neg modifiers to a float value.
    #[inline]
    fn apply_f32_mods(val: f32, abs: bool, neg: bool) -> f32 {
        let v = if abs { val.abs() } else { val };
        if neg {
            -v
        } else {
            v
        }
    }

    /// Execute the shader program to completion.
    pub fn run(&mut self, program: &ShaderProgram) {
        while !self.exited && self.budget > 0 {
            if self.pc >= program.instructions.len() {
                break;
            }

            let insn_word = program.instructions[self.pc];
            let (insn, guard) = decode(insn_word);

            self.pc += 1;
            self.budget -= 1;

            // Check predicate guard.
            if !guard.eval(&self.preds) {
                continue;
            }

            self.execute(&insn);
        }
    }

    /// Execute a single decoded instruction.
    fn execute(&mut self, insn: &Instruction) {
        match insn {
            // ── Floating point ───────────────────────────────────────
            Instruction::Fadd {
                dst,
                src_a,
                src_b,
                neg_a,
                neg_b,
                abs_a,
                abs_b,
                sat,
            } => {
                let a = Self::apply_f32_mods(self.read_reg_f32(*src_a), *abs_a, *neg_a);
                let b = Self::apply_f32_mods(self.resolve_src_b_f32(src_b), *abs_b, *neg_b);
                let result = a + b;
                let result = if *sat { Self::saturate(result) } else { result };
                self.write_reg_f32(*dst, result);
            }

            Instruction::Fmul {
                dst,
                src_a,
                src_b,
                neg_a,
                sat,
            } => {
                let a = self.read_reg_f32(*src_a);
                let a = if *neg_a { -a } else { a };
                let b = self.resolve_src_b_f32(src_b);
                let result = a * b;
                let result = if *sat { Self::saturate(result) } else { result };
                self.write_reg_f32(*dst, result);
            }

            Instruction::Ffma {
                dst,
                src_a,
                src_b,
                src_c,
                neg_b,
                neg_c,
                sat,
            } => {
                let a = self.read_reg_f32(*src_a);
                let b = self.resolve_src_b_f32(src_b);
                let b = if *neg_b { -b } else { b };
                let c = f32::from_bits(self.resolve_src_c(src_c));
                let c = if *neg_c { -c } else { c };
                let result = a.mul_add(b, c);
                let result = if *sat { Self::saturate(result) } else { result };
                self.write_reg_f32(*dst, result);
            }

            Instruction::Mufu { dst, src_a, op } => {
                let a = self.read_reg_f32(*src_a);
                let result = match op {
                    MufuOp::Cos => a.cos(),
                    MufuOp::Sin => a.sin(),
                    MufuOp::Ex2 => (a * std::f32::consts::LN_2).exp(),
                    MufuOp::Lg2 => a.log2(),
                    MufuOp::Rcp => 1.0 / a,
                    MufuOp::Rsq => 1.0 / a.sqrt(),
                    MufuOp::Sqrt => a.sqrt(),
                    MufuOp::Rcp64H | MufuOp::Rsq64H => {
                        // 64-bit variants: just use f32 approximation.
                        if matches!(op, MufuOp::Rcp64H) {
                            1.0 / a
                        } else {
                            1.0 / a.sqrt()
                        }
                    }
                };
                self.write_reg_f32(*dst, result);
            }

            // ── Data movement ────────────────────────────────────────
            Instruction::Mov { dst, src_b } => {
                let val = self.resolve_src_b(src_b);
                self.write_reg(*dst, val);
            }

            Instruction::Mov32i { dst, imm32 } => {
                self.write_reg(*dst, *imm32);
            }

            Instruction::Sel {
                dst,
                src_a,
                src_b,
                pred,
                neg_pred,
            } => {
                let p = self.read_pred(*pred);
                let p = if *neg_pred { !p } else { p };
                let val = if p {
                    self.read_reg(*src_a)
                } else {
                    self.resolve_src_b(src_b)
                };
                self.write_reg(*dst, val);
            }

            Instruction::Ldc {
                dst,
                src_reg,
                cb_index,
                cb_offset,
            } => {
                let reg_offset = self.read_reg(*src_reg) as i32;
                let offset = reg_offset + *cb_offset;
                let val = self.read_cbuf(*cb_index, offset);
                self.write_reg(*dst, val);
            }

            // ── Integer arithmetic ───────────────────────────────────
            Instruction::Iadd {
                dst,
                src_a,
                src_b,
                neg_a,
                neg_b,
            } => {
                let a = self.read_reg(*src_a) as i32;
                let a = if *neg_a { -a } else { a };
                let b = self.resolve_src_b(src_b) as i32;
                let b = if *neg_b { -b } else { b };
                self.write_reg(*dst, a.wrapping_add(b) as u32);
            }

            Instruction::Iadd32i { dst, src_a, imm32 } => {
                let a = self.read_reg(*src_a);
                self.write_reg(*dst, a.wrapping_add(*imm32));
            }

            Instruction::Iscadd {
                dst,
                src_a,
                src_b,
                shift,
            } => {
                let a = self.read_reg(*src_a);
                let b = self.resolve_src_b(src_b);
                let result = (a << (*shift & 31)).wrapping_add(b);
                self.write_reg(*dst, result);
            }

            Instruction::Shl { dst, src_a, src_b } => {
                let a = self.read_reg(*src_a);
                let shift = self.resolve_src_b(src_b) & 31;
                self.write_reg(*dst, a << shift);
            }

            Instruction::Shr {
                dst,
                src_a,
                src_b,
                is_signed,
            } => {
                let shift = self.resolve_src_b(src_b) & 31;
                if *is_signed {
                    let a = self.read_reg(*src_a) as i32;
                    self.write_reg(*dst, (a >> shift) as u32);
                } else {
                    let a = self.read_reg(*src_a);
                    self.write_reg(*dst, a >> shift);
                }
            }

            Instruction::Lop {
                dst,
                src_a,
                src_b,
                op,
                invert_a,
                invert_b,
            } => {
                let a = self.read_reg(*src_a);
                let a = if *invert_a { !a } else { a };
                let b = self.resolve_src_b(src_b);
                let b = if *invert_b { !b } else { b };
                let result = match op {
                    0 => a & b,
                    1 => a | b,
                    2 => a ^ b,
                    _ => b, // PASS_B
                };
                self.write_reg(*dst, result);
            }

            Instruction::Lop32i {
                dst,
                src_a,
                imm32,
                op,
                invert_a,
            } => {
                let a = self.read_reg(*src_a);
                let a = if *invert_a { !a } else { a };
                let result = match op {
                    0 => a & *imm32,
                    1 => a | *imm32,
                    2 => a ^ *imm32,
                    _ => *imm32,
                };
                self.write_reg(*dst, result);
            }

            // ── Predicate set ────────────────────────────────────────
            Instruction::Fsetp {
                pred_a,
                pred_b,
                src_a,
                src_b,
                compare,
                bop,
                bop_pred,
                neg_bop_pred,
                neg_a,
                neg_b,
                abs_a,
                abs_b,
                ftz: _,
            } => {
                let a = Self::apply_f32_mods(self.read_reg_f32(*src_a), *abs_a, *neg_a);
                let b = Self::apply_f32_mods(self.resolve_src_b_f32(src_b), *abs_b, *neg_b);
                let cmp_result = compare.eval(a, b);

                let bop_p = self.read_pred(*bop_pred);
                let bop_p = if *neg_bop_pred { !bop_p } else { bop_p };

                let result_a = bop.eval(cmp_result, bop_p);
                let result_b = bop.eval(!cmp_result, bop_p);

                self.write_pred(*pred_a, result_a);
                self.write_pred(*pred_b, result_b);
            }

            Instruction::Isetp {
                pred_a,
                pred_b,
                src_a,
                src_b,
                compare,
                bop,
                bop_pred,
                neg_bop_pred,
                is_signed,
            } => {
                let a = self.read_reg(*src_a);
                let b = self.resolve_src_b(src_b);
                let cmp_result = if *is_signed {
                    compare.eval_signed(a as i32, b as i32)
                } else {
                    compare.eval_unsigned(a, b)
                };

                let bop_p = self.read_pred(*bop_pred);
                let bop_p = if *neg_bop_pred { !bop_p } else { bop_p };

                let result_a = bop.eval(cmp_result, bop_p);
                let result_b = bop.eval(!cmp_result, bop_p);

                self.write_pred(*pred_a, result_a);
                self.write_pred(*pred_b, result_b);
            }

            // ── Conversion ───────────────────────────────────────────
            Instruction::F2i {
                dst,
                src_b,
                dst_signed,
            } => {
                let f = self.resolve_src_b_f32(src_b);
                let val = if *dst_signed {
                    let clamped = f.clamp(i32::MIN as f32, i32::MAX as f32);
                    clamped as i32 as u32
                } else {
                    let clamped = f.clamp(0.0, u32::MAX as f32);
                    clamped as u32
                };
                self.write_reg(*dst, val);
            }

            Instruction::I2f {
                dst,
                src_b,
                src_signed,
            } => {
                let i = self.resolve_src_b(src_b);
                let f = if *src_signed {
                    (i as i32) as f32
                } else {
                    i as f32
                };
                self.write_reg_f32(*dst, f);
            }

            // ── Attribute access ─────────────────────────────────────
            Instruction::Ald {
                dst,
                index_reg: _,
                offset,
                count,
            } => {
                // Attribute offset: offset/16 = attribute index, offset%16/4 = component.
                let attr_idx = (*offset as usize) / 16;
                let comp_start = ((*offset as usize) % 16) / 4;

                for i in 0..(*count as usize) {
                    let comp = comp_start + i;
                    let val = if attr_idx < self.input_attribs.len() && comp < 4 {
                        self.input_attribs[attr_idx][comp]
                    } else {
                        0.0
                    };
                    let reg_idx = (*dst).wrapping_add(i as u8);
                    self.write_reg_f32(reg_idx, val);
                }
            }

            Instruction::Ast {
                src,
                index_reg: _,
                offset,
                count,
            } => {
                for i in 0..(*count as usize) {
                    let byte_offset = *offset as u32 + (i as u32) * 4;
                    let reg_idx = (*src).wrapping_add(i as u8);
                    let val = self.read_reg_f32(reg_idx);
                    self.output_attrs.insert(byte_offset, val);
                }
            }

            Instruction::Ipa {
                dst,
                attr_offset,
                interp_mode,
                sample_mode: _,
                multiplier_reg,
                sat,
            } => {
                // Fragment shader interpolation.
                // attr_offset is the attribute byte offset. Generic attrs start at 0x80.
                // Position is at 0x70. We map: generic index = (attr_offset - 0x80) / 16,
                // component = ((attr_offset - 0x80) % 16) / 4.
                let offset = *attr_offset as usize;

                let val = if offset >= 0x80 {
                    let generic_idx = (offset - 0x80) / 16;
                    let comp = ((offset - 0x80) % 16) / 4;
                    if generic_idx < self.input_varyings.len() && comp < 4 {
                        self.input_varyings[generic_idx][comp]
                    } else {
                        0.0
                    }
                } else if (0x70..0x80).contains(&offset) {
                    // Position (gl_FragCoord).
                    let comp = (offset - 0x70) / 4;
                    if comp < self.input_varyings.len() && !self.input_varyings.is_empty() {
                        // Position data would come from a separate source.
                        // For now, return 0.
                        0.0
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };

                let val = match interp_mode {
                    IpaInterpMode::Multiply => val * self.read_reg_f32(*multiplier_reg),
                    IpaInterpMode::Constant => {
                        // Flat shading: use provoking vertex value directly.
                        val
                    }
                    _ => val, // Pass/Sc: use interpolated value
                };

                let val = if *sat { Self::saturate(val) } else { val };
                self.write_reg_f32(*dst, val);
            }

            // ── Texture ──────────────────────────────────────────────
            Instruction::Tex {
                dst,
                coord_reg,
                mask,
                cbuf_offset,
            } => {
                let u = self.read_reg_f32(*coord_reg);
                let v = self.read_reg_f32(coord_reg.wrapping_add(1));
                let tex_idx = (*cbuf_offset as u32) * 4; // Convert from 4-byte units
                let sample = (self.sample_texture)(tex_idx, u, v);

                let components = [sample.r, sample.g, sample.b, sample.a];
                let mut reg = *dst;
                for i in 0..4u8 {
                    if (*mask >> i) & 1 != 0 {
                        self.write_reg_f32(reg, components[i as usize]);
                        reg = reg.wrapping_add(1);
                    }
                }
            }

            Instruction::Texs {
                dst_a,
                dst_b,
                coord_reg,
                tex_type: _,
            } => {
                let u = self.read_reg_f32(*coord_reg);
                let v = self.read_reg_f32(coord_reg.wrapping_add(1));
                let sample = (self.sample_texture)(0, u, v);

                // TEXS writes up to 4 components split across dst_a and dst_b.
                // dst_a gets R,G and dst_b gets B,A (simplified).
                self.write_reg_f32(*dst_a, sample.r);
                self.write_reg_f32(dst_a.wrapping_add(1), sample.g);
                self.write_reg_f32(*dst_b, sample.b);
                self.write_reg_f32(dst_b.wrapping_add(1), sample.a);
            }

            // ── System ───────────────────────────────────────────────
            Instruction::S2r { dst, sys_reg } => {
                let val = match sys_reg {
                    SystemReg::LaneId => 0,
                    SystemReg::TidX | SystemReg::Tid => self.vertex_id,
                    SystemReg::TidY => 0,
                    SystemReg::TidZ => 0,
                    SystemReg::CtaidX => 0,
                    SystemReg::CtaidY => 0,
                    SystemReg::CtaidZ => 0,
                    SystemReg::InvocationId => self.instance_id,
                    SystemReg::EqMask => 1, // Single thread: just self
                    SystemReg::GeMask => 0xFFFF_FFFF, // All threads >= self
                    SystemReg::GtMask => 0xFFFF_FFFE, // All threads > self
                    SystemReg::LeMask => 1,
                    SystemReg::LtMask => 0,
                    _ => 0,
                };
                self.write_reg(*dst, val);
            }

            // ── Extended multiply-add (XMAD) ──────────────────────────
            Instruction::Xmad {
                dst,
                src_a,
                src_b,
                src_c,
                mode,
                hi_a,
                hi_b,
                psl,
            } => {
                let a_raw = self.read_reg(*src_a);
                let a = if *hi_a {
                    (a_raw >> 16) & 0xFFFF
                } else {
                    a_raw & 0xFFFF
                };
                let b_raw = self.resolve_src_b(src_b);
                let b = if *hi_b {
                    (b_raw >> 16) & 0xFFFF
                } else {
                    b_raw & 0xFFFF
                };
                let c = self.resolve_src_c(src_c);

                let product = a.wrapping_mul(b);
                let product = if *psl { product << 16 } else { product };

                let result = match mode {
                    0 => product.wrapping_add(c),              // CLO
                    1 => product.wrapping_add(c & 0xFFFF0000), // CHI: add only high 16 of C
                    2 => {
                        // CSFU: sign extend product, add C
                        let sign_ext = if product & 0x8000 != 0 {
                            product | 0xFFFF_0000
                        } else {
                            product
                        };
                        sign_ext.wrapping_add(c)
                    }
                    3 => {
                        // CBCC: carry-based combine (product + C with carry)
                        product.wrapping_add(c)
                    }
                    _ => product.wrapping_add(c),
                };
                self.write_reg(*dst, result);
            }

            // ── Integer min/max ─────────────────────────────────────────
            Instruction::Imnmx {
                dst,
                src_a,
                src_b,
                pred,
                neg_pred,
                is_signed,
            } => {
                let p = self.read_pred(*pred);
                let p = if *neg_pred { !p } else { p };
                let a = self.read_reg(*src_a);
                let b = self.resolve_src_b(src_b);
                let result = if *is_signed {
                    let (a_s, b_s) = (a as i32, b as i32);
                    (if p { a_s.min(b_s) } else { a_s.max(b_s) }) as u32
                } else {
                    if p {
                        a.min(b)
                    } else {
                        a.max(b)
                    }
                };
                self.write_reg(*dst, result);
            }

            // ── Float min/max ───────────────────────────────────────────
            Instruction::Fmnmx {
                dst,
                src_a,
                src_b,
                pred,
                neg_pred,
                abs_a,
                abs_b,
                neg_a,
                neg_b,
            } => {
                let p = self.read_pred(*pred);
                let p = if *neg_pred { !p } else { p };
                let a = Self::apply_f32_mods(self.read_reg_f32(*src_a), *abs_a, *neg_a);
                let b = Self::apply_f32_mods(self.resolve_src_b_f32(src_b), *abs_b, *neg_b);
                let result = if p { a.min(b) } else { a.max(b) };
                self.write_reg_f32(*dst, result);
            }

            // ── Bit field extract ───────────────────────────────────────
            Instruction::Bfe {
                dst,
                src_a,
                src_b,
                is_signed,
            } => {
                let pack = self.resolve_src_b(src_b);
                let offset = (pack & 0xFF) as u32;
                let count = ((pack >> 8) & 0xFF) as u32;
                let val = self.read_reg(*src_a);
                let result = if count == 0 || offset >= 32 {
                    0
                } else if *is_signed {
                    let count = count.min(32 - offset);
                    let shifted = (val as i32) << (32 - offset - count);
                    (shifted >> (32 - count)) as u32
                } else {
                    let count = count.min(32 - offset);
                    let mask = if count >= 32 {
                        u32::MAX
                    } else {
                        (1u32 << count) - 1
                    };
                    (val >> offset) & mask
                };
                self.write_reg(*dst, result);
            }

            // ── Bit field insert ────────────────────────────────────────
            Instruction::Bfi {
                dst,
                src_a,
                src_b,
                src_c,
            } => {
                let pack = self.resolve_src_b(src_b);
                let offset = (pack & 0xFF) as u32;
                let count = ((pack >> 8) & 0xFF) as u32;
                let insert_val = self.read_reg(*src_a);
                let base = self.resolve_src_c(src_c);
                let result = if count == 0 || offset >= 32 {
                    base
                } else {
                    let count = count.min(32 - offset);
                    let mask = if count >= 32 {
                        u32::MAX
                    } else {
                        (1u32 << count) - 1
                    };
                    (base & !(mask << offset)) | ((insert_val & mask) << offset)
                };
                self.write_reg(*dst, result);
            }

            // ── Population count ────────────────────────────────────────
            Instruction::Popc { dst, src_b } => {
                let val = self.resolve_src_b(src_b);
                self.write_reg(*dst, val.count_ones());
            }

            // ── Find leading one ────────────────────────────────────────
            Instruction::Flo {
                dst,
                src_b,
                is_signed: _,
                invert,
            } => {
                let mut val = self.resolve_src_b(src_b);
                if *invert {
                    val = !val;
                }
                let result = if val == 0 {
                    0xFFFF_FFFFu32 // -1 to indicate not found
                } else {
                    31 - val.leading_zeros()
                };
                self.write_reg(*dst, result);
            }

            // ── Funnel shift ────────────────────────────────────────────
            Instruction::Shf {
                dst,
                src_a,
                src_b,
                src_c,
                direction_right,
                data_type_u64: _,
                hi,
            } => {
                let low = self.read_reg(*src_a);
                let shift = self.resolve_src_b(src_b);
                let high = self.resolve_src_c(src_c);
                let shift = if *hi { shift.min(63) } else { shift & 31 };

                let result = if *direction_right {
                    // (high:low) >> shift
                    let combined = ((high as u64) << 32) | (low as u64);
                    (combined >> shift) as u32
                } else {
                    // (high:low) << shift
                    let combined = ((high as u64) << 32) | (low as u64);
                    ((combined << shift) >> 32) as u32
                };
                self.write_reg(*dst, result);
            }

            // ── Integer set ─────────────────────────────────────────────
            Instruction::Iset {
                dst,
                src_a,
                src_b,
                compare,
                bop,
                bop_pred,
                neg_bop_pred,
                is_signed,
                bf,
            } => {
                let a = self.read_reg(*src_a);
                let b = self.resolve_src_b(src_b);
                let cmp_result = if *is_signed {
                    compare.eval_signed(a as i32, b as i32)
                } else {
                    compare.eval_unsigned(a, b)
                };
                let bop_p = self.read_pred(*bop_pred);
                let bop_p = if *neg_bop_pred { !bop_p } else { bop_p };
                let result = bop.eval(cmp_result, bop_p);
                if *bf {
                    // Boolean float: 1.0f or 0.0f
                    self.write_reg_f32(*dst, if result { 1.0 } else { 0.0 });
                } else {
                    self.write_reg(*dst, if result { 0xFFFF_FFFF } else { 0 });
                }
            }

            // ── Float set ───────────────────────────────────────────────
            Instruction::Fset {
                dst,
                src_a,
                src_b,
                compare,
                bop,
                bop_pred,
                neg_bop_pred,
                bf,
            } => {
                let a = self.read_reg_f32(*src_a);
                let b = self.resolve_src_b_f32(src_b);
                let cmp_result = compare.eval(a, b);
                let bop_p = self.read_pred(*bop_pred);
                let bop_p = if *neg_bop_pred { !bop_p } else { bop_p };
                let result = bop.eval(cmp_result, bop_p);
                if *bf {
                    self.write_reg_f32(*dst, if result { 1.0 } else { 0.0 });
                } else {
                    self.write_reg(*dst, if result { 0xFFFF_FFFF } else { 0 });
                }
            }

            // ── 3-input logic (LOP3) ────────────────────────────────────
            Instruction::Lop3 {
                dst,
                src_a,
                src_b,
                src_c,
                lut,
                pred_out: _,
            } => {
                let a = self.read_reg(*src_a);
                let b = self.resolve_src_b(src_b);
                let c = self.resolve_src_c(src_c);
                // Apply the 8-bit LUT to each bit position independently.
                let mut result = 0u32;
                for bit in 0..32 {
                    let a_bit = ((a >> bit) & 1) as u8;
                    let b_bit = ((b >> bit) & 1) as u8;
                    let c_bit = ((c >> bit) & 1) as u8;
                    let lut_idx = (a_bit << 2) | (b_bit << 1) | c_bit;
                    let out_bit = ((*lut >> lut_idx) & 1) as u32;
                    result |= out_bit << bit;
                }
                self.write_reg(*dst, result);
            }

            // ── Predicate to register ───────────────────────────────────
            Instruction::P2r { dst, src_a, src_b } => {
                let mask = self.resolve_src_b(src_b) as u8;
                let base = self.read_reg(*src_a);
                let mut pred_bits = 0u32;
                for i in 0..7u8 {
                    if self.preds[i as usize] {
                        pred_bits |= 1 << i;
                    }
                }
                let result = (base & !(mask as u32)) | (pred_bits & mask as u32);
                self.write_reg(*dst, result);
            }

            // ── Register to predicate ───────────────────────────────────
            Instruction::R2p {
                src_a,
                src_b,
                src_c: _,
            } => {
                let mask = self.resolve_src_b(src_b) as u8;
                let val = self.read_reg(*src_a);
                for i in 0..7u8 {
                    if (mask >> i) & 1 != 0 {
                        self.preds[i as usize] = (val >> i) & 1 != 0;
                    }
                }
            }

            // ── Range reduction (pass-through) ──────────────────────────
            Instruction::Rro { dst, src_b } => {
                // Pass through the value — RRO is a hardware optimization hint.
                let val = self.resolve_src_b(src_b);
                self.write_reg(*dst, val);
            }

            // ── Float-to-float conversion ───────────────────────────────
            Instruction::F2f {
                dst,
                src_b,
                abs_b,
                neg_b,
                sat,
                dst_size: _,
                src_size: _,
                rounding: _,
            } => {
                // Simplified: treat all sizes as f32 (most common case).
                let val = Self::apply_f32_mods(self.resolve_src_b_f32(src_b), *abs_b, *neg_b);
                let val = if *sat { Self::saturate(val) } else { val };
                self.write_reg_f32(*dst, val);
            }

            // ── Integer-to-integer conversion ───────────────────────────
            Instruction::I2i {
                dst,
                src_b,
                abs_b,
                sat,
                dst_signed,
                src_signed,
                dst_size,
                src_size,
            } => {
                let raw = self.resolve_src_b(src_b);
                // Extract source value based on source size.
                let val: i64 = match src_size {
                    0 => {
                        if *src_signed {
                            (raw as u8 as i8) as i64
                        } else {
                            (raw as u8) as i64
                        }
                    }
                    1 => {
                        if *src_signed {
                            (raw as u16 as i16) as i64
                        } else {
                            (raw as u16) as i64
                        }
                    }
                    _ => {
                        if *src_signed {
                            (raw as i32) as i64
                        } else {
                            raw as i64
                        }
                    }
                };
                let val = if *abs_b { val.abs() } else { val };
                // Clamp to destination range if saturating.
                let result = if *sat {
                    match (*dst_signed, dst_size) {
                        (true, 0) => val.clamp(i8::MIN as i64, i8::MAX as i64) as u32,
                        (true, 1) => val.clamp(i16::MIN as i64, i16::MAX as i64) as u32,
                        (true, _) => val.clamp(i32::MIN as i64, i32::MAX as i64) as u32,
                        (false, 0) => val.clamp(0, u8::MAX as i64) as u32,
                        (false, 1) => val.clamp(0, u16::MAX as i64) as u32,
                        (false, _) => val.clamp(0, u32::MAX as i64) as u32,
                    }
                } else {
                    val as u32
                };
                self.write_reg(*dst, result);
            }

            // ── Global memory load ──────────────────────────────────────
            Instruction::Ldg {
                dst,
                src_a,
                offset,
                size,
            } => {
                let base_lo = self.read_reg(*src_a) as u64;
                let base_hi = self.read_reg(src_a.wrapping_add(1)) as u64;
                let addr = ((base_hi << 32) | base_lo).wrapping_add(*offset as u64);
                match size {
                    0 | 1 => {
                        // U8/S8
                        let mut buf = [0u8; 1];
                        (self.read_gpu)(addr, &mut buf);
                        let val = if *size == 1 {
                            buf[0] as i8 as i32 as u32
                        } else {
                            buf[0] as u32
                        };
                        self.write_reg(*dst, val);
                    }
                    2 | 3 => {
                        // U16/S16
                        let mut buf = [0u8; 2];
                        (self.read_gpu)(addr, &mut buf);
                        let val = u16::from_le_bytes(buf);
                        let val = if *size == 3 {
                            val as i16 as i32 as u32
                        } else {
                            val as u32
                        };
                        self.write_reg(*dst, val);
                    }
                    4 => {
                        // 32-bit
                        let mut buf = [0u8; 4];
                        (self.read_gpu)(addr, &mut buf);
                        self.write_reg(*dst, u32::from_le_bytes(buf));
                    }
                    5 => {
                        // 64-bit: load into dst and dst+1
                        let mut buf = [0u8; 8];
                        (self.read_gpu)(addr, &mut buf);
                        self.write_reg(*dst, u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]));
                        self.write_reg(
                            dst.wrapping_add(1),
                            u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
                        );
                    }
                    6 => {
                        // 128-bit: load into dst..dst+3
                        let mut buf = [0u8; 16];
                        (self.read_gpu)(addr, &mut buf);
                        for i in 0..4u8 {
                            let off = (i as usize) * 4;
                            let val = u32::from_le_bytes([
                                buf[off],
                                buf[off + 1],
                                buf[off + 2],
                                buf[off + 3],
                            ]);
                            self.write_reg(dst.wrapping_add(i), val);
                        }
                    }
                    _ => {
                        // Unknown size — load as 32-bit.
                        let mut buf = [0u8; 4];
                        (self.read_gpu)(addr, &mut buf);
                        self.write_reg(*dst, u32::from_le_bytes(buf));
                    }
                }
            }

            // ── Global memory store ─────────────────────────────────────
            Instruction::Stg {
                src,
                src_a,
                offset,
                size,
            } => {
                let base_lo = self.read_reg(*src_a) as u64;
                let base_hi = self.read_reg(src_a.wrapping_add(1)) as u64;
                let addr = ((base_hi << 32) | base_lo).wrapping_add(*offset as u64);
                if let Some(write_fn) = &self.write_gpu {
                    match size {
                        0 | 1 => {
                            let val = self.read_reg(*src) as u8;
                            write_fn(addr, &[val]);
                        }
                        2 | 3 => {
                            let val = self.read_reg(*src) as u16;
                            write_fn(addr, &val.to_le_bytes());
                        }
                        4 => {
                            let val = self.read_reg(*src);
                            write_fn(addr, &val.to_le_bytes());
                        }
                        5 => {
                            let lo = self.read_reg(*src);
                            let hi = self.read_reg(src.wrapping_add(1));
                            let mut buf = [0u8; 8];
                            buf[0..4].copy_from_slice(&lo.to_le_bytes());
                            buf[4..8].copy_from_slice(&hi.to_le_bytes());
                            write_fn(addr, &buf);
                        }
                        6 => {
                            let mut buf = [0u8; 16];
                            for i in 0..4u8 {
                                let val = self.read_reg(src.wrapping_add(i));
                                let off = (i as usize) * 4;
                                buf[off..off + 4].copy_from_slice(&val.to_le_bytes());
                            }
                            write_fn(addr, &buf);
                        }
                        _ => {
                            let val = self.read_reg(*src);
                            write_fn(addr, &val.to_le_bytes());
                        }
                    }
                }
                // If no write_gpu callback, silently ignore (NOP).
            }

            // ── 3-operand integer add ───────────────────────────────────
            Instruction::Iadd3 {
                dst,
                src_a,
                src_b,
                src_c,
                neg_a,
                neg_b,
                neg_c,
            } => {
                let a = self.read_reg(*src_a) as i32;
                let a = if *neg_a { -a } else { a };
                let b = self.resolve_src_b(src_b) as i32;
                let b = if *neg_b { -b } else { b };
                let c = self.resolve_src_c(src_c) as i32;
                let c = if *neg_c { -c } else { c };
                self.write_reg(*dst, a.wrapping_add(b).wrapping_add(c) as u32);
            }

            // ── Control flow ─────────────────────────────────────────
            Instruction::Bra { offset } => {
                // Offset is in instruction units (not bytes).
                // pc has already been incremented, so adjust relative to current position.
                let target = (self.pc as i32 - 1 + *offset) as usize;
                self.pc = target;
            }

            Instruction::Exit => {
                self.exited = true;
            }

            Instruction::Nop | Instruction::Unknown { .. } => {
                // Do nothing.
            }
        }
    }

    /// Collect vertex shader output: position from output_attrs offsets 0x70-0x7C,
    /// generic outputs from 0x80+.
    pub fn collect_vertex_output(&self) -> super::VertexShaderOutput {
        let mut position = [0.0f32, 0.0, 0.0, 1.0];
        for i in 0..4u32 {
            if let Some(&val) = self.output_attrs.get(&(0x70 + i * 4)) {
                position[i as usize] = val;
            }
        }

        let mut generics: HashMap<u32, [f32; 4]> = HashMap::new();
        for (&offset, &val) in &self.output_attrs {
            if offset >= 0x80 {
                let generic_idx = (offset - 0x80) / 16;
                let comp = ((offset - 0x80) % 16) / 4;
                if comp < 4 {
                    let entry = generics.entry(generic_idx).or_insert([0.0; 4]);
                    entry[comp as usize] = val;
                }
            }
        }

        super::VertexShaderOutput { position, generics }
    }

    /// Collect fragment shader output: color from output_attrs offsets 0x00-0x0C.
    pub fn collect_fragment_output(&self) -> super::FragmentShaderOutput {
        let mut color = [0.0f32, 0.0, 0.0, 1.0];
        for i in 0..4u32 {
            if let Some(&val) = self.output_attrs.get(&(i * 4)) {
                color[i as usize] = val;
            }
        }

        let depth = self.output_attrs.get(&0x10).copied();

        super::FragmentShaderOutput {
            color,
            depth,
            killed: self.killed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shader::*;

    fn dummy_read_gpu(_addr: u64, buf: &mut [u8]) {
        buf.fill(0);
    }

    fn dummy_sample(_idx: u32, _u: f32, _v: f32) -> TexSample {
        TexSample::default()
    }

    fn make_program(instructions: Vec<u64>) -> ShaderProgram {
        ShaderProgram {
            header: ShaderProgramHeader {
                shader_type: 1,
                kills_pixels: false,
                omap_systemb: 0xF0,
                omap_generic_vector: [0; 16],
                ps_imap_generic: [0; 32],
                ps_omap_target: 0,
                ps_omap_misc: 0,
            },
            instructions,
        }
    }

    #[test]
    fn test_mov32i_and_exit() {
        // MOV32I R0, 0x3F800000 (1.0f)
        // EXIT
        let mov32i = (0x0100u64 << 48) | (0x3F80_0000u64 << 20) | 0x0007_0000;
        let exit = 0xE300_0000_0007_0000u64;

        let program = make_program(vec![mov32i, exit]);
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.run(&program);

        assert!(ctx.exited);
        assert_eq!(ctx.read_reg_f32(0), 1.0);
    }

    #[test]
    fn test_fadd_reg() {
        // Set up R1 = 2.0, R2 = 3.0 via MOV32I
        let mov_r1 = (0x0100u64 << 48) | ((2.0f32.to_bits() as u64) << 20) | 0x0007_0001;
        let mov_r2 = (0x0100u64 << 48) | ((3.0f32.to_bits() as u64) << 20) | 0x0007_0002;
        // FADD R0, R1, R2 — FADD_reg pattern 0x5C58, dst=0, src_a=1, src_b_reg=2
        let fadd = (0x5C58u64 << 48) | ((2u64) << 20) | 0x0007_0100;
        let exit = 0xE300_0000_0007_0000u64;

        let program = make_program(vec![mov_r1, mov_r2, fadd, exit]);
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.run(&program);

        let result = ctx.read_reg_f32(0);
        assert!((result - 5.0).abs() < 1e-6, "Expected 5.0, got {}", result);
    }

    #[test]
    fn test_ffma_reg() {
        // R1 = 2.0, R2 = 3.0, R3 = 1.0
        let mov_r1 = (0x0100u64 << 48) | ((2.0f32.to_bits() as u64) << 20) | 0x0007_0001;
        let mov_r2 = (0x0100u64 << 48) | ((3.0f32.to_bits() as u64) << 20) | 0x0007_0002;
        let mov_r3 = (0x0100u64 << 48) | ((1.0f32.to_bits() as u64) << 20) | 0x0007_0003;
        // FFMA R0, R1, R2, R3 — pattern 0x5980, dst=0, src_a=1, src_b_reg=2, src_c_reg=3
        // src_c at bits [46:39] = 3
        let ffma = (0x5980u64 << 48) | ((3u64) << 39) | ((2u64) << 20) | 0x0007_0100;
        let exit = 0xE300_0000_0007_0000u64;

        let program = make_program(vec![mov_r1, mov_r2, mov_r3, ffma, exit]);
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.run(&program);

        // 2.0 * 3.0 + 1.0 = 7.0
        let result = ctx.read_reg_f32(0);
        assert!((result - 7.0).abs() < 1e-6, "Expected 7.0, got {}", result);
    }

    #[test]
    fn test_ldc() {
        // Create a constant buffer with a value at offset 0.
        let val: f32 = 42.0;
        let cb_data = val.to_bits().to_le_bytes();

        let read_gpu = |addr: u64, buf: &mut [u8]| {
            if addr == 0x1000 && buf.len() == 4 {
                buf.copy_from_slice(&cb_data);
            } else {
                buf.fill(0);
            }
        };

        // LDC R0, c[0][0] — cb_index=0, offset=0
        // Pattern: 0xEF90
        let ldc = (0xEF90u64 << 48) | 0x0007_FF00; // src_reg=R255(RZ), dst=R0
        let exit = 0xE300_0000_0007_0000u64;

        let program = make_program(vec![ldc, exit]);
        let mut ctx = ShaderExecContext::new(&read_gpu, &dummy_sample);
        ctx.cb_bindings.push(CbBinding {
            address: 0x1000,
            size: 256,
        });
        ctx.run(&program);

        assert_eq!(ctx.read_reg_f32(0), 42.0);
    }

    #[test]
    fn test_ald_ast() {
        // Set input attribute 0 = [1.0, 2.0, 3.0, 4.0]
        // ALD R0, a[0x00].xyzw (offset=0, count=4)
        // Pattern: 0xEFD8
        let ald = (0xEFD8u64 << 48)
            | (3u64 << 47)  // size=3 (4 components)
            | (0u64 << 20)  // offset=0
            | 0x0007_FF00; // dst=R0, index_reg=RZ

        // AST a[0x70].xyzw, R0 (write position from R0-R3)
        // Pattern: 0xEFF0
        let ast = (0xEFF0u64 << 48)
            | (3u64 << 47)     // size=3 (4 components)
            | (0x70u64 << 20)  // offset=0x70 (position)
            | 0x0007_FF00; // src=R0, index_reg=RZ

        let exit = 0xE300_0000_0007_0000u64;

        let program = make_program(vec![ald, ast, exit]);
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.input_attribs.push([1.0, 2.0, 3.0, 4.0]);
        ctx.run(&program);

        let output = ctx.collect_vertex_output();
        assert!((output.position[0] - 1.0).abs() < 1e-6);
        assert!((output.position[1] - 2.0).abs() < 1e-6);
        assert!((output.position[2] - 3.0).abs() < 1e-6);
        assert!((output.position[3] - 4.0).abs() < 1e-6);
    }

    #[test]
    fn test_predicate_branch() {
        // R0 = 1.0, R1 = 2.0
        let mov_r0 = (0x0100u64 << 48) | ((1.0f32.to_bits() as u64) << 20) | 0x0007_0000;
        let mov_r1 = (0x0100u64 << 48) | ((2.0f32.to_bits() as u64) << 20) | 0x0007_0001;

        // FSETP.LT P0, PT, R0, R1 — sets P0 = (1.0 < 2.0) = true
        // Pattern: 0x5BB0, pred_a=0 at bits[5:3], pred_b=7 at bits[2:0]
        // compare=LT(1) at bits[51:48], bop_pred=7 at bits[41:39]
        let fsetp = (0x5BB0u64 << 48)
            | (1u64 << 48)     // compare = LT
            | (7u64 << 39)     // bop_pred = PT
            | (1u64 << 20)     // src_b_reg = R1
            | (0x0007u64 << 16) // predicate guard = PT
            | (0u64 << 8)     // src_a = R0
            | (0u64 << 3)     // pred_a = P0
            | 7u64; // pred_b = P7

        // MOV R2, 99.0 (this should execute since P0 is true)
        // We'll use MOV32I with P0 guard
        let mov_guarded = (0x0100u64 << 48)
            | ((99.0f32.to_bits() as u64) << 20)
            | (0u64 << 16) // guard = P0 (index=0, not negated)
            | 0x0002; // dst = R2

        let exit = 0xE300_0000_0007_0000u64;

        let program = make_program(vec![mov_r0, mov_r1, fsetp, mov_guarded, exit]);
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.run(&program);

        // P0 should be true (1.0 < 2.0)
        assert!(ctx.preds[0]);
        // R2 should be 99.0 since P0 was true
        assert_eq!(ctx.read_reg_f32(2), 99.0);
    }

    #[test]
    fn test_iadd_shl() {
        // R0 = 10, R1 = 20
        let mov_r0 = (0x0100u64 << 48) | (10u64 << 20) | 0x0007_0000;
        let mov_r1 = (0x0100u64 << 48) | (20u64 << 20) | 0x0007_0001;

        // IADD R2, R0, R1 — pattern 0x5C10
        let iadd = (0x5C10u64 << 48) | (1u64 << 20) | 0x0007_0002 | (0u64 << 8);
        // SHL R3, R2, 2 (imm) — pattern 0x3848
        // imm20 = 2 at bits[38:20]
        let shl = (0x3848u64 << 48) | (2u64 << 20) | 0x0007_0203;

        let exit = 0xE300_0000_0007_0000u64;

        let program = make_program(vec![mov_r0, mov_r1, iadd, shl, exit]);
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.run(&program);

        assert_eq!(ctx.read_reg(2), 30); // 10 + 20
        assert_eq!(ctx.read_reg(3), 120); // 30 << 2
    }

    #[test]
    fn test_mufu_rcp() {
        // R0 = 4.0
        let mov_r0 = (0x0100u64 << 48) | ((4.0f32.to_bits() as u64) << 20) | 0x0007_0000;
        // MUFU R1, R0, RCP — pattern 0x5080, op=4(RCP) at bits[23:20]
        let mufu = (0x5080u64 << 48) | (4u64 << 20) | 0x0007_0001 | (0u64 << 8);
        let exit = 0xE300_0000_0007_0000u64;

        let program = make_program(vec![mov_r0, mufu, exit]);
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.run(&program);

        let result = ctx.read_reg_f32(1);
        assert!(
            (result - 0.25).abs() < 1e-6,
            "Expected 0.25, got {}",
            result
        );
    }

    #[test]
    fn test_collect_fragment_output() {
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.output_attrs.insert(0, 0.5);
        ctx.output_attrs.insert(4, 0.6);
        ctx.output_attrs.insert(8, 0.7);
        ctx.output_attrs.insert(12, 1.0);

        let output = ctx.collect_fragment_output();
        assert!((output.color[0] - 0.5).abs() < 1e-6);
        assert!((output.color[1] - 0.6).abs() < 1e-6);
        assert!((output.color[2] - 0.7).abs() < 1e-6);
        assert!((output.color[3] - 1.0).abs() < 1e-6);
        assert!(!output.killed);
        assert!(output.depth.is_none());
    }

    // ── Tests for new Phase 7 instructions ──────────────────────────

    #[test]
    fn test_xmad() {
        use crate::shader::decoder::Instruction;
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        // R1 = 0x0003, R2 = 0x0004, R3 = 10
        ctx.write_reg(1, 3);
        ctx.write_reg(2, 4);
        ctx.write_reg(3, 10);

        // XMAD: dst = (lo16(R1) * lo16(R2)) + R3 = 3*4 + 10 = 22
        ctx.execute(&Instruction::Xmad {
            dst: 0,
            src_a: 1,
            src_b: SrcB::Reg(2),
            src_c: SrcC::Reg(3),
            mode: 0, // CLO
            hi_a: false,
            hi_b: false,
            psl: false,
        });
        assert_eq!(ctx.read_reg(0), 22);
    }

    #[test]
    fn test_imnmx() {
        use crate::shader::decoder::Instruction;
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.write_reg(1, 10);
        ctx.write_reg(2, 20);
        ctx.preds[0] = true;

        // When pred is true, result = min(10, 20) = 10
        ctx.execute(&Instruction::Imnmx {
            dst: 0,
            src_a: 1,
            src_b: SrcB::Reg(2),
            pred: 0,
            neg_pred: false,
            is_signed: false,
        });
        assert_eq!(ctx.read_reg(0), 10);

        // When pred is false, result = max(10, 20) = 20
        ctx.preds[0] = false;
        ctx.execute(&Instruction::Imnmx {
            dst: 0,
            src_a: 1,
            src_b: SrcB::Reg(2),
            pred: 0,
            neg_pred: false,
            is_signed: false,
        });
        assert_eq!(ctx.read_reg(0), 20);
    }

    #[test]
    fn test_fmnmx() {
        use crate::shader::decoder::Instruction;
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.write_reg_f32(1, 3.0);
        ctx.write_reg_f32(2, 7.0);
        ctx.preds[0] = true;

        ctx.execute(&Instruction::Fmnmx {
            dst: 0,
            src_a: 1,
            src_b: SrcB::Reg(2),
            pred: 0,
            neg_pred: false,
            abs_a: false,
            abs_b: false,
            neg_a: false,
            neg_b: false,
        });
        assert!((ctx.read_reg_f32(0) - 3.0).abs() < 1e-6);
    }

    #[test]
    fn test_bfe() {
        use crate::shader::decoder::Instruction;
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        // Extract 3 bits starting at offset 2 from 0xFC (0b11111100)
        // bits [4:2] = 111 = 7
        ctx.write_reg(1, 0xFC);
        // Pack: offset=2, count=3 → pack = (3 << 8) | 2 = 0x302
        ctx.write_reg(2, 0x0302);

        ctx.execute(&Instruction::Bfe {
            dst: 0,
            src_a: 1,
            src_b: SrcB::Reg(2),
            is_signed: false,
        });
        assert_eq!(ctx.read_reg(0), 7); // (0xFC >> 2) & 7 = 0x3F & 7 = 7
    }

    #[test]
    fn test_bfe_unsigned() {
        use crate::shader::decoder::Instruction;
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.write_reg(1, 0xABCD_1234);
        // Extract 8 bits starting at bit 8
        ctx.write_reg(2, (8 << 8) | 8); // count=8, offset=8

        ctx.execute(&Instruction::Bfe {
            dst: 0,
            src_a: 1,
            src_b: SrcB::Reg(2),
            is_signed: false,
        });
        assert_eq!(ctx.read_reg(0), 0x12); // byte 1 of 0xABCD1234
    }

    #[test]
    fn test_bfi() {
        use crate::shader::decoder::Instruction;
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        // Insert 0xFF at bits [15:8] into 0x00000000
        ctx.write_reg(1, 0xFF); // insert value
        ctx.write_reg(2, (8 << 8) | 8); // pack: count=8, offset=8
        ctx.write_reg(3, 0); // base

        ctx.execute(&Instruction::Bfi {
            dst: 0,
            src_a: 1,
            src_b: SrcB::Reg(2),
            src_c: SrcC::Reg(3),
        });
        assert_eq!(ctx.read_reg(0), 0xFF00);
    }

    #[test]
    fn test_popc() {
        use crate::shader::decoder::Instruction;
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.write_reg(1, 0xFF); // 8 bits set

        ctx.execute(&Instruction::Popc {
            dst: 0,
            src_b: SrcB::Reg(1),
        });
        assert_eq!(ctx.read_reg(0), 8);
    }

    #[test]
    fn test_flo() {
        use crate::shader::decoder::Instruction;
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.write_reg(1, 0x80); // bit 7 set (128)

        ctx.execute(&Instruction::Flo {
            dst: 0,
            src_b: SrcB::Reg(1),
            is_signed: false,
            invert: false,
        });
        assert_eq!(ctx.read_reg(0), 7); // Leading one at bit 7
    }

    #[test]
    fn test_lop3() {
        use crate::shader::decoder::Instruction;
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.write_reg(1, 0xFF00);
        ctx.write_reg(2, 0x0FF0);
        ctx.write_reg(3, 0x00FF);

        // LUT = 0x80 = AND of all three: (a & b & c)
        ctx.execute(&Instruction::Lop3 {
            dst: 0,
            src_a: 1,
            src_b: SrcB::Reg(2),
            src_c: SrcC::Reg(3),
            lut: 0x80, // a AND b AND c truth table
            pred_out: 7,
        });
        assert_eq!(ctx.read_reg(0), 0xFF00 & 0x0FF0 & 0x00FF); // = 0x0000

        // LUT = 0xFE = OR of all three: a | b | c
        ctx.execute(&Instruction::Lop3 {
            dst: 0,
            src_a: 1,
            src_b: SrcB::Reg(2),
            src_c: SrcC::Reg(3),
            lut: 0xFE, // a OR b OR c
            pred_out: 7,
        });
        assert_eq!(ctx.read_reg(0), 0xFF00 | 0x0FF0 | 0x00FF); // = 0xFFFF
    }

    #[test]
    fn test_iset() {
        use crate::shader::decoder::Instruction;
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.write_reg(1, 5);
        ctx.write_reg(2, 10);

        // ISET: dst = (5 < 10) ? 0xFFFFFFFF : 0
        ctx.execute(&Instruction::Iset {
            dst: 0,
            src_a: 1,
            src_b: SrcB::Reg(2),
            compare: IntCompareOp::LT,
            bop: BoolOp::And,
            bop_pred: 7, // PT
            neg_bop_pred: false,
            is_signed: false,
            bf: false,
        });
        assert_eq!(ctx.read_reg(0), 0xFFFF_FFFF);

        // ISET bf mode: dst = (5 < 10) ? 1.0 : 0.0
        ctx.execute(&Instruction::Iset {
            dst: 0,
            src_a: 1,
            src_b: SrcB::Reg(2),
            compare: IntCompareOp::LT,
            bop: BoolOp::And,
            bop_pred: 7,
            neg_bop_pred: false,
            is_signed: false,
            bf: true,
        });
        assert_eq!(ctx.read_reg_f32(0), 1.0);
    }

    #[test]
    fn test_fset() {
        use crate::shader::decoder::Instruction;
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.write_reg_f32(1, 3.0);
        ctx.write_reg_f32(2, 5.0);

        ctx.execute(&Instruction::Fset {
            dst: 0,
            src_a: 1,
            src_b: SrcB::Reg(2),
            compare: FpCompareOp::LT,
            bop: BoolOp::And,
            bop_pred: 7,
            neg_bop_pred: false,
            bf: false,
        });
        assert_eq!(ctx.read_reg(0), 0xFFFF_FFFF); // 3.0 < 5.0 = true
    }

    #[test]
    fn test_shf_right() {
        use crate::shader::decoder::Instruction;
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        // Funnel shift right: (high:low) >> shift
        ctx.write_reg(1, 0x0000_0010); // low
        ctx.write_reg(3, 0xABCD_0000); // high
                                       // shift = 4

        ctx.execute(&Instruction::Shf {
            dst: 0,
            src_a: 1,
            src_b: SrcB::Imm20(4),
            src_c: SrcC::Reg(3),
            direction_right: true,
            data_type_u64: false,
            hi: false,
        });
        // (0xABCD_0000_0000_0010 >> 4) = 0x0ABCD_0000_0000_001 → low 32 bits = 0x00000001
        assert_eq!(ctx.read_reg(0), 0x0000_0001);
    }

    #[test]
    fn test_iadd3() {
        use crate::shader::decoder::Instruction;
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.write_reg(1, 10);
        ctx.write_reg(2, 20);
        ctx.write_reg(3, 30);

        ctx.execute(&Instruction::Iadd3 {
            dst: 0,
            src_a: 1,
            src_b: SrcB::Reg(2),
            src_c: SrcC::Reg(3),
            neg_a: false,
            neg_b: false,
            neg_c: false,
        });
        assert_eq!(ctx.read_reg(0), 60); // 10 + 20 + 30
    }

    #[test]
    fn test_iadd3_negate() {
        use crate::shader::decoder::Instruction;
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.write_reg(1, 100);
        ctx.write_reg(2, 30);
        ctx.write_reg(3, 20);

        ctx.execute(&Instruction::Iadd3 {
            dst: 0,
            src_a: 1,
            src_b: SrcB::Reg(2),
            src_c: SrcC::Reg(3),
            neg_a: false,
            neg_b: true,
            neg_c: true,
        });
        assert_eq!(ctx.read_reg(0), 50); // 100 - 30 - 20
    }

    #[test]
    fn test_p2r_r2p() {
        use crate::shader::decoder::Instruction;
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.preds[0] = true;
        ctx.preds[1] = false;
        ctx.preds[2] = true;
        ctx.write_reg(1, 0);

        // P2R: pack predicates into register
        ctx.execute(&Instruction::P2r {
            dst: 0,
            src_a: 1,
            src_b: SrcB::Imm20(0x7F), // mask = all 7 predicates
        });
        // P0=1, P1=0, P2=1 → bits = 0b101 = 5
        assert_eq!(ctx.read_reg(0) & 0x7, 5);

        // R2P: unpack register bits back into predicates
        ctx.preds = [false; 8];
        ctx.preds[7] = true;
        ctx.write_reg(5, 0b1010); // P1=1, P3=1
        ctx.execute(&Instruction::R2p {
            src_a: 5,
            src_b: SrcB::Imm20(0x0F), // mask = lower 4 predicates
            src_c: 0,
        });
        assert!(!ctx.preds[0]);
        assert!(ctx.preds[1]);
        assert!(!ctx.preds[2]);
        assert!(ctx.preds[3]);
    }

    #[test]
    fn test_f2f_passthrough() {
        use crate::shader::decoder::Instruction;
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.write_reg_f32(1, 42.5);

        ctx.execute(&Instruction::F2f {
            dst: 0,
            src_b: SrcB::Reg(1),
            abs_b: false,
            neg_b: false,
            sat: false,
            dst_size: 1,
            src_size: 1,
            rounding: 0,
        });
        assert_eq!(ctx.read_reg_f32(0), 42.5);
    }

    #[test]
    fn test_i2i_sign_extend() {
        use crate::shader::decoder::Instruction;
        let mut ctx = ShaderExecContext::new(&dummy_read_gpu, &dummy_sample);
        ctx.write_reg(1, 0x80); // -128 as S8

        ctx.execute(&Instruction::I2i {
            dst: 0,
            src_b: SrcB::Reg(1),
            abs_b: false,
            sat: false,
            dst_signed: true,
            src_signed: true,
            dst_size: 2, // S32
            src_size: 0, // S8
        });
        assert_eq!(ctx.read_reg(0) as i32, -128);
    }
}

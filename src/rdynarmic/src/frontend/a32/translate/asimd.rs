use crate::frontend::a32::decoder::DecodedArm;
use crate::frontend::a32::types::{ExtReg, Reg};
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::acc_type::AccType;
use crate::ir::value::Value;

#[derive(Clone, Copy)]
enum Comparison {
    Equal,
    GreaterEqual,
    Greater,
    LessEqual,
    Less,
}

fn compare_with_zero(ir: &mut A32IREmitter, inst: &DecodedArm, comparison: Comparison) -> bool {
    let d = (inst.raw >> 22) & 1 != 0;
    let sz = (inst.raw >> 18) & 0x3;
    let vd = (inst.raw >> 12) & 0xF;
    let f = (inst.raw >> 10) & 1 != 0;
    let q = (inst.raw >> 6) & 1 != 0;
    let m = (inst.raw >> 5) & 1 != 0;
    let vm = inst.raw & 0xF;

    if sz == 0b11 || (f && sz != 0b10) {
        return super::undefined_instruction(ir);
    }
    if q && ((vd & 1) != 0 || (vm & 1) != 0) {
        return super::undefined_instruction(ir);
    }

    let Some(d_reg) = to_vector_reg(q, d, vd) else {
        return super::undefined_instruction(ir);
    };
    let Some(m_reg) = to_vector_reg(q, m, vm) else {
        return super::undefined_instruction(ir);
    };
    let reg_m = ir.get_vector(m_reg);
    let zero = ir.ir().zero_vector();
    let result = if f {
        match comparison {
            Comparison::Equal => ir.ir().fp_vector_equal(32, reg_m, zero, false),
            Comparison::GreaterEqual => ir.ir().fp_vector_greater_equal(32, reg_m, zero, false),
            Comparison::Greater => ir.ir().fp_vector_greater(32, reg_m, zero, false),
            Comparison::LessEqual => ir.ir().fp_vector_greater_equal(32, zero, reg_m, false),
            Comparison::Less => ir.ir().fp_vector_greater(32, zero, reg_m, false),
        }
    } else {
        let esize = 8usize << sz;
        match comparison {
            Comparison::Equal => ir.ir().vector_equal(esize, reg_m, zero),
            Comparison::GreaterEqual => ir.ir().vector_greater_equal_signed(esize, reg_m, zero),
            Comparison::Greater => ir.ir().vector_greater_signed(esize, reg_m, zero),
            Comparison::LessEqual => ir.ir().vector_less_equal_signed(esize, reg_m, zero),
            Comparison::Less => ir.ir().vector_less_signed(esize, reg_m, zero),
        }
    };
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vcgt_zero(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    compare_with_zero(ir, inst, Comparison::Greater)
}

pub fn arm_asimd_vcge_zero(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    compare_with_zero(ir, inst, Comparison::GreaterEqual)
}

pub fn arm_asimd_vceq_zero(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    compare_with_zero(ir, inst, Comparison::Equal)
}

pub fn arm_asimd_vcle_zero(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    compare_with_zero(ir, inst, Comparison::LessEqual)
}

pub fn arm_asimd_vclt_zero(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    compare_with_zero(ir, inst, Comparison::Less)
}

pub(crate) fn to_vector_reg(q: bool, d: bool, vd: u32) -> Option<ExtReg> {
    if q {
        if vd & 1 != 0 {
            return None;
        }
        let q_index = (((if d { 16 } else { 0 }) + vd) / 2) as u8;
        Some(ExtReg::from_quad(q_index))
    } else {
        let d_index = ((if d { 16 } else { 0 }) + vd) as u8;
        Some(ExtReg::from_double(d_index))
    }
}

/// Matches upstream `AdvSIMDExpandImm(op, cmode, imm8)` from `imm.cpp`.
/// Expands an 8-bit immediate + cmode + op into a 64-bit vector element value.
fn adv_simd_expand_imm(op: bool, cmode: u32, imm8: u32) -> u64 {
    let byte = (imm8 & 0xFF) as u64;
    let cmode_hi3 = (cmode >> 1) & 0x7; // cmode<3:1>
    let cmode_lo = cmode & 1; // cmode<0>

    // replicate_element helpers
    let rep32 = |v: u64| -> u64 { (v & 0xFFFF_FFFF) | ((v & 0xFFFF_FFFF) << 32) };
    let rep16 = |v: u64| -> u64 {
        let v = v & 0xFFFF;
        v | (v << 16) | (v << 32) | (v << 48)
    };
    let rep8 = |v: u64| -> u64 {
        let v = v & 0xFF;
        v * 0x0101_0101_0101_0101
    };

    match cmode_hi3 {
        0b000 => rep32(byte),
        0b001 => rep32(byte << 8),
        0b010 => rep32(byte << 16),
        0b011 => rep32(byte << 24),
        0b100 => rep16(byte),
        0b101 => rep16(byte << 8),
        0b110 => {
            if cmode_lo == 0 {
                rep32((byte << 8) | 0xFF)
            } else {
                rep32((byte << 16) | 0xFFFF)
            }
        }
        0b111 => {
            if cmode_lo == 0 && !op {
                // VMOV.I8: replicate byte across all 8 bytes
                rep8(byte)
            } else if cmode_lo == 0 && op {
                // VMOV.I64: each bit of imm8 selects 0x00 or 0xFF per byte
                let mut result = 0u64;
                for i in 0..8 {
                    if (imm8 >> i) & 1 != 0 {
                        result |= 0xFFu64 << (i * 8);
                    }
                }
                result
            } else if cmode_lo == 1 && !op {
                // VMOV.F32: replicate float immediate
                let mut result = 0u32;
                if (imm8 >> 7) & 1 != 0 {
                    result |= 0x8000_0000;
                }
                if (imm8 >> 6) & 1 != 0 {
                    result |= 0x3E00_0000;
                } else {
                    result |= 0x4000_0000;
                }
                result |= (imm8 & 0x3F) << 19;
                rep32(result as u64)
            } else {
                // cmode_lo == 1 && op: VMOV.F64 immediate (single element, not replicated)
                let mut result = 0u64;
                if (imm8 >> 7) & 1 != 0 {
                    result |= 0x8000_0000_0000_0000;
                }
                if (imm8 >> 6) & 1 != 0 {
                    result |= 0x3FC0_0000_0000_0000;
                } else {
                    result |= 0x4000_0000_0000_0000;
                }
                result |= ((imm8 & 0x3F) as u64) << 48;
                result
            }
        }
        _ => unreachable!(),
    }
}

/// ASIMD one register modified immediate — matches upstream `asimd_VMOV_imm`.
/// Handles VMOV, VMVN, VORR, VBIC depending on cmode and op.
pub fn arm_asimd_vmov_imm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let d = (inst.raw >> 22) & 1 != 0;
    let vd = (inst.raw >> 12) & 0xF;
    let cmode = (inst.raw >> 8) & 0xF;
    let q = (inst.raw >> 6) & 1 != 0;
    let op = (inst.raw >> 5) & 1 != 0;

    if q && (vd & 1) != 0 {
        return super::undefined_instruction(ir);
    }

    // Upstream pattern: 1111001a1D000bcdVVVVmmmm0Qo1efgh
    // imm8 = a:b:c:d:e:f:g:h where a=bit24, b=bit18, c=bit17, d=bit16, e..h=bits3..0
    let imm8 = (((inst.raw >> 24) & 1) << 7)
        | (((inst.raw >> 18) & 1) << 6)
        | (((inst.raw >> 17) & 1) << 5)
        | (((inst.raw >> 16) & 1) << 4)
        | (((inst.raw >> 3) & 1) << 3)
        | (((inst.raw >> 2) & 1) << 2)
        | (((inst.raw >> 1) & 1) << 1)
        | (inst.raw & 1);

    let dest = match to_vector_reg(q, d, vd) {
        Some(reg) => reg,
        None => return super::undefined_instruction(ir),
    };

    let imm = adv_simd_expand_imm(op, cmode, imm8);

    let selector = (cmode << 1) | u32::from(op);
    match selector {
        // VMOV
        0b00000 | 0b00100 | 0b01000 | 0b01100 | 0b10000 | 0b10100 | 0b11000 | 0b11010 | 0b11100
        | 0b11101 | 0b11110 => {
            let imm64 = Value::ImmU64(imm);
            if q {
                let vec = if imm == 0 {
                    ir.ir().zero_vector()
                } else {
                    ir.ir().vector_broadcast(64, imm64)
                };
                ir.set_vector(dest, vec);
            } else {
                ir.set_extended_register_64(dest, imm64);
            }
            true
        }
        // VMVN
        0b00001 | 0b00101 | 0b01001 | 0b01101 | 0b10001 | 0b10101 | 0b11001 | 0b11011 => {
            let inv = !imm;
            let imm64 = Value::ImmU64(inv);
            if q {
                let vec = if inv == 0 {
                    ir.ir().zero_vector()
                } else {
                    ir.ir().vector_broadcast(64, imm64)
                };
                ir.set_vector(dest, vec);
            } else {
                ir.set_extended_register_64(dest, imm64);
            }
            true
        }
        // VORR (immediate)
        0b00010 | 0b00110 | 0b01010 | 0b01110 | 0b10010 | 0b10110 => {
            let imm64 = Value::ImmU64(imm);
            if q {
                let reg_value = ir.get_vector(dest);
                let imm_vec = ir.ir().vector_broadcast(64, imm64);
                let result = ir.ir().vector_or(reg_value, imm_vec);
                ir.set_vector(dest, result);
            } else {
                let reg_value = ir.get_extended_register_64(dest);
                let result = ir.ir().or_64(reg_value, imm64);
                ir.set_extended_register_64(dest, result);
            }
            true
        }
        // VBIC (immediate)
        0b00011 | 0b00111 | 0b01011 | 0b01111 | 0b10011 | 0b10111 => {
            let inv = !imm;
            let imm64 = Value::ImmU64(inv);
            if q {
                let reg_value = ir.get_vector(dest);
                let mask_vec = ir.ir().vector_broadcast(64, imm64);
                let result = ir.ir().vector_and(reg_value, mask_vec);
                ir.set_vector(dest, result);
            } else {
                let reg_value = ir.get_extended_register_64(dest);
                let result = ir.ir().and_64(reg_value, imm64);
                ir.set_extended_register_64(dest, result);
            }
            true
        }
        // 0b11111 = UNDEFINED
        0b11111 => super::undefined_instruction(ir),
        _ => false,
    }
}

pub fn arm_asimd_vdup_scalar(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let d = ((inst.raw >> 22) & 1) != 0;
    let imm4 = ((inst.raw >> 16) & 0xF) as u8;
    let vd = (inst.raw >> 12) & 0xF;
    let q = ((inst.raw >> 6) & 1) != 0;
    let m = ((inst.raw >> 5) & 1) != 0;
    let vm = inst.raw & 0xF;

    if q && (vd & 1) != 0 {
        return super::undefined_instruction(ir);
    }

    if (imm4 & 0b111) == 0 {
        return super::undefined_instruction(ir);
    }

    let imm4_lsb = imm4.trailing_zeros() as usize;
    let esize = 8usize << imm4_lsb;
    let index = imm4 >> (imm4_lsb + 1);
    let dest = match to_vector_reg(q, d, vd) {
        Some(reg) => reg,
        None => {
            return super::undefined_instruction(ir);
        }
    };
    let src = ExtReg::from_double(((if m { 16 } else { 0 }) + vm) as u8);

    let reg_m = ir.get_vector(src);
    let result = ir.ir().vector_broadcast_element(esize, reg_m, index);
    ir.set_vector(dest, result);
    true
}

pub fn arm_asimd_vrhadd(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let u = ((inst.raw >> 24) & 1) != 0;
    let d = ((inst.raw >> 22) & 1) != 0;
    let sz = (inst.raw >> 20) & 0x3;
    let vn = (inst.raw >> 16) & 0xF;
    let vd = (inst.raw >> 12) & 0xF;
    let n = ((inst.raw >> 7) & 1) != 0;
    let q = ((inst.raw >> 6) & 1) != 0;
    let m = ((inst.raw >> 5) & 1) != 0;
    let vm = inst.raw & 0xF;

    // Upstream `asimd_VRHADD` raises UndefinedInstruction (not Unpredictable)
    // for both the Q-misalignment and the sz==0b11 guards.
    if q && ((vd & 1) != 0 || (vn & 1) != 0 || (vm & 1) != 0) {
        return super::undefined_instruction(ir);
    }

    if sz == 0b11 {
        return super::undefined_instruction(ir);
    }

    let esize = 8usize << sz;
    let dest = match to_vector_reg(q, d, vd) {
        Some(reg) => reg,
        None => return super::undefined_instruction(ir),
    };
    let src_n = match to_vector_reg(q, n, vn) {
        Some(reg) => reg,
        None => return super::undefined_instruction(ir),
    };
    let src_m = match to_vector_reg(q, m, vm) {
        Some(reg) => reg,
        None => return super::undefined_instruction(ir),
    };

    let reg_n = ir.get_vector(src_n);
    let reg_m = ir.get_vector(src_m);
    let result = if u {
        ir.ir()
            .vector_rounding_halving_add_unsigned(esize, reg_n, reg_m)
    } else {
        ir.ir()
            .vector_rounding_halving_add_signed(esize, reg_n, reg_m)
    };
    ir.set_vector(dest, result);
    true
}

/// Get scalar register location for two-regs-scalar instructions.
/// Port of upstream `GetScalarLocation`.
fn get_scalar_location(esize: usize, m_bit: bool, vm: u32) -> (ExtReg, u8) {
    let q_index = if esize == 16 {
        ((vm >> 1) & 0b11) as u8
    } else {
        ((vm >> 1) & 0b111) as u8
    };
    let reg = ExtReg::from_quad(q_index);

    // index = {Vm[0], M, Vm[3]} >> (esize==16 ? 0 : 1)
    let raw_index = ((vm & 1) << 2) | ((m_bit as u32) << 1) | ((vm >> 3) & 1);
    let index = if esize == 16 {
        raw_index
    } else {
        raw_index >> 1
    } as u8;
    (reg, index)
}

/// VMLA_scalar (ASIMD) — Vector multiply accumulate by scalar.
/// Port of upstream `ScalarMultiply` with MultiplyBehavior::MultiplyAccumulate.
pub fn arm_asimd_vmla_scalar(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    // Pattern: 1111001Q1Dzznnnndddd0o0FN1M0mmmm
    let q = ((inst.raw >> 24) & 1) != 0;
    let d = ((inst.raw >> 22) & 1) != 0;
    let sz = (inst.raw >> 20) & 3;
    let vn = (inst.raw >> 16) & 0xF;
    let vd = (inst.raw >> 12) & 0xF;
    let op = ((inst.raw >> 10) & 1) != 0; // 0=MLA, 1=MLS
    let f = ((inst.raw >> 8) & 1) != 0; // 1=float, 0=integer
    let n = ((inst.raw >> 7) & 1) != 0;
    let m = ((inst.raw >> 5) & 1) != 0;
    let vm = inst.raw & 0xF;

    // Upstream `ScalarMultiply` returns DecodeError (not Undefined) for sz==0b11.
    if sz == 0b11 {
        return super::decode_error(ir);
    }
    if sz == 0b00 || (f && sz == 0b01) {
        return super::undefined_instruction(ir);
    }
    if q && ((vd & 1) != 0 || (vn & 1) != 0) {
        return super::undefined_instruction(ir);
    }

    let esize = 8usize << sz;
    let dest = match to_vector_reg(q, d, vd) {
        Some(r) => r,
        None => return super::undefined_instruction(ir),
    };
    let src_n = match to_vector_reg(q, n, vn) {
        Some(r) => r,
        None => return super::undefined_instruction(ir),
    };
    let (scalar_reg, scalar_idx) = get_scalar_location(esize, m, vm);

    let reg_n = ir.get_vector(src_n);
    let scalar_vec = ir.get_vector(scalar_reg);
    let reg_m = ir
        .ir()
        .vector_broadcast_element(esize, scalar_vec, scalar_idx);

    let product = if f {
        ir.ir().fp_vector_mul(esize, reg_n, reg_m, false)
    } else {
        ir.ir().vector_multiply(esize, reg_n, reg_m)
    };

    let reg_d = ir.get_vector(dest);
    let result = if op {
        // MultiplySubtract
        if f {
            ir.ir().fp_vector_sub(esize, reg_d, product, false)
        } else {
            ir.ir().vector_sub(esize, reg_d, product)
        }
    } else {
        // MultiplyAccumulate
        if f {
            ir.ir().fp_vector_add(esize, reg_d, product, false)
        } else {
            ir.ir().vector_add(esize, reg_d, product)
        }
    };

    ir.set_vector(dest, result);
    true
}

/// VMUL_scalar (ASIMD) — Vector multiply by scalar.
/// Port of upstream `ScalarMultiply` with MultiplyBehavior::Multiply.
pub fn arm_asimd_vmul_scalar(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    // Pattern: 1111001Q1Dzznnnndddd100FN1M0mmmm
    let q = ((inst.raw >> 24) & 1) != 0;
    let d = ((inst.raw >> 22) & 1) != 0;
    let sz = (inst.raw >> 20) & 3;
    let vn = (inst.raw >> 16) & 0xF;
    let vd = (inst.raw >> 12) & 0xF;
    let f = ((inst.raw >> 8) & 1) != 0; // 1=float, 0=integer
    let n = ((inst.raw >> 7) & 1) != 0;
    let m = ((inst.raw >> 5) & 1) != 0;
    let vm = inst.raw & 0xF;

    // Upstream `ScalarMultiply` returns DecodeError (not Undefined) for sz==0b11.
    if sz == 0b11 {
        return super::decode_error(ir);
    }
    if sz == 0b00 || (f && sz == 0b01) {
        return super::undefined_instruction(ir);
    }
    if q && ((vd & 1) != 0 || (vn & 1) != 0) {
        return super::undefined_instruction(ir);
    }

    let esize = 8usize << sz;
    let dest = match to_vector_reg(q, d, vd) {
        Some(r) => r,
        None => return super::undefined_instruction(ir),
    };
    let src_n = match to_vector_reg(q, n, vn) {
        Some(r) => r,
        None => return super::undefined_instruction(ir),
    };
    let (scalar_reg, scalar_idx) = get_scalar_location(esize, m, vm);

    let reg_n = ir.get_vector(src_n);
    let scalar_vec = ir.get_vector(scalar_reg);
    let reg_m = ir
        .ir()
        .vector_broadcast_element(esize, scalar_vec, scalar_idx);
    let result = if f {
        ir.ir().fp_vector_mul(esize, reg_n, reg_m, false)
    } else {
        ir.ir().vector_multiply(esize, reg_n, reg_m)
    };

    ir.set_vector(dest, result);
    true
}

/// VCVT_integer (ASIMD) — Vector integer/floating-point conversion.
/// Port of upstream `asimd_VCVT_integer`.
pub fn arm_asimd_vcvt_integer(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    // Pattern: 111100111D11zz11dddd011oUQM0mmmm
    let d = ((inst.raw >> 22) & 1) != 0;
    let sz = (inst.raw >> 18) & 0b11;
    let vd = (inst.raw >> 12) & 0xF;
    let op = ((inst.raw >> 8) & 1) != 0;
    let u = ((inst.raw >> 7) & 1) != 0;
    let q = ((inst.raw >> 6) & 1) != 0;
    let m = ((inst.raw >> 5) & 1) != 0;
    let vm = inst.raw & 0xF;

    if q && ((vd & 1) != 0 || (vm & 1) != 0) {
        return super::undefined_instruction(ir);
    }

    if sz != 0b10 {
        return super::undefined_instruction(ir);
    }

    let esize = 8usize << sz;
    let dest = match to_vector_reg(q, d, vd) {
        Some(r) => r,
        None => return super::undefined_instruction(ir),
    };
    let src = match to_vector_reg(q, m, vm) {
        Some(r) => r,
        None => return super::undefined_instruction(ir),
    };
    let reg_m = ir.get_vector(src);
    let result = if op {
        if u {
            ir.ir()
                .fp_vector_to_unsigned_fixed(esize, reg_m, 0, 3, false)
        } else {
            ir.ir().fp_vector_to_signed_fixed(esize, reg_m, 0, 3, false)
        }
    } else if u {
        ir.ir()
            .fp_vector_from_unsigned_fixed(esize, reg_m, 0, 0, false)
    } else {
        ir.ir()
            .fp_vector_from_signed_fixed(esize, reg_m, 0, 0, false)
    };

    ir.set_vector(dest, result);
    true
}

fn to_ext_reg_d(vd: u32, d: bool) -> ExtReg {
    ExtReg::from_double((((if d { 16 } else { 0 }) + vd) & 0x1f) as u8)
}

fn decode_vst_ld_multiple_type(type_: u32, size: u32, align: u32) -> Option<(usize, usize, usize)> {
    match type_ {
        0b0111 => {
            if (align & 0b10) != 0 {
                None
            } else {
                Some((1, 1, 0))
            }
        }
        0b1010 => {
            if align == 0b11 {
                None
            } else {
                Some((1, 2, 0))
            }
        }
        0b0110 => {
            if (align & 0b10) != 0 {
                None
            } else {
                Some((1, 3, 0))
            }
        }
        0b0010 => Some((1, 4, 0)),
        0b1000 => {
            if size == 0b11 || align == 0b11 {
                None
            } else {
                Some((2, 1, 1))
            }
        }
        0b1001 => {
            if size == 0b11 || align == 0b11 {
                None
            } else {
                Some((2, 1, 2))
            }
        }
        0b0011 => {
            if size == 0b11 {
                None
            } else {
                Some((2, 2, 2))
            }
        }
        0b0100 => {
            if size == 0b11 || (align & 0b10) != 0 {
                None
            } else {
                Some((3, 1, 1))
            }
        }
        0b0101 => {
            if size == 0b11 || (align & 0b10) != 0 {
                None
            } else {
                Some((3, 1, 2))
            }
        }
        0b0000 => {
            if size == 0b11 {
                None
            } else {
                Some((4, 1, 1))
            }
        }
        0b0001 => {
            if size == 0b11 {
                None
            } else {
                Some((4, 1, 2))
            }
        }
        _ => None,
    }
}

fn least_significant(ir: &mut A32IREmitter, bits: usize, value: Value) -> Value {
    match bits {
        8 => ir.ir().least_significant_byte(value),
        16 => ir.ir().least_significant_half(value),
        32 => ir.ir().least_significant_word(value),
        64 => value,
        _ => panic!("unsupported least_significant width {}", bits),
    }
}

fn read_memory(ir: &mut A32IREmitter, bits: usize, address: Value) -> Value {
    match bits {
        8 => ir.read_memory_8(address, AccType::Normal),
        16 => ir.read_memory_16(address, AccType::Normal),
        32 => ir.read_memory_32(address, AccType::Normal),
        64 => ir.read_memory_64(address, AccType::Normal),
        _ => panic!("unsupported read width {}", bits),
    }
}

fn write_memory(ir: &mut A32IREmitter, bits: usize, address: Value, value: Value) {
    match bits {
        8 => ir.write_memory_8(address, value, AccType::Normal),
        16 => ir.write_memory_16(address, value, AccType::Normal),
        32 => ir.write_memory_32(address, value, AccType::Normal),
        64 => ir.write_memory_64(address, value, AccType::Normal),
        _ => panic!("unsupported write width {}", bits),
    }
}

fn decode_vst_ld_multiple_operands(
    inst: &DecodedArm,
) -> Option<(bool, Reg, u32, u32, u32, u32, Reg)> {
    let d = ((inst.raw >> 22) & 1) != 0;
    let n = Reg::from_u32((inst.raw >> 16) & 0xF);
    let vd = (inst.raw >> 12) & 0xF;
    let type_ = (inst.raw >> 8) & 0xF;
    let size = (inst.raw >> 6) & 0x3;
    let align = (inst.raw >> 4) & 0x3;
    let m = Reg::from_u32(inst.raw & 0xF);

    if !n.is_valid() || !m.is_valid() {
        return None;
    }

    Some((d, n, vd, type_, size, align, m))
}

pub fn arm_v8_vst_multiple(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let Some((d, n, vd, type_, size, align, m)) = decode_vst_ld_multiple_operands(inst) else {
        return super::undefined_instruction(ir);
    };

    // Upstream returns DecodeError for these reserved type encodings.
    if type_ == 0b1011 || ((type_ >> 2) == 0b11) {
        return super::decode_error(ir);
    }

    let Some((nelem, regs, inc)) = decode_vst_ld_multiple_type(type_, size, align) else {
        return super::undefined_instruction(ir);
    };

    let d_reg = to_ext_reg_d(vd, d);
    let d_last = d_reg.index() + inc * (nelem - 1);
    if n == Reg::R15 || d_last + regs > 32 {
        return super::unpredictable_instruction(ir);
    }

    let ebytes = 1usize << size;
    let elements = 8 / ebytes;
    let wback = m != Reg::R15;
    let register_index = m != Reg::R15 && m != Reg::R13;

    let mut address = ir.get_register(n);
    for r in 0..regs {
        for e in 0..elements {
            for i in 0..nelem {
                let ext_reg = ExtReg::from_double((d_reg.index() + i * inc + r) as u8);
                let ext_value = ir.get_extended_register_64(ext_reg);
                let shifted = if e == 0 {
                    ext_value
                } else {
                    ir.ir()
                        .logical_shift_right_64(ext_value, Value::ImmU8((e * ebytes * 8) as u8))
                };
                let element = least_significant(ir, ebytes * 8, shifted);
                write_memory(ir, ebytes * 8, address, element);
                address =
                    ir.ir()
                        .add_32(address, Value::ImmU32(ebytes as u32), Value::ImmU1(false));
            }
        }
    }

    if wback {
        if register_index {
            let base = ir.get_register(n);
            let index = ir.get_register(m);
            let updated = ir.ir().add_32(base, index, Value::ImmU1(false));
            ir.set_register(n, updated);
        } else {
            let base = ir.get_register(n);
            let updated = ir.ir().add_32(
                base,
                Value::ImmU32((8 * nelem * regs) as u32),
                Value::ImmU1(false),
            );
            ir.set_register(n, updated);
        }
    }

    true
}

pub fn arm_v8_vld_multiple(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let Some((d, n, vd, type_, size, align, m)) = decode_vst_ld_multiple_operands(inst) else {
        return super::undefined_instruction(ir);
    };

    // Upstream returns DecodeError for these reserved type encodings.
    if type_ == 0b1011 || ((type_ >> 2) == 0b11) {
        return super::decode_error(ir);
    }

    let Some((nelem, regs, inc)) = decode_vst_ld_multiple_type(type_, size, align) else {
        return super::undefined_instruction(ir);
    };

    let d_reg = to_ext_reg_d(vd, d);
    let d_last = d_reg.index() + inc * (nelem - 1);
    if n == Reg::R15 || d_last + regs > 32 {
        return super::unpredictable_instruction(ir);
    }

    let ebytes = 1usize << size;
    let elements = 8 / ebytes;
    let wback = m != Reg::R15;
    let register_index = m != Reg::R15 && m != Reg::R13;

    for r in 0..regs {
        for i in 0..nelem {
            let ext_reg = ExtReg::from_double((d_reg.index() + i * inc + r) as u8);
            ir.set_extended_register_64(ext_reg, Value::ImmU64(0));
        }
    }

    let mut address = ir.get_register(n);
    for r in 0..regs {
        for e in 0..elements {
            for i in 0..nelem {
                let ext_reg = ExtReg::from_double((d_reg.index() + i * inc + r) as u8);
                let element = read_memory(ir, ebytes * 8, address);
                let element64 = match ebytes {
                    1 => ir.ir().zero_extend_byte_to_long(element),
                    2 => ir.ir().zero_extend_half_to_long(element),
                    4 => ir.ir().zero_extend_word_to_long(element),
                    8 => element,
                    _ => panic!("unsupported ebytes {}", ebytes),
                };
                let shifted = if e == 0 {
                    element64
                } else {
                    ir.ir()
                        .logical_shift_left_64(element64, Value::ImmU8((e * ebytes * 8) as u8))
                };
                let current = ir.get_extended_register_64(ext_reg);
                let merged = ir.ir().or_64(current, shifted);
                ir.set_extended_register_64(ext_reg, merged);
                address =
                    ir.ir()
                        .add_32(address, Value::ImmU32(ebytes as u32), Value::ImmU1(false));
            }
        }
    }

    if wback {
        if register_index {
            let base = ir.get_register(n);
            let index = ir.get_register(m);
            let updated = ir.ir().add_32(base, index, Value::ImmU1(false));
            ir.set_register(n, updated);
        } else {
            let base = ir.get_register(n);
            let updated = ir.ir().add_32(
                base,
                Value::ImmU32((8 * nelem * regs) as u32),
                Value::ImmU1(false),
            );
            ir.set_register(n, updated);
        }
    }

    true
}

/// VLD{1-4} (single element to all lanes) — broadcast load.
/// Port of upstream `v8_VLD_all_lanes`.
/// Encoding: 111101001D10nnnndddd11nnzzTammmm
pub fn arm_v8_vld_all_lanes(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let d = (inst.raw >> 22) & 1 != 0;
    let n = Reg::from_u32((inst.raw >> 16) & 0xF);
    let vd = (inst.raw >> 12) & 0xF;
    let nn = (inst.raw >> 8) & 0x3;
    let sz = (inst.raw >> 6) & 0x3;
    let t = (inst.raw >> 5) & 1 != 0;
    let a = (inst.raw >> 4) & 1 != 0;
    let m = Reg::from_u32(inst.raw & 0xF);

    let nelem = (nn + 1) as usize;

    if nelem == 1 && (sz == 0b11 || (sz == 0b00 && a)) {
        return super::undefined_instruction(ir);
    }
    if nelem == 2 && sz == 0b11 {
        return super::undefined_instruction(ir);
    }
    if nelem == 3 && (sz == 0b11 || a) {
        return super::undefined_instruction(ir);
    }
    if nelem == 4 && (sz == 0b11 && !a) {
        return super::undefined_instruction(ir);
    }

    let ebytes = if sz == 0b11 { 4usize } else { 1usize << sz };
    let inc = if t { 2usize } else { 1usize };
    let regs = if nelem == 1 { inc } else { 1 };
    // alignment is unused (matches upstream [[maybe_unused]]).

    let d_reg = to_ext_reg_d(vd, d);
    let d_last = d_reg.index() + inc * (nelem - 1);
    if n == Reg::R15 || d_last + regs > 32 {
        return super::unpredictable_instruction(ir);
    }

    let wback = m != Reg::R15;
    let register_index = m != Reg::R15 && m != Reg::R13;

    let mut address = ir.get_register(n);
    for i in 0..nelem {
        let element = read_memory(ir, ebytes * 8, address);
        let replicated = ir.ir().vector_broadcast(ebytes * 8, element);
        for r in 0..regs {
            let ext_reg = ExtReg::from_double((d_reg.index() + i * inc + r) as u8);
            ir.set_vector(ext_reg, replicated);
        }
        address = ir
            .ir()
            .add_32(address, Value::ImmU32(ebytes as u32), Value::ImmU1(false));
    }

    if wback {
        if register_index {
            let base = ir.get_register(n);
            let index = ir.get_register(m);
            let updated = ir.ir().add_32(base, index, Value::ImmU1(false));
            ir.set_register(n, updated);
        } else {
            let base = ir.get_register(n);
            let updated = ir.ir().add_32(
                base,
                Value::ImmU32((nelem * ebytes) as u32),
                Value::ImmU1(false),
            );
            ir.set_register(n, updated);
        }
    }

    true
}

/// Decode the shared fields of the ASIMD load/store single-element forms.
/// Returns `(index, inc, a, ebytes)` or `None` if the encoding is undefined.
/// Mirrors the common preamble of upstream `v8_VST_single`/`v8_VLD_single`.
fn decode_vst_ld_single(
    sz: u32,
    nelem: usize,
    index_align: u32,
) -> Option<(u8, usize, u32, usize)> {
    if nelem == 1 && ((index_align >> sz) & 1) != 0 {
        return None;
    }

    let ebytes = 1usize << sz;
    let index = ((index_align >> (sz + 1)) & ((1u32 << (3 - sz)) - 1)) as u8;
    let inc = if sz != 0 && ((index_align >> sz) & 1) != 0 {
        2usize
    } else {
        1usize
    };
    let a_end = if sz != 0 { sz - 1 } else { 0 };
    let a = index_align & ((1u32 << (a_end + 1)) - 1);

    if nelem == 1 && inc == 2 {
        return None;
    }
    if nelem == 1 && sz == 2 && a != 0b00 && a != 0b11 {
        return None;
    }
    if nelem == 2 && ((a >> 1) & 1) != 0 {
        return None;
    }
    if nelem == 3 && a != 0b00 {
        return None;
    }
    if nelem == 4 && a == 0b11 {
        return None;
    }

    Some((index, inc, a, ebytes))
}

/// VST{1-4} (single element from one lane).
/// Port of upstream `v8_VST_single`.
/// Encoding: 111101001D00nnnnddddzzNNaaaammmm
pub fn arm_v8_vst_single(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let d = (inst.raw >> 22) & 1 != 0;
    let n = Reg::from_u32((inst.raw >> 16) & 0xF);
    let vd = (inst.raw >> 12) & 0xF;
    let sz = (inst.raw >> 10) & 0x3;
    let nn = (inst.raw >> 8) & 0x3;
    let index_align = (inst.raw >> 4) & 0xF;
    let m = Reg::from_u32(inst.raw & 0xF);

    let nelem = (nn + 1) as usize;

    if sz == 0b11 {
        return super::decode_error(ir);
    }

    let Some((index, inc, _a, ebytes)) = decode_vst_ld_single(sz, nelem, index_align) else {
        return super::undefined_instruction(ir);
    };

    let d_reg = to_ext_reg_d(vd, d);
    let d_last = d_reg.index() + inc * (nelem - 1);
    if n == Reg::R15 || d_last + 1 > 32 {
        return super::unpredictable_instruction(ir);
    }

    let wback = m != Reg::R15;
    let register_index = m != Reg::R15 && m != Reg::R13;

    let mut address = ir.get_register(n);
    for i in 0..nelem {
        let ext_reg = ExtReg::from_double((d_reg.index() + i * inc) as u8);
        let vec = ir.get_vector(ext_reg);
        let element = ir.ir().vector_get_element(ebytes * 8, vec, index);
        write_memory(ir, ebytes * 8, address, element);
        address = ir
            .ir()
            .add_32(address, Value::ImmU32(ebytes as u32), Value::ImmU1(false));
    }

    if wback {
        if register_index {
            let base = ir.get_register(n);
            let index_reg = ir.get_register(m);
            let updated = ir.ir().add_32(base, index_reg, Value::ImmU1(false));
            ir.set_register(n, updated);
        } else {
            let base = ir.get_register(n);
            let updated = ir.ir().add_32(
                base,
                Value::ImmU32((nelem * ebytes) as u32),
                Value::ImmU1(false),
            );
            ir.set_register(n, updated);
        }
    }

    true
}

/// VLD{1-4} (single element to one lane).
/// Port of upstream `v8_VLD_single`.
/// Encoding: 111101001D10nnnnddddzzNNaaaammmm
pub fn arm_v8_vld_single(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let d = (inst.raw >> 22) & 1 != 0;
    let n = Reg::from_u32((inst.raw >> 16) & 0xF);
    let vd = (inst.raw >> 12) & 0xF;
    let sz = (inst.raw >> 10) & 0x3;
    let nn = (inst.raw >> 8) & 0x3;
    let index_align = (inst.raw >> 4) & 0xF;
    let m = Reg::from_u32(inst.raw & 0xF);

    let nelem = (nn + 1) as usize;

    if sz == 0b11 {
        return super::decode_error(ir);
    }

    let Some((index, inc, _a, ebytes)) = decode_vst_ld_single(sz, nelem, index_align) else {
        return super::undefined_instruction(ir);
    };

    let d_reg = to_ext_reg_d(vd, d);
    let d_last = d_reg.index() + inc * (nelem - 1);
    if n == Reg::R15 || d_last + 1 > 32 {
        return super::unpredictable_instruction(ir);
    }

    let wback = m != Reg::R15;
    let register_index = m != Reg::R15 && m != Reg::R13;

    let mut address = ir.get_register(n);
    for i in 0..nelem {
        let element = read_memory(ir, ebytes * 8, address);
        let ext_reg = ExtReg::from_double((d_reg.index() + i * inc) as u8);
        let vec = ir.get_vector(ext_reg);
        let new_reg = ir.ir().vector_set_element(ebytes * 8, vec, index, element);
        ir.set_vector(ext_reg, new_reg);
        address = ir
            .ir()
            .add_32(address, Value::ImmU32(ebytes as u32), Value::ImmU1(false));
    }

    if wback {
        if register_index {
            let base = ir.get_register(n);
            let index_reg = ir.get_register(m);
            let updated = ir.ir().add_32(base, index_reg, Value::ImmU1(false));
            ir.set_register(n, updated);
        } else {
            let base = ir.get_register(n);
            let updated = ir.ir().add_32(
                base,
                Value::ImmU32((nelem * ebytes) as u32),
                Value::ImmU1(false),
            );
            ir.set_register(n, updated);
        }
    }

    true
}

/// VTRN (ASIMD) — Vector Transpose.
/// Port of upstream `asimd_VTRN`.
/// Encoding: 111100111D11zz10dddd00001QM0mmmm
pub fn arm_asimd_vtrn(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let d_bit = ((inst.raw >> 22) & 1) != 0;
    let sz = (inst.raw >> 18) & 3;
    let vd = (inst.raw >> 12) & 0xF;
    let q = ((inst.raw >> 6) & 1) != 0;
    let m_bit = ((inst.raw >> 5) & 1) != 0;
    let vm = inst.raw & 0xF;

    if sz == 3 {
        return super::undefined_instruction(ir);
    }

    if q && ((vd & 1) != 0 || (vm & 1) != 0) {
        return super::undefined_instruction(ir);
    }

    let esize = 8 << sz;
    let d_reg = to_vector_reg(q, d_bit, vd);
    let m_reg = to_vector_reg(q, m_bit, vm);

    let (Some(d_reg), Some(m_reg)) = (d_reg, m_reg) else {
        return super::undefined_instruction(ir);
    };

    if d_reg == m_reg {
        return super::unpredictable_instruction(ir);
    }

    let reg_d = ir.get_vector(d_reg);
    let reg_m = ir.get_vector(m_reg);
    let result_d = ir
        .ir()
        .vector_transpose(esize as usize, reg_d, reg_m, false);
    let result_m = ir.ir().vector_transpose(esize as usize, reg_d, reg_m, true);

    ir.set_vector(d_reg, result_d);
    ir.set_vector(m_reg, result_m);
    true
}

/// VEXT — Vector Extract.
/// Upstream: `asimd_VEXT` in asimd_misc.cpp.
/// Encoding: 111100101D11nnnnddddiiiiNQM0mmmm
pub fn arm_asimd_vext(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let d = (inst.raw >> 22) & 1 != 0;
    let vn = (inst.raw >> 16) & 0xF;
    let vd = (inst.raw >> 12) & 0xF;
    let imm4 = ((inst.raw >> 8) & 0xF) as u8;
    let n = (inst.raw >> 7) & 1 != 0;
    let q = (inst.raw >> 6) & 1 != 0;
    let m = (inst.raw >> 5) & 1 != 0;
    let vm = inst.raw & 0xF;

    if q && ((vd & 1) != 0 || (vn & 1) != 0 || (vm & 1) != 0) {
        return super::undefined_instruction(ir);
    }

    if !q && (imm4 >> 3) != 0 {
        return super::undefined_instruction(ir);
    }

    let position = 8 * imm4;
    let d_reg = match to_vector_reg(q, d, vd) {
        Some(r) => r,
        None => return super::undefined_instruction(ir),
    };
    let n_reg = match to_vector_reg(q, n, vn) {
        Some(r) => r,
        None => return super::undefined_instruction(ir),
    };
    let m_reg = match to_vector_reg(q, m, vm) {
        Some(r) => r,
        None => return super::undefined_instruction(ir),
    };

    let reg_n = ir.get_vector(n_reg);
    let reg_m = ir.get_vector(m_reg);
    let result = if q {
        ir.ir().vector_extract(reg_n, reg_m, position)
    } else {
        ir.ir().vector_extract_lower(reg_n, reg_m, position)
    };

    ir.set_vector(d_reg, result);
    true
}

fn table_lookup(ir: &mut A32IREmitter, inst: &DecodedArm, is_vtbl: bool) -> bool {
    // Upstream: `TableLookup` in A32 `asimd_misc.cpp`.
    // Encoding: 111100111D11nnnndddd10zzN{0,1}M0mmmm
    let d = (inst.raw >> 22) & 1 != 0;
    let vn = (inst.raw >> 16) & 0xF;
    let vd = (inst.raw >> 12) & 0xF;
    let len = ((inst.raw >> 8) & 0x3) as usize;
    let n = (inst.raw >> 7) & 1 != 0;
    let m = (inst.raw >> 5) & 1 != 0;
    let vm = inst.raw & 0xF;

    let length = len + 1;
    let d_reg = ExtReg::from_double(((if d { 16 } else { 0 }) + vd) as u8);
    let m_reg = ExtReg::from_double(((if m { 16 } else { 0 }) + vm) as u8);
    let n_base = ((if n { 16 } else { 0 }) + vn) as u8;

    if n_base as usize + length > 32 {
        return super::unpredictable_instruction(ir);
    }

    let mut table_values = Vec::with_capacity(length);
    for i in 0..length {
        table_values.push(ir.get_extended_register_64(ExtReg::from_double(n_base + i as u8)));
    }

    let table = ir.ir().vector_table(&table_values);
    let indices = ir.get_extended_register_64(m_reg);
    let defaults = if is_vtbl {
        Value::ImmU64(0)
    } else {
        ir.get_extended_register_64(d_reg)
    };
    let result = ir.ir().vector_table_lookup_64(defaults, table, indices);
    ir.set_extended_register_64(d_reg, result);
    true
}

/// VTBL — Vector Table Lookup.
/// Upstream: `asimd_VTBL` in `asimd_misc.cpp`.
pub fn arm_asimd_vtbl(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    table_lookup(ir, inst, true)
}

/// VTBX — Vector Table Extension.
/// Upstream: `asimd_VTBX` in `asimd_misc.cpp`.
pub fn arm_asimd_vtbx(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    table_lookup(ir, inst, false)
}

/// VUZP — Vector Unzip.
/// Upstream: `asimd_VUZP` in asimd_two_regs_misc.cpp.
/// Encoding: 111100111D11zz10dddd00010QM0mmmm
pub fn arm_asimd_vuzp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let d = (inst.raw >> 22) & 1 != 0;
    let sz = (inst.raw >> 18) & 0x3;
    let vd = (inst.raw >> 12) & 0xF;
    let q = (inst.raw >> 6) & 1 != 0;
    let m = (inst.raw >> 5) & 1 != 0;
    let vm = inst.raw & 0xF;

    if sz == 0b11 || (!q && sz == 0b10) {
        return super::undefined_instruction(ir);
    }

    if q && ((vd & 1) != 0 || (vm & 1) != 0) {
        return super::undefined_instruction(ir);
    }

    let esize = 8 << sz;
    let d_reg = match to_vector_reg(q, d, vd) {
        Some(r) => r,
        None => return super::undefined_instruction(ir),
    };
    let m_reg = match to_vector_reg(q, m, vm) {
        Some(r) => r,
        None => return super::undefined_instruction(ir),
    };

    if d_reg == m_reg {
        return super::unpredictable_instruction(ir);
    }

    let reg_d = ir.get_vector(d_reg);
    let reg_m = ir.get_vector(m_reg);
    let result_d = if q {
        ir.ir()
            .vector_deinterleave_even(esize as usize, reg_d, reg_m)
    } else {
        ir.ir()
            .vector_deinterleave_even_lower(esize as usize, reg_d, reg_m)
    };
    let result_m = if q {
        ir.ir()
            .vector_deinterleave_odd(esize as usize, reg_d, reg_m)
    } else {
        ir.ir()
            .vector_deinterleave_odd_lower(esize as usize, reg_d, reg_m)
    };

    ir.set_vector(d_reg, result_d);
    ir.set_vector(m_reg, result_m);
    true
}

/// VZIP — Vector Zip.
/// Upstream: `asimd_VZIP` in `asimd_two_regs_misc.cpp`.
/// Encoding: 111100111D11zz10dddd00011QM0mmmm
pub fn arm_asimd_vzip(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let d = (inst.raw >> 22) & 1 != 0;
    let sz = (inst.raw >> 18) & 0x3;
    let vd = (inst.raw >> 12) & 0xF;
    let q = (inst.raw >> 6) & 1 != 0;
    let m = (inst.raw >> 5) & 1 != 0;
    let vm = inst.raw & 0xF;

    if sz == 0b11 || (!q && sz == 0b10) {
        return super::undefined_instruction(ir);
    }

    if q && ((vd & 1) != 0 || (vm & 1) != 0) {
        return super::undefined_instruction(ir);
    }

    let esize = 8usize << sz;
    let Some(d_reg) = to_vector_reg(q, d, vd) else {
        return super::undefined_instruction(ir);
    };
    let Some(m_reg) = to_vector_reg(q, m, vm) else {
        return super::undefined_instruction(ir);
    };

    if d_reg == m_reg {
        return super::unpredictable_instruction(ir);
    }

    let reg_d = ir.get_vector(d_reg);
    let reg_m = ir.get_vector(m_reg);
    let result = ir.ir().vector_interleave_lower(esize, reg_d, reg_m);

    if q {
        let result_m = ir.ir().vector_interleave_upper(esize, reg_d, reg_m);
        ir.set_vector(d_reg, result);
        ir.set_vector(m_reg, result_m);
    } else {
        let result_d = ir.ir().vector_get_element(64, result, 0);
        let result_m = ir.ir().vector_get_element(64, result, 1);
        ir.set_extended_register_64(d_reg, result_d);
        ir.set_extended_register_64(m_reg, result_m);
    }
    true
}

/// VRECPE — Vector Reciprocal Estimate.
/// Upstream: `asimd_VRECPE` in `asimd_two_regs_misc.cpp`.
/// Encoding: 111100111D11zz11dddd010F0QM0mmmm
pub fn arm_asimd_vrecpe(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let d = (inst.raw >> 22) & 1 != 0;
    let sz = (inst.raw >> 18) & 0x3;
    let vd = (inst.raw >> 12) & 0xF;
    let f = (inst.raw >> 8) & 1 != 0;
    let q = (inst.raw >> 6) & 1 != 0;
    let m = (inst.raw >> 5) & 1 != 0;
    let vm = inst.raw & 0xF;

    if q && ((vd & 1) != 0 || (vm & 1) != 0) {
        return super::undefined_instruction(ir);
    }
    if sz == 0b00 || sz == 0b11 || (!f && sz == 0b01) {
        return super::undefined_instruction(ir);
    }

    let esize = 8usize << sz;
    let Some(d_reg) = to_vector_reg(q, d, vd) else {
        return super::undefined_instruction(ir);
    };
    let Some(m_reg) = to_vector_reg(q, m, vm) else {
        return super::undefined_instruction(ir);
    };
    let reg_m = ir.get_vector(m_reg);
    let result = if f {
        ir.ir().fp_vector_recip_estimate(esize, reg_m, false)
    } else {
        ir.ir().vector_unsigned_recip_estimate(reg_m)
    };
    ir.set_vector(d_reg, result);
    true
}

/// VRSQRTE — Vector Reciprocal Square Root Estimate.
/// Upstream: `asimd_VRSQRTE` in `asimd_two_regs_misc.cpp`.
/// Encoding: 111100111D11zz11dddd010F1QM0mmmm
pub fn arm_asimd_vrsqrte(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let d = (inst.raw >> 22) & 1 != 0;
    let sz = (inst.raw >> 18) & 0x3;
    let vd = (inst.raw >> 12) & 0xF;
    let f = (inst.raw >> 8) & 1 != 0;
    let q = (inst.raw >> 6) & 1 != 0;
    let m = (inst.raw >> 5) & 1 != 0;
    let vm = inst.raw & 0xF;

    if q && ((vd & 1) != 0 || (vm & 1) != 0) {
        return super::undefined_instruction(ir);
    }

    if sz == 0b00 || sz == 0b11 || (!f && sz == 0b01) {
        return super::undefined_instruction(ir);
    }

    let esize = 8usize << sz;
    let Some(d_reg) = to_vector_reg(q, d, vd) else {
        return super::undefined_instruction(ir);
    };
    let Some(m_reg) = to_vector_reg(q, m, vm) else {
        return super::undefined_instruction(ir);
    };
    let reg_m = ir.get_vector(m_reg);
    let result = if f {
        ir.ir().fp_vector_rsqrt_estimate(esize, reg_m, false)
    } else {
        ir.ir().vector_unsigned_recip_sqrt_estimate(reg_m)
    };
    ir.set_vector(d_reg, result);
    true
}

/// VNEG — negate each element.
/// Upstream: `asimd_VNEG`.
pub fn arm_asimd_vneg_int(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let d = (inst.raw >> 22) & 1 != 0;
    let sz = (inst.raw >> 18) & 0x3;
    let vd = (inst.raw >> 12) & 0xF;
    let f = (inst.raw >> 10) & 1 != 0;
    let q = (inst.raw >> 6) & 1 != 0;
    let m = (inst.raw >> 5) & 1 != 0;
    let vm = inst.raw & 0xF;

    if sz == 0b11 || (f && sz != 0b10) || (q && ((vd & 1) != 0 || (vm & 1) != 0)) {
        return super::undefined_instruction(ir);
    }

    let esize = 8 << sz;
    let d_reg = match to_vector_reg(q, d, vd) {
        Some(r) => r,
        None => return super::undefined_instruction(ir),
    };
    let m_reg = match to_vector_reg(q, m, vm) {
        Some(r) => r,
        None => return super::undefined_instruction(ir),
    };

    let reg_m = ir.get_vector(m_reg);
    let result = if f {
        ir.ir().fp_vector_neg(32, reg_m)
    } else {
        let zero = ir.ir().zero_vector();
        ir.ir().vector_sub(esize as usize, zero, reg_m)
    };
    ir.set_vector(d_reg, result);
    true
}

/// VABS — absolute value of each element.
/// Upstream: `asimd_VABS`.
pub fn arm_asimd_vabs_int(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let d = (inst.raw >> 22) & 1 != 0;
    let sz = (inst.raw >> 18) & 0x3;
    let vd = (inst.raw >> 12) & 0xF;
    let f = (inst.raw >> 10) & 1 != 0;
    let q = (inst.raw >> 6) & 1 != 0;
    let m = (inst.raw >> 5) & 1 != 0;
    let vm = inst.raw & 0xF;

    if sz == 0b11 || (f && sz != 0b10) || (q && ((vd & 1) != 0 || (vm & 1) != 0)) {
        return super::undefined_instruction(ir);
    }

    let esize = 8 << sz;
    let d_reg = match to_vector_reg(q, d, vd) {
        Some(r) => r,
        None => return super::undefined_instruction(ir),
    };
    let m_reg = match to_vector_reg(q, m, vm) {
        Some(r) => r,
        None => return super::undefined_instruction(ir),
    };

    let reg_m = ir.get_vector(m_reg);
    let result = if f {
        ir.ir().fp_vector_abs(32, reg_m)
    } else {
        ir.ir().vector_abs(esize as usize, reg_m)
    };
    ir.set_vector(d_reg, result);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::decoder::ArmInstId;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::terminal::Terminal;

    fn translate_with(
        raw: u32,
        id: ArmInstId,
        f: fn(&mut A32IREmitter, &DecodedArm) -> bool,
    ) -> Vec<Opcode> {
        let loc = A32LocationDescriptor::at(0x1000);
        let mut block = Block::new(loc.to_location());
        let ok = {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            f(&mut ir, &DecodedArm { raw, id })
        };
        assert!(ok);
        block.instructions.iter().map(|inst| inst.opcode).collect()
    }

    /// Assert that translating `raw` stops the block by raising exactly
    /// `expected` with upstream's full `RaiseException` lifecycle
    /// (UpdateUpperLocationDescriptor + BranchWritePC + ExceptionRaised +
    /// CheckHalt/ReturnToDispatch terminal).
    fn assert_exception_translation(
        raw: u32,
        id: ArmInstId,
        f: fn(&mut A32IREmitter, &DecodedArm) -> bool,
        expected: crate::frontend::a32::types::Exception,
    ) {
        let loc = A32LocationDescriptor::at(0x1000);
        let mut block = Block::new(loc.to_location());
        let should_continue = {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            f(&mut ir, &DecodedArm { raw, id })
        };

        assert!(!should_continue);
        let opcodes: Vec<_> = block.instructions.iter().map(|inst| inst.opcode).collect();
        assert!(opcodes.contains(&Opcode::A32UpdateUpperLocationDescriptor));
        assert!(opcodes.contains(&Opcode::A32SetRegister)); // BranchWritePC(PC+4)
        assert_eq!(opcodes.last(), Some(&Opcode::A32ExceptionRaised));

        // The exact exception kind must match — not merely "some exception".
        let exc = block
            .instructions
            .iter()
            .rev()
            .find(|inst| inst.opcode == Opcode::A32ExceptionRaised)
            .expect("exception instruction present");
        match exc.args[1] {
            Value::ImmU64(code) => assert_eq!(code, expected.as_u32() as u64),
            ref other => panic!("exception code is not an immediate: {other:?}"),
        }

        assert!(matches!(
            block.terminal,
            Terminal::CheckHalt { ref else_ }
                if matches!(else_.as_ref(), Terminal::ReturnToDispatch)
        ));
    }

    fn assert_undefined_translation(
        raw: u32,
        id: ArmInstId,
        f: fn(&mut A32IREmitter, &DecodedArm) -> bool,
    ) {
        assert_exception_translation(
            raw,
            id,
            f,
            crate::frontend::a32::types::Exception::UndefinedInstruction,
        );
    }

    #[test]
    fn vtbl_emits_table_lookup64_for_regression_opcode() {
        let opcodes = translate_with(0xF3F8_19AC, ArmInstId::AsimdVtbl, arm_asimd_vtbl);
        assert_eq!(
            opcodes
                .iter()
                .filter(|&&op| op == Opcode::A32GetExtendedRegister64)
                .count(),
            3
        );
        assert!(opcodes.contains(&Opcode::VectorTable));
        assert!(opcodes.contains(&Opcode::VectorTableLookup64));
        assert_eq!(opcodes.last(), Some(&Opcode::A32SetExtendedRegister64));
    }

    #[test]
    fn vtbx_uses_destination_as_lookup_default() {
        let opcodes = translate_with(0xF3F8_19EC, ArmInstId::AsimdVtbx, arm_asimd_vtbx);
        assert_eq!(
            opcodes
                .iter()
                .filter(|&&op| op == Opcode::A32GetExtendedRegister64)
                .count(),
            4
        );
        assert!(opcodes.contains(&Opcode::VectorTable));
        assert!(opcodes.contains(&Opcode::VectorTableLookup64));
        assert_eq!(opcodes.last(), Some(&Opcode::A32SetExtendedRegister64));
    }

    #[test]
    fn vneg_float_emits_fp_vector_neg_for_regression_opcode() {
        let opcodes = translate_with(0xF3F9_67E6, ArmInstId::AsimdVnegInt, arm_asimd_vneg_int);
        assert!(opcodes.contains(&Opcode::FPVectorNeg32));
        assert!(!opcodes.contains(&Opcode::A32ExceptionRaised));
        assert_eq!(opcodes.last(), Some(&Opcode::A32SetVector));
    }

    #[test]
    fn vabs_float_emits_fp_vector_abs() {
        let opcodes = translate_with(0xF3F9_6766, ArmInstId::AsimdVabsInt, arm_asimd_vabs_int);
        assert!(opcodes.contains(&Opcode::FPVectorAbs32));
        assert!(!opcodes.contains(&Opcode::A32ExceptionRaised));
        assert_eq!(opcodes.last(), Some(&Opcode::A32SetVector));
    }

    #[test]
    fn vzip_q_emits_both_interleaves_for_regression_opcodes() {
        for raw in [0xF3FA_21E0, 0xF3FA_41E6] {
            let opcodes = translate_with(raw, ArmInstId::AsimdVzip, arm_asimd_vzip);
            assert!(opcodes.contains(&Opcode::VectorInterleaveLower32));
            assert!(opcodes.contains(&Opcode::VectorInterleaveUpper32));
            assert_eq!(
                opcodes
                    .iter()
                    .filter(|&&op| op == Opcode::A32SetVector)
                    .count(),
                2
            );
        }
    }

    #[test]
    fn vzip_d_splits_interleaved_result_into_two_registers() {
        let opcodes = translate_with(0xF3B6_0181, ArmInstId::AsimdVzip, arm_asimd_vzip);
        assert!(opcodes.contains(&Opcode::VectorInterleaveLower16));
        assert_eq!(
            opcodes
                .iter()
                .filter(|&&op| op == Opcode::VectorGetElement64)
                .count(),
            2
        );
        assert_eq!(
            opcodes
                .iter()
                .filter(|&&op| op == Opcode::A32SetExtendedRegister64)
                .count(),
            2
        );
    }

    #[test]
    fn vrsqrte_emits_fp_estimate_for_regression_opcodes() {
        for raw in [0xF3FB_05E2, 0xF3FB_85E4, 0xF3FB_E5E6] {
            let opcodes = translate_with(raw, ArmInstId::AsimdVrsqrte, arm_asimd_vrsqrte);
            assert!(opcodes.contains(&Opcode::FPVectorRSqrtEstimate32));
            assert!(!opcodes.contains(&Opcode::A32ExceptionRaised));
            assert_eq!(opcodes.last(), Some(&Opcode::A32SetVector));
        }
    }

    #[test]
    fn vrecpe_emits_fp_estimate() {
        let opcodes = translate_with(0xF3FB_0562, ArmInstId::AsimdVrecpe, arm_asimd_vrecpe);
        assert!(opcodes.contains(&Opcode::FPVectorRecipEstimate32));
        assert!(!opcodes.contains(&Opcode::A32ExceptionRaised));
        assert_eq!(opcodes.last(), Some(&Opcode::A32SetVector));
    }

    #[test]
    fn vceq_zero_emits_fp_equal_for_regression_opcodes() {
        for raw in [0xF3B9_6562, 0xF3B9_4564, 0xF3B9_2566] {
            let opcodes = translate_with(raw, ArmInstId::AsimdVceqZero, arm_asimd_vceq_zero);
            assert!(opcodes.contains(&Opcode::FPVectorEqual32));
            assert!(!opcodes.contains(&Opcode::A32ExceptionRaised));
            assert_eq!(opcodes.last(), Some(&Opcode::A32SetVector));
        }
    }

    #[test]
    fn compare_and_estimate_undefined_encodings_stop_translation() {
        assert_undefined_translation(0xF3BD_6562, ArmInstId::AsimdVceqZero, arm_asimd_vceq_zero);
        assert_undefined_translation(0xF3F3_0562, ArmInstId::AsimdVrecpe, arm_asimd_vrecpe);
        assert_undefined_translation(0xF3F3_05E2, ArmInstId::AsimdVrsqrte, arm_asimd_vrsqrte);
    }

    #[test]
    fn vtrn_undefined_size_stops_translation() {
        // sz==0b11 is UNDEFINED: previously returned `true` without a terminal,
        // letting the block keep translating past a raised exception.
        assert_undefined_translation(0xF3BE_0080, ArmInstId::AsimdVtrn, arm_asimd_vtrn);
    }

    #[test]
    fn vrhadd_undefined_size_stops_translation() {
        // sz==0b11 is UNDEFINED (upstream raises UndefinedInstruction, not
        // Unpredictable); either way it must stop translation with a terminal.
        assert_undefined_translation(0xF230_0100, ArmInstId::AsimdVrhadd, arm_asimd_vrhadd);
    }

    #[test]
    fn vld_all_lanes_broadcasts_for_regression_wedge_opcode() {
        // 0xF4E32CBF: VLD1.32 {d[]} broadcast, T=1 → regs=2, nelem=1, m=R15
        // (no writeback). Must emit exactly one 32-bit read + one 32-bit
        // broadcast, written to both destination D regs, no exception, and no
        let opcodes = translate_with(0xF4E3_2CBF, ArmInstId::V8VldAllLanes, arm_v8_vld_all_lanes);
        assert!(opcodes.contains(&Opcode::VectorBroadcast32));
        assert!(!opcodes.contains(&Opcode::A32ExceptionRaised));
        assert_eq!(
            opcodes
                .iter()
                .filter(|&&op| op == Opcode::A32ReadMemory32)
                .count(),
            1
        );
        assert_eq!(
            opcodes
                .iter()
                .filter(|&&op| op == Opcode::A32SetVector)
                .count(),
            2
        );
        // m == R15 → no writeback, so the base register is never rewritten.
        assert!(!opcodes.contains(&Opcode::A32SetRegister));
    }

    #[test]
    fn vld_all_lanes_immediate_writeback_updates_base() {
        // Same as above but m=R13 (SP) → immediate writeback (nelem*ebytes).
        // Encoding: swap m from 0xF to 0xD.
        let opcodes = translate_with(0xF4E3_2CBD, ArmInstId::V8VldAllLanes, arm_v8_vld_all_lanes);
        assert!(opcodes.contains(&Opcode::VectorBroadcast32));
        // Writeback rewrites the base register exactly once.
        assert_eq!(
            opcodes
                .iter()
                .filter(|&&op| op == Opcode::A32SetRegister)
                .count(),
            1
        );
    }

    #[test]
    fn vld_single_sets_one_lane() {
        // VLD1.32 {d[0]}: 111101001 D 10 nnnn dddd 10 00 0000 mmmm
        // sz=0b10, nn=0 (nelem=1), index_align=0 → index 0, m=R15 (no writeback).
        let opcodes = translate_with(0xF4A0_080F, ArmInstId::V8VldSingle, arm_v8_vld_single);
        assert!(opcodes.contains(&Opcode::A32ReadMemory32));
        assert!(opcodes.contains(&Opcode::VectorSetElement32));
        assert!(!opcodes.contains(&Opcode::A32ExceptionRaised));
        // Exactly one lane written back into the destination vector register.
        assert_eq!(
            opcodes
                .iter()
                .filter(|&&op| op == Opcode::A32SetVector)
                .count(),
            1
        );
    }

    #[test]
    fn vst_single_stores_one_lane() {
        // VST1.32 {d[0]}: 111101001 D 00 nnnn dddd 10 00 0000 mmmm, m=R15.
        let opcodes = translate_with(0xF480_080F, ArmInstId::V8VstSingle, arm_v8_vst_single);
        assert!(opcodes.contains(&Opcode::VectorGetElement32));
        assert!(opcodes.contains(&Opcode::A32WriteMemory32));
        assert!(!opcodes.contains(&Opcode::A32ExceptionRaised));
    }

    #[test]
    fn arm_udf_raises_undefined_with_full_lifecycle() {
        // The reserved ASIMD load/store and VFP encodings decode to ArmInstId::UDF,
        // which dispatches to arm_udf. Verify it raises exactly UndefinedInstruction
        // with UpdateUpperLocationDescriptor + BranchWritePC(PC+4) + terminal —
        // not merely "some exception".
        assert_exception_translation(
            0xF400_0B00,
            ArmInstId::UDF,
            crate::frontend::a32::translate::exception::arm_udf,
            crate::frontend::a32::types::Exception::UndefinedInstruction,
        );
    }

    #[test]
    fn scalar_multiply_reserved_size_raises_decode_error() {
        // VMLA (scalar) with sz==0b11 is a DecodeError upstream (not Undefined).
        // Encoding: 1111001Q1Dzznnnndddd0o0FN1M0mmmm with zz=0b11.
        assert_exception_translation(
            0xF2F0_0140,
            ArmInstId::AsimdVmlaScalar,
            arm_asimd_vmla_scalar,
            crate::frontend::a32::types::Exception::DecodeError,
        );
    }
}

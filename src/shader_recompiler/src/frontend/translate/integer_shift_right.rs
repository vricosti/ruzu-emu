// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/shader_recompiler/frontend/maxwell/translate/impl/integer_shift_right.cpp

use super::{bit, field, TranslatorVisitor};
use crate::frontend::maxwell_opcodes::{MaxwellOpcode, SrcType};
use crate::ir::value::Value;

/// Core SHR logic, matching upstream's `SHR(TranslatorVisitor& v, u64 insn, const IR::U32& shift)`.
fn shr_inner(tv: &mut TranslatorVisitor, insn: u64, shift: Value) {
    let dst = field(insn, 0, 8);
    let src_a_idx = field(insn, 8, 8);
    let is_wrapped = bit(insn, 39);
    let brev = bit(insn, 40);
    let xmode = bit(insn, 43);
    let cc = bit(insn, 47);
    let is_signed = bit(insn, 48);

    // Upstream throws NotImplementedException for SHR.XMODE and SHR.CC.
    if xmode {
        panic!("SHR.XMODE not implemented");
    }
    if cc {
        panic!("SHR.CC not implemented");
    }
    let _ = dst;

    let mut base = tv.x(src_a_idx);
    if brev {
        base = tv.ir.bit_reverse_32(base);
    }

    let safe_shift = if is_wrapped {
        tv.ir.bitwise_and_32(shift, Value::ImmU32(31))
    } else {
        shift
    };

    let mut result = if is_signed {
        tv.ir.shift_right_arithmetic_32(base, safe_shift)
    } else {
        tv.ir.shift_right_logical_32(base, safe_shift)
    };

    if !is_wrapped {
        // clamp out-of-range shifts: shift >= 32 yields 0 (unsigned) or sign-extension (signed)
        let is_safe = tv.ir.u_less_than(shift, Value::ImmU32(32));
        let clamped = if is_signed {
            // negative (signed) -> -1 (all ones), non-negative -> 0
            let is_negative = tv.ir.s_less_than(result, Value::ImmU32(0));
            tv.ir
                .select_u32(is_negative, Value::ImmU32(u32::MAX), Value::ImmU32(0))
        } else {
            Value::ImmU32(0)
        };
        result = tv.ir.select_u32(is_safe, result, clamped);
    }

    tv.set_x(dst, result);
}

/// SHR — Shift right (logical or arithmetic, reg/cbuf/imm variants).
pub fn shr(tv: &mut TranslatorVisitor, insn: u64, opcode: MaxwellOpcode) {
    let shift = match opcode.src_type() {
        SrcType::Register => {
            let reg_idx = field(insn, 20, 8);
            tv.x(reg_idx)
        }
        SrcType::ConstantBuffer => {
            let cb_index = field(insn, 34, 5);
            let cb_offset = field(insn, 20, 14) << 2;
            let binding = Value::ImmU32(cb_index);
            let offset = Value::ImmU32(cb_offset);
            tv.ir.get_cbuf_u32(binding, offset)
        }
        SrcType::Immediate => {
            // GetImm20: 20-bit sign-extended immediate
            let imm = field(insn, 20, 20);
            let sign_ext = if imm & (1 << 19) != 0 {
                imm | !((1u32 << 20) - 1)
            } else {
                imm
            };
            Value::ImmU32(sign_ext)
        }
    };
    shr_inner(tv, insn, shift);
}

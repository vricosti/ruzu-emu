// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/shader_recompiler/frontend/maxwell/translate/impl/integer_shift_left.cpp

use super::{bit, field, TranslatorVisitor};
use crate::frontend::maxwell_opcodes::{MaxwellOpcode, SrcType};
use crate::ir::value::Value;

/// Core SHL logic, matching upstream's `SHL(TranslatorVisitor& v, u64 insn, const IR::U32& unsafe_shift)`.
fn shl_inner(tv: &mut TranslatorVisitor, insn: u64, unsafe_shift: Value) {
    let dst = field(insn, 0, 8);
    let src_a_idx = field(insn, 8, 8);
    let w = bit(insn, 39);
    let x = bit(insn, 43);
    let cc = bit(insn, 47);

    // Upstream throws NotImplementedException for SHL.X and SHL.CC.
    if x {
        panic!("SHL.X not implemented");
    }
    if cc {
        panic!("SHL.CC not implemented");
    }
    let _ = dst;

    let base = tv.x(src_a_idx);
    let result = if w {
        // .W: wrap shift into [0,31]
        let shift = tv.ir.bitwise_and_32(unsafe_shift, Value::ImmU32(31));
        tv.ir.shift_left_logical_32(base, shift)
    } else {
        // no .W: clamp — shift >= 32 yields 0
        let is_safe = tv.ir.u_less_than(unsafe_shift, Value::ImmU32(32));
        let unsafe_result = tv.ir.shift_left_logical_32(base, unsafe_shift);
        tv.ir.select_u32(is_safe, unsafe_result, Value::ImmU32(0))
    };

    tv.set_x(dst, result);
}

/// SHL — Shift left logical (reg/cbuf/imm variants).
pub fn shl(tv: &mut TranslatorVisitor, insn: u64, opcode: MaxwellOpcode) {
    let unsafe_shift = match opcode.src_type() {
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
    shl_inner(tv, insn, unsafe_shift);
}

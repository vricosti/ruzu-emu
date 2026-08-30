// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/shader_recompiler/frontend/maxwell/translate/impl/integer_popcount.cpp

use super::{bit, field, TranslatorVisitor};
use crate::frontend::maxwell_opcodes::{MaxwellOpcode, SrcType};
use crate::ir::value::Value;

/// Core POPC logic, matching upstream's `POPC(TranslatorVisitor& v, u64 insn, const IR::U32& src)`.
fn popc_inner(tv: &mut TranslatorVisitor, insn: u64, src: Value) {
    let dst = field(insn, 0, 8);
    let tilde = bit(insn, 40);

    let operand = if tilde {
        tv.ir.bitwise_not_32(src)
    } else {
        src
    };
    let result = tv.ir.bit_count_32(operand);
    tv.set_x(dst, result);
}

/// POPC — Population count (reg/cbuf/imm variants).
pub fn popc(tv: &mut TranslatorVisitor, insn: u64, opcode: MaxwellOpcode) {
    let src = match opcode.src_type() {
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
            let imm = field(insn, 20, 20);
            let sign_ext = if imm & (1 << 19) != 0 {
                imm | !((1u32 << 20) - 1)
            } else {
                imm
            };
            Value::ImmU32(sign_ext)
        }
    };
    popc_inner(tv, insn, src);
}

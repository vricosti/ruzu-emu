// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of upstream `impl/warp_shuffle.cpp`.

use super::{bit, field, TranslatorVisitor};
use crate::ir::types::ShaderStage;
use crate::ir::value::{Pred, Value};

// Upstream `QUAD_MASK`: segmentation mask 28 (lanes stay within their quad)
// with clamp 3.
const QUAD_MASK: u32 = (28 << 8) | 3;

/// SHFL — Warp shuffle.
///
/// Matches upstream `TranslatorVisitor::SHFL(u64 insn)`.
pub fn shfl(v: &mut TranslatorVisitor<'_>, insn: u64) {
    let dest_reg = field(insn, 0, 8);
    let src_reg = field(insn, 8, 8);
    let pred_idx = field(insn, 48, 3);
    let mode = (insn >> 30) & 3;

    // src_a: index operand — register or 5-bit immediate
    let src_a_flag = bit(insn, 28);
    let src_a_imm = field(insn, 20, 5);
    let src_a = if src_a_flag {
        Value::ImmU32(src_a_imm)
    } else {
        v.x(field(insn, 20, 8))
    };

    // src_b: mask operand — register or 13-bit immediate
    let src_b_flag = bit(insn, 29);
    let src_b_imm = field(insn, 34, 13);
    let src_b = if src_b_flag {
        Value::ImmU32(src_b_imm)
    } else {
        v.x(field(insn, 39, 8))
    };

    // clamp = mask[4:0], seg_mask = mask[12:8]
    // Upstream: a fragment-stage shuffle with immediate operands and the quad
    // mask is lowered to the quad group operations (QuadBroadcast / QuadSwap),
    // which are always in bounds.
    let is_quad_candidate = src_b_flag
        && src_b_imm == QUAD_MASK
        && src_a_flag
        && v.ir.program.stage == ShaderStage::Fragment;
    if is_quad_candidate {
        // mode 0 = IDX, mode 3 = BFLY
        if mode == 0 && src_a_imm <= 3 {
            let value = v.x(src_reg);
            let result = v.ir.quad_broadcast(value, Value::ImmU32(src_a_imm));
            v.set_x(dest_reg, result);
            v.ir.set_pred(Pred(pred_idx as u8), Value::ImmU1(true));
            return;
        }
        if mode == 3 && (1..=3).contains(&src_a_imm) {
            let value = v.x(src_reg);
            let result = v.ir.quad_swap(value, Value::ImmU32(src_a_imm - 1));
            v.set_x(dest_reg, result);
            v.ir.set_pred(Pred(pred_idx as u8), Value::ImmU1(true));
            return;
        }
    }
    let clamp =
        v.ir.bit_field_u_extract(src_b, Value::ImmU32(0), Value::ImmU32(5));
    let seg_mask =
        v.ir.bit_field_u_extract(src_b, Value::ImmU32(8), Value::ImmU32(5));

    let value = v.x(src_reg);
    // Upstream `TranslatorVisitor::SHFL` dispatches on the 2-bit mode:
    //   0 = IDX, 1 = UP, 2 = DOWN, 3 = BFLY (matching `IR::ShuffleMode`).
    // Each routes to a distinct IR opcode (`Shuffle{Index,Up,Down,Butterfly}`).
    let result = match mode {
        0 => v.ir.shuffle_index(value, src_a, clamp, seg_mask),
        1 => v.ir.shuffle_up(value, src_a, clamp, seg_mask),
        2 => v.ir.shuffle_down(value, src_a, clamp, seg_mask),
        3 => v.ir.shuffle_butterfly(value, src_a, clamp, seg_mask),
        _ => unreachable!("2-bit mode field"),
    };

    let in_bounds = v.ir.get_in_bounds_from_op(result);
    v.ir.set_pred(Pred(pred_idx as u8), in_bounds);
    v.set_x(dest_reg, result);
}

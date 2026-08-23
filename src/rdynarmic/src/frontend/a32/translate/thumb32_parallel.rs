//! Thumb32 parallel addition and subtraction.
//!
//! Upstream owner: `frontend/A32/translate/impl/thumb32_parallel.cpp`.

use super::helpers::{most_significant_half, pack_2x16_to_1x32};
use crate::frontend::a32::decoder_thumb32::DecodedThumb32;
use crate::frontend::a32::types::Reg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::value::Value;

pub fn thumb32_sadd8(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_add_s8(reg_n, reg_m);
    ir.set_register(d, result.result);
    ir.set_ge_flags(result.ge);
    true
}

pub fn thumb32_sadd16(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_add_s16(reg_n, reg_m);
    ir.set_register(d, result.result);
    ir.set_ge_flags(result.ge);
    true
}

pub fn thumb32_sasx(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_add_sub_s16(reg_n, reg_m);
    ir.set_register(d, result.result);
    ir.set_ge_flags(result.ge);
    true
}

pub fn thumb32_ssax(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_sub_add_s16(reg_n, reg_m);
    ir.set_register(d, result.result);
    ir.set_ge_flags(result.ge);
    true
}

pub fn thumb32_ssub8(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_sub_s8(reg_n, reg_m);
    ir.set_register(d, result.result);
    ir.set_ge_flags(result.ge);
    true
}

pub fn thumb32_ssub16(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_sub_s16(reg_n, reg_m);
    ir.set_register(d, result.result);
    ir.set_ge_flags(result.ge);
    true
}

pub fn thumb32_uadd8(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_add_u8(reg_n, reg_m);
    ir.set_register(d, result.result);
    ir.set_ge_flags(result.ge);
    true
}

pub fn thumb32_uadd16(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_add_u16(reg_n, reg_m);
    ir.set_register(d, result.result);
    ir.set_ge_flags(result.ge);
    true
}

pub fn thumb32_uasx(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_add_sub_u16(reg_n, reg_m);
    ir.set_register(d, result.result);
    ir.set_ge_flags(result.ge);
    true
}

pub fn thumb32_usax(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_sub_add_u16(reg_n, reg_m);
    ir.set_register(d, result.result);
    ir.set_ge_flags(result.ge);
    true
}

pub fn thumb32_usub8(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_sub_u8(reg_n, reg_m);
    ir.set_register(d, result.result);
    ir.set_ge_flags(result.ge);
    true
}

pub fn thumb32_usub16(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_sub_u16(reg_n, reg_m);
    ir.set_register(d, result.result);
    ir.set_ge_flags(result.ge);
    true
}

pub fn thumb32_qadd8(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_saturated_add_s8(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_qadd16(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_saturated_add_s16(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_qsub8(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_saturated_sub_s8(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_qsub16(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_saturated_sub_s16(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_uqadd8(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_saturated_add_u8(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_uqadd16(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_saturated_add_u16(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_uqsub8(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_saturated_sub_u8(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_uqsub16(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_saturated_sub_u16(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_shadd8(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_halving_add_s8(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_shadd16(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_halving_add_s16(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_shasx(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_halving_add_sub_s16(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_shsax(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_halving_sub_add_s16(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_shsub8(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_halving_sub_s8(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_shsub16(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_halving_sub_s16(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_uhadd8(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_halving_add_u8(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_uhadd16(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_halving_add_u16(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_uhasx(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_halving_add_sub_u16(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_uhsax(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_halving_sub_add_u16(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_uhsub8(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_halving_sub_u8(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_uhsub16(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_halving_sub_u16(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_qasx(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_n = ir.get_register(n);
    let reg_m = ir.get_register(m);
    let half = ir.ir().least_significant_half(reg_n);
    let n_lo = ir.ir().sign_extend_half_to_word(half);
    let half = most_significant_half(ir, reg_n);
    let n_hi = ir.ir().sign_extend_half_to_word(half);
    let half = ir.ir().least_significant_half(reg_m);
    let m_lo = ir.ir().sign_extend_half_to_word(half);
    let half = most_significant_half(ir, reg_m);
    let m_hi = ir.ir().sign_extend_half_to_word(half);
    let arithmetic = ir.ir().sub_32(n_lo, m_hi, Value::ImmU1(true));
    let low = ir.ir().signed_saturation(arithmetic, 16).result;
    let arithmetic = ir.ir().add_32(n_hi, m_lo, Value::ImmU1(false));
    let high = ir.ir().signed_saturation(arithmetic, 16).result;
    let result = pack_2x16_to_1x32(ir, low, high);
    ir.set_register(d, result);
    true
}

pub fn thumb32_qsax(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_n = ir.get_register(n);
    let reg_m = ir.get_register(m);
    let half = ir.ir().least_significant_half(reg_n);
    let n_lo = ir.ir().sign_extend_half_to_word(half);
    let half = most_significant_half(ir, reg_n);
    let n_hi = ir.ir().sign_extend_half_to_word(half);
    let half = ir.ir().least_significant_half(reg_m);
    let m_lo = ir.ir().sign_extend_half_to_word(half);
    let half = most_significant_half(ir, reg_m);
    let m_hi = ir.ir().sign_extend_half_to_word(half);
    let arithmetic = ir.ir().add_32(n_lo, m_hi, Value::ImmU1(false));
    let low = ir.ir().signed_saturation(arithmetic, 16).result;
    let arithmetic = ir.ir().sub_32(n_hi, m_lo, Value::ImmU1(true));
    let high = ir.ir().signed_saturation(arithmetic, 16).result;
    let result = pack_2x16_to_1x32(ir, low, high);
    ir.set_register(d, result);
    true
}

pub fn thumb32_uqasx(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_n = ir.get_register(n);
    let reg_m = ir.get_register(m);
    let half = ir.ir().least_significant_half(reg_n);
    let n_lo = ir.ir().zero_extend_half_to_word(half);
    let half = most_significant_half(ir, reg_n);
    let n_hi = ir.ir().zero_extend_half_to_word(half);
    let half = ir.ir().least_significant_half(reg_m);
    let m_lo = ir.ir().zero_extend_half_to_word(half);
    let half = most_significant_half(ir, reg_m);
    let m_hi = ir.ir().zero_extend_half_to_word(half);
    let arithmetic = ir.ir().sub_32(n_lo, m_hi, Value::ImmU1(true));
    let low = ir.ir().unsigned_saturation(arithmetic, 16).result;
    let arithmetic = ir.ir().add_32(n_hi, m_lo, Value::ImmU1(false));
    let high = ir.ir().unsigned_saturation(arithmetic, 16).result;
    let result = pack_2x16_to_1x32(ir, low, high);
    ir.set_register(d, result);
    true
}

pub fn thumb32_uqsax(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_n = ir.get_register(n);
    let reg_m = ir.get_register(m);
    let half = ir.ir().least_significant_half(reg_n);
    let n_lo = ir.ir().zero_extend_half_to_word(half);
    let half = most_significant_half(ir, reg_n);
    let n_hi = ir.ir().zero_extend_half_to_word(half);
    let half = ir.ir().least_significant_half(reg_m);
    let m_lo = ir.ir().zero_extend_half_to_word(half);
    let half = most_significant_half(ir, reg_m);
    let m_hi = ir.ir().zero_extend_half_to_word(half);
    let arithmetic = ir.ir().add_32(n_lo, m_hi, Value::ImmU1(false));
    let low = ir.ir().unsigned_saturation(arithmetic, 16).result;
    let arithmetic = ir.ir().sub_32(n_hi, m_lo, Value::ImmU1(true));
    let high = ir.ir().unsigned_saturation(arithmetic, 16).result;
    let result = pack_2x16_to_1x32(ir, low, high);
    ir.set_register(d, result);
    true
}

#[cfg(test)]
mod tests {
    use crate::frontend::a32::decoder_thumb32::{decode_thumb32, Thumb32InstId};
    use crate::frontend::a32::types::{Exception, Reg};
    use crate::ir::a32_emitter::A32IREmitter;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::value::Value;

    const PATTERNS: &[(u32, Thumb32InstId)] = &[
        (0xfa90_f000, Thumb32InstId::SADD16),
        (0xfaa0_f000, Thumb32InstId::SASX),
        (0xfae0_f000, Thumb32InstId::SSAX),
        (0xfad0_f000, Thumb32InstId::SSUB16),
        (0xfa80_f000, Thumb32InstId::SADD8),
        (0xfac0_f000, Thumb32InstId::SSUB8),
        (0xfa90_f010, Thumb32InstId::QADD16),
        (0xfaa0_f010, Thumb32InstId::QASX),
        (0xfae0_f010, Thumb32InstId::QSAX),
        (0xfad0_f010, Thumb32InstId::QSUB16),
        (0xfa80_f010, Thumb32InstId::QADD8),
        (0xfac0_f010, Thumb32InstId::QSUB8),
        (0xfa90_f020, Thumb32InstId::SHADD16),
        (0xfaa0_f020, Thumb32InstId::SHASX),
        (0xfae0_f020, Thumb32InstId::SHSAX),
        (0xfad0_f020, Thumb32InstId::SHSUB16),
        (0xfa80_f020, Thumb32InstId::SHADD8),
        (0xfac0_f020, Thumb32InstId::SHSUB8),
        (0xfa90_f040, Thumb32InstId::UADD16),
        (0xfaa0_f040, Thumb32InstId::UASX),
        (0xfae0_f040, Thumb32InstId::USAX),
        (0xfad0_f040, Thumb32InstId::USUB16),
        (0xfa80_f040, Thumb32InstId::UADD8),
        (0xfac0_f040, Thumb32InstId::USUB8),
        (0xfa90_f050, Thumb32InstId::UQADD16),
        (0xfaa0_f050, Thumb32InstId::UQASX),
        (0xfae0_f050, Thumb32InstId::UQSAX),
        (0xfad0_f050, Thumb32InstId::UQSUB16),
        (0xfa80_f050, Thumb32InstId::UQADD8),
        (0xfac0_f050, Thumb32InstId::UQSUB8),
        (0xfa90_f060, Thumb32InstId::UHADD16),
        (0xfaa0_f060, Thumb32InstId::UHASX),
        (0xfae0_f060, Thumb32InstId::UHSAX),
        (0xfad0_f060, Thumb32InstId::UHSUB16),
        (0xfa80_f060, Thumb32InstId::UHADD8),
        (0xfac0_f060, Thumb32InstId::UHSUB8),
    ];

    fn translate(raw: u32) -> (bool, Block) {
        let location = A32LocationDescriptor::at(0x1000).set_t_flag(true);
        let decoded = decode_thumb32((raw >> 16) as u16, raw as u16);
        let mut block = Block::new(location.to_location());
        let result = {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            super::super::thumb32::translate_thumb32(
                &mut ir,
                &decoded,
                super::super::TranslationOptions::default(),
            )
        };
        (result, block)
    }

    #[test]
    fn all_parallel_patterns_decode_and_translate() {
        for &(expected, id) in PATTERNS {
            let raw = expected | 0x0001_0203;
            assert_eq!(
                decode_thumb32((raw >> 16) as u16, raw as u16).id,
                id,
                "{raw:08X}"
            );
            assert!(translate(raw).0, "{raw:08X}");
        }
    }

    #[test]
    fn invalid_registers_raise_before_reads() {
        let (result, block) = translate(0xfa81_ff03);
        assert!(!result);
        assert!(!block
            .instructions
            .iter()
            .any(|i| i.opcode == Opcode::A32GetRegister));
        assert!(block
            .instructions
            .iter()
            .any(|i| i.opcode == Opcode::A32ExceptionRaised
                && i.args[1]
                    == Value::ImmU64(Exception::UnpredictableInstruction.as_u32() as u64)));
    }

    #[test]
    fn ge_operation_preserves_register_result_and_flags_order() {
        let (_, block) = translate(0xfa81_f203);
        let ops = block
            .instructions
            .iter()
            .map(|i| i.opcode)
            .collect::<Vec<_>>();
        assert_eq!(
            ops,
            vec![
                Opcode::A32GetRegister,
                Opcode::A32GetRegister,
                Opcode::PackedAddS8,
                Opcode::GetGEFromOp,
                Opcode::A32SetRegister,
                Opcode::A32SetGEFlags
            ]
        );
        assert_eq!(block.instructions[0].args[0], Value::ImmA32Reg(Reg::R3));
        assert_eq!(block.instructions[1].args[0], Value::ImmA32Reg(Reg::R1));
    }

    #[test]
    fn crossed_saturating_halves_keep_upstream_expansion() {
        let (_, block) = translate(0xfaa1_f213);
        assert_eq!(
            block
                .instructions
                .iter()
                .filter(|i| i.opcode == Opcode::SignedSaturation)
                .count(),
            2
        );
        assert_eq!(
            block
                .instructions
                .iter()
                .filter(|i| i.opcode == Opcode::GetOverflowFromOp)
                .count(),
            2
        );
        assert!(block.instructions.iter().any(|i| i.opcode == Opcode::Sub32));
        assert!(block.instructions.iter().any(|i| i.opcode == Opcode::Add32));
        assert_eq!(
            block.instructions.last().map(|i| i.opcode),
            Some(Opcode::A32SetRegister)
        );
    }
}

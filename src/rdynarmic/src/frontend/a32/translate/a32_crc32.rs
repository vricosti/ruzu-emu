//! Port of frontend/A32/translate/impl/a32_crc32.cpp.

use crate::frontend::a32::decoder::DecodedArm;
use crate::frontend::a32::types::Reg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::cond::Cond;

enum CRCType {
    Castagnoli,
    Iso,
}

fn crc32_variant(ir: &mut A32IREmitter, inst: &DecodedArm, kind: CRCType) -> bool {
    let (n, d, m) = (inst.rn(), inst.rd(), inst.rm());
    let size = (inst.raw >> 21) & 3;
    // Upstream rejects these encodings before testing the instruction condition.
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    if size == 3 {
        return super::unpredictable_instruction(ir);
    }
    if inst.cond() != Cond::AL {
        return super::unpredictable_instruction(ir);
    }

    let accumulator = ir.get_register(n);
    let data = ir.get_register(m);
    let result = match (kind, size) {
        (CRCType::Iso, 0) => ir.ir().crc32_iso_8(accumulator, data),
        (CRCType::Iso, 1) => ir.ir().crc32_iso_16(accumulator, data),
        (CRCType::Iso, 2) => ir.ir().crc32_iso_32(accumulator, data),
        (CRCType::Castagnoli, 0) => ir.ir().crc32_castagnoli_8(accumulator, data),
        (CRCType::Castagnoli, 1) => ir.ir().crc32_castagnoli_16(accumulator, data),
        (CRCType::Castagnoli, 2) => ir.ir().crc32_castagnoli_32(accumulator, data),
        _ => unreachable!(),
    };
    ir.set_register(d, result);
    true
}

pub(super) fn arm_crc32(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    crc32_variant(ir, inst, CRCType::Iso)
}

pub(super) fn arm_crc32c(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    crc32_variant(ir, inst, CRCType::Castagnoli)
}

#[cfg(test)]
mod tests {
    use crate::frontend::a32::translate::{translate, TranslationOptions};
    use crate::frontend::a32::types::Exception;
    use crate::ir::{location::A32LocationDescriptor, opcode::Opcode, value::Value};

    #[test]
    fn a32_crc32_widths_and_polynomials() {
        for (polynomial, opcodes) in [
            (
                0,
                [Opcode::CRC32ISO8, Opcode::CRC32ISO16, Opcode::CRC32ISO32],
            ),
            (
                0x200,
                [
                    Opcode::CRC32Castagnoli8,
                    Opcode::CRC32Castagnoli16,
                    Opcode::CRC32Castagnoli32,
                ],
            ),
        ] {
            for (size, opcode) in opcodes.into_iter().enumerate() {
                let word = 0xe100_3042 | polynomial | ((size as u32) << 21);
                let block = translate(
                    A32LocationDescriptor::at(0x1000),
                    &|pc| (pc == 0x1000).then_some(word),
                    TranslationOptions::default(),
                );
                assert!(block.instructions.iter().any(|inst| inst.opcode == opcode));
            }
        }
    }

    #[test]
    fn a32_crc32_invalid_encodings_are_unconditionally_unpredictable() {
        for polynomial in [0, 0x200] {
            let base = 0xe100_3042 | polynomial;
            let mut invalid = vec![
                base | (3 << 21),
                base | (15 << 16),
                (base & !0xf000) | 0xf000,
                (base & !15) | 15,
            ];
            invalid.extend(
                (0..16)
                    .filter(|cond| *cond != 14)
                    .map(|cond| (base & 0x0fff_ffff) | (cond << 28)),
            );
            for word in invalid {
                let block = translate(
                    A32LocationDescriptor::at(0x1000),
                    &|pc| (pc == 0x1000).then_some(word),
                    TranslationOptions::default(),
                );
                assert_eq!(block.cond, None, "word={word:08x}");
                let fault = block
                    .instructions
                    .iter()
                    .find(|inst| inst.opcode == Opcode::A32ExceptionRaised)
                    .unwrap();
                assert_eq!(
                    fault.args[1],
                    Value::ImmU64(Exception::UnpredictableInstruction.as_u32() as u64)
                );
            }
        }
    }
}

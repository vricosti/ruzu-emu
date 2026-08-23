use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Reg;
use crate::interface::a64::config::InstructionCacheOperation;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

fn instruction_cache_instruction(
    visitor: &mut TranslatorVisitor<'_>,
    operation: InstructionCacheOperation,
    value: Value,
) -> bool {
    visitor
        .ir
        .instruction_cache_operation_raised(operation, value);
    let next_pc = visitor.ir.pc() + 4;
    let next_pc = visitor.ir.ir().imm64(next_pc);
    visitor.ir.set_pc(next_pc);
    visitor
        .ir
        .set_term(Terminal::check_halt(Terminal::ReturnToDispatch));
    false
}

impl TranslatorVisitor<'_> {
    pub fn ic_iallu(&mut self, _inst: &DecodedInst) -> bool {
        instruction_cache_instruction(
            self,
            InstructionCacheOperation::InvalidateAllToPoU,
            Value::ImmU64(0),
        )
    }

    pub fn ic_ialluis(&mut self, _inst: &DecodedInst) -> bool {
        instruction_cache_instruction(
            self,
            InstructionCacheOperation::InvalidateAllToPoUInnerSharable,
            Value::ImmU64(0),
        )
    }

    pub fn ic_ivau(&mut self, inst: &DecodedInst) -> bool {
        let value = self.x(64, Reg::from_u32(inst.rd()));
        instruction_cache_instruction(self, InstructionCacheOperation::InvalidateByVaToPoU, value)
    }
}

#[cfg(test)]
mod tests {
    use crate::frontend::a64::translate::{translate, TranslationOptions};
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::terminal::Terminal;
    use crate::ir::value::Value;

    #[test]
    fn instruction_cache_operations_emit_and_end_the_block() {
        let encodings = [
            (0xd508_751f, 1u64, false),
            (0xd508_711f, 2, false),
            (0xd50b_7521, 0, true),
        ];

        for (encoding, expected_operation, has_register_value) in encodings {
            let location = A64LocationDescriptor::new(0x5000, 0, false);
            let block = translate(
                location,
                &|pc| (pc == 0x5000).then_some(encoding),
                TranslationOptions::default(),
            );
            let operation = block
                .instructions
                .iter()
                .find(|inst| inst.opcode == Opcode::A64InstructionCacheOperationRaised)
                .expect("instruction-cache operation must be emitted");

            assert_eq!(operation.args[0], Value::ImmU64(expected_operation));
            assert_eq!(operation.args[1].is_immediate(), !has_register_value);
            assert!(matches!(block.terminal, Terminal::CheckHalt { .. }));
            assert!(block.instructions.iter().any(|inst| {
                inst.opcode == Opcode::A64SetPC && inst.args[0] == Value::ImmU64(0x5004)
            }));
        }
    }
}

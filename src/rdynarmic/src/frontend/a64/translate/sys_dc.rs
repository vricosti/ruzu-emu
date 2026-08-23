use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Reg;
use crate::interface::a64::config::DataCacheOperation;

fn data_cache_instruction(
    visitor: &mut TranslatorVisitor<'_>,
    operation: DataCacheOperation,
    rt: Reg,
) -> bool {
    let value = visitor.x(64, rt);
    visitor.ir.data_cache_operation_raised(operation, value);
    true
}

impl TranslatorVisitor<'_> {
    pub fn dc_ivac(&mut self, inst: &DecodedInst) -> bool {
        data_cache_instruction(
            self,
            DataCacheOperation::InvalidateByVaToPoC,
            Reg::from_u32(inst.rd()),
        )
    }

    pub fn dc_isw(&mut self, inst: &DecodedInst) -> bool {
        data_cache_instruction(
            self,
            DataCacheOperation::InvalidateBySetWay,
            Reg::from_u32(inst.rd()),
        )
    }

    pub fn dc_csw(&mut self, inst: &DecodedInst) -> bool {
        data_cache_instruction(
            self,
            DataCacheOperation::CleanBySetWay,
            Reg::from_u32(inst.rd()),
        )
    }

    pub fn dc_cisw(&mut self, inst: &DecodedInst) -> bool {
        data_cache_instruction(
            self,
            DataCacheOperation::CleanAndInvalidateBySetWay,
            Reg::from_u32(inst.rd()),
        )
    }

    pub fn dc_zva(&mut self, inst: &DecodedInst) -> bool {
        data_cache_instruction(self, DataCacheOperation::ZeroByVa, Reg::from_u32(inst.rd()))
    }

    pub fn dc_cvac(&mut self, inst: &DecodedInst) -> bool {
        data_cache_instruction(
            self,
            DataCacheOperation::CleanByVaToPoC,
            Reg::from_u32(inst.rd()),
        )
    }

    pub fn dc_cvau(&mut self, inst: &DecodedInst) -> bool {
        data_cache_instruction(
            self,
            DataCacheOperation::CleanByVaToPoU,
            Reg::from_u32(inst.rd()),
        )
    }

    pub fn dc_cvap(&mut self, inst: &DecodedInst) -> bool {
        data_cache_instruction(
            self,
            DataCacheOperation::CleanByVaToPoP,
            Reg::from_u32(inst.rd()),
        )
    }

    pub fn dc_civac(&mut self, inst: &DecodedInst) -> bool {
        data_cache_instruction(
            self,
            DataCacheOperation::CleanAndInvalidateByVaToPoC,
            Reg::from_u32(inst.rd()),
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::frontend::a64::translate::{translate, TranslationOptions};
    use crate::frontend::a64::types::Reg;
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::value::Value;

    #[test]
    fn all_data_cache_instructions_emit_upstream_operation_ids() {
        let encodings = [
            (0xd508_7621, 7u64),
            (0xd508_7641, 6),
            (0xd508_7a41, 2),
            (0xd508_7e41, 0),
            (0xd50b_7421, 8),
            (0xd50b_7a21, 3),
            (0xd50b_7b21, 4),
            (0xd50b_7c21, 5),
            (0xd50b_7e21, 1),
        ];

        for (encoding, expected_operation) in encodings {
            let code = [encoding, 0xd400_0001];
            let location = A64LocationDescriptor::new(0x4000, 0, false);
            let block = translate(
                location,
                &|pc| code.get(((pc - 0x4000) / 4) as usize).copied(),
                TranslationOptions::default(),
            );
            let operation = block
                .instructions
                .iter()
                .find(|inst| inst.opcode == Opcode::A64DataCacheOperationRaised)
                .expect("data-cache operation must be emitted");

            assert_eq!(operation.args[0], Value::ImmU64(location.unique_hash()));
            assert_eq!(operation.args[1], Value::ImmU64(expected_operation));
            let Value::Inst(value) = operation.args[2] else {
                panic!("cache-maintenance value must come from X1");
            };
            assert_eq!(block.get(value).opcode, Opcode::A64GetX);
            assert_eq!(block.get(value).args[0], Value::ImmA64Reg(Reg::R1));
        }
    }
}

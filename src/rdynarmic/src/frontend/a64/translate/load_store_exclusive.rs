use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::{AccType, Reg};
use crate::ir::emitter::MemOp;

fn exclusive_shared_decode_and_operation(
    visitor: &mut TranslatorVisitor<'_>,
    pair: bool,
    size: usize,
    load: bool,
    ordered: bool,
    rs: Option<Reg>,
    rt2: Option<Reg>,
    rn: Reg,
    rt: Reg,
) -> bool {
    let acc_type = if ordered {
        AccType::Ordered
    } else {
        AccType::Atomic
    };
    let mem_op = if load { MemOp::Load } else { MemOp::Store };
    let element_size = 8usize << size;
    let register_size = if element_size == 64 { 64 } else { 32 };
    let data_size = if pair { element_size * 2 } else { element_size };
    let bytes = data_size / 8;

    if mem_op == MemOp::Load && pair && rt == rt2.expect("pair loads have Rt2") {
        return visitor.unpredictable_instruction();
    } else if mem_op == MemOp::Store && (rs.expect("stores have Rs") == rt || (pair && rs == rt2)) {
        if !visitor.options.define_unpredictable_behaviour {
            return visitor.unpredictable_instruction();
        }
        // UNPREDICTABLE: execute the Constraint_NONE case.
    } else if mem_op == MemOp::Store && rs.expect("stores have Rs") == rn && rn != Reg::R31 {
        return visitor.unpredictable_instruction();
    }

    let address = visitor.base_address(rn);

    match mem_op {
        MemOp::Store => {
            let data = if pair && element_size == 64 {
                let first = visitor.x(64, rt);
                let second = visitor.x(64, rt2.expect("pair stores have Rt2"));
                visitor.ir.ir().pack_2x64_to_1x128(first, second)
            } else if pair && element_size == 32 {
                let first = visitor.x(32, rt);
                let second = visitor.x(32, rt2.expect("pair stores have Rt2"));
                visitor.ir.ir().pack_2x32_to_1x64(first, second)
            } else {
                visitor.x(element_size, rt)
            };
            let status = visitor.exclusive_mem_write(address, bytes, acc_type, data);
            visitor.set_x(32, rs.expect("stores have Rs"), status);
        }
        MemOp::Load => {
            let data = visitor.exclusive_mem_read(address, bytes, acc_type);
            if pair && element_size == 64 {
                let first = visitor.ir.ir().vector_get_element(64, data, 0);
                let second = visitor.ir.ir().vector_get_element(64, data, 1);
                visitor.set_x(64, rt, first);
                visitor.set_x(64, rt2.expect("pair loads have Rt2"), second);
            } else if pair && element_size == 32 {
                let first = visitor.ir.ir().least_significant_word(data);
                let second = visitor.ir.ir().most_significant_word(data).result;
                visitor.set_x(32, rt, first);
                visitor.set_x(32, rt2.expect("pair loads have Rt2"), second);
            } else {
                let extended = visitor.zero_extend(data, register_size);
                visitor.set_x(register_size, rt, extended);
            }
        }
        MemOp::Prefetch => unreachable!(),
    }

    true
}

fn ordered_shared_decode_and_operation(
    visitor: &mut TranslatorVisitor<'_>,
    size: usize,
    load: bool,
    ordered: bool,
    rn: Reg,
    rt: Reg,
) -> bool {
    let acc_type = if ordered {
        AccType::Ordered
    } else {
        AccType::LimitedOrdered
    };
    let mem_op = if load { MemOp::Load } else { MemOp::Store };
    let element_size = 8usize << size;
    let register_size = if element_size == 64 { 64 } else { 32 };
    let data_size = element_size;
    let bytes = data_size / 8;
    let address = visitor.base_address(rn);

    match mem_op {
        MemOp::Store => {
            let data = visitor.x(data_size, rt);
            visitor.mem_write(address, data, bytes, acc_type);
        }
        MemOp::Load => {
            let data = visitor.mem_read(address, bytes, acc_type);
            let extended = visitor.zero_extend(data, register_size);
            visitor.set_x(register_size, rt, extended);
        }
        MemOp::Prefetch => unreachable!(),
    }

    true
}

impl<'a> TranslatorVisitor<'a> {
    pub fn stxr(&mut self, inst: &DecodedInst) -> bool {
        exclusive_shared_decode_and_operation(
            self,
            false,
            inst.size() as usize,
            false,
            false,
            Some(Reg::from_u32(inst.rs())),
            None,
            Reg::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
        )
    }

    pub fn stlxr(&mut self, inst: &DecodedInst) -> bool {
        exclusive_shared_decode_and_operation(
            self,
            false,
            inst.size() as usize,
            false,
            true,
            Some(Reg::from_u32(inst.rs())),
            None,
            Reg::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
        )
    }

    pub fn stxp(&mut self, inst: &DecodedInst) -> bool {
        exclusive_shared_decode_and_operation(
            self,
            true,
            2 | inst.size() as usize & 1,
            false,
            false,
            Some(Reg::from_u32(inst.rs())),
            Some(Reg::from_u32(inst.rt2())),
            Reg::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
        )
    }

    pub fn stlxp(&mut self, inst: &DecodedInst) -> bool {
        exclusive_shared_decode_and_operation(
            self,
            true,
            2 | inst.size() as usize & 1,
            false,
            true,
            Some(Reg::from_u32(inst.rs())),
            Some(Reg::from_u32(inst.rt2())),
            Reg::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
        )
    }

    pub fn ldxr(&mut self, inst: &DecodedInst) -> bool {
        exclusive_shared_decode_and_operation(
            self,
            false,
            inst.size() as usize,
            true,
            false,
            None,
            None,
            Reg::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
        )
    }

    pub fn ldaxr(&mut self, inst: &DecodedInst) -> bool {
        exclusive_shared_decode_and_operation(
            self,
            false,
            inst.size() as usize,
            true,
            true,
            None,
            None,
            Reg::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
        )
    }

    pub fn ldxp(&mut self, inst: &DecodedInst) -> bool {
        exclusive_shared_decode_and_operation(
            self,
            true,
            2 | inst.size() as usize & 1,
            true,
            false,
            None,
            Some(Reg::from_u32(inst.rt2())),
            Reg::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
        )
    }

    pub fn ldaxp(&mut self, inst: &DecodedInst) -> bool {
        exclusive_shared_decode_and_operation(
            self,
            true,
            2 | inst.size() as usize & 1,
            true,
            true,
            None,
            Some(Reg::from_u32(inst.rt2())),
            Reg::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
        )
    }

    pub fn stllr(&mut self, inst: &DecodedInst) -> bool {
        ordered_shared_decode_and_operation(
            self,
            inst.size() as usize,
            false,
            false,
            Reg::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
        )
    }

    pub fn stlr(&mut self, inst: &DecodedInst) -> bool {
        ordered_shared_decode_and_operation(
            self,
            inst.size() as usize,
            false,
            true,
            Reg::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
        )
    }

    pub fn ldlar(&mut self, inst: &DecodedInst) -> bool {
        ordered_shared_decode_and_operation(
            self,
            inst.size() as usize,
            true,
            false,
            Reg::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
        )
    }

    pub fn ldar(&mut self, inst: &DecodedInst) -> bool {
        ordered_shared_decode_and_operation(
            self,
            inst.size() as usize,
            true,
            true,
            Reg::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a64::decoder::{decode, A64InstructionName};
    use crate::frontend::a64::translate::TranslationOptions;
    use crate::ir::block::Block;
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::value::Value;

    fn translate_one(
        encoding: u32,
        options: TranslationOptions,
    ) -> (A64InstructionName, Block, bool) {
        let decoded = decode(encoding).expect("exclusive instruction must decode");
        let location = A64LocationDescriptor::new(0x1000, 0, false);
        let mut block = Block::new(location.to_location());
        let translated = {
            let mut visitor = TranslatorVisitor::new(&mut block, location, options);
            visitor.dispatch(&decoded)
        };
        (decoded.name, block, translated)
    }

    fn has_exception(block: &Block) -> bool {
        block
            .instructions
            .iter()
            .any(|instruction| instruction.opcode == Opcode::A64ExceptionRaised)
    }

    #[test]
    fn store_data_alias_honours_define_unpredictable_behaviour() {
        // STXR W1, X1, [X2]: Rs aliases Rt.
        let encoding = 0xC801_7C41;
        let (name, block, translated) = translate_one(encoding, TranslationOptions::default());
        assert_eq!(name, A64InstructionName::STXR);
        assert!(!translated);
        assert!(has_exception(&block));

        let options = TranslationOptions {
            define_unpredictable_behaviour: true,
            ..TranslationOptions::default()
        };
        let (_, block, translated) = translate_one(encoding, options);
        assert!(translated);
        assert!(!has_exception(&block));
        assert!(block
            .instructions
            .iter()
            .any(|instruction| instruction.opcode == Opcode::A64ExclusiveWriteMemory64));
    }

    #[test]
    fn store_data_alias_precedes_base_alias_constraint() {
        let location = A64LocationDescriptor::new(0x1000, 0, false);
        let mut block = Block::new(location.to_location());
        let options = TranslationOptions {
            define_unpredictable_behaviour: true,
            ..TranslationOptions::default()
        };
        let translated = {
            let mut visitor = TranslatorVisitor::new(&mut block, location, options);
            exclusive_shared_decode_and_operation(
                &mut visitor,
                false,
                3,
                false,
                false,
                Some(Reg::R1),
                None,
                Reg::R1,
                Reg::R1,
            )
        };

        assert!(translated);
        assert!(!has_exception(&block));
        assert!(block
            .instructions
            .iter()
            .any(|instruction| instruction.opcode == Opcode::A64ExclusiveWriteMemory64));
    }

    #[test]
    fn store_base_alias_remains_unpredictable() {
        // STXR W2, X1, [X2]: Rs aliases Rn but not Rt.
        let options = TranslationOptions {
            define_unpredictable_behaviour: true,
            ..TranslationOptions::default()
        };
        let (_, block, translated) = translate_one(0xC802_7C41, options);
        assert!(!translated);
        assert!(has_exception(&block));
    }

    #[test]
    fn pair_load_rejects_identical_destination_registers() {
        // LDXP X1, X1, [X2].
        let (name, block, translated) = translate_one(0xC87F_0441, TranslationOptions::default());
        assert_eq!(name, A64InstructionName::LDXP);
        assert!(!translated);
        assert!(has_exception(&block));
    }

    #[test]
    fn all_exclusive_visitors_select_upstream_width_and_access_type() {
        let cases = [
            (
                0xC804_7C41,
                A64InstructionName::STXR,
                Opcode::A64ExclusiveWriteMemory64,
                3,
                AccType::Atomic,
            ),
            (
                0xC804_FC41,
                A64InstructionName::STLXR,
                Opcode::A64ExclusiveWriteMemory64,
                3,
                AccType::Ordered,
            ),
            (
                0xC824_0C41,
                A64InstructionName::STXP,
                Opcode::A64ExclusiveWriteMemory128,
                3,
                AccType::Atomic,
            ),
            (
                0xC824_8C41,
                A64InstructionName::STLXP,
                Opcode::A64ExclusiveWriteMemory128,
                3,
                AccType::Ordered,
            ),
            (
                0xC85F_7C41,
                A64InstructionName::LDXR,
                Opcode::A64ExclusiveReadMemory64,
                2,
                AccType::Atomic,
            ),
            (
                0xC85F_FC41,
                A64InstructionName::LDAXR,
                Opcode::A64ExclusiveReadMemory64,
                2,
                AccType::Ordered,
            ),
            (
                0xC87F_0C41,
                A64InstructionName::LDXP,
                Opcode::A64ExclusiveReadMemory128,
                2,
                AccType::Atomic,
            ),
            (
                0xC87F_8C41,
                A64InstructionName::LDAXP,
                Opcode::A64ExclusiveReadMemory128,
                2,
                AccType::Ordered,
            ),
        ];

        for (encoding, expected_name, expected_opcode, acc_index, expected_acc_type) in cases {
            let (name, block, translated) = translate_one(encoding, TranslationOptions::default());
            assert_eq!(name, expected_name);
            assert!(translated);
            let memory = block
                .instructions
                .iter()
                .find(|instruction| instruction.opcode == expected_opcode)
                .expect("exclusive visitor must emit its selected access width");
            assert_eq!(memory.args[acc_index], Value::ImmAccType(expected_acc_type));
        }
    }

    #[test]
    fn ordered_family_selects_limited_and_full_ordering() {
        let cases = [
            (
                0xC89F_7C41,
                A64InstructionName::STLLR,
                Opcode::A64WriteMemory64,
                3,
                AccType::LimitedOrdered,
            ),
            (
                0xC89F_FC41,
                A64InstructionName::STLR,
                Opcode::A64WriteMemory64,
                3,
                AccType::Ordered,
            ),
            (
                0xC8DF_7C41,
                A64InstructionName::LDLAR,
                Opcode::A64ReadMemory64,
                2,
                AccType::LimitedOrdered,
            ),
            (
                0xC8DF_FC41,
                A64InstructionName::LDAR,
                Opcode::A64ReadMemory64,
                2,
                AccType::Ordered,
            ),
        ];

        for (encoding, expected_name, expected_opcode, acc_index, expected_acc_type) in cases {
            let (name, block, translated) = translate_one(encoding, TranslationOptions::default());
            assert_eq!(name, expected_name);
            assert!(translated);
            let memory = block
                .instructions
                .iter()
                .find(|instruction| instruction.opcode == expected_opcode)
                .expect("ordered operation must emit its memory access");
            assert_eq!(memory.args[acc_index], Value::ImmAccType(expected_acc_type));
        }
    }
}

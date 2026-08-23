use crate::interface::a64::config::DataCacheOperation;
use crate::ir::acc_type::AccType;
use crate::ir::block::Block;
use crate::ir::opcode::Opcode;
use crate::ir::value::Value;

fn insert_add(block: &mut Block, cursor: &mut usize, address: Value, offset: u64) -> Value {
    let result = block.insert(
        *cursor,
        Opcode::Add64,
        &[address, Value::ImmU64(offset), Value::ImmU1(false)],
    );
    *cursor += 1;
    Value::Inst(result)
}

fn insert_write(
    block: &mut Block,
    cursor: &mut usize,
    opcode: Opcode,
    location: Value,
    address: Value,
    value: Value,
) {
    block.insert(
        *cursor,
        opcode,
        &[location, address, value, Value::ImmAccType(AccType::Dczva)],
    );
    *cursor += 1;
}

/// Applies the A64 callback configuration to cache-maintenance IR.
///
/// Upstream owner: `ir/opt_passes.cpp::A64CallbackConfigPass`.
pub fn a64_callback_config(block: &mut Block, hook_data_cache_operations: bool, dczid_el0: u32) {
    if hook_data_cache_operations {
        return;
    }

    let mut index = 0;
    while index < block.instructions.len() {
        if block.instructions[index].opcode != Opcode::A64DataCacheOperationRaised {
            index += 1;
            continue;
        }

        let location = block.instructions[index].args[0];
        let operation = block.instructions[index].args[1].get_u64();
        let mut address = block.instructions[index].args[2];
        let mut cursor = index;

        if operation == DataCacheOperation::ZeroByVa as u64 {
            let mut bytes = 4usize << (dczid_el0 & 0b1111);
            let zero_u128 = block.insert(cursor, Opcode::ZeroExtendLongToQuad, &[Value::ImmU64(0)]);
            cursor += 1;

            while bytes >= 16 {
                insert_write(
                    block,
                    &mut cursor,
                    Opcode::A64WriteMemory128,
                    location,
                    address,
                    Value::Inst(zero_u128),
                );
                address = insert_add(block, &mut cursor, address, 16);
                bytes -= 16;
            }

            while bytes >= 8 {
                insert_write(
                    block,
                    &mut cursor,
                    Opcode::A64WriteMemory64,
                    location,
                    address,
                    Value::ImmU64(0),
                );
                address = insert_add(block, &mut cursor, address, 8);
                bytes -= 8;
            }

            while bytes >= 4 {
                insert_write(
                    block,
                    &mut cursor,
                    Opcode::A64WriteMemory32,
                    location,
                    address,
                    Value::ImmU32(0),
                );
                address = insert_add(block, &mut cursor, address, 4);
                bytes -= 4;
            }
        }

        block.instructions[cursor].tombstone();
        index = cursor + 1;
    }

    block.recompute_use_counts();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::location::A64LocationDescriptor;

    fn cache_block(operation: DataCacheOperation) -> Block {
        let location = A64LocationDescriptor::new(0x1234, 0x0140_0000, false).to_location();
        let mut block = Block::new(location);
        let address = block.append(
            Opcode::A64GetX,
            &[Value::ImmA64Reg(crate::frontend::a64::types::Reg::R3)],
        );
        block.append(
            Opcode::A64DataCacheOperationRaised,
            &[
                Value::ImmU64(location.value()),
                Value::ImmU64(operation as u64),
                Value::Inst(address),
            ],
        );
        block
    }

    #[test]
    fn hooked_operations_are_left_for_the_backend() {
        let mut block = cache_block(DataCacheOperation::ZeroByVa);
        let before = format!("{block:?}");

        a64_callback_config(&mut block, true, 4);

        assert_eq!(format!("{block:?}"), before);
    }

    #[test]
    fn unhooked_nonzero_operations_are_invalidated() {
        let mut block = cache_block(DataCacheOperation::CleanByVaToPoC);

        a64_callback_config(&mut block, false, 4);

        assert!(!block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A64DataCacheOperationRaised));
        assert!(!block
            .instructions
            .iter()
            .any(|inst| inst.opcode.is_memory_write()));
    }

    #[test]
    fn dczva_uses_configured_block_size_and_access_type() {
        let mut block = cache_block(DataCacheOperation::ZeroByVa);
        let location = Value::ImmU64(block.location.value());

        a64_callback_config(&mut block, false, 4);

        let writes: Vec<_> = block
            .instructions
            .iter()
            .filter(|inst| inst.opcode == Opcode::A64WriteMemory128)
            .collect();
        assert_eq!(writes.len(), 4);
        assert!(writes.iter().all(|inst| {
            inst.args[0] == location && inst.args[3] == Value::ImmAccType(AccType::Dczva)
        }));
        assert!(!block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A64DataCacheOperationRaised));
    }

    #[test]
    fn smallest_dczva_block_uses_one_word_write() {
        let mut block = cache_block(DataCacheOperation::ZeroByVa);

        a64_callback_config(&mut block, false, 0);

        let writes: Vec<_> = block
            .instructions
            .iter()
            .filter(|inst| inst.opcode.is_memory_write())
            .collect();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].opcode, Opcode::A64WriteMemory32);
        assert_eq!(writes[0].args[2], Value::ImmU32(0));
        assert_eq!(writes[0].args[3], Value::ImmAccType(AccType::Dczva));
    }
}

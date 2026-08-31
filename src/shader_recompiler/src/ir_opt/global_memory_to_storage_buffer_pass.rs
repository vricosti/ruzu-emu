// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `ir_opt/global_memory_to_storage_buffer_pass.cpp`
//!
//! Converts global memory accesses to storage buffer accesses.
//! This pass identifies constant buffer addresses used as SSBO descriptors
//! and rewrites global load/store instructions to use indexed storage
//! buffer operations instead.

use crate::host_translate_info::HostTranslateInfo;
use crate::ir::instruction::Inst;
use crate::ir::opcodes::Opcode;
use crate::ir::program::{Program, ShaderInfo};
use crate::ir::value::{InstRef, Value};
use crate::shader_info::StorageBufferDescriptor;
use std::collections::{BTreeSet, HashSet, VecDeque};

/// Address in constant buffers to the storage buffer descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct StorageBufferAddr {
    index: u32,
    offset: u32,
}

#[derive(Debug, Clone, Copy)]
struct StorageInst {
    storage_buffer: StorageBufferAddr,
    inst: InstRef,
}

#[derive(Debug, Clone)]
struct StorageInfo {
    set: BTreeSet<StorageBufferAddr>,
    to_replace: Vec<StorageInst>,
    writes: BTreeSet<StorageBufferAddr>,
}

#[derive(Debug, Clone, Copy)]
struct Bias {
    index: u32,
    offset_begin: u32,
    offset_end: u32,
    alignment: u32,
}

#[derive(Debug, Clone, Copy)]
struct LowAddrInfo {
    value: Value,
    imm_offset: i32,
}

fn is_global_memory(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::LoadGlobalU8
            | Opcode::LoadGlobalS8
            | Opcode::LoadGlobalU16
            | Opcode::LoadGlobalS16
            | Opcode::LoadGlobal32
            | Opcode::LoadGlobal64
            | Opcode::LoadGlobal128
            | Opcode::WriteGlobalU8
            | Opcode::WriteGlobalS8
            | Opcode::WriteGlobalU16
            | Opcode::WriteGlobalS16
            | Opcode::WriteGlobal32
            | Opcode::WriteGlobal64
            | Opcode::WriteGlobal128
            | Opcode::GlobalAtomicIAdd32
            | Opcode::GlobalAtomicSMin32
            | Opcode::GlobalAtomicUMin32
            | Opcode::GlobalAtomicSMax32
            | Opcode::GlobalAtomicUMax32
            | Opcode::GlobalAtomicInc32
            | Opcode::GlobalAtomicDec32
            | Opcode::GlobalAtomicAnd32
            | Opcode::GlobalAtomicOr32
            | Opcode::GlobalAtomicXor32
            | Opcode::GlobalAtomicExchange32
            | Opcode::GlobalAtomicIAdd64
            | Opcode::GlobalAtomicSMin64
            | Opcode::GlobalAtomicUMin64
            | Opcode::GlobalAtomicSMax64
            | Opcode::GlobalAtomicUMax64
            | Opcode::GlobalAtomicAnd64
            | Opcode::GlobalAtomicOr64
            | Opcode::GlobalAtomicXor64
            | Opcode::GlobalAtomicExchange64
            | Opcode::GlobalAtomicIAdd32x2
            | Opcode::GlobalAtomicSMin32x2
            | Opcode::GlobalAtomicUMin32x2
            | Opcode::GlobalAtomicSMax32x2
            | Opcode::GlobalAtomicUMax32x2
            | Opcode::GlobalAtomicAnd32x2
            | Opcode::GlobalAtomicOr32x2
            | Opcode::GlobalAtomicXor32x2
            | Opcode::GlobalAtomicExchange32x2
            | Opcode::GlobalAtomicAddF32
            | Opcode::GlobalAtomicAddF16x2
            | Opcode::GlobalAtomicAddF32x2
            | Opcode::GlobalAtomicMinF16x2
            | Opcode::GlobalAtomicMinF32x2
            | Opcode::GlobalAtomicMaxF16x2
            | Opcode::GlobalAtomicMaxF32x2
    )
}

fn is_global_memory_write(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::WriteGlobalU8
            | Opcode::WriteGlobalS8
            | Opcode::WriteGlobalU16
            | Opcode::WriteGlobalS16
            | Opcode::WriteGlobal32
            | Opcode::WriteGlobal64
            | Opcode::WriteGlobal128
            | Opcode::GlobalAtomicIAdd32
            | Opcode::GlobalAtomicSMin32
            | Opcode::GlobalAtomicUMin32
            | Opcode::GlobalAtomicSMax32
            | Opcode::GlobalAtomicUMax32
            | Opcode::GlobalAtomicInc32
            | Opcode::GlobalAtomicDec32
            | Opcode::GlobalAtomicAnd32
            | Opcode::GlobalAtomicOr32
            | Opcode::GlobalAtomicXor32
            | Opcode::GlobalAtomicExchange32
            | Opcode::GlobalAtomicIAdd64
            | Opcode::GlobalAtomicSMin64
            | Opcode::GlobalAtomicUMin64
            | Opcode::GlobalAtomicSMax64
            | Opcode::GlobalAtomicUMax64
            | Opcode::GlobalAtomicAnd64
            | Opcode::GlobalAtomicOr64
            | Opcode::GlobalAtomicXor64
            | Opcode::GlobalAtomicExchange64
            | Opcode::GlobalAtomicIAdd32x2
            | Opcode::GlobalAtomicSMin32x2
            | Opcode::GlobalAtomicUMin32x2
            | Opcode::GlobalAtomicSMax32x2
            | Opcode::GlobalAtomicUMax32x2
            | Opcode::GlobalAtomicAnd32x2
            | Opcode::GlobalAtomicOr32x2
            | Opcode::GlobalAtomicXor32x2
            | Opcode::GlobalAtomicExchange32x2
            | Opcode::GlobalAtomicAddF32
            | Opcode::GlobalAtomicAddF16x2
            | Opcode::GlobalAtomicAddF32x2
            | Opcode::GlobalAtomicMinF16x2
            | Opcode::GlobalAtomicMinF32x2
            | Opcode::GlobalAtomicMaxF16x2
            | Opcode::GlobalAtomicMaxF32x2
    )
}

fn global_to_storage(opcode: Opcode) -> Option<Opcode> {
    Some(match opcode {
        Opcode::LoadGlobalS8 => Opcode::LoadStorageS8,
        Opcode::LoadGlobalU8 => Opcode::LoadStorageU8,
        Opcode::LoadGlobalS16 => Opcode::LoadStorageS16,
        Opcode::LoadGlobalU16 => Opcode::LoadStorageU16,
        Opcode::LoadGlobal32 => Opcode::LoadStorage32,
        Opcode::LoadGlobal64 => Opcode::LoadStorage64,
        Opcode::LoadGlobal128 => Opcode::LoadStorage128,
        Opcode::WriteGlobalS8 => Opcode::WriteStorageS8,
        Opcode::WriteGlobalU8 => Opcode::WriteStorageU8,
        Opcode::WriteGlobalS16 => Opcode::WriteStorageS16,
        Opcode::WriteGlobalU16 => Opcode::WriteStorageU16,
        Opcode::WriteGlobal32 => Opcode::WriteStorage32,
        Opcode::WriteGlobal64 => Opcode::WriteStorage64,
        Opcode::WriteGlobal128 => Opcode::WriteStorage128,
        Opcode::GlobalAtomicIAdd32 => Opcode::StorageAtomicIAdd32,
        Opcode::GlobalAtomicSMin32 => Opcode::StorageAtomicSMin32,
        Opcode::GlobalAtomicUMin32 => Opcode::StorageAtomicUMin32,
        Opcode::GlobalAtomicSMax32 => Opcode::StorageAtomicSMax32,
        Opcode::GlobalAtomicUMax32 => Opcode::StorageAtomicUMax32,
        Opcode::GlobalAtomicInc32 => Opcode::StorageAtomicInc32,
        Opcode::GlobalAtomicDec32 => Opcode::StorageAtomicDec32,
        Opcode::GlobalAtomicAnd32 => Opcode::StorageAtomicAnd32,
        Opcode::GlobalAtomicOr32 => Opcode::StorageAtomicOr32,
        Opcode::GlobalAtomicXor32 => Opcode::StorageAtomicXor32,
        Opcode::GlobalAtomicExchange32 => Opcode::StorageAtomicExchange32,
        Opcode::GlobalAtomicIAdd64 => Opcode::StorageAtomicIAdd64,
        Opcode::GlobalAtomicSMin64 => Opcode::StorageAtomicSMin64,
        Opcode::GlobalAtomicUMin64 => Opcode::StorageAtomicUMin64,
        Opcode::GlobalAtomicSMax64 => Opcode::StorageAtomicSMax64,
        Opcode::GlobalAtomicUMax64 => Opcode::StorageAtomicUMax64,
        Opcode::GlobalAtomicAnd64 => Opcode::StorageAtomicAnd64,
        Opcode::GlobalAtomicOr64 => Opcode::StorageAtomicOr64,
        Opcode::GlobalAtomicXor64 => Opcode::StorageAtomicXor64,
        Opcode::GlobalAtomicExchange64 => Opcode::StorageAtomicExchange64,
        Opcode::GlobalAtomicIAdd32x2 => Opcode::StorageAtomicIAdd32x2,
        Opcode::GlobalAtomicSMin32x2 => Opcode::StorageAtomicSMin32x2,
        Opcode::GlobalAtomicUMin32x2 => Opcode::StorageAtomicUMin32x2,
        Opcode::GlobalAtomicSMax32x2 => Opcode::StorageAtomicSMax32x2,
        Opcode::GlobalAtomicUMax32x2 => Opcode::StorageAtomicUMax32x2,
        Opcode::GlobalAtomicAnd32x2 => Opcode::StorageAtomicAnd32x2,
        Opcode::GlobalAtomicOr32x2 => Opcode::StorageAtomicOr32x2,
        Opcode::GlobalAtomicXor32x2 => Opcode::StorageAtomicXor32x2,
        Opcode::GlobalAtomicExchange32x2 => Opcode::StorageAtomicExchange32x2,
        Opcode::GlobalAtomicAddF32 => Opcode::StorageAtomicAddF32,
        Opcode::GlobalAtomicAddF16x2 => Opcode::StorageAtomicAddF16x2,
        Opcode::GlobalAtomicAddF32x2 => Opcode::StorageAtomicAddF32x2,
        Opcode::GlobalAtomicMinF16x2 => Opcode::StorageAtomicMinF16x2,
        Opcode::GlobalAtomicMinF32x2 => Opcode::StorageAtomicMinF32x2,
        Opcode::GlobalAtomicMaxF16x2 => Opcode::StorageAtomicMaxF16x2,
        Opcode::GlobalAtomicMaxF32x2 => Opcode::StorageAtomicMaxF32x2,
        _ => return None,
    })
}

fn inst_recursive_from_value(program: &Program, value: Value) -> Option<(InstRef, &Inst)> {
    let Value::Inst(mut inst_ref) = value else {
        return None;
    };
    loop {
        let inst = program.block(inst_ref.block).inst(inst_ref.inst);
        if inst.opcode != Opcode::Identity {
            return Some((inst_ref, inst));
        }
        let Some(Value::Inst(next)) = inst.args.first().copied() else {
            return Some((inst_ref, inst));
        };
        inst_ref = next;
    }
}

fn track_low_address(program: &Program, global_inst: &Inst) -> Option<LowAddrInfo> {
    let addr = *global_inst.args.first()?;
    if addr.is_immediate() {
        return None;
    }

    let (_, mut addr_inst) = inst_recursive_from_value(program, addr)?;
    let mut imm_offset = 0;
    if addr_inst.opcode == Opcode::IAdd64 {
        let offset = addr_inst.args.get(1)?;
        if !offset.is_immediate() {
            return None;
        }
        imm_offset = offset.imm_u64() as i64 as i32;
        let base = *addr_inst.args.first()?;
        if base.is_immediate() {
            return None;
        }
        addr_inst = inst_recursive_from_value(program, base)?.1;
    }

    if addr_inst.opcode == Opcode::PackUint2x32 {
        let vector = *addr_inst.args.first()?;
        if vector.is_immediate() {
            return None;
        }
        addr_inst = inst_recursive_from_value(program, vector)?.1;
    }

    if addr_inst.opcode != Opcode::CompositeConstructU32x2 {
        return None;
    }

    Some(LowAddrInfo {
        value: *addr_inst.args.first()?,
        imm_offset,
    })
}

fn meets_bias(storage_buffer: StorageBufferAddr, bias: Bias) -> bool {
    storage_buffer.index == bias.index
        && storage_buffer.offset >= bias.offset_begin
        && storage_buffer.offset < bias.offset_end
}

fn track(program: &Program, value: Value, bias: Option<Bias>) -> Option<StorageBufferAddr> {
    let mut queue = VecDeque::from([value]);
    let mut visited = HashSet::new();
    while let Some(value) = queue.pop_front() {
        let Value::Inst(inst_ref) = value else {
            continue;
        };
        if !visited.insert(inst_ref) {
            continue;
        }
        let inst = program.block(inst_ref.block).inst(inst_ref.inst);
        if matches!(inst.opcode, Opcode::GetCbufU32 | Opcode::GetCbufU32x2) {
            let Some(index) = inst.args.first() else {
                continue;
            };
            let Some(offset) = inst.args.get(1) else {
                continue;
            };
            if !index.is_immediate() || !offset.is_immediate() {
                continue;
            }
            let storage_buffer = StorageBufferAddr {
                index: index.imm_u32(),
                offset: offset.imm_u32(),
            };
            let alignment = bias.map_or(8, |bias| bias.alignment);
            if storage_buffer.offset % alignment != 0 {
                continue;
            }
            if bias.is_some_and(|bias| !meets_bias(storage_buffer, bias)) {
                continue;
            }
            return Some(storage_buffer);
        }
        queue.extend(inst.args.iter().copied());
        queue.extend(inst.phi_args.iter().map(|(_, value)| *value));
    }
    None
}

fn collect_storage_buffer(program: &Program, inst_ref: InstRef, info: &mut StorageInfo) {
    const NVN_BIAS: Bias = Bias {
        index: 0,
        offset_begin: 0x100,
        offset_end: 0x700,
        alignment: 16,
    };
    let inst = program.block(inst_ref.block).inst(inst_ref.inst);
    let Some(low_addr) = track_low_address(program, inst) else {
        return;
    };
    let storage_buffer = track(program, low_addr.value, Some(NVN_BIAS))
        .or_else(|| track(program, low_addr.value, None));
    let Some(storage_buffer) = storage_buffer else {
        return;
    };
    if is_global_memory_write(inst.opcode) {
        info.writes.insert(storage_buffer);
    }
    info.set.insert(storage_buffer);
    info.to_replace.push(StorageInst {
        storage_buffer,
        inst: inst_ref,
    });
}

fn replace_uses_with(program: &mut Program, old: InstRef, replacement: Value) {
    let old_value = Value::Inst(old);
    for block in &mut program.blocks {
        for inst in block.iter_mut() {
            for arg in &mut inst.args {
                if *arg == old_value {
                    *arg = replacement;
                }
            }
            for (_, value) in &mut inst.phi_args {
                if *value == old_value {
                    *value = replacement;
                }
            }
        }
    }
    for node in &mut program.syntax_list {
        match node {
            crate::ir::program::SyntaxNode::If { cond, .. }
            | crate::ir::program::SyntaxNode::Repeat { cond, .. }
            | crate::ir::program::SyntaxNode::Break { cond, .. } => {
                if *cond == old_value {
                    *cond = replacement;
                }
            }
            _ => {}
        }
    }
}

fn insert_before(
    program: &mut Program,
    before: InstRef,
    opcode: Opcode,
    args: Vec<Value>,
) -> Value {
    let inst_idx = program
        .block_mut(before.block)
        .insert_inst_before(before.inst, Inst::new(opcode, args));
    Value::Inst(InstRef {
        block: before.block,
        inst: inst_idx,
    })
}

fn storage_offset(
    program: &mut Program,
    inst_ref: InstRef,
    storage_buffer: StorageBufferAddr,
    alignment: u32,
) -> Option<Value> {
    let inst = program.block(inst_ref.block).inst(inst_ref.inst).clone();
    let low_addr = track_low_address(program, &inst)?;
    let mut offset = low_addr.value;
    if low_addr.imm_offset != 0 {
        offset = insert_before(
            program,
            inst_ref,
            Opcode::IAdd32,
            vec![offset, Value::ImmU32(low_addr.imm_offset as u32)],
        );
    }
    let low_cbuf = insert_before(
        program,
        inst_ref,
        Opcode::GetCbufU32,
        vec![
            Value::ImmU32(storage_buffer.index),
            Value::ImmU32(storage_buffer.offset),
        ],
    );
    let alignment = alignment.max(1);
    let aligned_low_cbuf = insert_before(
        program,
        inst_ref,
        Opcode::BitwiseAnd32,
        vec![low_cbuf, Value::ImmU32(!(alignment - 1))],
    );
    Some(insert_before(
        program,
        inst_ref,
        Opcode::ISub32,
        vec![offset, aligned_low_cbuf],
    ))
}

fn storage_opcode(opcode: Opcode) -> Opcode {
    global_to_storage(opcode).unwrap_or_else(|| {
        std::panic::panic_any(crate::exception::InvalidArgument::new(format!(
            "Invalid global memory opcode {opcode:?}"
        )))
    })
}

fn replace_load(program: &mut Program, inst: InstRef, storage_index: u32, offset: Value) {
    let new_opcode = storage_opcode(program.block(inst.block).inst(inst.inst).opcode);
    let value = insert_before(
        program,
        inst,
        new_opcode,
        vec![Value::ImmU32(storage_index), offset],
    );
    replace_uses_with(program, inst, value);
    program
        .block_mut(inst.block)
        .inst_mut(inst.inst)
        .invalidate();
}

fn replace_write(program: &mut Program, inst: InstRef, storage_index: u32, offset: Value) {
    let source = program.block(inst.block).inst(inst.inst).args[1];
    let new_opcode = storage_opcode(program.block(inst.block).inst(inst.inst).opcode);
    insert_before(
        program,
        inst,
        new_opcode,
        vec![Value::ImmU32(storage_index), offset, source],
    );
    program
        .block_mut(inst.block)
        .inst_mut(inst.inst)
        .invalidate();
}

fn replace_atomic(program: &mut Program, inst: InstRef, storage_index: u32, offset: Value) {
    let source = program.block(inst.block).inst(inst.inst).args[1];
    let new_opcode = storage_opcode(program.block(inst.block).inst(inst.inst).opcode);
    let value = insert_before(
        program,
        inst,
        new_opcode,
        vec![Value::ImmU32(storage_index), offset, source],
    );
    replace_uses_with(program, inst, value);
    program
        .block_mut(inst.block)
        .inst_mut(inst.inst)
        .invalidate();
}

fn replace(program: &mut Program, storage_inst: StorageInst, storage_index: u32, offset: Value) {
    let opcode = program
        .block(storage_inst.inst.block)
        .inst(storage_inst.inst.inst)
        .opcode;
    match opcode {
        Opcode::LoadGlobalS8
        | Opcode::LoadGlobalU8
        | Opcode::LoadGlobalS16
        | Opcode::LoadGlobalU16
        | Opcode::LoadGlobal32
        | Opcode::LoadGlobal64
        | Opcode::LoadGlobal128 => replace_load(program, storage_inst.inst, storage_index, offset),
        Opcode::WriteGlobalS8
        | Opcode::WriteGlobalU8
        | Opcode::WriteGlobalS16
        | Opcode::WriteGlobalU16
        | Opcode::WriteGlobal32
        | Opcode::WriteGlobal64
        | Opcode::WriteGlobal128 => {
            replace_write(program, storage_inst.inst, storage_index, offset)
        }
        Opcode::GlobalAtomicIAdd32
        | Opcode::GlobalAtomicSMin32
        | Opcode::GlobalAtomicUMin32
        | Opcode::GlobalAtomicSMax32
        | Opcode::GlobalAtomicUMax32
        | Opcode::GlobalAtomicInc32
        | Opcode::GlobalAtomicDec32
        | Opcode::GlobalAtomicAnd32
        | Opcode::GlobalAtomicOr32
        | Opcode::GlobalAtomicXor32
        | Opcode::GlobalAtomicExchange32
        | Opcode::GlobalAtomicIAdd64
        | Opcode::GlobalAtomicSMin64
        | Opcode::GlobalAtomicUMin64
        | Opcode::GlobalAtomicSMax64
        | Opcode::GlobalAtomicUMax64
        | Opcode::GlobalAtomicAnd64
        | Opcode::GlobalAtomicOr64
        | Opcode::GlobalAtomicXor64
        | Opcode::GlobalAtomicExchange64
        | Opcode::GlobalAtomicIAdd32x2
        | Opcode::GlobalAtomicSMin32x2
        | Opcode::GlobalAtomicUMin32x2
        | Opcode::GlobalAtomicSMax32x2
        | Opcode::GlobalAtomicUMax32x2
        | Opcode::GlobalAtomicAnd32x2
        | Opcode::GlobalAtomicOr32x2
        | Opcode::GlobalAtomicXor32x2
        | Opcode::GlobalAtomicExchange32x2
        | Opcode::GlobalAtomicAddF32
        | Opcode::GlobalAtomicAddF16x2
        | Opcode::GlobalAtomicAddF32x2
        | Opcode::GlobalAtomicMinF16x2
        | Opcode::GlobalAtomicMinF32x2
        | Opcode::GlobalAtomicMaxF16x2
        | Opcode::GlobalAtomicMaxF32x2 => {
            replace_atomic(program, storage_inst.inst, storage_index, offset)
        }
        _ => {
            std::panic::panic_any(crate::exception::InvalidArgument::new(format!(
                "Invalid global memory opcode {opcode:?}"
            )));
        }
    }
}

/// Convert global memory instructions to storage buffer instructions.
pub fn global_memory_to_storage_buffer_pass(program: &mut Program, host_info: &HostTranslateInfo) {
    let mut info = StorageInfo {
        set: BTreeSet::new(),
        to_replace: Vec::new(),
        writes: BTreeSet::new(),
    };
    for block_idx in program.post_order_blocks.clone() {
        let inst_indices: Vec<u32> = program
            .block(block_idx)
            .indexed_iter()
            .filter_map(|(inst_idx, inst)| is_global_memory(inst.opcode).then_some(inst_idx))
            .collect();
        for inst_idx in inst_indices {
            collect_storage_buffer(
                program,
                InstRef {
                    block: block_idx,
                    inst: inst_idx,
                },
                &mut info,
            );
        }
    }

    program.info.storage_buffers_descriptors = info
        .set
        .iter()
        .map(|storage_buffer| StorageBufferDescriptor {
            cbuf_index: storage_buffer.index,
            cbuf_offset: storage_buffer.offset,
            count: 1,
            is_written: info.writes.contains(storage_buffer),
        })
        .collect();

    for storage_inst in info.to_replace.clone() {
        let Some(storage_index) = info
            .set
            .iter()
            .position(|storage_buffer| *storage_buffer == storage_inst.storage_buffer)
        else {
            continue;
        };
        let Some(offset) = storage_offset(
            program,
            storage_inst.inst,
            storage_inst.storage_buffer,
            host_info.min_ssbo_alignment as u32,
        ) else {
            continue;
        };
        replace(program, storage_inst, storage_index as u32, offset);
    }
}

/// Join storage buffer descriptors from `source` into `base`.
///
/// Upstream: `JoinStorageInfo` in `global_memory_to_storage_buffer_pass.cpp`.
pub fn join_storage_info(base: &mut ShaderInfo, source: &mut ShaderInfo) {
    let descriptors = &mut base.storage_buffers_descriptors;
    for desc in &source.storage_buffers_descriptors {
        if let Some(existing) = descriptors.iter_mut().find(|existing| {
            desc.cbuf_index == existing.cbuf_index
                && desc.cbuf_offset == existing.cbuf_offset
                && desc.count == existing.count
        }) {
            existing.is_written |= desc.is_written;
            continue;
        }
        descriptors.push(desc.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::basic_block::Block;
    use crate::ir::types::ShaderStage;

    #[test]
    fn global_load_with_u32_low_address_rewrites_to_storage_load() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        program.post_order_blocks = vec![0];
        let cbuf = program.block_mut(0).append_inst(Inst::new(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(0), Value::ImmU32(0x100)],
        ));
        let addr = program.block_mut(0).append_inst(Inst::new(
            Opcode::IAdd32,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: cbuf,
                }),
                Value::ImmU32(0x20),
            ],
        ));
        let address_vector = program.block_mut(0).append_inst(Inst::new(
            Opcode::CompositeConstructU32x2,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: addr,
                }),
                Value::ImmU32(0),
            ],
        ));
        let packed_address = program.block_mut(0).append_inst(Inst::new(
            Opcode::PackUint2x32,
            vec![Value::Inst(InstRef {
                block: 0,
                inst: address_vector,
            })],
        ));
        let load = program.block_mut(0).append_inst(Inst::new(
            Opcode::LoadGlobal32,
            vec![Value::Inst(InstRef {
                block: 0,
                inst: packed_address,
            })],
        ));
        program.block_mut(0).append_inst(Inst::new(
            Opcode::SetAttribute,
            vec![
                Value::Attribute(crate::ir::value::Attribute::generic(0, 0)),
                Value::Inst(InstRef {
                    block: 0,
                    inst: load,
                }),
                Value::ImmU32(0),
            ],
        ));

        global_memory_to_storage_buffer_pass(
            &mut program,
            &HostTranslateInfo {
                min_ssbo_alignment: 0x100,
                ..Default::default()
            },
        );

        assert_eq!(program.info.storage_buffers_descriptors.len(), 1);
        assert_eq!(program.info.storage_buffers_descriptors[0].cbuf_index, 0);
        assert_eq!(
            program.info.storage_buffers_descriptors[0].cbuf_offset,
            0x100
        );
        assert!(program
            .block(0)
            .iter()
            .any(|inst| inst.opcode == Opcode::LoadStorage32));
        assert!(!program
            .block(0)
            .iter()
            .any(|inst| inst.opcode == Opcode::LoadGlobal32));
        assert!(program.block(0).iter().any(|inst| {
            inst.opcode == Opcode::BitwiseAnd32
                && inst.args.get(1) == Some(&Value::ImmU32(0xffff_ff00))
        }));
    }

    #[test]
    fn global_load_with_noncanonical_address_keeps_global_fallback() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        program.post_order_blocks = vec![0];
        let cbuf = program.block_mut(0).append_inst(Inst::new(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(0), Value::ImmU32(0x110)],
        ));
        let addr = program.block_mut(0).append_inst(Inst::new(
            Opcode::IAdd32,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: cbuf,
                }),
                Value::ImmU32(0x20),
            ],
        ));
        program.block_mut(0).append_inst(Inst::new(
            Opcode::LoadGlobal32,
            vec![Value::Inst(InstRef {
                block: 0,
                inst: addr,
            })],
        ));

        global_memory_to_storage_buffer_pass(
            &mut program,
            &HostTranslateInfo {
                min_ssbo_alignment: 0x100,
                ..Default::default()
            },
        );

        assert!(program.info.storage_buffers_descriptors.is_empty());
        assert!(program
            .block(0)
            .iter()
            .any(|inst| inst.opcode == Opcode::LoadGlobal32));
        assert!(!program
            .block(0)
            .iter()
            .any(|inst| inst.opcode == Opcode::LoadStorage32));
    }

    #[test]
    fn global_atomic_rewrite_preserves_the_return_value() {
        let mut program = Program::new(ShaderStage::Compute);
        program.blocks.push(Block::new());
        program.post_order_blocks = vec![0];
        let cbuf = program.block_mut(0).append_inst(Inst::new(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(0), Value::ImmU32(0x110)],
        ));
        let address = program.block_mut(0).append_inst(Inst::new(
            Opcode::IAdd32,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: cbuf,
                }),
                Value::ImmU32(0x20),
            ],
        ));
        let address_vector = program.block_mut(0).append_inst(Inst::new(
            Opcode::CompositeConstructU32x2,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: address,
                }),
                Value::ImmU32(0),
            ],
        ));
        let packed_address = program.block_mut(0).append_inst(Inst::new(
            Opcode::PackUint2x32,
            vec![Value::Inst(InstRef {
                block: 0,
                inst: address_vector,
            })],
        ));
        let atomic = program.block_mut(0).append_inst(Inst::new(
            Opcode::GlobalAtomicIAdd32,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: packed_address,
                }),
                Value::ImmU32(7),
            ],
        ));
        program.block_mut(0).append_inst(Inst::new(
            Opcode::IAdd32,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: atomic,
                }),
                Value::ImmU32(1),
            ],
        ));

        global_memory_to_storage_buffer_pass(
            &mut program,
            &HostTranslateInfo {
                min_ssbo_alignment: 0x100,
                ..Default::default()
            },
        );

        let (storage_index, _) = program
            .block(0)
            .indexed_iter()
            .find(|(_, inst)| inst.opcode == Opcode::StorageAtomicIAdd32)
            .expect("global atomic was not rewritten");
        let storage_value = Value::Inst(InstRef {
            block: 0,
            inst: storage_index,
        });
        assert!(program.block(0).iter().any(|inst| {
            inst.opcode == Opcode::IAdd32
                && inst.args.first() == Some(&storage_value)
                && inst.args.get(1) == Some(&Value::ImmU32(1))
        }));
        assert!(!program
            .block(0)
            .iter()
            .any(|inst| inst.opcode == Opcode::GlobalAtomicIAdd32));
        assert!(program.info.storage_buffers_descriptors[0].is_written);
    }
}

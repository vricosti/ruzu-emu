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
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use crate::storage_write_extent::{StorageWriteExtent, merge_storage_write_extent};

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
            | Opcode::GlobalAtomicCompareExchange32
            | Opcode::GlobalAtomicExchange32
            | Opcode::GlobalAtomicIAdd64
            | Opcode::GlobalAtomicSMin64
            | Opcode::GlobalAtomicUMin64
            | Opcode::GlobalAtomicSMax64
            | Opcode::GlobalAtomicUMax64
            | Opcode::GlobalAtomicAnd64
            | Opcode::GlobalAtomicOr64
            | Opcode::GlobalAtomicXor64
            | Opcode::GlobalAtomicCompareExchange64
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
            | Opcode::GlobalAtomicCompareExchange32
            | Opcode::GlobalAtomicExchange32
            | Opcode::GlobalAtomicIAdd64
            | Opcode::GlobalAtomicSMin64
            | Opcode::GlobalAtomicUMin64
            | Opcode::GlobalAtomicSMax64
            | Opcode::GlobalAtomicUMax64
            | Opcode::GlobalAtomicAnd64
            | Opcode::GlobalAtomicOr64
            | Opcode::GlobalAtomicXor64
            | Opcode::GlobalAtomicCompareExchange64
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
        Opcode::GlobalAtomicCompareExchange32 => Opcode::StorageAtomicCompareExchange32,
        Opcode::GlobalAtomicExchange32 => Opcode::StorageAtomicExchange32,
        Opcode::GlobalAtomicIAdd64 => Opcode::StorageAtomicIAdd64,
        Opcode::GlobalAtomicSMin64 => Opcode::StorageAtomicSMin64,
        Opcode::GlobalAtomicUMin64 => Opcode::StorageAtomicUMin64,
        Opcode::GlobalAtomicSMax64 => Opcode::StorageAtomicSMax64,
        Opcode::GlobalAtomicUMax64 => Opcode::StorageAtomicUMax64,
        Opcode::GlobalAtomicAnd64 => Opcode::StorageAtomicAnd64,
        Opcode::GlobalAtomicOr64 => Opcode::StorageAtomicOr64,
        Opcode::GlobalAtomicXor64 => Opcode::StorageAtomicXor64,
        Opcode::GlobalAtomicCompareExchange64 => Opcode::StorageAtomicCompareExchange64,
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

/// Lichii's CbufWord: identify the word, not merely the vector containing it.
fn cbuf_word(program: &Program, inst: &Inst) -> Option<StorageBufferAddr> {
    if inst.opcode == Opcode::CompositeExtractU32x2 {
        let (_, vector) = inst_recursive_from_value(program, inst.args[0])?;
        let Value::ImmU32(element) = inst.args[1] else { return None };
        if vector.opcode != Opcode::GetCbufU32x2 { return None; }
        let (Value::ImmU32(index), Value::ImmU32(offset)) = (vector.args[0], vector.args[1]) else { return None };
        return Some(StorageBufferAddr { index, offset: offset.wrapping_add(element.wrapping_mul(4)) });
    }
    if inst.opcode != Opcode::GetCbufU32 { return None; }
    let (Value::ImmU32(index), Value::ImmU32(offset)) = (inst.args[0], inst.args[1]) else { return None };
    Some(StorageBufferAddr { index, offset })
}

fn store_size(opcode: Opcode) -> u32 {
    use Opcode::*;
    match opcode {
        WriteGlobalS8 | WriteGlobalU8 => 1,
        WriteGlobalS16 | WriteGlobalU16 => 2,
        WriteGlobal128 => 16,
        // Include CAS64: it writes eight bytes too (omitted by the reference table).
        WriteGlobal64 | GlobalAtomicIAdd64 | GlobalAtomicSMin64 | GlobalAtomicUMin64 |
        GlobalAtomicSMax64 | GlobalAtomicUMax64 | GlobalAtomicAnd64 | GlobalAtomicOr64 |
        GlobalAtomicXor64 | GlobalAtomicCompareExchange64 | GlobalAtomicExchange64 |
        GlobalAtomicIAdd32x2 | GlobalAtomicSMin32x2 | GlobalAtomicUMin32x2 |
        GlobalAtomicSMax32x2 | GlobalAtomicUMax32x2 | GlobalAtomicAnd32x2 |
        GlobalAtomicOr32x2 | GlobalAtomicXor32x2 | GlobalAtomicExchange32x2 |
        GlobalAtomicAddF32x2 | GlobalAtomicMinF32x2 | GlobalAtomicMaxF32x2 => 8,
        _ => 4,
    }
}

/// Affine unsigned bound in local xyz, group xyz and one pointer word.
/// Exact expressions retain modular arithmetic; inexact bounds cannot be subtracted.
#[derive(Clone, Copy, Debug)]
struct IdBound {
    c: i64,
    coef: [u64; 6],
    base: u32,
    exact: bool,
}

impl Default for IdBound {
    fn default() -> Self { Self { c: 0, coef: [0; 6], base: 0, exact: true } }
}

impl IdBound {
    fn is_constant(self) -> bool { self.base == 0 && self.coef == [0; 6] }
    fn has_group_terms(self) -> bool { self.coef[3..].iter().any(|&v| v != 0) }
}

/// Port of Lichii's StoreExtentAnalysis, owned by this lowering pass.
struct StoreExtentAnalysis<'a> {
    program: &'a Program,
    buffer: StorageBufferAddr,
    workgroup: [u32; 3],
    memo: HashMap<InstRef, Option<IdBound>>,
    active: HashSet<InstRef>,
    guard: Option<IdBound>,
    guard_failed: bool,
}

impl<'a> StoreExtentAnalysis<'a> {
    const CONST_LIMIT: i64 = 1 << 40;
    const COEF_LIMIT: u64 = 1 << 32;
    const MAX_DEPTH: u32 = 256;

    fn new(program: &'a Program, buffer: StorageBufferAddr) -> Self {
        Self { program, buffer, workgroup: program.workgroup_size, memo: HashMap::new(),
            active: HashSet::new(), guard: None, guard_failed: false }
    }

    fn add_store(&mut self, inst: &Inst, extent: &mut StorageWriteExtent) {
        match self.store_extent(inst) {
            Some(store) => merge_storage_write_extent(extent, &store),
            None => *extent = StorageWriteExtent::default(),
        }
    }

    fn store_extent(&mut self, inst: &Inst) -> Option<StorageWriteExtent> {
        let addr = track_low_address(self.program, inst)?;
        let low = self.evaluate(addr.value, 0)?;
        if low.base != 1 || self.guard_failed { return None; }
        let start = low.c + i64::from(addr.imm_offset);
        let end = start + i64::from(store_size(inst.opcode));
        if start < 0 || end > i64::from(u32::MAX) { return None; }
        let mut result = StorageWriteExtent { bounded: true, const_end: end as u32, ..Default::default() };
        for i in 0..3 {
            if low.coef[i] >= Self::COEF_LIMIT || low.coef[i + 3] >= Self::COEF_LIMIT { return None; }
            result.local_stride[i] = low.coef[i] as u32;
            result.group_stride[i] = low.coef[i + 3] as u32;
        }
        if let Some(guard) = self.guard {
            if guard.c > 0xffff { return None; }
            result.guarded = true;
            result.guard_const = guard.c.max(0) as u32;
            for i in 0..3 {
                if guard.coef[i] > 0xffff || guard.coef[i + 3] > 0xffff { return None; }
                result.guard_local_stride[i] = guard.coef[i] as u32;
                result.guard_group_stride[i] = guard.coef[i + 3] as u32;
            }
        }
        Some(result)
    }

    fn checked(value: IdBound) -> Option<IdBound> {
        (value.c.abs() <= Self::CONST_LIMIT && value.base <= 1 &&
            value.coef.iter().all(|&v| v < Self::COEF_LIMIT)).then_some(value)
    }
    fn constant(c: i64) -> IdBound { IdBound { c, ..Default::default() } }
    fn inexact(value: IdBound) -> IdBound { IdBound { exact: false, ..value } }
    fn sum(lhs: IdBound, rhs: IdBound) -> Option<IdBound> {
        if (!lhs.exact && rhs.c < 0) || (!rhs.exact && lhs.c < 0) { return None; }
        Self::checked(IdBound { c: lhs.c + rhs.c, base: lhs.base + rhs.base,
            exact: lhs.exact && rhs.exact, coef: std::array::from_fn(|i| lhs.coef[i] + rhs.coef[i]) })
    }
    fn max(lhs: IdBound, rhs: IdBound) -> Option<IdBound> {
        if lhs.base != rhs.base || lhs.c < 0 || rhs.c < 0 { return None; }
        Some(IdBound { c: lhs.c.max(rhs.c), base: lhs.base, exact: false,
            coef: std::array::from_fn(|i| lhs.coef[i].max(rhs.coef[i])) })
    }
    fn scale(value: IdBound, factor: u64, exact_factor: bool) -> Option<IdBound> {
        if value.base != 0 || factor >= Self::COEF_LIMIT || (!exact_factor && value.c < 0) { return None; }
        let magnitude = value.c.unsigned_abs();
        if magnitude != 0 && factor > Self::CONST_LIMIT as u64 / magnitude { return None; }
        if value.coef.iter().any(|&v| v != 0 && factor > (Self::COEF_LIMIT - 1) / v) { return None; }
        Self::checked(IdBound { c: value.c * factor as i64, exact: value.exact && exact_factor,
            coef: value.coef.map(|v| v * factor), ..Default::default() })
    }
    fn shift_down(value: IdBound, shift: u32) -> Option<IdBound> {
        if value.base != 0 || value.c < 0 || shift >= 32 { return None; }
        let up = |x: u64| (x + (1u64 << shift) - 1) >> shift;
        Some(IdBound { c: up(value.c as u64) as i64, coef: value.coef.map(up), exact: false, ..Default::default() })
    }
    fn static_max(&self, value: IdBound) -> Option<u64> {
        if value.base != 0 || value.has_group_terms() || value.c < 0 { return None; }
        let mut total = value.c as u64;
        for i in 0..3 {
            if self.workgroup[i] == 0 { return None; }
            // Reject overflowing malformed workgroups rather than proving a wrapped bound.
            total = total.checked_add(value.coef[i] * u64::from(self.workgroup[i] - 1))?;
        }
        Some(total)
    }
    fn guard(&mut self, value: IdBound) {
        if value.base != 0 || value.c < 0 { self.guard_failed = true; return; }
        self.guard = match self.guard { Some(old) => Self::max(old, value), None => Some(value) };
        self.guard_failed |= self.guard.is_none();
    }
    fn bit_field_extract(&mut self, inst: &Inst, depth: u32) -> Option<IdBound> {
        let (Value::ImmU32(offset), Value::ImmU32(count)) = (inst.args[1], inst.args[2]) else { return None };
        if offset >= 32 || count == 0 || count > 32 { return None; }
        if let Value::ImmU32(v) = inst.args[0] {
            return Some(Self::constant(((u64::from(v) >> offset) & ((1u64 << count) - 1)) as i64));
        }
        let source = self.evaluate(inst.args[0], depth + 1);
        let field_max = (count <= 24).then(|| Self::inexact(Self::constant((1i64 << count) - 1)));
        let Some(source) = source.filter(|v| v.base == 0 && v.c >= 0) else { return field_max };
        if self.static_max(source).is_some_and(|v| v >> offset == 0) { return Some(Self::constant(0)); }
        if offset == 0 { return Some(if count == 32 { source } else { Self::inexact(source) }); }
        if offset >= 16 { self.guard(source); return Some(Self::constant(0)); }
        Self::shift_down(source, offset)
    }
    fn evaluate(&mut self, value: Value, depth: u32) -> Option<IdBound> {
        if let Value::ImmU32(v) = value { return Some(Self::constant(i64::from(v as i32))); }
        if value.is_immediate() || depth > Self::MAX_DEPTH { return None; }
        let (key, inst) = inst_recursive_from_value(self.program, value)?;
        if let Some(result) = self.memo.get(&key) { return *result; }
        if !self.active.insert(key) { return None; }
        let result = self.evaluate_inst(inst, depth);
        self.active.remove(&key);
        self.memo.insert(key, result);
        result
    }
    fn evaluate_inst(&mut self, inst: &Inst, depth: u32) -> Option<IdBound> {
        if let Some(word) = cbuf_word(self.program, inst) {
            return (word == self.buffer).then_some(IdBound { base: 1, ..Default::default() });
        }
        use Opcode::*;
        let usable = |v: &IdBound| v.base == 0 && v.c >= 0;
        match inst.opcode {
            CompositeExtractU32x3 => {
                let Value::ImmU32(element) = inst.args[1] else { return None };
                if element > 2 { return None; }
                let (_, vector) = inst_recursive_from_value(self.program, inst.args[0])?;
                let axis = match vector.opcode { LocalInvocationId => element, WorkgroupId => element + 3, _ => return None };
                let mut result = IdBound::default(); result.coef[axis as usize] = 1; Some(result)
            }
            IAdd32 | ISub32 | IMul32 | BitwiseOr32 | BitwiseXor32 | UMax32 => {
                let lhs = self.evaluate(inst.args[0], depth + 1)?;
                let rhs = self.evaluate(inst.args[1], depth + 1)?;
                match inst.opcode {
                    IAdd32 => Self::sum(lhs, rhs),
                    ISub32 if rhs.is_constant() && rhs.exact => Self::sum(lhs, Self::constant(-rhs.c)),
                    IMul32 if rhs.is_constant() && rhs.c >= 0 => Self::scale(lhs, rhs.c as u64, rhs.exact),
                    IMul32 if lhs.is_constant() && lhs.c >= 0 => Self::scale(rhs, lhs.c as u64, lhs.exact),
                    BitwiseOr32 | BitwiseXor32 | UMax32 if usable(&lhs) && usable(&rhs) => Self::sum(lhs, rhs).map(Self::inexact),
                    _ => None,
                }
            }
            ShiftLeftLogical32 | ShiftRightLogical32 => {
                let Value::ImmU32(shift) = inst.args[1] else { return None };
                if shift >= 32 { return None; }
                let lhs = self.evaluate(inst.args[0], depth + 1)?;
                if inst.opcode == ShiftLeftLogical32 { Self::scale(lhs, 1u64 << shift, true) }
                else { Self::shift_down(lhs, shift) }
            }
            BitwiseAnd32 | UMin32 => {
                let lhs = self.evaluate(inst.args[0], depth + 1).filter(usable);
                let rhs = self.evaluate(inst.args[1], depth + 1).filter(usable);
                if inst.opcode == UMin32 { return rhs.or(lhs).map(Self::inexact); }
                [rhs, lhs].into_iter().flatten().find(|v| v.is_constant() && v.c <= 0xffffff)
                    .or(lhs).or(rhs).map(Self::inexact)
            }
            BitFieldUExtract => self.bit_field_extract(inst, depth),
            BitFieldInsert => {
                let (Value::ImmU32(offset), Value::ImmU32(_)) = (inst.args[2], inst.args[3]) else { return None };
                if offset >= 32 { return None; }
                let base = self.evaluate(inst.args[0], depth + 1)?;
                let insert = self.evaluate(inst.args[1], depth + 1)?;
                if !usable(&base) || !usable(&insert) { return None; }
                Self::sum(base, Self::scale(insert, 1u64 << offset, true)?).map(Self::inexact)
            }
            SelectU32 => {
                let lhs = self.evaluate(inst.args[1], depth + 1)?;
                let rhs = self.evaluate(inst.args[2], depth + 1)?;
                Self::max(lhs, rhs)
            }
            Phi => {
                let mut result = None;
                for &(_, value) in &inst.phi_args {
                    let incoming = self.evaluate(value, depth + 1)?;
                    result = Some(match result { Some(old) => Self::max(old, incoming)?, None => incoming });
                }
                result
            }
            _ => None,
        }
    }
}

/// Port of upstream `Track`: breadth-first search for the constant-buffer read
/// that supplies the storage buffer pointer. Upstream's `BreadthFirstSearch`
/// visits the rightmost argument first; an address of the form
/// `index * stride + pointer` whose index is itself read from a constant
/// buffer resolves to the pointer only in that order.
fn track(program: &Program, value: Value, bias: Option<Bias>) -> Option<StorageBufferAddr> {
    crate::ir::breadth_first_search::breadth_first_search(value, program, |inst: &Inst| {
        if !matches!(inst.opcode, Opcode::GetCbufU32 | Opcode::GetCbufU32x2) {
            return None;
        }
        let index = inst.args.first()?;
        let offset = inst.args.get(1)?;
        if !index.is_immediate() || !offset.is_immediate() {
            return None;
        }
        let storage_buffer = StorageBufferAddr {
            index: index.imm_u32(),
            offset: offset.imm_u32(),
        };
        let alignment = bias.map_or(8, |bias| bias.alignment);
        if storage_buffer.offset % alignment != 0 {
            return None;
        }
        if bias.is_some_and(|bias| !meets_bias(storage_buffer, bias)) {
            return None;
        }
        Some(storage_buffer)
    })
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
    program
        .block_mut(inst.block)
        .inst_mut(inst.inst)
        .replace_uses_with(value);
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
    // CAS has both a comparator and a replacement; preserve all data operands.
    let sources = program.block(inst.block).inst(inst.inst).args[1..].to_vec();
    let new_opcode = storage_opcode(program.block(inst.block).inst(inst.inst).opcode);
    let mut args = vec![Value::ImmU32(storage_index), offset];
    args.extend(sources);
    let value = insert_before(program, inst, new_opcode, args);
    program
        .block_mut(inst.block)
        .inst_mut(inst.inst)
        .replace_uses_with(value);
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
        | Opcode::GlobalAtomicCompareExchange32
        | Opcode::GlobalAtomicExchange32
        | Opcode::GlobalAtomicIAdd64
        | Opcode::GlobalAtomicSMin64
        | Opcode::GlobalAtomicUMin64
        | Opcode::GlobalAtomicSMax64
        | Opcode::GlobalAtomicUMax64
        | Opcode::GlobalAtomicAnd64
        | Opcode::GlobalAtomicOr64
        | Opcode::GlobalAtomicXor64
        | Opcode::GlobalAtomicCompareExchange64
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
    let mut untracked_writes = false;
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
            let previous_count = info.to_replace.len();
            collect_storage_buffer(
                program,
                InstRef {
                    block: block_idx,
                    inst: inst_idx,
                },
                &mut info,
            );
            untracked_writes |= info.to_replace.len() == previous_count
                && is_global_memory_write(program.block(block_idx).inst(inst_idx).opcode);
        }
    }

    // A store whose pointer is unknown may alias any descriptor. As in Lichii,
    // do not narrow any write range in that case. Analyze before rewriting IR.
    let mut extents = BTreeMap::new();
    if !untracked_writes {
        let mut analyses = BTreeMap::new();
        for storage in &info.to_replace {
            let inst = program.block(storage.inst.block).inst(storage.inst.inst);
            if !is_global_memory_write(inst.opcode) { continue; }
            let analysis = analyses.entry(storage.storage_buffer)
                .or_insert_with(|| StoreExtentAnalysis::new(program, storage.storage_buffer));
            let extent = extents.entry(storage.storage_buffer)
                .or_insert_with(|| StorageWriteExtent { bounded: true, ..Default::default() });
            analysis.add_store(inst, extent);
        }
    }
    program.info.storage_buffers_descriptors = info
        .set
        .iter()
        .map(|storage_buffer| StorageBufferDescriptor {
            cbuf_index: storage_buffer.index,
            cbuf_offset: storage_buffer.offset,
            count: 1,
            write_extent: extents.get(storage_buffer).copied().unwrap_or_default(),
            is_written: info.writes.contains(storage_buffer), ..Default::default() })
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
pub fn join_storage_info(base: &mut ShaderInfo, source: &mut ShaderInfo) -> Vec<u32> {
    let descriptors = &mut base.storage_buffers_descriptors;
    let mut mapping = Vec::with_capacity(source.storage_buffers_descriptors.len());
    for desc in &source.storage_buffers_descriptors {
        if let Some(index) = descriptors.iter().position(|existing| {
            desc.cbuf_index == existing.cbuf_index
                && desc.cbuf_offset == existing.cbuf_offset
                && desc.count == existing.count
                && desc.indirect_offset == existing.indirect_offset
        }) {
            if desc.is_written {
                if descriptors[index].is_written {
                    crate::storage_write_extent::merge_storage_write_extent(
                        &mut descriptors[index].write_extent, &desc.write_extent);
                } else {
                    descriptors[index].write_extent = desc.write_extent;
                }
            }
            descriptors[index].is_written |= desc.is_written;
            mapping.push(index as u32);
            continue;
        }
        mapping.push(descriptors.len() as u32);
        descriptors.push(desc.clone());
    }
    mapping
}

/// Remap the binding operand in the cloned second vertex program after joining
/// its descriptors. Hardware cbuf identifiers are deliberately not remapped.
pub fn remap_storage_bindings(blocks: &mut [crate::ir::basic_block::Block], mapping: &[u32]) {
    for block in blocks {
        for inst in block.iter_mut() {
            use Opcode::*;
            if !matches!(inst.opcode,
                LoadStorageU8 | LoadStorageS8 | LoadStorageU16 | LoadStorageS16 |
                LoadStorage32 | LoadStorage64 | LoadStorage128 |
                WriteStorageU8 | WriteStorageS8 | WriteStorageU16 | WriteStorageS16 |
                WriteStorage32 | WriteStorage64 | WriteStorage128 |
                StorageAtomicIAdd32 | StorageAtomicSMin32 | StorageAtomicUMin32 |
                StorageAtomicSMax32 | StorageAtomicUMax32 | StorageAtomicInc32 |
                StorageAtomicDec32 | StorageAtomicAnd32 | StorageAtomicOr32 |
                StorageAtomicXor32 | StorageAtomicExchange32 | StorageAtomicCompareExchange32 |
                StorageAtomicIAdd64 | StorageAtomicSMin64 | StorageAtomicUMin64 |
                StorageAtomicSMax64 | StorageAtomicUMax64 | StorageAtomicAnd64 |
                StorageAtomicOr64 | StorageAtomicXor64 | StorageAtomicExchange64 |
                StorageAtomicCompareExchange64 | StorageAtomicIAdd32x2 |
                StorageAtomicSMin32x2 | StorageAtomicUMin32x2 | StorageAtomicSMax32x2 |
                StorageAtomicUMax32x2 | StorageAtomicAnd32x2 | StorageAtomicOr32x2 |
                StorageAtomicXor32x2 | StorageAtomicExchange32x2 | StorageAtomicAddF32 |
                StorageAtomicAddF16x2 | StorageAtomicAddF32x2 | StorageAtomicMinF16x2 |
                StorageAtomicMinF32x2 | StorageAtomicMaxF16x2 | StorageAtomicMaxF32x2
            ) { continue; }
            let Value::ImmU32(index) = inst.args[0] else {
                panic!("storage binding must be an immediate after storage lowering");
            };
            inst.args[0] = Value::ImmU32(mapping[index as usize]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::basic_block::Block;
    use crate::ir::types::ShaderStage;

    fn extent_inst(program: &mut Program, opcode: Opcode, args: Vec<Value>) -> Value {
        let inst = program.block_mut(0).append_inst(Inst::new(opcode, args));
        Value::Inst(InstRef { block: 0, inst })
    }

    #[test]
    fn affine_store_extent_and_untracked_write_fallback() {
        use crate::storage_write_extent::resolve_storage_write_extent;
        for unknown_store in [false, true] {
            let mut p = Program::new(ShaderStage::Compute);
            p.blocks.push(Block::new()); p.post_order_blocks.push(0);
            p.workgroup_size = [8, 1, 1];
            let pointer = extent_inst(&mut p, Opcode::GetCbufU32, vec![Value::ImmU32(0), Value::ImmU32(0x100)]);
            let local = extent_inst(&mut p, Opcode::LocalInvocationId, vec![]);
            let local = extent_inst(&mut p, Opcode::CompositeExtractU32x3, vec![local, Value::ImmU32(0)]);
            let group = extent_inst(&mut p, Opcode::WorkgroupId, vec![]);
            let group = extent_inst(&mut p, Opcode::CompositeExtractU32x3, vec![group, Value::ImmU32(0)]);
            let local = extent_inst(&mut p, Opcode::IMul32, vec![local, Value::ImmU32(4)]);
            let group = extent_inst(&mut p, Opcode::IMul32, vec![group, Value::ImmU32(32)]);
            let offset = extent_inst(&mut p, Opcode::IAdd32, vec![local, group]);
            let low = extent_inst(&mut p, Opcode::IAdd32, vec![pointer, offset]);
            let address = extent_inst(&mut p, Opcode::CompositeConstructU32x2, vec![low, Value::ImmU32(0)]);
            let address = extent_inst(&mut p, Opcode::PackUint2x32, vec![address]);
            extent_inst(&mut p, Opcode::WriteGlobal32, vec![address, Value::ImmU32(1)]);
            if unknown_store {
                extent_inst(&mut p, Opcode::WriteGlobal32, vec![Value::ImmU64(4096), Value::ImmU32(1)]);
            }
            global_memory_to_storage_buffer_pass(&mut p, &HostTranslateInfo::default());
            let extent = &p.info.storage_buffers_descriptors[0].write_extent;
            if unknown_store { assert!(!extent.bounded); }
            else {
                assert_eq!(extent.local_stride, [4, 0, 0]);
                assert_eq!(extent.group_stride, [32, 0, 0]);
                assert_eq!(resolve_storage_write_extent(extent, [8, 1, 1], Some([3, 1, 1])), Some(96));
                assert_eq!(resolve_storage_write_extent(extent, [8, 1, 1], None), None);
            }
        }
    }

    #[test]
    fn store_extent_rejects_wrap_cycles_and_other_pointer_words() {
        let mut p = Program::new(ShaderStage::Compute);
        p.blocks.push(Block::new());
        let pair = extent_inst(&mut p, Opcode::GetCbufU32x2, vec![Value::ImmU32(0), Value::ImmU32(0x100)]);
        let low = extent_inst(&mut p, Opcode::CompositeExtractU32x2, vec![pair, Value::ImmU32(0)]);
        let high = extent_inst(&mut p, Opcode::CompositeExtractU32x2, vec![pair, Value::ImmU32(1)]);
        let cycle = extent_inst(&mut p, Opcode::Phi, vec![]);
        let Value::Inst(cycle_ref) = cycle else { unreachable!() };
        p.block_mut(0).inst_mut(cycle_ref.inst).phi_args.push((0, cycle));
        let mut analysis = StoreExtentAnalysis::new(&p, StorageBufferAddr { index: 0, offset: 0x100 });
        assert_eq!(analysis.evaluate(low, 0).unwrap().base, 1);
        assert!(analysis.evaluate(high, 0).is_none());
        assert!(analysis.evaluate(cycle, 0).is_none());
        assert!(StoreExtentAnalysis::sum(StoreExtentAnalysis::inexact(IdBound::default()),
            StoreExtentAnalysis::constant(-1)).is_none());
        assert!(StoreExtentAnalysis::scale(StoreExtentAnalysis::constant(1 << 40), 2, true).is_none());
        assert_eq!(store_size(Opcode::GlobalAtomicCompareExchange64), 8);
        assert_eq!(store_size(Opcode::GlobalAtomicAddF32x2), 8);
        assert_eq!(store_size(Opcode::WriteGlobal128), 16);
    }

    #[test]
    fn high_half_extent_requires_dispatch_guard() {
        use crate::storage_write_extent::resolve_storage_write_extent;
        let mut p = Program::new(ShaderStage::Compute);
        p.blocks.push(Block::new());
        let pointer = extent_inst(&mut p, Opcode::GetCbufU32, vec![Value::ImmU32(0), Value::ImmU32(0x100)]);
        let group = extent_inst(&mut p, Opcode::WorkgroupId, vec![]);
        let group = extent_inst(&mut p, Opcode::CompositeExtractU32x3, vec![group, Value::ImmU32(0)]);
        let high = extent_inst(&mut p, Opcode::BitFieldUExtract, vec![group, Value::ImmU32(16), Value::ImmU32(16)]);
        let low = extent_inst(&mut p, Opcode::IAdd32, vec![pointer, high]);
        let address = extent_inst(&mut p, Opcode::CompositeConstructU32x2, vec![low, Value::ImmU32(0)]);
        let store = Inst::new(Opcode::WriteGlobal32, vec![address, Value::ImmU32(0)]);
        let mut analysis = StoreExtentAnalysis::new(&p, StorageBufferAddr { index: 0, offset: 0x100 });
        let extent = analysis.store_extent(&store).unwrap();
        assert!(extent.guarded);
        assert_eq!(resolve_storage_write_extent(&extent, [1; 3], Some([65536, 1, 1])), Some(4));
        assert_eq!(resolve_storage_write_extent(&extent, [1; 3], Some([65537, 1, 1])), None);
    }

    #[test]
    fn joined_storage_preserves_indirect_identity_and_merges_write_extents() {
        use crate::storage_write_extent::StorageWriteExtent;
        let descriptor = StorageBufferDescriptor { cbuf_index: 2, cbuf_offset: 32,
            is_written: true, write_extent: StorageWriteExtent { bounded: true, const_end: 16, ..Default::default() },
            ..Default::default() };
        let mut base = ShaderInfo::default();
        base.storage_buffers_descriptors.push(descriptor.clone());
        let mut source = ShaderInfo::default();
        source.storage_buffers_descriptors.push(StorageBufferDescriptor {
            write_extent: StorageWriteExtent { bounded: true, const_end: 32, ..Default::default() },
            ..descriptor.clone() });
        source.storage_buffers_descriptors.push(StorageBufferDescriptor { indirect_offset: 8, ..descriptor });
        assert_eq!(join_storage_info(&mut base, &mut source), [0, 1]);
        assert_eq!(base.storage_buffers_descriptors[0].write_extent.const_end, 32);
        assert_eq!(base.storage_buffers_descriptors[1].indirect_offset, 8);
        source.storage_buffers_descriptors.truncate(1);
        source.storage_buffers_descriptors[0].write_extent = Default::default();
        join_storage_info(&mut base, &mut source);
        assert!(!base.storage_buffers_descriptors[0].write_extent.bounded);
    }

    #[test]
    fn load_and_atomic_replacement_preserve_definition_identity_until_cleanup() {
        for atomic in [false, true] {
            let mut program = Program::new(ShaderStage::Compute);
            program.blocks = vec![Block::new(), Block::new()];
            program.post_order_blocks = vec![1, 0];
            let opcode = if atomic { Opcode::GlobalAtomicIAdd32 } else { Opcode::LoadGlobal32 };
            let args = if atomic { vec![Value::ImmU64(4096), Value::ImmU32(7)] }
                else { vec![Value::ImmU64(4096)] };
            let old = program.block_mut(0).append_inst(Inst::new(opcode, args));
            let old_ref = InstRef { block: 0, inst: old };
            let old_value = Value::Inst(old_ref);
            let user = program.block_mut(1).append_inst(Inst::new(
                Opcode::IAdd32, vec![old_value, Value::ImmU32(3)],
            ));
            let mut phi = Inst::new(Opcode::Phi, vec![]);
            phi.phi_args.push((0, old_value));
            let phi = program.block_mut(1).append_inst(phi);
            if atomic { replace_atomic(&mut program, old_ref, 2, Value::ImmU32(16)); }
            else { replace_load(&mut program, old_ref, 2, Value::ImmU32(16)); }
            let definition = program.block(0).inst(old);
            assert_eq!(definition.opcode, Opcode::Identity);
            let replacement = definition.args[0];
            let Value::Inst(new_ref) = replacement else { panic!("replacement must be an instruction") };
            assert_eq!(program.block(new_ref.block).inst(new_ref.inst).opcode,
                if atomic { Opcode::StorageAtomicIAdd32 } else { Opcode::LoadStorage32 });
            assert_eq!(program.block(1).inst(user).args[0], old_value);
            assert_eq!(program.block(1).inst(phi).phi_args[0].1, old_value);
            super::super::identity_removal_pass::identity_removal_pass(&mut program);
            assert_eq!(program.block(1).inst(user).args[0], replacement);
            assert_eq!(program.block(1).inst(phi).phi_args[0].1, replacement);
            assert!(program.block(0).instructions[old as usize].is_none());
        }
    }

    /// `index * stride + pointer`, both read from constant buffers at the same
    /// depth: upstream's rightmost-first search resolves to the pointer.
    #[test]
    fn track_resolves_scaled_index_plus_pointer_to_the_pointer() {
        let mut program = Program::new(ShaderStage::Compute);
        program.blocks.push(Block::new());
        fn append(program: &mut Program, opcode: Opcode, args: Vec<Value>) -> Value {
            let inst = program.block_mut(0).append_inst(Inst::new(opcode, args));
            Value::Inst(InstRef { block: 0, inst })
        }
        let index = append(&mut program, Opcode::GetCbufU32, vec![Value::ImmU32(1), Value::ImmU32(8)]);
        let scaled = append(&mut program, Opcode::ShiftLeftLogical32, vec![index, Value::ImmU32(3)]);
        let pointer_pair = append(&mut program, Opcode::GetCbufU32x2, vec![Value::ImmU32(0), Value::ImmU32(80)]);
        let pointer = append(&mut program, Opcode::CompositeExtractU32x2, vec![pointer_pair, Value::ImmU32(0)]);
        let address = append(&mut program, Opcode::IAdd32, vec![scaled, pointer]);
        assert_eq!(
            track(&program, address, None),
            Some(StorageBufferAddr { index: 0, offset: 80 })
        );
        // The same operands in the other order still follow upstream's order.
        let swapped = append(&mut program, Opcode::IAdd32, vec![pointer, scaled]);
        assert_eq!(
            track(&program, swapped, None),
            Some(StorageBufferAddr { index: 1, offset: 8 })
        );
    }

    #[test]
    fn cas_lowering_preserves_both_operands_and_side_effects() {
        for (global, storage, compare, replacement) in [
            (
                Opcode::GlobalAtomicCompareExchange32,
                Opcode::StorageAtomicCompareExchange32,
                Value::ImmU32(17),
                Value::ImmU32(29),
            ),
            (
                Opcode::GlobalAtomicCompareExchange64,
                Opcode::StorageAtomicCompareExchange64,
                Value::ImmU64(0x100000011),
                Value::ImmU64(0x200000029),
            ),
        ] {
            assert!(is_global_memory(global));
            assert!(is_global_memory_write(global));
            assert!(global.may_have_side_effects());
            assert!(storage.may_have_side_effects());
            let mut program = Program::new(ShaderStage::Compute);
            program.blocks.push(Block::new());
            let index = program.block_mut(0).append_inst(Inst::new(
                global,
                vec![Value::ImmU64(4096), compare, replacement],
            ));
            replace_atomic(
                &mut program,
                InstRef {
                    block: 0,
                    inst: index,
                },
                3,
                Value::ImmU32(24),
            );
            let emitted = program
                .block(0)
                .iter()
                .find(|i| i.opcode == storage)
                .unwrap();
            assert_eq!(
                emitted.args,
                vec![Value::ImmU32(3), Value::ImmU32(24), compare, replacement]
            );
        }
    }

    #[test]
    fn cas_global_pass_discovers_writable_descriptor_and_tracks_width() {
        use crate::ir::emitter::Emitter;
        use crate::ir::types::Type;
        for wide in [false, true] {
            let mut program = Program::new(ShaderStage::Compute);
            program.blocks.push(Block::new());
            program.post_order_blocks = vec![0];
            let cbuf = program.block_mut(0).append_inst(Inst::new(
                Opcode::GetCbufU32,
                vec![Value::ImmU32(0), Value::ImmU32(0x100)],
            ));
            let mut ir = Emitter::new(&mut program, 0);
            let pair = ir.composite_construct_u32x2(
                Value::Inst(InstRef {
                    block: 0,
                    inst: cbuf,
                }),
                Value::ImmU32(0),
            );
            let address = ir.pack_uint_2x32(pair);
            let (compare, replacement) = if wide {
                (Value::ImmU64(17), Value::ImmU64(29))
            } else {
                (Value::ImmU32(17), Value::ImmU32(29))
            };
            ir.global_atomic_compare_exchange(address, compare, replacement, wide);
            global_memory_to_storage_buffer_pass(
                &mut program,
                &HostTranslateInfo {
                    support_int64: true,
                    min_ssbo_alignment: 0x100,
                    ..Default::default()
                },
            );
            super::super::collect_shader_info_pass::collect_shader_info_pass(&mut program);
            assert_eq!(program.info.storage_buffers_descriptors.len(), 1);
            assert!(program.info.storage_buffers_descriptors[0].is_written);
            let opcode = if wide {
                Opcode::StorageAtomicCompareExchange64
            } else {
                Opcode::StorageAtomicCompareExchange32
            };
            let atomic = program
                .block(0)
                .iter()
                .find(|i| i.opcode == opcode)
                .unwrap();
            assert_eq!(&atomic.args[2..], &[compare, replacement]);
            let ty = if wide { Type::U64 } else { Type::U32 };
            assert_ne!(program.info.used_storage_buffer_types & ty as u32, 0);
            assert_eq!(program.info.uses_int64_bit_atomics, wide);
        }
    }

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
        assert_eq!(program.block(0).inst(atomic).opcode, Opcode::Identity);
        assert_eq!(program.block(0).inst(atomic).args, vec![storage_value]);
        super::super::identity_removal_pass::identity_removal_pass(&mut program);
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

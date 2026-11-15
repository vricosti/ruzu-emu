// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `ir_opt/global_memory_to_storage_buffer_pass.cpp`
//!
//! Converts global memory accesses to storage buffer accesses.
//! This pass identifies constant buffer addresses used as SSBO descriptors
//! and rewrites global load/store instructions to use indexed storage
//! buffer operations instead.
//!
//! The pointer tracking (paired pointer words, iterator words, atomic
//! pointers, native pointer candidates and the run-time routed accesses of
//! native programs) follows Lichii's
//! `shader_recompiler/ir_opt/global_memory_to_storage_buffer_pass.cpp`.

use crate::environment::Environment;
use crate::host_translate_info::HostTranslateInfo;
use crate::ir::instruction::Inst;
use crate::ir::opcodes::Opcode;
use crate::ir::program::{Program, ShaderInfo, SyntaxNode};
use crate::ir::types::{ShaderStage, Type};
use crate::ir::value::{InstRef, Value};
use crate::shader_info::StorageBufferDescriptor;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use crate::storage_write_extent::{StorageWriteExtent, merge_storage_write_extent};

/// `indirect` of a pointer read directly from a constant buffer (upstream `~0U`).
const DIRECT: u32 = u32::MAX;

/// Address in constant buffers to the storage buffer descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct StorageBufferAddr {
    index: u32,
    offset: u32,
    /// Offset of the buffer pointer in the memory the constant buffer pointer points to
    indirect: u32,
}

impl StorageBufferAddr {
    const fn direct(index: u32, offset: u32) -> Self {
        Self { index, offset, indirect: DIRECT }
    }
}

/// Upstream `StorageBufferSet` (a sorted flat set).
type StorageBufferSet = BTreeSet<StorageBufferAddr>;

/// Global memory instruction and the storage buffer it uses
#[derive(Debug, Clone, Copy)]
struct StorageInst {
    storage_buffer: StorageBufferAddr,
    inst: InstRef,
}

#[derive(Debug, Clone)]
struct StorageInfo {
    set: StorageBufferSet,
    to_replace: Vec<StorageInst>,
    writes: StorageBufferSet,
    /// Constant buffer words that step strided iterators, see `collect_iterator_words`
    iterator_words: StorageBufferSet,
    /// Constant buffer pointers a global atomic uses as its address, see `collect_atomic_pointers`
    atomic_pointers: StorageBufferSet,
    /// Track results by the instruction the search starts from, with and without the NVN bias.
    biased_tracks: HashMap<InstRef, Option<StorageBufferAddr>>,
    unbiased_tracks: HashMap<InstRef, Option<StorageBufferAddr>>,
    /// Instructions from which a search with (or without) the bias cannot succeed.
    biased_exhausted: HashSet<InstRef>,
    unbiased_exhausted: HashSet<InstRef>,
    native_candidates: HashMap<InstRef, Vec<StorageBufferAddr>>,
    /// Native accesses whose candidates include slots of a dynamically indexed pointer array.
    native_dynamic: HashSet<InstRef>,
    /// A native access with several candidate pointers was left global.
    native_runtime_bounds: bool,
    /// Paired accesses bind the loaded pointers that compete with their pair; cleared when those
    /// leave too few bindings.
    bind_competing_loaded: bool,
    /// A paired access bound the loaded pointers that compete with its pair.
    competing_loaded_bound: bool,
    /// Pointer table entries reached through a phi or select of table pointers, or through a
    /// table pointer read from a dynamically indexed constant buffer array. They are bound only
    /// in the bindings the program's other storage buffers leave free (table_budget).
    tables_enabled: bool,
    tables_seen: bool,
    table_budget: usize,
    table_used: usize,
    /// The storage buffers the pass without table entries bound.
    counted: StorageBufferSet,
    table_candidates: Vec<StorageBufferAddr>,
    /// Buffers a store or atomic that stayed global may write to (at run time, at any offset)
    unbounded_writes: StorageBufferSet,
    /// A store or atomic stayed global without a buffer: it may land in any written buffer
    untracked_writes: bool,
}

impl Default for StorageInfo {
    fn default() -> Self {
        Self {
            set: StorageBufferSet::new(),
            to_replace: Vec::new(),
            writes: StorageBufferSet::new(),
            iterator_words: StorageBufferSet::new(),
            atomic_pointers: StorageBufferSet::new(),
            biased_tracks: HashMap::new(),
            unbiased_tracks: HashMap::new(),
            biased_exhausted: HashSet::new(),
            unbiased_exhausted: HashSet::new(),
            native_candidates: HashMap::new(),
            native_dynamic: HashSet::new(),
            native_runtime_bounds: false,
            bind_competing_loaded: true,
            competing_loaded_bound: false,
            tables_enabled: false,
            tables_seen: false,
            table_budget: 0,
            table_used: 0,
            counted: StorageBufferSet::new(),
            table_candidates: Vec::new(),
            unbounded_writes: StorageBufferSet::new(),
            untracked_writes: false,
        }
    }
}

/// Bias towards a certain range of constant buffers when looking for storage buffers
#[derive(Debug, Clone, Copy)]
struct Bias {
    index: u32,
    offset_begin: u32,
    offset_end: u32,
    alignment: u32,
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


/// Upstream `Value::InstRecursive` for a value that is not an immediate: the
/// instruction past any `Identity` chain. `None` is upstream `IsImmediate()`
/// (an immediate, or a non-instruction value such as a register).
fn inst_of(program: &Program, value: Value) -> Option<InstRef> {
    match value.resolve(program) {
        Value::Inst(inst) => Some(inst),
        _ => None,
    }
}

fn inst_at(program: &Program, inst: InstRef) -> &Inst {
    program.block(inst.block).inst(inst.inst)
}

fn is_imm(program: &Program, value: Value) -> bool {
    inst_of(program, value).is_none()
}

/// Upstream `Value::U32()` of an immediate argument.
fn imm32(program: &Program, value: Value) -> Option<u32> {
    match value.resolve(program) {
        Value::ImmU32(v) => Some(v),
        Value::ImmU1(v) => Some(v as u32),
        Value::ImmU8(v) => Some(v as u32),
        Value::ImmU16(v) => Some(v as u32),
        _ => None,
    }
}

/// Upstream `Value::U64()` of an immediate argument.
fn imm64(program: &Program, value: Value) -> Option<u64> {
    match value.resolve(program) {
        Value::ImmU64(v) => Some(v),
        Value::ImmU32(v) => Some(v as u64),
        _ => None,
    }
}

const fn is_aligned(value: u32, alignment: u32) -> bool {
    value % alignment == 0
}

/// Returns true when the backend can pick a global access's buffer among the bound ones at run time
fn is_runtime_routable(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::LoadGlobal32
            | Opcode::LoadGlobal64
            | Opcode::LoadGlobal128
            | Opcode::WriteGlobal32
            | Opcode::WriteGlobal64
            | Opcode::WriteGlobal128
            | Opcode::GlobalAtomicIAdd32
            | Opcode::GlobalAtomicUMax32
            | Opcode::GlobalAtomicCompareExchange32
    )
}

/// Returns true when a storage buffer address satisfies a bias
fn meets_bias(storage_buffer: StorageBufferAddr, bias: Bias) -> bool {
    storage_buffer.index == bias.index
        && storage_buffer.offset >= bias.offset_begin
        && storage_buffer.offset < bias.offset_end
}

#[derive(Debug, Clone, Copy)]
struct LowAddrInfo {
    value: Value,
    high: Value,
    imm_offset: i32,
}

/// Tries to track the first 32-bits of a global memory instruction
fn track_low_address(program: &Program, inst: &Inst) -> Option<LowAddrInfo> {
    // The first argument is the low level GPU pointer to the global memory instruction
    let addr = *inst.args.first()?;
    // This address is expected to either be a PackUint2x32, a IAdd64, or a CompositeConstructU32x2
    let mut addr_inst = inst_at(program, inst_of(program, addr)?);
    let mut imm_offset = 0i32;
    if addr_inst.opcode == Opcode::IAdd64 {
        // The instruction is canonicalized with the address first and the immediate offset second.
        let imm_offset_value = addr_inst.args[1];
        if !is_imm(program, imm_offset_value) {
            return None;
        }
        imm_offset = imm64(program, imm_offset_value)? as i64 as i32;
        addr_inst = inst_at(program, inst_of(program, addr_inst.args[0])?);
    }
    // With IAdd64 handled, now PackUint2x32 is expected
    if addr_inst.opcode == Opcode::PackUint2x32 {
        addr_inst = inst_at(program, inst_of(program, addr_inst.args[0])?);
    }
    // The vector is expected to be a CompositeConstructU32x2
    if addr_inst.opcode != Opcode::CompositeConstructU32x2 {
        return None;
    }
    Some(LowAddrInfo {
        value: addr_inst.args[0],
        high: addr_inst.args[1],
        imm_offset,
    })
}

/// Tries to track the storage buffer address used by a global memory instruction.
///
/// Upstream: `Track`, a `BreadthFirstSearchPruned` (rightmost arguments
/// first) that does not follow a pointer loaded from memory back to its own
/// constant-buffer descriptor.
fn track(
    program: &Program,
    value: Value,
    bias: Option<Bias>,
    exhausted: &mut HashSet<InstRef>,
) -> Option<StorageBufferAddr> {
    let pred = |_: InstRef, inst: &Inst| -> Option<StorageBufferAddr> {
        if !matches!(inst.opcode, Opcode::GetCbufU32 | Opcode::GetCbufU32x2) {
            return None;
        }
        // Definitely not a storage buffer if it's read from a non-immediate index;
        // SSBO arrays (non-immediate offsets) are not supported.
        let index = imm32(program, inst.args[0])?;
        let offset = imm32(program, inst.args[1])?;
        let storage_buffer = StorageBufferAddr::direct(index, offset);
        let alignment = bias.map_or(8, |bias| bias.alignment);
        if !is_aligned(storage_buffer.offset, alignment) {
            return None;
        }
        if bias.is_some_and(|bias| !meets_bias(storage_buffer, bias)) {
            return None;
        }
        Some(storage_buffer)
    };
    let stop_at_memory = |operand: Value| -> Value {
        match inst_of(program, operand) {
            Some(inst) if is_global_memory(inst_at(program, inst).opcode) => Value::ImmU32(0),
            _ => operand,
        }
    };
    crate::ir::breadth_first_search::breadth_first_search_pruned(
        value,
        program,
        pred,
        exhausted,
        stop_at_memory,
    )
}

/// Finds the one constant buffer word added directly into the low address, which is the base
/// pointer even when other constant buffer reads feed the computed offset
fn track_addend(program: &Program, value: Value) -> Option<StorageBufferAddr> {
    fn visit(
        program: &Program,
        term: Value,
        result: &mut Option<StorageBufferAddr>,
        ambiguous: &mut bool,
    ) {
        if *ambiguous {
            return;
        }
        let Some(inst) = inst_of(program, term) else {
            return;
        };
        let inst = inst_at(program, inst);
        if inst.opcode == Opcode::IAdd32 {
            visit(program, inst.args[0], result, ambiguous);
            visit(program, inst.args[1], result, ambiguous);
            return;
        }
        if inst.opcode != Opcode::GetCbufU32 {
            return;
        }
        let (Some(index), Some(offset)) = (imm32(program, inst.args[0]), imm32(program, inst.args[1]))
        else {
            return;
        };
        if is_imm(program, inst.args[0]) && is_imm(program, inst.args[1]) && is_aligned(offset, 8) {
            *ambiguous = result.is_some();
            *result = Some(StorageBufferAddr::direct(index, offset));
        }
    }
    let mut result = None;
    let mut ambiguous = false;
    visit(program, value, &mut result, &mut ambiguous);
    if ambiguous { None } else { result }
}

/// Returns the constant buffer word an instruction reads, including one half of a 64-bit read
fn cbuf_word(program: &Program, inst: &Inst) -> Option<StorageBufferAddr> {
    if inst.opcode == Opcode::CompositeExtractU32x2 {
        let vector = inst_at(program, inst_of(program, inst.args[0])?);
        if vector.opcode != Opcode::GetCbufU32x2 {
            return None;
        }
        let index = imm32(program, vector.args[0])?;
        let offset = imm32(program, vector.args[1])?;
        let element = imm32(program, inst.args[1])?;
        return Some(StorageBufferAddr::direct(
            index,
            offset.wrapping_add(element.wrapping_mul(4)),
        ));
    }
    if inst.opcode != Opcode::GetCbufU32 {
        return None;
    }
    Some(StorageBufferAddr::direct(
        imm32(program, inst.args[0])?,
        imm32(program, inst.args[1])?,
    ))
}

/// Constant buffer word read by a value, `None` for an immediate.
fn value_cbuf_word(program: &Program, value: Value) -> Option<StorageBufferAddr> {
    cbuf_word(program, inst_at(program, inst_of(program, value)?))
}


/// A condition value and the polarity under which it holds.
type Predicate = (Value, bool);

#[derive(Debug, Clone, Copy)]
struct Scope {
    condition: Predicate,
    header: Option<u32>,
    merge: u32,
}

/// Upstream `BitKey`: the instruction, the bit, the assumed condition's
/// instruction and its polarity class.
type BitKey = (InstRef, u32, Option<InstRef>, i32);

/// Structured control flow facts used while tracking pointers: the conditions
/// that guard each block, the single-branch merges whose phis select between
/// the branch and its bypass, and the loop each block belongs to.
///
/// Upstream: `TrackingConditions`. Blocks are indices and instructions are
/// `InstRef`s instead of pointers.
struct TrackingConditions<'a> {
    program: &'a Program,
    owners: HashMap<InstRef, u32>,
    scopes: HashMap<u32, Vec<Scope>>,
    merges: HashMap<u32, Vec<Scope>>,
    loops: HashMap<u32, Option<u32>>,
    current: Option<u32>,
    bit_cache: RefCell<HashMap<BitKey, Option<Predicate>>>,
}

impl<'a> TrackingConditions<'a> {
    fn new(program: &'a Program) -> Self {
        let mut this = Self {
            program,
            owners: HashMap::new(),
            scopes: HashMap::new(),
            merges: HashMap::new(),
            loops: HashMap::new(),
            current: None,
            bit_cache: RefCell::new(HashMap::new()),
        };
        for &block in &program.post_order_blocks {
            for (inst_index, inst) in program.block(block).indexed_iter() {
                if inst.opcode == Opcode::Phi {
                    this.owners.insert(InstRef { block, inst: inst_index }, block);
                }
            }
        }
        let mut active: Vec<Scope> = Vec::new();
        let mut loop_stack: Vec<u32> = Vec::new();
        let mut previous: Option<u32> = None;
        for node in &program.syntax_list {
            match *node {
                SyntaxNode::Block(block) => {
                    previous = Some(block);
                    this.scopes.insert(block, active.clone());
                    this.loops.insert(block, loop_stack.last().copied());
                }
                SyntaxNode::If { cond, merge, .. } => {
                    this.bit_cache.borrow_mut().clear();
                    let scope = Scope { condition: this.literal(cond, 256), header: previous, merge };
                    this.merges.entry(scope.merge).or_default().push(scope);
                    active.push(scope);
                }
                SyntaxNode::EndIf { .. } => {
                    active.pop();
                }
                SyntaxNode::Loop { body, .. } => loop_stack.push(body),
                SyntaxNode::Repeat { .. } => {
                    loop_stack.pop();
                }
                _ => {}
            }
        }
        this
    }

    fn order(&self, block: u32) -> u32 {
        self.program.block(block).order
    }

    fn phi_block(&self, phi: InstRef, arg: usize) -> u32 {
        inst_at(self.program, phi).phi_args[arg].0
    }

    fn constant(value: Predicate) -> Option<bool> {
        match value.0 {
            Value::ImmU1(v) => Some(v == value.1),
            _ => None,
        }
    }

    fn bit(&self, value: Value, bit: u32, depth: u32, assumed: Option<Predicate>) -> Option<Predicate> {
        let Some(inst) = inst_of(self.program, value) else {
            return self.bit_impl(value, bit, depth, assumed);
        };
        let key: BitKey = (
            inst,
            bit,
            assumed.and_then(|assumed| inst_of(self.program, assumed.0)),
            assumed.map_or(0, |assumed| match Self::constant(assumed) {
                Some(constant) => 4 + constant as i32,
                None => 1 + assumed.1 as i32,
            }),
        );
        if let Some(found) = self.bit_cache.borrow().get(&key) {
            return *found;
        }
        self.bit_cache.borrow_mut().insert(key, None);
        let result = self.bit_impl(value, bit, depth, assumed);
        self.bit_cache.borrow_mut().insert(key, result);
        result
    }

    fn bit_impl(&self, value: Value, bit: u32, depth: u32, assumed: Option<Predicate>) -> Option<Predicate> {
        if bit >= 32 || depth == 0 {
            return None;
        }
        let Some(inst_ref) = inst_of(self.program, value) else {
            let value = imm32(self.program, value)?;
            return Some((Value::ImmU1(((value >> bit) & 1) != 0), true));
        };
        let program = self.program;
        let inst = inst_at(program, inst_ref);
        if inst.opcode == Opcode::Phi && inst.num_args() == 2 {
            if let Some(&owner) = self.owners.get(&inst_ref) {
                if let Some(merge) = self.merges.get(&owner).filter(|merge| merge.len() == 1) {
                    let branch = merge[0];
                    let bypass = if Some(self.phi_block(inst_ref, 0)) == branch.header { 0 } else { 1 };
                    if Some(self.phi_block(inst_ref, bypass)) == branch.header
                        && self.order(self.phi_block(inst_ref, 1 - bypass)) < self.order(owner)
                    {
                        if let Some(assumed) = assumed.filter(|assumed| assumed.0 == branch.condition.0) {
                            let take_body = assumed.1 == branch.condition.1;
                            let arg = if take_body { 1 - bypass } else { bypass };
                            return self.bit(*inst.arg(arg), bit, depth - 1, Some(assumed));
                        }
                        let mut false_guard = branch.condition;
                        false_guard.1 = !false_guard.1;
                        let no = self.bit(*inst.arg(bypass), bit, depth - 1, Some(false_guard));
                        let yes = self.bit(*inst.arg(1 - bypass), bit, depth - 1, Some(branch.condition));
                        if let (Some(no), Some(yes)) = (no, yes) {
                            if no == yes {
                                return Some(no);
                            }
                            if let (Some(nc), Some(yc)) = (Self::constant(no), Self::constant(yes)) {
                                if nc == yc {
                                    return Some((Value::ImmU1(yc), true));
                                }
                                let mut result = branch.condition;
                                result.1 = result.1 == yc;
                                return Some(result);
                            }
                        }
                    }
                }
            }
            return None;
        }
        if inst.opcode == Opcode::SelectU32 {
            let yes = self.bit(inst.args[1], bit, depth - 1, assumed);
            let no = self.bit(inst.args[2], bit, depth - 1, assumed);
            if let (Some(yes), Some(no)) = (yes, no) {
                if yes == no {
                    return Some(yes);
                }
                if let (Some(yc), Some(nc)) = (Self::constant(yes), Self::constant(no)) {
                    if nc == yc {
                        return Some((Value::ImmU1(yc), true));
                    }
                    let mut cond = self.literal(inst.args[0], depth - 1);
                    cond.1 = cond.1 == yc;
                    return Some(cond);
                }
            }
        }
        if matches!(inst.opcode, Opcode::BitwiseOr32 | Opcode::BitwiseAnd32) {
            let lhs = self.bit(inst.args[0], bit, depth - 1, assumed);
            let rhs = self.bit(inst.args[1], bit, depth - 1, assumed);
            let is_or = inst.opcode == Opcode::BitwiseOr32;
            if let Some(constant) = lhs.and_then(Self::constant) {
                return if constant == is_or { lhs } else { rhs };
            }
            if let Some(constant) = rhs.and_then(Self::constant) {
                return if constant == is_or { rhs } else { lhs };
            }
        }
        if inst.opcode == Opcode::BitFieldUExtract {
            if let (Some(offset), Some(count)) = (imm32(program, inst.args[1]), imm32(program, inst.args[2])) {
                if bit >= count {
                    return Some((Value::ImmU1(false), true));
                }
                return self.bit(inst.args[0], bit.wrapping_add(offset), depth - 1, assumed);
            }
        }
        None
    }

    fn literal(&self, value: Value, depth: u32) -> Predicate {
        let program = self.program;
        let value = value.resolve(program);
        let Value::Inst(inst_ref) = value else {
            return (value, true);
        };
        if depth == 0 {
            return (value, true);
        }
        let inst = inst_at(program, inst_ref);
        match inst.opcode {
            Opcode::ConditionRef => return self.literal(inst.args[0], depth - 1),
            Opcode::LogicalNot => {
                let mut result = self.literal(inst.args[0], depth - 1);
                result.1 = !result.1;
                return result;
            }
            Opcode::SelectU1 => {
                if let Some(constant) = Self::constant(self.literal(inst.args[0], depth - 1)) {
                    return self.literal(inst.args[if constant { 1 } else { 2 }], depth - 1);
                }
            }
            Opcode::IEqual | Opcode::INotEqual | Opcode::GetZeroFromOp => {
                let compares_zero = inst.opcode == Opcode::GetZeroFromOp
                    || (is_imm(program, inst.args[1]) && imm32(program, inst.args[1]) == Some(0));
                if compares_zero {
                    let is_zero = inst.opcode != Opcode::INotEqual;
                    let input = inst.args[0].resolve(program);
                    if let Value::ImmU32(input) = input {
                        return (Value::ImmU1(input == 0), is_zero);
                    }
                    if let Value::Inst(extract_ref) = input {
                        let extract = inst_at(program, extract_ref);
                        if extract.opcode == Opcode::BitwiseAnd32 {
                            for arg in 0..2 {
                                let mask = extract.args[arg].resolve(program);
                                if let Value::ImmU32(mask) = mask {
                                    if mask.is_power_of_two() {
                                        if let Some(mut result) = self.bit(
                                            extract.args[1 - arg],
                                            mask.trailing_zeros(),
                                            depth - 1,
                                            None,
                                        ) {
                                            result.1 = result.1 != is_zero;
                                            return result;
                                        }
                                    }
                                }
                            }
                        }
                        if extract.opcode == Opcode::BitFieldUExtract
                            && is_imm(program, extract.args[2])
                            && imm32(program, extract.args[2]) == Some(1)
                        {
                            if let Some(mut result) = self.bit(input, 0, depth - 1, None) {
                                result.1 = result.1 != is_zero;
                                return result;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        (Value::Inst(inst_ref), true)
    }

    fn is_back_edge(&self, phi: InstRef, arg: usize) -> bool {
        self.owners
            .get(&phi)
            .is_some_and(|&owner| self.order(self.phi_block(phi, arg)) >= self.order(owner))
    }

    fn allows_phi(&self, phi: InstRef, arg: usize) -> bool {
        let Some(&owner) = self.owners.get(&phi) else {
            return true;
        };
        let current_loop = self.current.and_then(|current| self.loops.get(&current));
        match (self.loops.get(&owner), current_loop) {
            (Some(owner_loop), Some(current_loop)) if owner_loop == current_loop => {}
            _ => return true,
        }
        let merge = self.merges.get(&owner);
        let current_scopes = self.current.and_then(|current| self.scopes.get(&current));
        let (Some(merge), Some(current_scopes)) = (merge, current_scopes) else {
            return true;
        };
        for branch in merge {
            let (condition, positive) = branch.condition;
            for known in current_scopes {
                let (known_condition, known_positive) = known.condition;
                if condition != known_condition {
                    continue;
                }
                let taken = positive == known_positive;
                let bypass = Some(self.phi_block(phi, arg)) == branch.header;
                if bypass == taken {
                    return false;
                }
            }
        }
        true
    }
}


/// Visits the constant buffer words summed into a value, nearest first, looking through phis
fn for_each_summed_word(
    program: &Program,
    value: Value,
    mut func: impl FnMut(StorageBufferAddr, u32),
    conditions: Option<&TrackingConditions>,
) {
    let mut queue: VecDeque<(Value, u32, bool)> = VecDeque::from([(value, 0, true)]);
    let mut visited: [HashSet<InstRef>; 2] = [HashSet::new(), HashSet::new()];
    while let Some((term, depth, same_iteration)) = queue.pop_front() {
        let Some(inst_ref) = inst_of(program, term) else {
            continue;
        };
        if !visited[same_iteration as usize].insert(inst_ref) {
            continue;
        }
        let inst = inst_at(program, inst_ref);
        if let Some(word) = cbuf_word(program, inst) {
            func(word, depth);
            continue;
        }
        if matches!(inst.opcode, Opcode::IAdd32 | Opcode::Phi | Opcode::SelectU32) {
            let is_phi = inst.opcode == Opcode::Phi;
            let first = if inst.opcode == Opcode::SelectU32 { 1 } else { 0 };
            for arg in first..inst.num_args() {
                if is_phi
                    && same_iteration
                    && conditions.is_some_and(|conditions| !conditions.allows_phi(inst_ref, arg))
                {
                    continue;
                }
                let next_iteration = same_iteration
                    && !(is_phi && conditions.is_some_and(|conditions| conditions.is_back_edge(inst_ref, arg)));
                queue.push_back((*inst.arg(arg), depth + 1, next_iteration));
            }
        }
    }
}

struct WordSum {
    words: StorageBufferSet,
    count: u32,
    budget: u32,
    reaches_loop: bool,
}

impl Default for WordSum {
    fn default() -> Self {
        Self { words: StorageBufferSet::new(), count: 0, budget: 64, reaches_loop: false }
    }
}

/// Collects the constant buffer words of an addition tree and returns false when another value
/// feeds it. Reaching the phi given as the loop variable is recorded in the sum.
fn sum_words(program: &Program, value: Value, loop_phi: Option<InstRef>, sum: &mut WordSum) -> bool {
    let Some(inst_ref) = inst_of(program, value) else {
        return true;
    };
    if sum.budget == 0 {
        return false;
    }
    sum.budget -= 1;
    if Some(inst_ref) == loop_phi {
        sum.reaches_loop = true;
        return false;
    }
    let inst = inst_at(program, inst_ref);
    if let Some(word) = cbuf_word(program, inst) {
        sum.words.insert(word);
        sum.count += 1;
        return true;
    }
    if inst.opcode != Opcode::IAdd32 {
        return false;
    }
    let lhs = sum_words(program, inst.args[0], loop_phi, sum);
    if !lhs && loop_phi.is_none() {
        return false;
    }
    let rhs = sum_words(program, inst.args[1], loop_phi, sum);
    lhs && rhs
}

/// Finds the constant buffer words that step strided iterators (CUTLASS-style kernel parameters):
/// words added to a loop-carried value on its back edge, and words summed with two or more other
/// words into an offset table. A base pointer is neither.
fn collect_iterator_words(program: &Program) -> StorageBufferSet {
    let mut result = StorageBufferSet::new();
    for &block in &program.post_order_blocks {
        for (inst_index, inst) in program.block(block).indexed_iter() {
            let inst_ref = InstRef { block, inst: inst_index };
            if inst.opcode == Opcode::Phi {
                for arg in 0..inst.num_args() {
                    let mut sum = WordSum::default();
                    sum_words(program, *inst.arg(arg), Some(inst_ref), &mut sum);
                    if sum.reaches_loop {
                        result.extend(sum.words.iter().copied());
                    }
                }
            } else if inst.opcode == Opcode::IAdd32 {
                let mut sum = WordSum::default();
                if sum_words(program, Value::Inst(inst_ref), None, &mut sum) && sum.words.len() >= 3 {
                    result.extend(sum.words.iter().copied());
                }
            }
        }
    }
    // Strongly connected additions through phis and selects (Tarjan). Upstream recurses; this
    // walks the same edges in the same order with an explicit stack, which yields the same
    // components without bounding the search depth by the host stack.
    let successors = |inst_ref: InstRef| -> Vec<InstRef> {
        let inst = inst_at(program, inst_ref);
        match inst.opcode {
            Opcode::IAdd32 | Opcode::Phi | Opcode::SelectU32 => {
                let first = if inst.opcode == Opcode::SelectU32 { 1 } else { 0 };
                (first..inst.num_args())
                    .filter_map(|arg| inst_of(program, *inst.arg(arg)))
                    .collect()
            }
            _ => Vec::new(),
        }
    };
    let mut indices: HashMap<InstRef, u32> = HashMap::new();
    let mut lowlinks: HashMap<InstRef, u32> = HashMap::new();
    let mut on_stack: HashSet<InstRef> = HashSet::new();
    let mut stack: Vec<InstRef> = Vec::new();
    let mut next_index = 0u32;
    let mut visit = |root: InstRef, result: &mut StorageBufferSet| {
        if indices.contains_key(&root) {
            return;
        }
        // (instruction, its successors, next successor to visit)
        let mut frames: Vec<(InstRef, Vec<InstRef>, usize)> = Vec::new();
        let enter = |inst: InstRef,
                     indices: &mut HashMap<InstRef, u32>,
                     lowlinks: &mut HashMap<InstRef, u32>,
                     on_stack: &mut HashSet<InstRef>,
                     stack: &mut Vec<InstRef>,
                     next_index: &mut u32| {
            let index = *next_index;
            *next_index += 1;
            indices.insert(inst, index);
            lowlinks.insert(inst, index);
            stack.push(inst);
            on_stack.insert(inst);
        };
        enter(root, &mut indices, &mut lowlinks, &mut on_stack, &mut stack, &mut next_index);
        frames.push((root, successors(root), 0));
        while let Some(frame) = frames.last_mut() {
            let inst = frame.0;
            if frame.2 < frame.1.len() {
                let next = frame.1[frame.2];
                frame.2 += 1;
                if !indices.contains_key(&next) {
                    enter(next, &mut indices, &mut lowlinks, &mut on_stack, &mut stack, &mut next_index);
                    frames.push((next, successors(next), 0));
                } else if on_stack.contains(&next) {
                    let low = lowlinks[&inst].min(indices[&next]);
                    lowlinks.insert(inst, low);
                }
                continue;
            }
            frames.pop();
            if let Some(parent) = frames.last() {
                let low = lowlinks[&parent.0].min(lowlinks[&inst]);
                lowlinks.insert(parent.0, low);
            }
            if lowlinks[&inst] != indices[&inst] {
                continue;
            }
            let mut component = Vec::new();
            loop {
                let member = stack.pop().expect("Tarjan stack");
                on_stack.remove(&member);
                component.push(member);
                if member == inst {
                    break;
                }
            }
            if component.len() < 2 {
                continue;
            }
            let members: HashSet<InstRef> = component.iter().copied().collect();
            for &add in &component {
                let add_inst = inst_at(program, add);
                if add_inst.opcode != Opcode::IAdd32 {
                    continue;
                }
                for arg in 0..add_inst.num_args() {
                    let term = *add_inst.arg(arg);
                    if inst_of(program, term).is_some_and(|term| members.contains(&term)) {
                        continue;
                    }
                    for_each_summed_word(program, term, |word, _| { result.insert(word); }, None);
                }
            }
        }
    };
    for &block in &program.post_order_blocks {
        for (inst_index, inst) in program.block(block).indexed_iter() {
            if inst.opcode == Opcode::Phi {
                visit(InstRef { block, inst: inst_index }, &mut result);
            }
        }
    }
    result
}

/// Returns true when a condition reads a constant buffer word, as the carry out of an addition to
/// that word does
fn reads_cbuf_word(program: &Program, condition: Value, word: StorageBufferAddr) -> bool {
    let mut queue: VecDeque<Value> = VecDeque::from([condition]);
    let mut visited: HashSet<InstRef> = HashSet::new();
    while visited.len() < 64 {
        let Some(value) = queue.pop_front() else {
            break;
        };
        let Some(inst_ref) = inst_of(program, value) else {
            continue;
        };
        if !visited.insert(inst_ref) {
            continue;
        }
        let inst = inst_at(program, inst_ref);
        if cbuf_word(program, inst) == Some(word) {
            return true;
        }
        for arg in 0..inst.num_args() {
            queue.push_back(*inst.arg(arg));
        }
    }
    false
}

/// Finds the 64-bit constant buffer pointers a global atomic's address is based on: the whole
/// address apart from an immediate offset, or a 64-bit sum whose high half adds the pointer's high
/// word and the carry out of its low word. An atomic has no global fallback, so such a pointer is a
/// buffer even while it is unmapped. A 32-bit scalar added to a pointer carries nothing.
fn collect_atomic_pointers(program: &Program) -> StorageBufferSet {
    let mut result = StorageBufferSet::new();
    for &block in &program.post_order_blocks {
        for (_, inst) in program.block(block).indexed_iter() {
            if !is_global_memory_write(inst.opcode) || inst.return_type() == Type::Void {
                continue;
            }
            let Some(addr) = track_low_address(program, inst) else {
                continue;
            };
            let low = value_cbuf_word(program, addr.value);
            if let Some(low) = low {
                if is_aligned(low.offset, 8)
                    && value_cbuf_word(program, addr.high)
                        == Some(StorageBufferAddr::direct(low.index, low.offset.wrapping_add(4)))
                {
                    result.insert(low);
                    continue;
                }
            }
            let mut high_words = StorageBufferSet::new();
            let mut carries: Vec<Value> = Vec::new();
            let mut pending: Vec<Value> = vec![addr.high];
            let mut visited: HashSet<InstRef> = HashSet::new();
            while let Some(term) = pending.pop() {
                let Some(term_ref) = inst_of(program, term) else {
                    continue;
                };
                if !visited.insert(term_ref) {
                    continue;
                }
                let term_inst = inst_at(program, term_ref);
                if let Some(high_word) = cbuf_word(program, term_inst) {
                    high_words.insert(high_word);
                    continue;
                }
                match term_inst.opcode {
                    Opcode::IAdd32 | Opcode::Phi => {
                        for arg in 0..term_inst.num_args() {
                            pending.push(*term_inst.arg(arg));
                        }
                    }
                    Opcode::SelectU32 => {
                        if is_imm(program, term_inst.args[1]) && is_imm(program, term_inst.args[2]) {
                            carries.push(term_inst.args[0]);
                        }
                    }
                    _ => {}
                }
            }
            if carries.is_empty() {
                continue;
            }
            for_each_summed_word(
                program,
                addr.value,
                |pointer, _| {
                    if is_aligned(pointer.offset, 8)
                        && high_words.contains(&StorageBufferAddr::direct(
                            pointer.index,
                            pointer.offset.wrapping_add(4),
                        ))
                        && carries.iter().any(|&carry| reads_cbuf_word(program, carry, pointer))
                    {
                        result.insert(pointer);
                    }
                },
                None,
            );
        }
    }
    result
}

/// Finds the 64-bit constant buffer pointer summed into an address: its low word is summed into
/// the low half and its high word into the high half. Iterator steps and 32-bit scalars such as
/// strides and sizes are skipped. Competing bases require the legacy nearest-word fallback.
fn track_pointer_addend(
    program: &Program,
    addr: &LowAddrInfo,
    iterator_words: &StorageBufferSet,
    conditions: &TrackingConditions,
    legacy_nearest: bool,
) -> Option<StorageBufferAddr> {
    let mut high_words = StorageBufferSet::new();
    for_each_summed_word(program, addr.high, |word, _| { high_words.insert(word); }, Some(conditions));
    let mut result: Option<StorageBufferAddr> = None;
    let mut result_depth = 0u32;
    let mut tied = false;
    let mut candidates = StorageBufferSet::new();
    for_each_summed_word(
        program,
        addr.value,
        |word, depth| {
            if !is_aligned(word.offset, 8)
                || iterator_words.contains(&word)
                || !high_words.contains(&StorageBufferAddr::direct(word.index, word.offset.wrapping_add(4)))
            {
                return;
            }
            candidates.insert(word);
            if result.is_none() || depth < result_depth {
                result = Some(word);
                result_depth = depth;
                tied = false;
            } else if depth == result_depth && result != Some(word) {
                tied = true;
            }
        },
        Some(conditions),
    );
    if tied || (!legacy_nearest && candidates.len() != 1) { None } else { result }
}

/// The first slot and stride of a pointer array read at a dynamic constant buffer offset:
/// (index << shift) + base or index * stride + base.
fn dynamic_pointer_slots(program: &Program, offset: Value) -> Option<(u32, u32)> {
    let stride_of = |value: Value| -> Option<u32> {
        let inst = inst_at(program, inst_of(program, value)?);
        if inst.opcode == Opcode::ShiftLeftLogical32 && is_imm(program, inst.args[1]) {
            let shift = imm32(program, inst.args[1])?;
            if shift < 16 {
                return Some(1 << shift);
            }
        }
        if inst.opcode == Opcode::IMul32 && is_imm(program, inst.args[1]) {
            return imm32(program, inst.args[1]);
        }
        None
    };
    let mut base = 0u32;
    let mut stride = stride_of(offset);
    if stride.is_none() {
        if let Some(add_ref) = inst_of(program, offset) {
            let add = inst_at(program, add_ref);
            if add.opcode == Opcode::IAdd32 {
                for arg in 0..2 {
                    if stride.is_some() {
                        break;
                    }
                    if is_imm(program, add.args[arg ^ 1]) {
                        base = imm32(program, add.args[arg ^ 1])?;
                        stride = stride_of(add.args[arg]);
                    }
                }
            }
        }
    }
    match stride {
        Some(stride) if stride >= 8 && is_aligned(stride, 8) => Some((base, stride)),
        _ => None,
    }
}

/// Slots of a pointer array read at a dynamic offset, following phis and selects of the offset.
/// A stride of zero is a single slot.
fn collect_pointer_slots(
    program: &Program,
    offset: Value,
    slots: &mut Vec<(u32, u32)>,
    visited: &mut HashSet<InstRef>,
) {
    let Some(inst_ref) = inst_of(program, offset) else {
        if let Some(offset) = imm32(program, offset) {
            if !slots.contains(&(offset, 0)) {
                slots.push((offset, 0));
            }
        }
        return;
    };
    if !visited.insert(inst_ref) {
        return;
    }
    if let Some(pattern) = dynamic_pointer_slots(program, offset) {
        if !slots.contains(&pattern) {
            slots.push(pattern);
        }
        return;
    }
    let inst = inst_at(program, inst_ref);
    match inst.opcode {
        Opcode::Phi => {
            for arg in 0..inst.num_args() {
                collect_pointer_slots(program, *inst.arg(arg), slots, visited);
            }
        }
        Opcode::SelectU32 => {
            collect_pointer_slots(program, inst.args[1], slots, visited);
            collect_pointer_slots(program, inst.args[2], slots, visited);
        }
        _ => {}
    }
}

/// Maximum slots of a dynamically indexed pointer array (upstream `max_slots`).
const MAX_POINTER_SLOTS: u32 = 16;

/// Native programs: every constant buffer pointer the low address can come from, following only
/// additions, selects and phis. A predicated address assignment becomes a phi whose other input
/// can point into a different buffer, so the search does not stop at the first pointer. A pointer
/// read from an array at a dynamic offset contributes every mapped slot of the array.
fn native_pointer_candidates(
    program: &Program,
    low_addr: Value,
    env: &mut dyn Environment,
    dynamic: &mut bool,
    info: &mut StorageInfo,
) -> Vec<StorageBufferAddr> {
    let mut candidates: Vec<StorageBufferAddr> = Vec::new();
    let Some(start) = inst_of(program, low_addr) else {
        return candidates;
    };
    let mut pending: Vec<InstRef> = vec![start];
    let mut visited: HashSet<InstRef> = HashSet::from([start]);
    let enqueue = |value: Value, pending: &mut Vec<InstRef>, visited: &mut HashSet<InstRef>| {
        if let Some(inst) = inst_of(program, value) {
            if visited.insert(inst) {
                pending.push(inst);
            }
        }
    };
    let mut next = 0;
    while next < pending.len() {
        let inst_ref = pending[next];
        next += 1;
        let inst = inst_at(program, inst_ref);
        match inst.opcode {
            Opcode::GetCbufU32 | Opcode::GetCbufU32x2 => {
                let index = inst.args[0];
                let offset = inst.args[1];
                if is_imm(program, index) && !is_imm(program, offset) {
                    let Some(index) = imm32(program, index) else { continue };
                    let mut slots = Vec::new();
                    let mut slot_visited = HashSet::new();
                    collect_pointer_slots(program, offset, &mut slots, &mut slot_visited);
                    for (first, stride) in slots {
                        let count = if stride != 0 { MAX_POINTER_SLOTS } else { 1 };
                        for slot in 0..count {
                            let pointer = StorageBufferAddr::direct(
                                index,
                                first.wrapping_add(slot.wrapping_mul(stride)),
                            );
                            if is_aligned(pointer.offset, 8)
                                && !candidates.contains(&pointer)
                                && env.is_cbuf_storage_buffer_pointer(pointer.index, pointer.offset)
                            {
                                candidates.push(pointer);
                                *dynamic = true;
                            }
                        }
                    }
                    continue;
                }
                let (Some(index), Some(offset)) = (imm32(program, index), imm32(program, offset)) else {
                    continue;
                };
                if !is_imm(program, inst.args[0]) || !is_imm(program, inst.args[1]) {
                    continue;
                }
                let pointer = StorageBufferAddr::direct(index, offset);
                if is_aligned(pointer.offset, 8)
                    && !candidates.contains(&pointer)
                    && env.is_cbuf_storage_buffer_pointer(pointer.index, pointer.offset)
                {
                    candidates.push(pointer);
                }
            }
            Opcode::IAdd32 | Opcode::Phi => {
                for arg in (0..inst.num_args()).rev() {
                    enqueue(*inst.arg(arg), &mut pending, &mut visited);
                }
            }
            Opcode::SelectU32 => {
                enqueue(inst.args[1], &mut pending, &mut visited);
                enqueue(inst.args[2], &mut pending, &mut visited);
            }
            Opcode::ShuffleIndex => {
                // A pointer made uniform (R2UR broadcasts the first active lane's value).
                enqueue(inst.args[0], &mut pending, &mut visited);
            }
            Opcode::CompositeExtractU32x2 => {
                if is_imm(program, inst.args[1]) && imm32(program, inst.args[1]) == Some(0) {
                    enqueue(inst.args[0], &mut pending, &mut visited);
                }
            }
            Opcode::LoadGlobal64 => {
                native_loaded_pointer_candidates(program, inst, env, dynamic, info, &mut candidates);
            }
            _ => {}
        }
    }
    candidates
}

/// The `LoadGlobal64` case of upstream `NativePointerCandidates`: a pointer loaded from the
/// memory a constant buffer pointer points to (the binding reads it again for every dispatch and
/// draw). The table address is a tracked low word, or a constant buffer pointer packed whole.
fn native_loaded_pointer_candidates(
    program: &Program,
    inst: &Inst,
    env: &mut dyn Environment,
    dynamic: &mut bool,
    info: &mut StorageInfo,
    candidates: &mut Vec<StorageBufferAddr>,
) {
    let holder_addr = track_low_address(program, inst);
    let holder_root: InstRef;
    let holder_offset: i32;
    if let Some(holder_addr) = holder_addr {
        let Some(root) = inst_of(program, holder_addr.value) else {
            return;
        };
        holder_root = root;
        holder_offset = holder_addr.imm_offset;
    } else {
        let mut address = inst.args[0];
        let mut offset = 0i32;
        if let Some(address_ref) = inst_of(program, address) {
            let address_inst = inst_at(program, address_ref);
            if address_inst.opcode == Opcode::IAdd64 && is_imm(program, address_inst.args[1]) {
                let Some(value) = imm64(program, address_inst.args[1]) else { return };
                offset = value as i64 as i32;
                address = address_inst.args[0];
            }
        }
        let Some(address_ref) = inst_of(program, address) else {
            return;
        };
        let address_inst = inst_at(program, address_ref);
        if address_inst.opcode != Opcode::PackUint2x32 {
            return;
        }
        let Some(root) = inst_of(program, address_inst.args[0]) else {
            return;
        };
        holder_root = root;
        holder_offset = offset;
    }
    if holder_offset < 0 || !is_aligned(holder_offset as u32, 8) {
        return;
    }
    let indirect = holder_offset as u32;
    let stage = env.shader_stage();
    // The table pointer can also be one of several constant buffer pointers (a phi or select of
    // them), or an entry of a constant buffer array indexed at run time (one table per object
    // type: every slot whose table entry is a pointer). Those are table candidates, bound only in
    // the bindings the other storage buffers leave free.
    let mut add = |pointer: StorageBufferAddr, table: bool, info: &mut StorageInfo, candidates: &mut Vec<StorageBufferAddr>| {
        if candidates.contains(&pointer) {
            return;
        }
        if table && stage != ShaderStage::Compute {
            // Graphics programs bind every candidate for every draw: table entries multiply the
            // storage buffers of draws that never read them.
            return;
        }
        if table {
            info.tables_seen = true;
            if !info.tables_enabled {
                return;
            }
            if !info.table_candidates.contains(&pointer) {
                info.table_candidates.push(pointer);
            }
        }
        candidates.push(pointer);
        *dynamic = true;
    };
    let mut holders: Vec<(InstRef, bool)> = vec![(holder_root, false)];
    let mut holder_visited: HashSet<InstRef> = HashSet::from([holder_root]);
    let mut pending_holder = 0;
    while pending_holder < holders.len() {
        let (holder_ref, via_phi) = holders[pending_holder];
        pending_holder += 1;
        let holder = inst_at(program, holder_ref);
        let mut enqueue_holder = |value: Value, branch: bool| {
            if let Some(value) = inst_of(program, value) {
                if holders.len() < 8 && holder_visited.insert(value) {
                    holders.push((value, via_phi || branch));
                }
            }
        };
        match holder.opcode {
            Opcode::CompositeExtractU32x2 => {
                if is_imm(program, holder.args[1]) && imm32(program, holder.args[1]) == Some(0) {
                    enqueue_holder(holder.args[0], false);
                }
                continue;
            }
            Opcode::Phi => {
                for arg in 0..holder.num_args() {
                    enqueue_holder(*holder.arg(arg), true);
                }
                continue;
            }
            Opcode::SelectU32 => {
                enqueue_holder(holder.args[1], true);
                enqueue_holder(holder.args[2], true);
                continue;
            }
            Opcode::GetCbufU32 | Opcode::GetCbufU32x2 => {}
            _ => continue,
        }
        if !is_imm(program, holder.args[0]) {
            continue;
        }
        let Some(index) = imm32(program, holder.args[0]) else { continue };
        if is_imm(program, holder.args[1]) {
            let Some(offset) = imm32(program, holder.args[1]) else { continue };
            let pointer = StorageBufferAddr { index, offset, indirect };
            if is_aligned(pointer.offset, 8)
                && env.is_cbuf_storage_buffer_pointer(pointer.index, pointer.offset)
            {
                add(pointer, via_phi || holder_addr.is_none(), info, candidates);
            }
            continue;
        }
        let mut slots = Vec::new();
        let mut slot_visited = HashSet::new();
        collect_pointer_slots(program, holder.args[1], &mut slots, &mut slot_visited);
        for (first, stride) in slots {
            let count = if stride != 0 { MAX_POINTER_SLOTS } else { 1 };
            for slot in 0..count {
                let pointer = StorageBufferAddr {
                    index,
                    offset: first.wrapping_add(slot.wrapping_mul(stride)),
                    indirect,
                };
                if is_aligned(pointer.offset, 8)
                    && env.is_cbuf_indirect_storage_buffer_pointer(
                        pointer.index,
                        pointer.offset,
                        pointer.indirect,
                    )
                {
                    add(pointer, true, info, candidates);
                }
            }
        }
    }
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


/// Upstream `cached_track` in `CollectStorageBuffers`: accesses through the same address share
/// one search.
fn cached_track(
    program: &Program,
    low_addr: Value,
    bias: Option<Bias>,
    cache: &mut HashMap<InstRef, Option<StorageBufferAddr>>,
    exhausted: &mut HashSet<InstRef>,
) -> Option<StorageBufferAddr> {
    let Some(start) = inst_of(program, low_addr) else {
        return track(program, low_addr, bias, exhausted);
    };
    if let Some(result) = cache.get(&start) {
        return *result;
    }
    let result = track(program, low_addr, bias, exhausted);
    cache.insert(start, result);
    result
}

/// Collects the storage buffer used by a global memory instruction and the instruction itself.
/// Returns false when the access stays global without a buffer.
fn collect_storage_buffers(
    program: &Program,
    inst_ref: InstRef,
    info: &mut StorageInfo,
    mut env: Option<&mut (dyn Environment + '_)>,
    conditions: &TrackingConditions,
) -> bool {
    // NVN puts storage buffers in a specific range, we have to bias towards these addresses to
    // avoid getting false positives
    const NVN_BIAS: Bias = Bias {
        index: 0,
        offset_begin: 0x100,
        offset_end: 0x700,
        alignment: 16,
    };
    let inst = inst_at(program, inst_ref);
    // Track the low address of the instruction
    let Some(low_addr_info) = track_low_address(program, inst) else {
        // Failed to track the low address, use NVN fallbacks
        return false;
    };
    // First try to find storage buffers in the NVN address
    let low_addr = low_addr_info.value;
    let paired_pointer =
        track_pointer_addend(program, &low_addr_info, &info.iterator_words, conditions, false);
    let mut storage_buffer = paired_pointer;
    let native = env.as_deref().is_some_and(|env| env.has_native_instructions());
    if native {
        if let (Some(buffer), Some(env)) = (storage_buffer, env.as_deref_mut()) {
            if !env.is_cbuf_storage_buffer_pointer(buffer.index, buffer.offset) {
                storage_buffer = None;
            }
        }
    }
    // A paired constant buffer pointer can be only one input of a select or phi whose other input
    // is a pointer loaded from that buffer (Ocarina of Time's foliage unit placement records are
    // read from c0[12] or from *c0[12]). Rebasing on the constant buffer pointer then reads the
    // loaded path's records out of bounds, as zeros, so look for loaded pointers here as well.
    let paired_found = storage_buffer.is_some();
    if native {
        if let (Some(start), Some(env)) = (inst_of(program, low_addr), env.as_deref_mut()) {
            if !info.native_candidates.contains_key(&start) {
                let mut dynamic = false;
                let candidates = native_pointer_candidates(program, low_addr, env, &mut dynamic, info);
                info.native_candidates.insert(start, candidates);
                if dynamic {
                    info.native_dynamic.insert(start);
                }
            }
            let candidates = info.native_candidates[&start].clone();
            let has_loaded_pointer = candidates.iter().any(|candidate| candidate.indirect != DIRECT);
            if paired_found
                && (!has_loaded_pointer || !info.bind_competing_loaded || !is_runtime_routable(inst.opcode))
            {
                // No pointer loaded from memory competes with the paired one, or the access cannot
                // stay global: keep the pair.
            } else if candidates.len() > 1
                || paired_found
                || (!candidates.is_empty() && info.native_dynamic.contains(&start))
            {
                // Bind every candidate and keep the access global; the backend picks the buffer at
                // run time. Rebasing on one of them drops the other path's accesses.
                let is_write = is_global_memory_write(inst.opcode);
                let mut accepted = false;
                if let Some(paired) = storage_buffer {
                    // The paired pointer is the other input of the select or phi; bind it too.
                    info.competing_loaded_bound = true;
                    info.set.insert(paired);
                    if is_write {
                        info.writes.insert(paired);
                    }
                }
                for candidate in &candidates {
                    if info.table_candidates.contains(candidate) {
                        let holder = StorageBufferAddr::direct(candidate.index, candidate.offset);
                        let takes_binding = |buffer: &StorageBufferAddr| {
                            !info.set.contains(buffer) && !info.counted.contains(buffer)
                        };
                        let added = usize::from(takes_binding(candidate))
                            + usize::from(candidate.indirect != DIRECT && takes_binding(&holder));
                        if info.table_used + added > info.table_budget {
                            continue;
                        }
                        info.table_used += added;
                    }
                    info.set.insert(*candidate);
                    if is_write {
                        info.writes.insert(*candidate);
                        info.unbounded_writes.insert(*candidate);
                    }
                    if candidate.indirect != DIRECT {
                        // The backend reads the pointer through the buffer that holds it.
                        info.set.insert(StorageBufferAddr::direct(candidate.index, candidate.offset));
                    }
                    accepted = true;
                }
                if accepted {
                    info.native_runtime_bounds = true;
                    return true;
                }
                // Only table entries, and no free binding for them: as if none had been found.
            } else if candidates.len() == 1 && !paired_found {
                // The one pointer the address is built from. The NVN search below only takes
                // 16-byte aligned slots, so for a pointer at an 8-byte slot it can return a
                // dimension or stride the address arithmetic reads instead.
                storage_buffer = Some(candidates[0]);
            }
        }
    }
    if native && storage_buffer.is_none() {
        // A pointer that is an atomic's address keeps its buffer while unmapped, for the atomic
        // and every access based on it. The binding reads the pointer again for every dispatch.
        match paired_pointer {
            Some(pointer) if info.atomic_pointers.contains(&pointer) => storage_buffer = Some(pointer),
            _ => return false,
        }
    }
    if storage_buffer.is_none() {
        storage_buffer =
            track_pointer_addend(program, &low_addr_info, &info.iterator_words, conditions, true);
    }
    if storage_buffer.is_none() {
        storage_buffer = track_addend(program, low_addr);
    }
    if storage_buffer.is_none() {
        storage_buffer = cached_track(
            program,
            low_addr,
            Some(NVN_BIAS),
            &mut info.biased_tracks,
            &mut info.biased_exhausted,
        );
    }
    let storage_buffer = match storage_buffer {
        Some(storage_buffer) => storage_buffer,
        None => {
            // If it fails, track without a bias
            match cached_track(
                program,
                low_addr,
                None,
                &mut info.unbiased_tracks,
                &mut info.unbiased_exhausted,
            ) {
                Some(storage_buffer) => {
                    log::debug!(
                        "Storage buffer tracked without bias, index {} offset {}",
                        storage_buffer.index,
                        storage_buffer.offset
                    );
                    storage_buffer
                }
                None => {
                    // If that also fails, use NVN fallbacks
                    log::warn!("Storage buffer failed to track, using global memory fallbacks");
                    return false;
                }
            }
        }
    };
    // Collect storage buffer and the instruction
    if is_global_memory_write(inst.opcode) {
        info.writes.insert(storage_buffer);
    }
    info.set.insert(storage_buffer);
    info.to_replace.push(StorageInst { storage_buffer, inst: inst_ref });
    true
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
) -> Value {
    let inst = program.block(inst_ref.block).inst(inst_ref.inst).clone();
    let mut offset = match track_low_address(program, &inst) {
        Some(low_addr) => {
            let mut offset = low_addr.value;
            if low_addr.imm_offset != 0 {
                offset = insert_before(
                    program,
                    inst_ref,
                    Opcode::IAdd32,
                    vec![offset, Value::ImmU32(low_addr.imm_offset as u32)],
                );
            }
            offset
        }
        None => insert_before(program, inst_ref, Opcode::ConvertU32U64, vec![inst.args[0]]),
    };
    // Subtract the least significant 32 bits from the guest offset. The result is the storage
    // buffer offset in bytes.
    let low_cbuf = insert_before(
        program,
        inst_ref,
        Opcode::GetCbufU32,
        vec![
            Value::ImmU32(storage_buffer.index),
            Value::ImmU32(storage_buffer.offset),
        ],
    );
    // Align the offset base to match the host alignment requirements
    let alignment = alignment.max(1);
    let aligned_low_cbuf = insert_before(
        program,
        inst_ref,
        Opcode::BitwiseAnd32,
        vec![low_cbuf, Value::ImmU32(!(alignment - 1))],
    );
    offset = insert_before(program, inst_ref, Opcode::ISub32, vec![offset, aligned_low_cbuf]);
    offset
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

/// Upstream `collect` lambda of `GlobalMemoryToStorageBufferPass`.
fn collect(
    program: &Program,
    info: &mut StorageInfo,
    mut env: Option<&mut (dyn Environment + '_)>,
    conditions: &mut TrackingConditions,
) {
    for &block in &program.post_order_blocks {
        let insts: Vec<u32> = program
            .block(block)
            .indexed_iter()
            .filter_map(|(inst, data)| is_global_memory(data.opcode).then_some(inst))
            .collect();
        for inst in insts {
            conditions.current = Some(block);
            let inst_ref = InstRef { block, inst };
            let bound = collect_storage_buffers(program, inst_ref, info, env.as_deref_mut(), conditions);
            info.untracked_writes |= !bound && is_global_memory_write(inst_at(program, inst_ref).opcode);
        }
    }
}

/// Convert global memory instructions to storage buffer instructions.
pub fn global_memory_to_storage_buffer_pass(
    mut env: Option<&mut (dyn Environment + '_)>,
    program: &mut Program,
    host_info: &HostTranslateInfo,
) {
    let has_global_memory = program.post_order_blocks.iter().any(|&block| {
        program.block(block).iter().any(|inst| is_global_memory(inst.opcode))
    });
    if !has_global_memory {
        return;
    }
    let max_ssbos = storage_buffer_limit(host_info);
    let (info, extents) = {
        let program: &Program = program;
        let mut conditions = TrackingConditions::new(program);
        let mut info = StorageInfo::default();
        // Without 64-bit integers the backend cannot route global accesses among bound buffers.
        info.bind_competing_loaded = host_info.support_int64;
        info.iterator_words = collect_iterator_words(program);
        info.atomic_pointers = collect_atomic_pointers(program);
        collect(program, &mut info, env.as_deref_mut(), &mut conditions);
        if info.competing_loaded_bound && info.set.len() > max_ssbos {
            // Not enough bindings for the loaded pointers that compete with paired ones: keep the
            // pairs, as if no loaded pointer competed.
            let mut paired = StorageInfo::default();
            paired.iterator_words = std::mem::take(&mut info.iterator_words);
            paired.atomic_pointers = std::mem::take(&mut info.atomic_pointers);
            paired.bind_competing_loaded = false;
            info = paired;
            collect(program, &mut info, env.as_deref_mut(), &mut conditions);
        }
        if info.tables_seen && info.set.len() < max_ssbos {
            // Again with pointer table entries, in the bindings the first pass left free.
            let mut with_tables = StorageInfo::default();
            with_tables.iterator_words = info.iterator_words.clone();
            with_tables.atomic_pointers = info.atomic_pointers.clone();
            with_tables.bind_competing_loaded = info.bind_competing_loaded;
            with_tables.tables_enabled = true;
            with_tables.table_budget = max_ssbos - info.set.len();
            with_tables.counted = info.set.clone();
            std::mem::swap(&mut info, &mut with_tables);
            collect(program, &mut info, env.as_deref_mut(), &mut conditions);
            if info.set.len() > max_ssbos {
                // Never more storage buffers than a stage binds: without the table entries, then.
                info = with_tables;
            }
        }
        if info.set.len() > max_ssbos {
            // Binding only some of them would drop the accesses through the others.
            std::panic::panic_any(crate::exception::NotImplementedException::new(format!(
                "Binding {} storage buffers to a stage that binds {}",
                info.set.len(),
                max_ssbos
            )));
        }
        // Store extents, before the accesses are rebased (the analysis reads the guest addresses).
        // None while a store stays global: the backend routes it to the descriptor whose pointer is
        // the closest one at or below the address, which need not be one of the store's candidates.
        let mut extents: BTreeMap<StorageBufferAddr, StorageWriteExtent> = BTreeMap::new();
        if !info.untracked_writes && info.unbounded_writes.is_empty() {
            let mut analyses: BTreeMap<StorageBufferAddr, StoreExtentAnalysis> = BTreeMap::new();
            for storage_inst in &info.to_replace {
                let inst = inst_at(program, storage_inst.inst);
                if !is_global_memory_write(inst.opcode)
                    || info.unbounded_writes.contains(&storage_inst.storage_buffer)
                {
                    continue;
                }
                let analysis = analyses
                    .entry(storage_inst.storage_buffer)
                    .or_insert_with(|| StoreExtentAnalysis::new(program, storage_inst.storage_buffer));
                // An empty bounded extent, widened by every store
                let extent = extents
                    .entry(storage_inst.storage_buffer)
                    .or_insert_with(|| StorageWriteExtent { bounded: true, ..Default::default() });
                analysis.add_store(inst, extent);
            }
        }
        (info, extents)
    };
    program.info.native_global_bounds |= info.native_runtime_bounds;
    for storage_buffer in &info.set {
        let is_written = info.writes.contains(storage_buffer);
        let extent = extents.get(storage_buffer);
        program.info.storage_buffers_descriptors.push(StorageBufferDescriptor {
            cbuf_index: storage_buffer.index,
            cbuf_offset: storage_buffer.offset,
            count: 1,
            is_written,
            indirect_offset: storage_buffer.indirect,
            write_extent: match extent {
                Some(extent) if is_written => *extent,
                _ => StorageWriteExtent::default(),
            },
            ..Default::default()
        });
    }
    for storage_inst in &info.to_replace {
        let storage_buffer = storage_inst.storage_buffer;
        let index = info
            .set
            .iter()
            .position(|buffer| *buffer == storage_buffer)
            .expect("replaced storage buffer is in the set") as u32;
        let offset = storage_offset(
            program,
            storage_inst.inst,
            storage_buffer,
            host_info.min_ssbo_alignment as u32,
        );
        replace(program, *storage_inst, index, offset);
    }
}

/// Storage buffers a stage can bind: the descriptor limit of the device, at most `MAX_SSBOS`.
///
/// Upstream: `StorageBufferLimit`. `HostTranslateInfo` has no per-stage storage buffer limit in
/// this port; only the descriptor set limit is applied.
pub fn storage_buffer_limit(host_info: &HostTranslateInfo) -> usize {
    let mut limit = crate::shader_info::Info::MAX_SSBOS;
    for device_limit in [host_info.max_descriptor_set_storage_buffers] {
        if device_limit != 0 {
            limit = limit.min(device_limit as usize);
        }
    }
    limit
}

/// Join storage buffer descriptors from `source` into `base`.
///
/// Upstream: `JoinStorageInfo` in `global_memory_to_storage_buffer_pass.cpp`.
pub fn join_storage_info(base: &mut ShaderInfo, source: &mut ShaderInfo) -> Vec<u32> {
    base.native_global_bounds |= source.native_global_bounds;
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
            global_memory_to_storage_buffer_pass(None, &mut p, &HostTranslateInfo::default());
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
        let mut analysis = StoreExtentAnalysis::new(&p, StorageBufferAddr::direct(0, 0x100));
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
        let mut analysis = StoreExtentAnalysis::new(&p, StorageBufferAddr::direct(0, 0x100));
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
            track(&program, address, None, &mut HashSet::new()),
            Some(StorageBufferAddr::direct(0, 80))
        );
        // The same operands in the other order still follow upstream's order.
        let swapped = append(&mut program, Opcode::IAdd32, vec![pointer, scaled]);
        assert_eq!(
            track(&program, swapped, None, &mut HashSet::new()),
            Some(StorageBufferAddr::direct(1, 8))
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
            global_memory_to_storage_buffer_pass(None, &mut program,
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

        global_memory_to_storage_buffer_pass(None, &mut program,
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

        global_memory_to_storage_buffer_pass(None, &mut program,
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

        global_memory_to_storage_buffer_pass(None, &mut program,
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

    /// Native program environment: the pointer slots listed in `pointers` are mapped.
    struct NativeEnvironment {
        texture_pass_caches: crate::environment::TexturePassCaches,
        sph: crate::program_header::ProgramHeader,
        pointers: Vec<(u32, u32)>,
    }

    impl Environment for NativeEnvironment {
        fn is_cbuf_storage_buffer_pointer(&mut self, index: u32, offset: u32) -> bool {
            self.pointers.contains(&(index, offset))
        }
        fn has_native_instructions(&self) -> bool { true }
        fn texture_pass_caches(&mut self) -> &mut crate::environment::TexturePassCaches {
            &mut self.texture_pass_caches
        }
        fn read_instruction(&mut self, _address: u32) -> u64 { 0 }
        fn read_cbuf_value(&mut self, _cbuf_index: u32, _cbuf_offset: u32) -> u32 { 0 }
        fn read_texture_type(&mut self, _raw_handle: u32) -> crate::shader_info::TextureType {
            crate::shader_info::TextureType::Color2D
        }
        fn read_texture_pixel_format(&mut self, _raw_handle: u32) -> crate::shader_info::TexturePixelFormat {
            crate::shader_info::TexturePixelFormat::A8B8G8R8Unorm
        }
        fn is_texture_pixel_format_integer(&mut self, _raw_handle: u32) -> bool { false }
        fn read_viewport_transform_state(&mut self) -> u32 { 0 }
        fn texture_bound_buffer(&self) -> u32 { 0 }
        fn local_memory_size(&self) -> u32 { 0 }
        fn shared_memory_size(&self) -> u32 { 0 }
        fn workgroup_size(&self) -> [u32; 3] { [128, 1, 1] }
        fn has_hle_macro_state(&self) -> bool { false }
        fn get_replace_const_buffer(&mut self, _bank: u32, _offset: u32) -> Option<crate::shader_info::ReplaceConstant> {
            None
        }
        fn dump(&mut self, _pipeline_hash: u64, _shader_hash: u64) {}
        fn sph(&self) -> &crate::program_header::ProgramHeader { &self.sph }
        fn gp_passthrough_mask(&self) -> &[u32; 8] {
            static MASK: [u32; 8] = [0; 8];
            &MASK
        }
        fn shader_stage(&self) -> ShaderStage { ShaderStage::Compute }
        fn start_address(&self) -> u32 { 0 }
        fn is_proprietary_driver(&self) -> bool { true }
    }

    /// Ocarina of Time's Kokiri Forest append kernels: an atomic whose pointer is read from a
    /// constant buffer array at a run-time index (`c4[index * 8 + 0xe0]`). Lichii binds every
    /// mapped slot and leaves the atomic global for the backend to route; it was rejected with
    /// "Global atomic address has no registered storage ranges".
    #[test]
    fn native_dynamic_pointer_array_binds_every_mapped_slot_and_routes_at_run_time() {
        let mut program = Program::new(ShaderStage::Compute);
        program.blocks.push(Block::new());
        program.post_order_blocks.push(0);
        let index = extent_inst(&mut program, Opcode::GetCbufU32, vec![Value::ImmU32(0), Value::ImmU32(0x10)]);
        let scaled = extent_inst(&mut program, Opcode::ShiftLeftLogical32, vec![index, Value::ImmU32(3)]);
        let offset = extent_inst(&mut program, Opcode::IAdd32, vec![scaled, Value::ImmU32(0xe0)]);
        let pointer = extent_inst(&mut program, Opcode::GetCbufU32x2, vec![Value::ImmU32(4), offset]);
        let low = extent_inst(&mut program, Opcode::CompositeExtractU32x2, vec![pointer, Value::ImmU32(0)]);
        let high = extent_inst(&mut program, Opcode::CompositeExtractU32x2, vec![pointer, Value::ImmU32(1)]);
        let vector = extent_inst(&mut program, Opcode::CompositeConstructU32x2, vec![low, high]);
        let address = extent_inst(&mut program, Opcode::PackUint2x32, vec![vector]);
        extent_inst(&mut program, Opcode::GlobalAtomicIAdd32, vec![address, Value::ImmU32(1)]);
        let mut env = NativeEnvironment {
            texture_pass_caches: Default::default(),
            sph: Default::default(),
            pointers: vec![(4, 0xe0), (4, 0xe8), (4, 0xf8)],
        };
        let host = HostTranslateInfo { support_int64: true, ..Default::default() };
        global_memory_to_storage_buffer_pass(Some(&mut env), &mut program, &host);
        assert!(program.info.native_global_bounds);
        let bound: Vec<_> = program.info.storage_buffers_descriptors.iter()
            .map(|desc| (desc.cbuf_index, desc.cbuf_offset, desc.indirect_offset, desc.is_written))
            .collect();
        assert_eq!(bound, [(4, 0xe0, u32::MAX, true), (4, 0xe8, u32::MAX, true), (4, 0xf8, u32::MAX, true)]);
        // The access stays global, routed among the bound buffers at run time.
        assert!(program.block(0).iter().any(|inst| inst.opcode == Opcode::GlobalAtomicIAdd32));
        assert!(!program.block(0).iter().any(|inst| inst.opcode == Opcode::StorageAtomicIAdd32));
        // Written through a run-time route: no narrowed write extent.
        assert!(program.info.storage_buffers_descriptors.iter().all(|desc| !desc.write_extent.bounded));
    }

    /// A pointer loaded from the memory a constant buffer pointer points to is bound through
    /// its holder (indirect descriptor plus the holder itself).
    #[test]
    fn native_loaded_pointer_binds_indirect_descriptor_and_its_holder() {
        let mut program = Program::new(ShaderStage::Compute);
        program.blocks.push(Block::new());
        program.post_order_blocks.push(0);
        let table = extent_inst(&mut program, Opcode::GetCbufU32x2, vec![Value::ImmU32(0), Value::ImmU32(0x60)]);
        let table_low = extent_inst(&mut program, Opcode::CompositeExtractU32x2, vec![table, Value::ImmU32(0)]);
        let table_high = extent_inst(&mut program, Opcode::CompositeExtractU32x2, vec![table, Value::ImmU32(1)]);
        let table_vector = extent_inst(&mut program, Opcode::CompositeConstructU32x2, vec![table_low, table_high]);
        let table_address = extent_inst(&mut program, Opcode::PackUint2x32, vec![table_vector]);
        let entry_address = extent_inst(&mut program, Opcode::IAdd64, vec![table_address, Value::ImmU64(0x18)]);
        let loaded = extent_inst(&mut program, Opcode::LoadGlobal64, vec![entry_address]);
        let low = extent_inst(&mut program, Opcode::CompositeExtractU32x2, vec![loaded, Value::ImmU32(0)]);
        let high = extent_inst(&mut program, Opcode::CompositeExtractU32x2, vec![loaded, Value::ImmU32(1)]);
        let vector = extent_inst(&mut program, Opcode::CompositeConstructU32x2, vec![low, high]);
        let address = extent_inst(&mut program, Opcode::PackUint2x32, vec![vector]);
        extent_inst(&mut program, Opcode::LoadGlobal32, vec![address]);
        let mut env = NativeEnvironment {
            texture_pass_caches: Default::default(),
            sph: Default::default(),
            pointers: vec![(0, 0x60)],
        };
        let host = HostTranslateInfo { support_int64: true, ..Default::default() };
        global_memory_to_storage_buffer_pass(Some(&mut env), &mut program, &host);
        let bound: Vec<_> = program.info.storage_buffers_descriptors.iter()
            .map(|desc| (desc.cbuf_index, desc.cbuf_offset, desc.indirect_offset))
            .collect();
        assert!(bound.contains(&(0, 0x60, 0x18)));
        assert!(bound.contains(&(0, 0x60, u32::MAX)));
        assert!(program.info.native_global_bounds);
    }

    #[test]
    fn pruned_search_skips_exhausted_instructions_and_stops_at_loaded_pointers() {
        let mut program = Program::new(ShaderStage::Compute);
        program.blocks.push(Block::new());
        let pointer = extent_inst(&mut program, Opcode::GetCbufU32, vec![Value::ImmU32(0), Value::ImmU32(0x100)]);
        let vector = extent_inst(&mut program, Opcode::CompositeConstructU32x2, vec![pointer, Value::ImmU32(0)]);
        let address = extent_inst(&mut program, Opcode::PackUint2x32, vec![vector]);
        let loaded = extent_inst(&mut program, Opcode::LoadGlobal32, vec![address]);
        // A pointer loaded from memory is not an offset into the buffer that supplied it.
        assert_eq!(track(&program, loaded, None, &mut HashSet::new()), None);
        let unrelated = extent_inst(&mut program, Opcode::IAdd32, vec![Value::ImmU32(1), Value::ImmU32(2)]);
        let mut exhausted = HashSet::new();
        assert_eq!(track(&program, unrelated, None, &mut exhausted), None);
        assert!(!exhausted.is_empty());
        assert_eq!(track(&program, pointer, None, &mut HashSet::new()), Some(StorageBufferAddr::direct(0, 0x100)));
    }
}

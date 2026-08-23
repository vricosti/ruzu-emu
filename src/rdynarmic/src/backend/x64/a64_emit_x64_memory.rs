//! Port of upstream `dynarmic/src/dynarmic/backend/x64/a64_emit_x64_memory.cpp`
//! (the A64-specific glue around the shared memory-emit templates in
//! `emit_x64_memory.cpp.inc`).
//!
//! Currently scoped to:
//!
//! - `gen_fastmem_fallbacks`: pre-generate the per-(ordered, bitsize,
//!   vaddr_idx, value_idx) fallback stubs that the SIGSEGV handler
//!   redirects to when a fastmem mov faults. Mirrors upstream
//!   `A64EmitX64::GenFastmemFallbacks` (a64_emit_x64_memory.cpp:113-280).
//!
//! The fallback table includes scalar and 128-bit exclusive paths. The
//! architecture-specific exclusive emitters remain in
//! `emit_exclusive_memory.rs`.

use std::collections::HashMap;

#[cfg(target_os = "windows")]
use rxbyak::xmmword_ptr;
use rxbyak::{
    dword_ptr, qword_ptr, CodeAssembler, JmpType, Label, Reg, RegExp, EDX, R13, R15, RAX, RBX, RCX,
    RDX, RSP,
};

use crate::backend::x64::abi;
use crate::backend::x64::block_of_code::FORCE_RETURN;
use crate::backend::x64::emit_context::{
    DeferredEmitCtx, EmitCallbacks, EmitContext, RawExclusiveWriteCallbacks,
};
use crate::backend::x64::emit_terminal::emit_jmp_to_offset;
use crate::backend::x64::emit_x64_memory::{
    emit_fastmem_vaddr_a64, emit_read_memory_mov, emit_vaddr_lookup_a64, emit_write_memory_mov,
    is_ordered,
};
use crate::backend::x64::exception_handler::{DoNotFastmemMarker, FastmemPatchInfo};
use crate::backend::x64::hostloc::HostLoc;
use crate::backend::x64::jit_state::A64JitState;
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::backend::x64::value_classify::{ir_value_is_vector_backed, ir_value_resolves_to_xmm};
use crate::halt_reason::HaltReason;
use crate::ir::inst::Inst;
use crate::ir::location::{A64LocationDescriptor, LocationDescriptor};
use crate::ir::value::InstRef;

/// Emit upstream `A64EmitX64::EmitCheckMemoryAbort` after a memory access.
pub(crate) fn emit_a64_check_memory_abort(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst: &Inst,
    end: Option<&Label>,
) {
    if !ctx.config.memory.check_halt_on_memory_access {
        return;
    }

    let skip = ra.asm.create_label();
    let skip_target = end.unwrap_or(&skip);
    let current_location = A64LocationDescriptor::from_location(LocationDescriptor::new(
        inst.args[0].get_imm_as_u64(),
    ));
    ra.asm
        .test(
            dword_ptr(RegExp::from(R15) + A64JitState::offset_of_halt_reason() as i32),
            HaltReason::MEMORY_ABORT.bits() as i32,
        )
        .unwrap();
    ra.asm.jz(skip_target, JmpType::Near).unwrap();
    ra.asm.mov(RAX, current_location.pc() as i64).unwrap();
    ra.asm
        .mov(
            qword_ptr(RegExp::from(R15) + A64JitState::offset_of_pc() as i32),
            RAX,
        )
        .unwrap();
    if let Some(offsets) = ctx.dispatcher_offsets {
        emit_jmp_to_offset(ra.asm, offsets[FORCE_RETURN], ctx.code_base_ptr);
    } else {
        ra.asm.ret().unwrap();
    }
    if end.is_none() {
        ra.asm.bind(&skip).unwrap();
    }
}

/// Pre-generated fallback-stub address table.
///
/// Each key is `(ordered, bitsize, vaddr_idx, value_idx)` matching
/// upstream's `std::tuple<bool, size_t, int, int>`. Values are
/// **byte offsets** into the shared code buffer (not absolute pointers
/// — those are computed at lookup time as `code_base + offset`).
///
/// When a fastmem mov at code address X faults, the SIGSEGV handler
/// looks up X in the patch table and finds the stub offset to jump to;
/// the stub saves caller-save regs, calls the appropriate
/// `read_callback(vaddr)` / `write_callback(vaddr, value)`, restores
/// caller-saves, zero-extends the result for reads, and returns.
#[derive(Default)]
pub struct FastmemFallbacksTable {
    /// `(ordered, bitsize, vaddr_idx, value_idx) → stub_offset`.
    pub read: HashMap<(bool, usize, u8, u8), usize>,
    pub write: HashMap<(bool, usize, u8, u8), usize>,
    pub exclusive_write: HashMap<(bool, usize, u8, u8), usize>,
}

impl FastmemFallbacksTable {
    pub fn new() -> Self {
        Self {
            read: HashMap::new(),
            write: HashMap::new(),
            exclusive_write: HashMap::new(),
        }
    }

    /// Look up a read fallback stub. Returns the byte offset into the
    /// owning code buffer. Panics if the key was never generated.
    pub fn read_stub(&self, ordered: bool, bitsize: usize, vaddr_idx: u8, value_idx: u8) -> usize {
        *self
            .read
            .get(&(ordered, bitsize, vaddr_idx, value_idx))
            .unwrap_or_else(|| {
                panic!(
                    "no read fallback stub for (ordered={}, bitsize={}, vaddr={}, value={})",
                    ordered, bitsize, vaddr_idx, value_idx
                )
            })
    }

    pub fn write_stub(&self, ordered: bool, bitsize: usize, vaddr_idx: u8, value_idx: u8) -> usize {
        *self
            .write
            .get(&(ordered, bitsize, vaddr_idx, value_idx))
            .unwrap_or_else(|| {
                panic!(
                    "no write fallback stub for (ordered={}, bitsize={}, vaddr={}, value={})",
                    ordered, bitsize, vaddr_idx, value_idx
                )
            })
    }

    pub fn exclusive_write_stub(
        &self,
        ordered: bool,
        bitsize: usize,
        vaddr_idx: u8,
        value_idx: u8,
    ) -> usize {
        *self
            .exclusive_write
            .get(&(ordered, bitsize, vaddr_idx, value_idx))
            .unwrap_or_else(|| {
                panic!(
                    "no exclusive-write fallback stub for (ordered={}, bitsize={}, vaddr={}, value={})",
                    ordered, bitsize, vaddr_idx, value_idx
                )
            })
    }
}

/// Indices of GPRs that are valid as `vaddr_idx` / `value_idx` values
/// for fastmem stubs. RSP (4) is excluded because it has special
/// addressing semantics; R15 (15) is excluded because it holds the
/// JitState pointer at JIT runtime and is callee-saved across the
/// call into the run-loop.
///
/// Matches upstream's `idxes{0..15}` minus `4` and `15` (see
/// `a64_emit_x64_memory.cpp:114-138`).
const VALID_GPR_IDXES: [u8; 14] = [0, 1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14];

/// Pre-generate the full table of fastmem fallback stubs into `asm`'s
/// code buffer and record the byte offsets in the returned table.
///
/// Generates `2 (ordered) × 14 (vaddr) × 14 (value) × 4 (bitsize) × 2
/// (read+write) = 3136` stubs. At ~30-50 bytes each, the table costs
/// roughly 100-150 KB of code-buffer space — comparable to upstream's
/// fallback table size.
///
/// Mirrors upstream `A64EmitX64::GenFastmemFallbacks` in
/// `a64_emit_x64_memory.cpp:113-280`.
pub fn gen_fastmem_fallbacks(
    asm: &mut CodeAssembler,
    callbacks: &EmitCallbacks,
    raw_exclusive_write_callbacks: Option<&RawExclusiveWriteCallbacks>,
) -> FastmemFallbacksTable {
    let mut table = FastmemFallbacksTable::new();

    for ordered in [false, true] {
        for &vaddr_idx in &VALID_GPR_IDXES {
            for value_idx in 0u8..16 {
                asm.align(16).unwrap();
                let read_off = asm.size();
                emit_read_fallback_128(asm, callbacks, ordered, vaddr_idx, value_idx);
                table
                    .read
                    .insert((ordered, 128, vaddr_idx, value_idx), read_off);

                if let Some(raw_callbacks) = raw_exclusive_write_callbacks {
                    asm.align(16).unwrap();
                    let exclusive_write_off = asm.size();
                    emit_exclusive_write_fallback_128(asm, raw_callbacks, vaddr_idx, value_idx);
                    table
                        .exclusive_write
                        .insert((ordered, 128, vaddr_idx, value_idx), exclusive_write_off);
                }
            }

            for &value_idx in &VALID_GPR_IDXES {
                for &bitsize in &[8usize, 16, 32, 64] {
                    asm.align(16).unwrap();
                    let read_off = asm.size();
                    emit_read_fallback(asm, callbacks, ordered, bitsize, vaddr_idx, value_idx);
                    table
                        .read
                        .insert((ordered, bitsize, vaddr_idx, value_idx), read_off);

                    asm.align(16).unwrap();
                    let write_off = asm.size();
                    emit_write_fallback(asm, callbacks, ordered, bitsize, vaddr_idx, value_idx);
                    table
                        .write
                        .insert((ordered, bitsize, vaddr_idx, value_idx), write_off);

                    if let Some(raw_callbacks) = raw_exclusive_write_callbacks {
                        asm.align(16).unwrap();
                        let exclusive_write_off = asm.size();
                        emit_exclusive_write_fallback(
                            asm,
                            raw_callbacks,
                            bitsize,
                            vaddr_idx,
                            value_idx,
                        );
                        table.exclusive_write.insert(
                            (ordered, bitsize, vaddr_idx, value_idx),
                            exclusive_write_off,
                        );
                    }
                }
            }
        }
    }

    table
}

fn emit_read_fallback_128(
    asm: &mut CodeAssembler,
    callbacks: &EmitCallbacks,
    ordered: bool,
    vaddr_idx: u8,
    value_idx: u8,
) {
    #[cfg(target_os = "windows")]
    let (saved, local) = abi::push_caller_save_registers_and_adjust_stack_except_with_local(
        asm,
        Some(HostLoc::Xmm(value_idx)),
        16,
    )
    .unwrap();
    #[cfg(not(target_os = "windows"))]
    let saved =
        abi::push_caller_save_registers_and_adjust_stack_except(asm, Some(HostLoc::Xmm(value_idx)))
            .unwrap();
    if ordered {
        asm.mfence().unwrap();
    }

    #[cfg(target_os = "windows")]
    callbacks
        .memory_read_128
        .emit_call(asm, &|code, params| {
            code.mov(params[0], Reg::gpr64(vaddr_idx))?;
            code.lea(params[1], qword_ptr(RegExp::from(RSP) + local as i32))?;
            Ok(())
        })
        .unwrap();

    #[cfg(not(target_os = "windows"))]
    callbacks
        .memory_read_128
        .emit_call(asm, &|code, params| {
            code.mov(params[0], Reg::gpr64(vaddr_idx))?;
            Ok(())
        })
        .unwrap();

    #[cfg(target_os = "windows")]
    asm.movups(
        Reg::xmm(value_idx),
        xmmword_ptr(RegExp::from(RSP) + local as i32),
    )
    .unwrap();
    #[cfg(not(target_os = "windows"))]
    {
        asm.movq(Reg::xmm(value_idx), RAX).unwrap();
        asm.pinsrq(Reg::xmm(value_idx), RDX, 1).unwrap();
    }

    abi::pop_caller_save_registers_and_adjust_stack(asm, &saved).unwrap();
    asm.ret().unwrap();
}

/// Emit a single read-fallback stub.
///
/// Stub layout (matches upstream `a64_emit_x64_memory.cpp:201-219`):
///
/// ```text
///   push_caller_saves(except value_idx)
///   if vaddr_idx != ABI_PARAM2: mov ABI_PARAM2, <vaddr_idx>
///   if ordered: mfence
///   call read_callback             (ArgCallback sets RDI = context)
///   if value_idx != RAX: mov <value_idx>, RAX
///   pop_caller_saves(except value_idx)
///   zero_extend_from(bitsize, <value_idx>)
///   ret
/// ```
fn emit_read_fallback(
    asm: &mut CodeAssembler,
    callbacks: &EmitCallbacks,
    ordered: bool,
    bitsize: usize,
    vaddr_idx: u8,
    value_idx: u8,
) {
    let value_reg = Reg::gpr64(value_idx);

    // RUZU_FALLBACK_MARK_XMM15=1 — also mark for read fallback.
    if std::env::var("RUZU_FALLBACK_MARK_XMM15").is_ok() {
        asm.db(0x66).unwrap();
        asm.db(0x45).unwrap();
        asm.db(0x0F).unwrap();
        asm.db(0x74).unwrap();
        asm.db(0xFF).unwrap();
    }

    // Push caller-saves, skipping value_idx (it'll hold the result).
    let saved =
        abi::push_caller_save_registers_and_adjust_stack_except(asm, Some(HostLoc::Gpr(value_idx)))
            .unwrap();

    let vaddr_param = abi::ABI_PARAMS[1].to_reg64();
    if vaddr_idx != vaddr_param.get_idx() {
        asm.mov(vaddr_param, Reg::gpr64(vaddr_idx)).unwrap();
    }
    if ordered {
        asm.mfence().unwrap();
    }

    let callback = match bitsize {
        8 => &callbacks.memory_read_8,
        16 => &callbacks.memory_read_16,
        32 => &callbacks.memory_read_32,
        64 => &callbacks.memory_read_64,
        _ => unreachable!(),
    };
    callback.emit_call_simple(asm).unwrap();

    if value_idx != RAX.get_idx() {
        asm.mov(value_reg, RAX).unwrap();
    }

    abi::pop_caller_save_registers_and_adjust_stack(asm, &saved).unwrap();

    // Zero-extend result to 64 bits (the IR caller expects a 64-bit
    // register holding the bitsize-truncated, zero-extended value).
    emit_zero_extend(asm, bitsize, value_reg);

    asm.ret().unwrap();
}

/// Emit a single write-fallback stub.
///
/// Stub layout (matches upstream `a64_emit_x64_memory.cpp:221-248`):
///
/// ```text
///   push_caller_saves
///   if (vaddr_idx == ABI_PARAM3 && value_idx == ABI_PARAM2):
///       xchg ABI_PARAM2, ABI_PARAM3
///   elif (vaddr_idx == ABI_PARAM3):
///       mov ABI_PARAM2, <vaddr_idx>
///       if value_idx != ABI_PARAM3: mov ABI_PARAM3, <value_idx>
///   else:
///       if value_idx != ABI_PARAM3: mov ABI_PARAM3, <value_idx>
///       if vaddr_idx != ABI_PARAM2: mov ABI_PARAM2, <vaddr_idx>
///   zero_extend_from(bitsize, ABI_PARAM3)
///   call write_callback             (ArgCallback sets RDI = context)
///   if ordered: mfence
///   pop_caller_saves
///   ret
/// ```
fn emit_write_fallback(
    asm: &mut CodeAssembler,
    callbacks: &EmitCallbacks,
    ordered: bool,
    bitsize: usize,
    vaddr_idx: u8,
    value_idx: u8,
) {
    // RUZU_FALLBACK_MARK_XMM15=1 — set xmm15 to all-FFs at fallback entry
    // so a subsequent W128 callback can detect whether ANY fastmem
    // fallback fired during this block's execution.
    if std::env::var("RUZU_FALLBACK_MARK_XMM15").is_ok() {
        // pcmpeqb xmm15, xmm15 → xmm15 = 0xFF...FF
        // 66 45 0F 74 FF (REX.RB to make BOTH operands xmm15)
        asm.db(0x66).unwrap();
        asm.db(0x45).unwrap();
        asm.db(0x0F).unwrap();
        asm.db(0x74).unwrap();
        asm.db(0xFF).unwrap();
    }
    let saved = abi::push_caller_save_registers_and_adjust_stack(asm).unwrap();

    // Marshal vaddr → ABI_PARAM2, value → ABI_PARAM3. Handle aliasing carefully:
    // upstream's order avoids overwriting a source we still need.
    let vaddr_param = abi::ABI_PARAMS[1].to_reg64();
    let value_param = abi::ABI_PARAMS[2].to_reg64();
    let value_param_idx = value_param.get_idx();
    let vaddr_param_idx = vaddr_param.get_idx();

    if vaddr_idx == value_param_idx && value_idx == vaddr_param_idx {
        asm.xchg(vaddr_param, value_param).unwrap();
    } else if vaddr_idx == value_param_idx {
        // Preserve vaddr before overwriting its source with the value.
        asm.mov(vaddr_param, Reg::gpr64(vaddr_idx)).unwrap();
        if value_idx != value_param_idx {
            asm.mov(value_param, Reg::gpr64(value_idx)).unwrap();
        }
    } else {
        if value_idx != value_param_idx {
            asm.mov(value_param, Reg::gpr64(value_idx)).unwrap();
        }
        if vaddr_idx != vaddr_param_idx {
            asm.mov(vaddr_param, Reg::gpr64(vaddr_idx)).unwrap();
        }
    }

    // Zero-extend the value in ABI_PARAM3 from `bitsize` to 64 bits before
    // the callback so the host sees a clean value.
    emit_zero_extend(asm, bitsize, value_param);

    let callback = match bitsize {
        8 => &callbacks.memory_write_8,
        16 => &callbacks.memory_write_16,
        32 => &callbacks.memory_write_32,
        64 => &callbacks.memory_write_64,
        _ => unreachable!(),
    };
    callback.emit_call_simple(asm).unwrap();

    if ordered {
        asm.mfence().unwrap();
    }

    abi::pop_caller_save_registers_and_adjust_stack(asm, &saved).unwrap();
    asm.ret().unwrap();
}

/// Emit the fallback used when an inline `cmpxchg` exclusive store faults.
/// The monitor lock is already held and RAX contains the expected value, so
/// this calls the raw user callback directly instead of re-entering the
/// monitor-aware slow trampoline.
pub(crate) fn emit_exclusive_write_fallback(
    asm: &mut CodeAssembler,
    callbacks: &RawExclusiveWriteCallbacks,
    bitsize: usize,
    vaddr_idx: u8,
    value_idx: u8,
) {
    let (saved, local) = abi::push_caller_save_registers_and_adjust_stack_except_with_local(
        asm,
        Some(HostLoc::Gpr(RAX.get_idx())),
        32,
    )
    .unwrap();

    // Snapshot all sources before assigning ABI registers. This handles all
    // vaddr/value/parameter alias combinations without changing semantics.
    asm.mov(
        qword_ptr(RegExp::from(RSP) + local as i32),
        Reg::gpr64(vaddr_idx),
    )
    .unwrap();
    asm.mov(
        qword_ptr(RegExp::from(RSP) + local as i32 + 8),
        Reg::gpr64(value_idx),
    )
    .unwrap();
    asm.mov(qword_ptr(RegExp::from(RSP) + local as i32 + 16), RAX)
        .unwrap();

    let callback = match bitsize {
        8 => &callbacks.write_8,
        16 => &callbacks.write_16,
        32 => &callbacks.write_32,
        64 => &callbacks.write_64,
        _ => unreachable!(),
    };
    callback
        .emit_call(asm, &|code, params| {
            code.mov(params[0], qword_ptr(RegExp::from(RSP) + local as i32))?;
            code.mov(params[1], qword_ptr(RegExp::from(RSP) + local as i32 + 8))?;
            code.mov(params[2], qword_ptr(RegExp::from(RSP) + local as i32 + 16))?;
            Ok(())
        })
        .unwrap();

    abi::pop_caller_save_registers_and_adjust_stack(asm, &saved).unwrap();
    asm.ret().unwrap();
}

fn emit_exclusive_write_fallback_128(
    asm: &mut CodeAssembler,
    callbacks: &RawExclusiveWriteCallbacks,
    vaddr_idx: u8,
    _value_idx: u8,
) {
    let (saved, local) = abi::push_caller_save_registers_and_adjust_stack_except_with_local(
        asm,
        Some(HostLoc::Gpr(RAX.get_idx())),
        48,
    )
    .unwrap();
    asm.mov(qword_ptr(RegExp::from(RSP) + local as i32), RBX)
        .unwrap();
    asm.mov(qword_ptr(RegExp::from(RSP) + local as i32 + 8), RCX)
        .unwrap();
    asm.mov(qword_ptr(RegExp::from(RSP) + local as i32 + 16), RAX)
        .unwrap();
    asm.mov(qword_ptr(RegExp::from(RSP) + local as i32 + 24), RDX)
        .unwrap();
    asm.mov(
        qword_ptr(RegExp::from(RSP) + local as i32 + 32),
        Reg::gpr64(vaddr_idx),
    )
    .unwrap();

    callbacks
        .write_128
        .emit_call(asm, &|code, params| {
            code.mov(params[0], qword_ptr(RegExp::from(RSP) + local as i32 + 32))?;
            code.lea(params[1], qword_ptr(RegExp::from(RSP) + local as i32))?;
            code.lea(params[2], qword_ptr(RegExp::from(RSP) + local as i32 + 16))?;
            Ok(())
        })
        .unwrap();

    abi::pop_caller_save_registers_and_adjust_stack(asm, &saved).unwrap();
    asm.ret().unwrap();
}

/// Zero-extend a register's low `bitsize` bits to 64 bits.
///
/// `bitsize == 32`: `mov r32, r32` (implicitly zero-extends).
/// `bitsize == 16`: `movzx r32, r16`.
/// `bitsize == 8` : `movzx r32, r8` (uses REX-required low byte form
/// for idx 4..=7 to access SPL/BPL/SIL/DIL instead of AH/CH/DH/BH).
/// `bitsize == 64`: no-op.
fn emit_zero_extend(asm: &mut CodeAssembler, bitsize: usize, reg: Reg) {
    let idx = reg.get_idx();
    match bitsize {
        8 => {
            let r32 = Reg::gpr32(idx);
            let r8 = if (4..8).contains(&idx) {
                Reg::new_ext8(idx)
            } else {
                Reg::gpr8(idx)
            };
            asm.movzx(r32, r8).unwrap();
        }
        16 => {
            let r32 = Reg::gpr32(idx);
            let r16 = Reg::gpr16(idx);
            asm.movzx(r32, r16).unwrap();
        }
        32 => {
            let r32 = Reg::gpr32(idx);
            asm.mov(r32, r32).unwrap();
        }
        64 => { /* already 64-bit */ }
        _ => unreachable!("invalid bitsize: {}", bitsize),
    }
}

// ---------------------------------------------------------------------------
// EmitMemoryRead / EmitMemoryWrite — A64 memory dispatcher template body
// (port of `emit_x64_memory.cpp.inc:54-220` instantiated for A64).
//
// Three paths matching upstream:
//
// 1. Pure callback (no fastmem, no page table): mfence (if ordered) +
//    callback emit + zero_extend. host_call defines the result.
//
// 2. Fastmem (`ctx.fastmem_available` set, no `do_not_fastmem` marker):
//    - emit `EmitFastmemVAddr` to compute `[r13 + masked-vaddr]`
//    - emit `EmitReadMemoryMov<bitsize>` (or write) — record its offset
//    - push deferred-emit closure that binds `abort:`, calls the
//      pre-generated fallback stub, records the FastmemPatchInfo
//      (key=mov RIP), and jumps to `end:`
//    - bind `end:` after the mov so the fast path falls through
//
// 3. Page table (no fastmem, `page_table_present` set): same shape as
//    fastmem but uses `EmitVAddrLookup` and does NOT record patch info.
//
// Ordinary 128-bit accesses remain on the callback path in `emit_memory.rs`;
// exclusive inline accesses use this module's 128-bit fallback entries from
// `emit_exclusive_memory.rs`.
// ---------------------------------------------------------------------------

/// Emit a `call rel32` to a target byte offset within the same code
/// buffer. Used by deferred-emit closures to jump to the pre-generated
/// fallback stubs.
///
/// Encoding: `0xE8 rel32` where `rel32 = target - (current + 5)`.
pub(crate) fn emit_call_to_offset(asm: &mut CodeAssembler, target_offset: usize) {
    let current = asm.size();
    let rel32 = (target_offset as i64) - (current as i64 + 5);
    asm.db(0xE8).unwrap();
    let rel32_le = (rel32 as i32) as u32;
    asm.dd(rel32_le).unwrap();
}

/// Matches upstream `A64EmitX64::ShouldFastmem`.
pub(crate) fn should_fastmem(ctx: &EmitContext, inst_ref: InstRef) -> Option<DoNotFastmemMarker> {
    if !ctx.fastmem_available {
        return None;
    }

    let marker = (ctx.location, inst_ref.0);
    ctx.do_not_fastmem
        .map(|markers| (!markers.contains(&marker)).then_some(marker))
        .unwrap_or(Some(marker))
}

/// A64 IR memory read dispatcher. `BITSIZE` ∈ {8, 16, 32, 64}.
/// 128-bit reads stay on the existing callback path in `emit_memory.rs`.
///
/// Mirrors upstream `A64EmitX64::EmitMemoryRead<BITSIZE, callback>` in
/// `emit_x64_memory.cpp.inc:54-139` (instantiated for A64).
pub fn emit_a64_memory_read<const BITSIZE: usize>(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    debug_assert!(matches!(BITSIZE, 8 | 16 | 32 | 64));
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    // args[2] is the access-type immediate (Value::ImmAccType).
    let ordered = is_ordered(args[2].value.get_acc_type());

    let mem_conf = &ctx.config.memory;
    // RUZU_NO_FASTMEM_R64=1 — force 64-bit reads through slow-path
    // callback, mirror of RUZU_NO_FASTMEM_W64. Used to test fastmem-read
    // vs slow-path-write coherency.
    let force_callback_for_r64 = BITSIZE == 64 && std::env::var_os("RUZU_NO_FASTMEM_R64").is_some();
    let fastmem_marker = (!force_callback_for_r64)
        .then(|| should_fastmem(ctx, inst_ref))
        .flatten();

    // Path 1: pure callback when neither fastmem nor page table apply.
    if !mem_conf.page_table_present && fastmem_marker.is_none() {
        ra.host_call(Some(inst_ref), &mut [None, Some(&mut args[1]), None, None]);
        if ordered {
            ra.asm.mfence().unwrap();
        }
        let cb = match BITSIZE {
            8 => &ctx.config.callbacks.memory_read_8,
            16 => &ctx.config.callbacks.memory_read_16,
            32 => &ctx.config.callbacks.memory_read_32,
            64 => &ctx.config.callbacks.memory_read_64,
            _ => unreachable!(),
        };
        cb.emit_call_simple(&mut *ra.asm).unwrap();
        // host_call already defined the result in RAX → inst_ref.
        // Zero-extension below 64-bit is performed by callback ABI.
        return;
    }

    // Allocate vaddr (use) + value (scratch).
    let vaddr = ra.use_gpr(&mut args[1]);
    let value = ra.scratch_gpr();
    let vaddr_idx = vaddr.get_idx();
    let value_idx = value.get_idx();

    // RUZU_TRAP_LDR_BYTE5_21=1 — trap if the load address (vaddr) has the
    // STK corrupt pattern (byte 5 = 0x21, byte 4 = 0x01, bytes 6,7 = 0).
    // Catches the moment a corrupt heap-shifted pointer is used as a
    // memory address. Emitted only for 64-bit reads to avoid clutter.
    if BITSIZE == 64 && std::env::var_os("RUZU_TRAP_LDR_BYTE5_21").is_some() && vaddr_idx != 11 {
        let ok = ra.asm.create_label();
        // Save scratch: push rax (no flag changes), then build the check.
        // This needs to happen BEFORE the fastmem mov so we don't fault
        // on the mov before checking. Use a scratch via pushf/pushax-like
        // approach:
        //   push rax
        //   mov rax, vaddr_reg
        //   shr rax, 40        ; rax = bits 40-63 of vaddr (= bytes 5-7)
        //   cmp eax, 0x21      ; bytes 5-7 must be 0x21,0,0
        //   jne .restore_ok
        //   mov rax, vaddr_reg
        //   shr rax, 32
        //   and eax, 0xFF      ; eax = byte 4
        //   cmp eax, 0x01      ; byte 4 must be 0x01 (heap second byte)
        //   jne .restore_ok
        //   ; matched — pop rax (preserve flags from cmp by NOT using popfq),
        //   ; then UD2 with vaddr_reg-equivalent marker in rax
        //   pop rax            ; restore rax (no flag change)
        //   mov rax, vaddr_reg ; put vaddr in rax for SIGILL handler
        //   ud2
        //   .restore_ok:
        //   pop rax
        let vaddr_reg = rxbyak::Reg::gpr64(vaddr_idx);
        ra.asm.push(rxbyak::RAX).unwrap();
        ra.asm.mov(rxbyak::RAX, vaddr_reg).unwrap();
        ra.asm.shr(rxbyak::RAX, 40u8).unwrap();
        ra.asm.cmp(rxbyak::EAX, 0x21i32).unwrap();
        ra.asm.jne(&ok, JmpType::Near).unwrap();
        ra.asm.mov(rxbyak::RAX, vaddr_reg).unwrap();
        ra.asm.shr(rxbyak::RAX, 32u8).unwrap();
        ra.asm.and_(rxbyak::EAX, 0xFFi32).unwrap();
        ra.asm.cmp(rxbyak::EAX, 0x01i32).unwrap();
        ra.asm.jne(&ok, JmpType::Near).unwrap();
        // matched — preserve the same trap stack convention as W64 traps:
        // [rsp+16] = vaddr. Additionally [rsp+24] = saved guest x30 read
        // from [guest_sp+8], which identifies the caller of functions that
        // have already pushed x29/x30 in their prologue.
        let r11 = rxbyak::Reg::gpr64(11);
        ra.asm.pop(rxbyak::RAX).unwrap();
        ra.asm
            .mov(r11, rxbyak::qword_ptr(RegExp::from(R15) + 248))
            .unwrap();
        ra.asm
            .mov(r11, rxbyak::qword_ptr(RegExp::from(R13) + r11 + 8))
            .unwrap();
        ra.asm.push(r11).unwrap();
        ra.asm.push(vaddr_reg).unwrap();
        ra.asm.push(r11).unwrap();
        ra.asm.pushf().unwrap();
        ra.asm.mov(r11, 0xCAFE_F00Du32 as i32).unwrap();
        ra.asm.mov(rxbyak::RAX, vaddr_reg).unwrap();
        ra.asm.ud2().unwrap();
        ra.asm.bind(&ok).unwrap();
        ra.asm.pop(rxbyak::RAX).unwrap();
    }

    // Look up the pre-generated fallback stub address.
    let fallbacks = unsafe {
        &*(ctx
            .fastmem_fallbacks
            .expect("fastmem path used but fastmem_fallbacks not set on EmitContext")
            as *const FastmemFallbacksTable)
    };
    let wrapped_fn_off = fallbacks.read_stub(ordered, BITSIZE, vaddr_idx, value_idx);

    let abort = ra.asm.create_label();
    let end = ra.asm.create_label();

    if let Some(marker) = fastmem_marker {
        // Path 2: fastmem.
        let mut require_abort = false;
        let src_ptr = emit_fastmem_vaddr_a64(ra, ctx, abort, vaddr, &mut require_abort, None);
        let mov_off = emit_read_memory_mov::<BITSIZE>(ra.asm, value_idx, src_ptr, ordered);

        // RUZU_TRAP_FASTMEM_R64_VALUE_PATTERN=1 — after a 64-bit
        // fastmem-direct load, trap if the loaded value matches STK's
        // heap-shifted pointer pattern. Optional:
        // RUZU_TRAP_FASTMEM_R64_VALUE_PATTERN_AT=0xPC restricts emission to
        // one guest block, avoiding benign string-scan false positives.
        // RUZU_TRAP_FASTMEM_R64_HEAPSHIFT=1 tightens the match further for
        // the observed STK heap range by requiring byte 3 >= 0x60. This
        // filters UTF-8/string false positives such as 0x0000210100670067.
        // Vaddr is recovered by ruzu-cmd's SIGILL handler from [RSP+16],
        // matching the W64 trap stack layout.
        let trap_value_pc = std::env::var("RUZU_TRAP_FASTMEM_R64_VALUE_PATTERN_AT")
            .ok()
            .and_then(|s| u64::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok());
        let trap_heapshift = std::env::var_os("RUZU_TRAP_FASTMEM_R64_HEAPSHIFT").is_some();
        let trap_value_here = (std::env::var_os("RUZU_TRAP_FASTMEM_R64_VALUE_PATTERN").is_some()
            || trap_heapshift)
            && trap_value_pc.map_or(true, |pc| {
                A64LocationDescriptor::from_location(ctx.location).pc() == pc
            });
        if BITSIZE == 64 && trap_value_here && value_idx != 11 && vaddr_idx != 11 {
            let ok = ra.asm.create_label();
            let value_reg = rxbyak::Reg::gpr64(value_idx);
            let vaddr_reg = rxbyak::Reg::gpr64(vaddr_idx);
            let r11 = rxbyak::Reg::gpr64(11);
            let r11_32 = rxbyak::Reg::gpr32(11);

            ra.asm.push(vaddr_reg).unwrap();
            ra.asm.push(r11).unwrap();
            ra.asm.pushf().unwrap();

            ra.asm.mov(r11, value_reg).unwrap();
            ra.asm.shr(r11, 40u8).unwrap();
            ra.asm.cmp(r11_32, 0x21i32).unwrap();
            ra.asm.jne(&ok, JmpType::Near).unwrap();
            ra.asm.mov(r11, value_reg).unwrap();
            ra.asm.shr(r11, 32u8).unwrap();
            ra.asm.and_(r11_32, 0xFFi32).unwrap();
            ra.asm.cmp(r11_32, 0x01i32).unwrap();
            ra.asm.jne(&ok, JmpType::Near).unwrap();
            if trap_heapshift {
                ra.asm.mov(r11, value_reg).unwrap();
                ra.asm.shr(r11, 24u8).unwrap();
                ra.asm.and_(r11_32, 0xFFi32).unwrap();
                ra.asm.cmp(r11_32, 0x60i32).unwrap();
                ra.asm.jb(&ok, JmpType::Near).unwrap();
            }

            ra.asm.mov(rxbyak::RAX, value_reg).unwrap();
            ra.asm.mov(r11, 0xCAFE_F00Du32 as i32).unwrap();
            ra.asm.ud2().unwrap();

            ra.asm.bind(&ok).unwrap();
            ra.asm.popf().unwrap();
            ra.asm.pop(r11).unwrap();
            ra.asm.pop(vaddr_reg).unwrap();
        }

        let recompile = mem_conf.recompile_on_fastmem_failure;
        ctx.deferred_emits
            .borrow_mut()
            .push(Box::new(move |dctx: &mut DeferredEmitCtx<'_>| {
                let asm = &mut *dctx.asm;
                asm.bind(&abort).unwrap();
                emit_call_to_offset(asm, wrapped_fn_off);

                let resume_off = asm.size();
                let inst_rip = dctx.code_base + mov_off as u64;
                let resume_rip = dctx.code_base + resume_off as u64;
                let stub_rip = dctx.code_base + wrapped_fn_off as u64;
                dctx.fastmem_patches.add(
                    inst_rip,
                    FastmemPatchInfo::new(resume_rip, stub_rip, Some(marker), recompile),
                );

                // EmitCheckMemoryAbort is deferred — only relevant when
                // `check_halt_on_memory_access` is set, which ruzu does
                // not currently enable. Stay parity-faithful by still
                // recording the patch entry but skip the inline check.

                asm.jmp(&end, JmpType::Near).unwrap();
            }));
    } else {
        // Path 3: page table (debug_assert page_table_present).
        debug_assert!(mem_conf.page_table_present);
        let src_ptr = emit_vaddr_lookup_a64(ra, ctx, BITSIZE, abort, vaddr);
        let _mov_off = emit_read_memory_mov::<BITSIZE>(ra.asm, value_idx, src_ptr, ordered);

        ctx.deferred_emits
            .borrow_mut()
            .push(Box::new(move |dctx: &mut DeferredEmitCtx<'_>| {
                let asm = &mut *dctx.asm;
                asm.bind(&abort).unwrap();
                emit_call_to_offset(asm, wrapped_fn_off);
                asm.jmp(&end, JmpType::Near).unwrap();
            }));
    }

    ra.asm.bind(&end).unwrap();
    ra.define_value(inst_ref, value);
}

/// A64 IR memory write dispatcher. `BITSIZE` ∈ {8, 16, 32, 64}.
///
/// Mirrors upstream `A64EmitX64::EmitMemoryWrite<BITSIZE, callback>` in
/// `emit_x64_memory.cpp.inc:141-220` (instantiated for A64).
pub fn emit_a64_memory_write<const BITSIZE: usize>(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    debug_assert!(matches!(BITSIZE, 8 | 16 | 32 | 64));
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    // args[3] is the access-type immediate (Value::ImmAccType).
    let ordered = is_ordered(args[3].value.get_acc_type());

    let mem_conf = &ctx.config.memory;
    // RUZU_NO_FASTMEM_W{8,16,32,64}=1 — disable fastmem direct stores for
    // a specific width; force them through the slow-path callback so the
    // `memory_write_*` callbacks see every store at that width. Used to
    // hunt the STK `(valid_addr << 8) | byte` corruption: with W64 alone
    // forced through callback we observed valid data, yet fastmem READ
    // saw corrupt — so a non-64-bit fastmem-direct write must be
    // bypassing. Bisecting by width nails down which.
    let force_callback = match BITSIZE {
        8 => std::env::var_os("RUZU_NO_FASTMEM_W8").is_some(),
        16 => std::env::var_os("RUZU_NO_FASTMEM_W16").is_some(),
        32 => std::env::var_os("RUZU_NO_FASTMEM_W32").is_some(),
        64 => std::env::var_os("RUZU_NO_FASTMEM_W64").is_some(),
        _ => false,
    };
    let fastmem_marker = (!force_callback)
        .then(|| should_fastmem(ctx, inst_ref))
        .flatten();
    let value_resolves_to_xmm =
        matches!(BITSIZE, 32 | 64) && ir_value_resolves_to_xmm(ctx, ra, &args[2].value);
    let value_is_vector_backed =
        matches!(BITSIZE, 32 | 64) && ir_value_is_vector_backed(ctx, &args[2].value);

    // Path 1: pure callback.
    if !mem_conf.page_table_present && fastmem_marker.is_none() {
        if matches!(BITSIZE, 32 | 64) && (value_resolves_to_xmm || value_is_vector_backed) {
            let (first, rest) = args.split_at_mut(2);
            ra.use_loc(&mut first[1], abi::ABI_PARAMS[1]);
            ra.use_loc(&mut rest[0], HostLoc::Xmm(1));
            ra.end_of_alloc_scope();
            ra.host_call(None, &mut [None, None, None, None]);
            if BITSIZE == 64 {
                ra.asm.movq(RDX, Reg::xmm(1)).unwrap();
            } else {
                ra.asm.movd(EDX, Reg::xmm(1)).unwrap();
            }
            let cb = match BITSIZE {
                32 => &ctx.config.callbacks.memory_write_32,
                64 => &ctx.config.callbacks.memory_write_64,
                _ => unreachable!(),
            };
            cb.emit_call_simple(&mut *ra.asm).unwrap();
            if ordered {
                ra.asm.mfence().unwrap();
            }
            return;
        }
        let (first, rest) = args.split_at_mut(2);
        ra.host_call(
            None,
            &mut [None, Some(&mut first[1]), Some(&mut rest[0]), None],
        );
        let cb = match BITSIZE {
            8 => &ctx.config.callbacks.memory_write_8,
            16 => &ctx.config.callbacks.memory_write_16,
            32 => &ctx.config.callbacks.memory_write_32,
            64 => &ctx.config.callbacks.memory_write_64,
            _ => unreachable!(),
        };
        cb.emit_call_simple(&mut *ra.asm).unwrap();
        if ordered {
            ra.asm.mfence().unwrap();
        }
        return;
    }

    // Allocate vaddr (use) + value (use).
    let vaddr = ra.use_gpr(&mut args[1]);
    let value = if matches!(BITSIZE, 32 | 64) && (value_resolves_to_xmm || value_is_vector_backed) {
        // `A64GetS`/`A64GetD` are typed as U128 upstream but physically hold
        // their scalar payload in the low 32/64 bits of XMM. For scalar
        // memory writes, upstream stores that low lane; avoid routing through
        // `UseGpr`, which would treat the value as a full 128-bit XMM->GPR
        // move.
        let value_xmm = ra.use_xmm(&mut args[2]);
        let value_gpr = ra.scratch_gpr();
        if BITSIZE == 64 {
            ra.asm.movq(value_gpr, value_xmm).unwrap();
        } else {
            ra.asm.movd(value_gpr.cvt32().unwrap(), value_xmm).unwrap();
        }
        value_gpr
    } else if ordered {
        // `xchg [mem], reg` overwrites the register with the old memory value.
        // Upstream therefore requires a scratch copy for ordered writes.
        ra.use_scratch_gpr(&mut args[2])
    } else {
        ra.use_gpr(&mut args[2])
    };
    let vaddr_idx = vaddr.get_idx();
    let value_idx = value.get_idx();

    let fallbacks = unsafe {
        &*(ctx
            .fastmem_fallbacks
            .expect("fastmem path used but fastmem_fallbacks not set on EmitContext")
            as *const FastmemFallbacksTable)
    };
    let wrapped_fn_off = fallbacks.write_stub(ordered, BITSIZE, vaddr_idx, value_idx);

    let abort = ra.asm.create_label();
    let end = ra.asm.create_label();

    if let Some(marker) = fastmem_marker {
        // Path 2: fastmem.
        let mut require_abort = false;
        let dest_ptr = emit_fastmem_vaddr_a64(ra, ctx, abort, vaddr, &mut require_abort, None);
        let mov_off = emit_write_memory_mov::<BITSIZE>(ra.asm, dest_ptr, value_idx, ordered);

        // RUZU_TRAP_FASTMEM_W64_CORRUPT=1 — for 64-bit fastmem-direct writes,
        // emit inline check: if value's byte 5 = 0x21, byte 4 = 0x01, bytes
        // 6,7 = 0 (the STK heap-shifted-pointer corrupt pattern), trap with
        // UD2.
        // RUZU_TRAP_FASTMEM_W64_VADDR=0xVADDR — alternative filter: trap if
        // vaddr_reg == VADDR (regardless of value). Used to find ANY
        // fastmem-direct W64 write that targets a tracked address.
        // RUZU_TRAP_FASTMEM_W64_CORRUPT_VADDR=0xVADDR — combined filter:
        // trap only when the tracked address receives the corrupt STK
        // heap-shifted-pointer pattern. This avoids perturbing allocator
        // timing with hundreds of normal free-list writes.
        // RUZU_TRAP_FASTMEM_W64_ODD_BIN_VALUE=1 — trap when a fastmem W64
        // writes an odd pointer-looking value into STK's static allocator-bin
        // page (value low bit set and value>>16 == 0x8149). This targets the
        // observed `0x814903E1` link that later causes an odd-address metadata
        // write at `0x814903F9`.
        // RUZU_TRAP_FASTMEM_W64_ODD_BIN_VALUE_HEAP_DST=1 — same value filter,
        // additionally require a heap destination (`vaddr >> 32 == 0x21`) to
        // skip the NRO loader copying literal static data during boot.
        // RUZU_TRAP_FASTMEM_W64_ODD_BIN_VALUE_VADDR=0xVADDR — same value
        // filter, additionally require an exact destination. Used after the
        // bad link's source metadata address is known.
        // Vaddr is recovered from stack via sentinel marker.
        let trap_corrupt_value = std::env::var_os("RUZU_TRAP_FASTMEM_W64_CORRUPT").is_some();
        let trap_target_vaddr = std::env::var("RUZU_TRAP_FASTMEM_W64_VADDR")
            .ok()
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok());
        let trap_corrupt_target_vaddr = std::env::var("RUZU_TRAP_FASTMEM_W64_CORRUPT_VADDR")
            .ok()
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok());
        let trap_odd_bin_value = std::env::var_os("RUZU_TRAP_FASTMEM_W64_ODD_BIN_VALUE").is_some();
        let trap_odd_bin_value_heap_dst =
            std::env::var_os("RUZU_TRAP_FASTMEM_W64_ODD_BIN_VALUE_HEAP_DST").is_some();
        let trap_odd_bin_value_vaddr = std::env::var("RUZU_TRAP_FASTMEM_W64_ODD_BIN_VALUE_VADDR")
            .ok()
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok());
        // RUZU_TRAP_FASTMEM_W64_VALUE_TAGGED_PHANTOM=1 — trap when a W64
        // fastmem-direct store writes a value matching the bin-phantom-tagged
        // pattern. Specifically: low bit set, high 32 bits zero, and bits
        // 8..31 match 0x008149_03 (i.e. value is in `0x814903XX..0x814903FF`
        // with low bit set). This is the value that taints chunk[+16] and
        // later causes the misaligned `str x3, [x4, #24]` at PC 0x80E441B8.
        let trap_tagged_phantom =
            std::env::var_os("RUZU_TRAP_FASTMEM_W64_VALUE_TAGGED_PHANTOM").is_some();
        // Use R11 (=11) as scratch — rarely used by fastmem path. Skip if
        // value or vaddr happens to be in R11.
        if BITSIZE == 64
            && (trap_corrupt_value
                || trap_target_vaddr.is_some()
                || trap_corrupt_target_vaddr.is_some())
            && value_idx != 11
            && vaddr_idx != 11
        {
            let ok = ra.asm.create_label();
            let vaddr_reg = rxbyak::Reg::gpr64(vaddr_idx);
            let r11 = rxbyak::Reg::gpr64(11);
            let r11_32 = rxbyak::Reg::gpr32(11);
            // Stash vaddr on stack so SIGILL handler can recover it via [rsp+16]
            // (after push r11 + pushf = 16 bytes pushed).
            // Use R11 as scratch (not RAX, to avoid value/vaddr conflict).
            // SIGILL handler reads R11 to detect the sentinel.
            ra.asm.push(vaddr_reg).unwrap();
            ra.asm.push(r11).unwrap();
            ra.asm.pushf().unwrap();
            if let Some(target_vaddr) = trap_target_vaddr {
                ra.asm.mov(r11, target_vaddr as i64).unwrap();
                ra.asm.cmp(r11, vaddr_reg).unwrap();
                ra.asm.jne(&ok, JmpType::Near).unwrap();
            } else {
                if let Some(target_vaddr) = trap_corrupt_target_vaddr {
                    ra.asm.mov(r11, target_vaddr as i64).unwrap();
                    ra.asm.cmp(r11, vaddr_reg).unwrap();
                    ra.asm.jne(&ok, JmpType::Near).unwrap();
                }
                // CRITICAL: for ORDERED writes, emit_write_memory_mov uses
                // `xchg [mem], reg` — which SWAPS the value reg with memory.
                // So after the store, value_reg holds the OLD memory value,
                // not the value we just stored. Reading value_reg here misses
                // ordered-store corruption. Instead, re-read MEMORY at
                // [R13 + vaddr_reg] — this gives the value that's actually
                // in the slot now (the just-stored value, post-mov-or-xchg).
                ra.asm
                    .mov(r11, rxbyak::qword_ptr(RegExp::from(R13) + vaddr_reg))
                    .unwrap();
                ra.asm.shr(r11, 40u8).unwrap();
                ra.asm.cmp(r11_32, 0x21i32).unwrap();
                ra.asm.jne(&ok, JmpType::Near).unwrap();
                ra.asm
                    .mov(r11, rxbyak::qword_ptr(RegExp::from(R13) + vaddr_reg))
                    .unwrap();
                ra.asm.shr(r11, 32u8).unwrap();
                ra.asm.and_(r11_32, 0xFFi32).unwrap();
                ra.asm.cmp(r11_32, 0x01i32).unwrap();
                ra.asm.jne(&ok, JmpType::Near).unwrap();
            }
            // matched — set R11 to a sentinel so handler knows it's a fastmem
            // corrupt trap. Vaddr is on stack at [rsp+16].
            ra.asm.mov(r11, 0xCAFE_F00Du32 as i32).unwrap();
            ra.asm.ud2().unwrap();
            ra.asm.bind(&ok).unwrap();
            ra.asm.popf().unwrap();
            ra.asm.pop(r11).unwrap();
            ra.asm.pop(vaddr_reg).unwrap();
        }
        // RUZU_TRAP_FASTMEM_W64_ODD_BIN_SKIP_LOW_BIT=1 — for the
        // RUZU_TRAP_FASTMEM_W64_ODD_BIN_VALUE* family, skip the
        // `(value & 1) == 1` filter and match on the prefix alone. Used
        // to find seed writes that place an ALIGNED bin-phantom pointer
        // (e.g. 0x814903E0 — low bit clear) into chunk metadata BEFORE
        // some later operation tags it with the PINUSE bit. The
        // tag-bit-set form `0x814903E1` is the result of an OR somewhere
        // downstream; the seed could be the aligned value.
        let trap_odd_bin_skip_low_bit =
            std::env::var_os("RUZU_TRAP_FASTMEM_W64_ODD_BIN_SKIP_LOW_BIT").is_some();
        if BITSIZE == 64
            && (trap_odd_bin_value || trap_odd_bin_value_heap_dst)
            && trap_odd_bin_value_vaddr.is_none()
            && value_idx != 11
            && vaddr_idx != 11
        {
            let ok = ra.asm.create_label();
            let value_reg = rxbyak::Reg::gpr64(value_idx);
            let vaddr_reg = rxbyak::Reg::gpr64(vaddr_idx);
            let r11 = rxbyak::Reg::gpr64(11);
            let r11_32 = rxbyak::Reg::gpr32(11);
            // Same stack convention as the other fastmem traps: after
            // push(vaddr), push(r11), pushf(), the SIGILL handler recovers
            // the destination guest vaddr from [rsp+16].
            ra.asm.push(vaddr_reg).unwrap();
            ra.asm.push(r11).unwrap();
            ra.asm.pushf().unwrap();
            if !trap_odd_bin_skip_low_bit {
                ra.asm.mov(r11, value_reg).unwrap();
                ra.asm.and_(r11_32, 1i32).unwrap();
                ra.asm.cmp(r11_32, 1i32).unwrap();
                ra.asm.jne(&ok, JmpType::Near).unwrap();
            }
            ra.asm.mov(r11, value_reg).unwrap();
            ra.asm.shr(r11, 16u8).unwrap();
            // RUZU_TRAP_FASTMEM_W64_ODD_BIN_PREFIX=0xPREFIX — override the
            // hardcoded `0x8149` high-16 check. Default 0x8149. Useful for
            // hunting upstream tainting writes that produce slightly
            // different mstate-region-tagged values like `0x8148_FFFF`
            // (which is the predecessor of `0x8149FFFF` in the chain at
            // PC=0x80211BF8).
            let prefix = std::env::var("RUZU_TRAP_FASTMEM_W64_ODD_BIN_PREFIX")
                .ok()
                .and_then(|s| i32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
                .unwrap_or(0x8149i32);
            ra.asm.cmp(r11_32, prefix).unwrap();
            ra.asm.jne(&ok, JmpType::Near).unwrap();
            if trap_odd_bin_value_heap_dst {
                ra.asm.mov(r11, vaddr_reg).unwrap();
                ra.asm.shr(r11, 32u8).unwrap();
                ra.asm.cmp(r11_32, 0x21i32).unwrap();
                ra.asm.jne(&ok, JmpType::Near).unwrap();
            }
            ra.asm.mov(r11, 0xCAFE_F00Du32 as i32).unwrap();
            ra.asm.ud2().unwrap();
            ra.asm.bind(&ok).unwrap();
            ra.asm.popf().unwrap();
            ra.asm.pop(r11).unwrap();
            ra.asm.pop(vaddr_reg).unwrap();
        }
        if BITSIZE == 64 && trap_odd_bin_value_vaddr.is_some() && value_idx != 11 && vaddr_idx != 11
        {
            let ok = ra.asm.create_label();
            let value_reg = rxbyak::Reg::gpr64(value_idx);
            let vaddr_reg = rxbyak::Reg::gpr64(vaddr_idx);
            let r11 = rxbyak::Reg::gpr64(11);
            let r11_32 = rxbyak::Reg::gpr32(11);
            ra.asm.push(vaddr_reg).unwrap();
            ra.asm.push(r11).unwrap();
            ra.asm.pushf().unwrap();
            ra.asm
                .mov(r11, trap_odd_bin_value_vaddr.unwrap() as i64)
                .unwrap();
            ra.asm.cmp(r11, vaddr_reg).unwrap();
            ra.asm.jne(&ok, JmpType::Near).unwrap();
            ra.asm.mov(r11, value_reg).unwrap();
            ra.asm.and_(r11_32, 1i32).unwrap();
            ra.asm.cmp(r11_32, 1i32).unwrap();
            ra.asm.jne(&ok, JmpType::Near).unwrap();
            ra.asm.mov(r11, value_reg).unwrap();
            ra.asm.shr(r11, 16u8).unwrap();
            ra.asm.cmp(r11_32, 0x8149i32).unwrap();
            ra.asm.jne(&ok, JmpType::Near).unwrap();
            ra.asm.mov(r11, 0xCAFE_F00Du32 as i32).unwrap();
            ra.asm.ud2().unwrap();
            ra.asm.bind(&ok).unwrap();
            ra.asm.popf().unwrap();
            ra.asm.pop(r11).unwrap();
            ra.asm.pop(vaddr_reg).unwrap();
        }

        // Tagged-bin-phantom-value trap: emit after every W64 fastmem-direct
        // store. Read `[R13 + vaddr_reg]` (the just-stored value) and check:
        //   - low bit set
        //   - high 32 bits zero
        //   - bits 24..31 == 0x81 (mstate page)
        //   - bits 16..23 == 0x49
        //   - bits 8..15  == 0x03
        // Matches 0x814903XX with low bit set: 0x814903E1, 0x814903F1, ...
        if BITSIZE == 64 && trap_tagged_phantom && vaddr_idx != 11 {
            let ok = ra.asm.create_label();
            let vaddr_reg = rxbyak::Reg::gpr64(vaddr_idx);
            let r11 = rxbyak::Reg::gpr64(11);
            let r11_32 = rxbyak::Reg::gpr32(11);
            ra.asm.push(vaddr_reg).unwrap();
            ra.asm.push(r11).unwrap();
            ra.asm.pushf().unwrap();
            // r11 = [R13 + vaddr_reg]
            ra.asm
                .mov(r11, rxbyak::qword_ptr(RegExp::from(R13) + vaddr_reg))
                .unwrap();
            // test low bit
            ra.asm.mov(r11_32, r11_32).unwrap(); // truncate to 32 to drop high
                                                 // Actually we want all 64 bits checks. Restart:
            ra.asm
                .mov(r11, rxbyak::qword_ptr(RegExp::from(R13) + vaddr_reg))
                .unwrap();
            // bit 0 must be 1
            ra.asm.test(r11, 1i32).unwrap();
            ra.asm.jz(&ok, JmpType::Near).unwrap();
            // high 32 bits must be 0 — check via shr 32; cmp 0
            ra.asm
                .mov(r11, rxbyak::qword_ptr(RegExp::from(R13) + vaddr_reg))
                .unwrap();
            ra.asm.shr(r11, 32u8).unwrap();
            ra.asm.cmp(r11_32, 0i32).unwrap();
            ra.asm.jne(&ok, JmpType::Near).unwrap();
            // bits 8..31 must be 0x814903
            ra.asm
                .mov(r11, rxbyak::qword_ptr(RegExp::from(R13) + vaddr_reg))
                .unwrap();
            ra.asm.shr(r11, 8u8).unwrap();
            ra.asm.and_(r11_32, 0x00FFFFFFi32).unwrap();
            ra.asm.cmp(r11_32, 0x00814903i32).unwrap();
            ra.asm.jne(&ok, JmpType::Near).unwrap();
            // Match — sentinel 0xCAFEF0E1 (E1 = "phantom-tagged")
            ra.asm.mov(r11, 0xCAFE_F0E1u32 as i32).unwrap();
            ra.asm.ud2().unwrap();
            ra.asm.bind(&ok).unwrap();
            ra.asm.popf().unwrap();
            ra.asm.pop(r11).unwrap();
            ra.asm.pop(vaddr_reg).unwrap();
        }

        // RUZU_TRAP_FASTMEM_ANY_VADDR_RANGE=0xLO:0xHI — trap ANY fastmem-direct
        // write (W8 / W16 / W32 / W64) whose vaddr falls in `[LO, HI)`. Width-
        // agnostic. Designed to catch the corrupting write to STK's allocator
        // mstate slot at 0x814903F8: the existing W64-only traps showed all
        // 64-bit writes to that exact vaddr carry VALID values, yet poll-mode
        // observes the slot containing the corrupt pattern. Hypothesis: the
        // corruption is from W8 or W32 fastmem-direct stores piecing the value
        // together. Bisection confirmed RUZU_NO_FASTMEM_W8/W32/W64 each
        // individually prevents the wedge.
        let trap_any_range = std::env::var("RUZU_TRAP_FASTMEM_ANY_VADDR_RANGE")
            .ok()
            .and_then(|s| {
                let parts: Vec<&str> = s.split(':').collect();
                if parts.len() != 2 {
                    return None;
                }
                let lo = u64::from_str_radix(parts[0].trim().trim_start_matches("0x"), 16).ok()?;
                let hi = u64::from_str_radix(parts[1].trim().trim_start_matches("0x"), 16).ok()?;
                Some((lo, hi))
            });
        // RUZU_TRAP_SLOT_AFTER_WRITE=0xSLOT:0xMASK:0xVALUE — after every
        // fastmem-direct write (ANY width, ANY vaddr), read 8 bytes from
        // `[R13 + SLOT]` and trap if `(slot & MASK) == VALUE`. Catches the
        // case where the corrupting write goes to a DIFFERENT vaddr that
        // host-aliases to the slot (memfd overlap, cache coherency, etc.) —
        // those aren't caught by vaddr- or value-filtered traps that check
        // the write's own register/destination.
        let trap_slot_after_write =
            std::env::var("RUZU_TRAP_SLOT_AFTER_WRITE")
                .ok()
                .and_then(|s| {
                    let parts: Vec<&str> = s.split(':').collect();
                    if parts.len() != 3 {
                        return None;
                    }
                    let slot =
                        u64::from_str_radix(parts[0].trim().trim_start_matches("0x"), 16).ok()?;
                    let mask =
                        u64::from_str_radix(parts[1].trim().trim_start_matches("0x"), 16).ok()?;
                    let value =
                        u64::from_str_radix(parts[2].trim().trim_start_matches("0x"), 16).ok()?;
                    Some((slot, mask, value))
                });
        if let Some((slot, mask, expected)) = trap_slot_after_write {
            // Need a non-clobbered scratch. value_idx might be 11 in some
            // configs (rare). Use R10 instead — but we still need R11 for
            // the sentinel. Push both.
            let ok = ra.asm.create_label();
            let r10 = rxbyak::Reg::gpr64(10);
            let r11 = rxbyak::Reg::gpr64(11);
            // Stack convention: same as other traps so SIGILL handler can
            // recover vaddr from [rsp+16]. Push value_reg's vaddr-companion
            // (a placeholder — the slot vaddr is constant, dump it via aux).
            let vaddr_reg = rxbyak::Reg::gpr64(vaddr_idx);
            ra.asm.push(vaddr_reg).unwrap();
            ra.asm.push(r11).unwrap();
            ra.asm.pushf().unwrap();
            // Need r10 too — push it separately.
            ra.asm.push(r10).unwrap();
            // r11 = [R13 + SLOT]
            ra.asm.mov(r11, slot as i64).unwrap();
            ra.asm
                .mov(r11, rxbyak::qword_ptr(RegExp::from(R13) + r11))
                .unwrap();
            // r10 = mask
            ra.asm.mov(r10, mask as i64).unwrap();
            ra.asm.and_(r11, r10).unwrap();
            // r10 = expected
            ra.asm.mov(r10, expected as i64).unwrap();
            ra.asm.cmp(r11, r10).unwrap();
            ra.asm.jne(&ok, JmpType::Near).unwrap();
            // Match — set sentinel and UD2. Use width=0x80 to distinguish
            // from the size-encoded sentinels of the range trap.
            ra.asm.mov(r11, 0xCAFE_F080u32 as i32).unwrap();
            // Stash slot vaddr in aux: write it to [rsp+24] manually. But
            // we're already in the trap area — instead encode in r10 as
            // BAD_BIT marker. For now skip aux.
            ra.asm.ud2().unwrap();
            ra.asm.bind(&ok).unwrap();
            ra.asm.pop(r10).unwrap();
            ra.asm.popf().unwrap();
            ra.asm.pop(r11).unwrap();
            ra.asm.pop(vaddr_reg).unwrap();
        }

        if let Some((lo, hi)) = trap_any_range {
            if vaddr_idx != 11 {
                let ok = ra.asm.create_label();
                let vaddr_reg = rxbyak::Reg::gpr64(vaddr_idx);
                let r11 = rxbyak::Reg::gpr64(11);
                // Same stack convention as W64 traps: SIGILL handler reads
                // vaddr from [rsp+16] after push r11 + pushf (= 16 bytes).
                ra.asm.push(vaddr_reg).unwrap();
                ra.asm.push(r11).unwrap();
                ra.asm.pushf().unwrap();
                // if vaddr < LO → skip
                ra.asm.mov(r11, lo as i64).unwrap();
                ra.asm.cmp(vaddr_reg, r11).unwrap();
                ra.asm.jb(&ok, JmpType::Near).unwrap();
                // if vaddr >= HI → skip
                ra.asm.mov(r11, hi as i64).unwrap();
                ra.asm.cmp(vaddr_reg, r11).unwrap();
                ra.asm.jae(&ok, JmpType::Near).unwrap();
                // in range — set sentinel + UD2. Encode BITSIZE into the
                // low byte of the sentinel so SIGILL handler can identify
                // the write width: 0xCAFEF008/16/32/64. The 0xCAFEF00D
                // sentinel used by other traps remains distinguishable —
                // those use exactly 0x0D, ours use 0x08/0x10/0x20/0x40.
                let sentinel: u32 = 0xCAFEF000 | (BITSIZE as u32);
                ra.asm.mov(r11, sentinel as i32).unwrap();
                ra.asm.ud2().unwrap();
                ra.asm.bind(&ok).unwrap();
                ra.asm.popf().unwrap();
                ra.asm.pop(r11).unwrap();
                ra.asm.pop(vaddr_reg).unwrap();
            }
        }

        let recompile = mem_conf.recompile_on_fastmem_failure;
        ctx.deferred_emits
            .borrow_mut()
            .push(Box::new(move |dctx: &mut DeferredEmitCtx<'_>| {
                let asm = &mut *dctx.asm;
                asm.bind(&abort).unwrap();
                emit_call_to_offset(asm, wrapped_fn_off);

                let resume_off = asm.size();
                let inst_rip = dctx.code_base + mov_off as u64;
                let resume_rip = dctx.code_base + resume_off as u64;
                let stub_rip = dctx.code_base + wrapped_fn_off as u64;
                dctx.fastmem_patches.add(
                    inst_rip,
                    FastmemPatchInfo::new(resume_rip, stub_rip, Some(marker), recompile),
                );

                asm.jmp(&end, JmpType::Near).unwrap();
            }));
    } else {
        // Path 3: page table.
        debug_assert!(mem_conf.page_table_present);
        let dest_ptr = emit_vaddr_lookup_a64(ra, ctx, BITSIZE, abort, vaddr);
        let _mov_off = emit_write_memory_mov::<BITSIZE>(ra.asm, dest_ptr, value_idx, ordered);

        ctx.deferred_emits
            .borrow_mut()
            .push(Box::new(move |dctx: &mut DeferredEmitCtx<'_>| {
                let asm = &mut *dctx.asm;
                asm.bind(&abort).unwrap();
                emit_call_to_offset(asm, wrapped_fn_off);
                asm.jmp(&end, JmpType::Near).unwrap();
            }));
    }

    ra.asm.bind(&end).unwrap();
}

// ---------------------------------------------------------------------------
// Per-bitsize dispatcher wrappers (mirror upstream
// `A64EmitX64::EmitA64ReadMemory{8,16,32,64}` /
// `A64EmitX64::EmitA64WriteMemory{8,16,32,64}` one-line forwarders).
// ---------------------------------------------------------------------------

pub fn emit_a64_read_memory_8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a64_memory_read::<8>(ctx, ra, inst_ref, inst);
}
pub fn emit_a64_read_memory_16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a64_memory_read::<16>(ctx, ra, inst_ref, inst);
}
pub fn emit_a64_read_memory_32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a64_memory_read::<32>(ctx, ra, inst_ref, inst);
}
pub fn emit_a64_read_memory_64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a64_memory_read::<64>(ctx, ra, inst_ref, inst);
}

pub fn emit_a64_write_memory_8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a64_memory_write::<8>(ctx, ra, inst_ref, inst);
}
pub fn emit_a64_write_memory_16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a64_memory_write::<16>(ctx, ra, inst_ref, inst);
}
pub fn emit_a64_write_memory_32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a64_memory_write::<32>(ctx, ra, inst_ref, inst);
}
pub fn emit_a64_write_memory_64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a64_memory_write::<64>(ctx, ra, inst_ref, inst);
}

// `JmpType` is referenced by the deferred-emit closures above.
#[allow(dead_code)]
fn _suppress_unused(_: JmpType) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::x64::callback::ArgCallback;
    use std::sync::atomic::{AtomicU64, Ordering};

    const PROBE_CONTEXT: u64 = 0x1234_5678_9ABC_DEF0;
    static WRITE_CONTEXT: AtomicU64 = AtomicU64::new(0);
    static WRITE_VADDR: AtomicU64 = AtomicU64::new(0);
    static WRITE_VALUE: AtomicU64 = AtomicU64::new(0);

    extern "C" fn dummy_read(_ctx: u64, _vaddr: u64) -> u64 {
        0
    }
    extern "C" fn dummy_write(_ctx: u64, _vaddr: u64, _val: u64) {}
    extern "C" fn dummy_raw_write(_ctx: u64, _vaddr: u64, _value: u64, _expected: u64) -> u64 {
        1
    }
    extern "C" fn dummy_raw_write_128(
        _ctx: u64,
        _vaddr: u64,
        _value: *const [u64; 2],
        _expected: *const [u64; 2],
    ) -> u64 {
        1
    }
    extern "C" fn probe_read(ctx: u64, vaddr: u64) -> u64 {
        ctx ^ vaddr
    }
    extern "C" fn probe_write(ctx: u64, vaddr: u64, value: u64) {
        WRITE_CONTEXT.store(ctx, Ordering::SeqCst);
        WRITE_VADDR.store(vaddr, Ordering::SeqCst);
        WRITE_VALUE.store(value, Ordering::SeqCst);
    }

    fn dummy_callbacks() -> EmitCallbacks {
        let mk_arg = || -> Box<dyn crate::backend::x64::callback::Callback> {
            Box::new(ArgCallback::new(dummy_read as u64, 0))
        };
        let mk_arg_w = || -> Box<dyn crate::backend::x64::callback::Callback> {
            Box::new(ArgCallback::new(dummy_write as u64, 0))
        };
        EmitCallbacks {
            memory_read_8: mk_arg(),
            memory_read_16: mk_arg(),
            memory_read_32: mk_arg(),
            memory_read_64: mk_arg(),
            memory_read_128: mk_arg(),
            memory_write_8: mk_arg_w(),
            memory_write_16: mk_arg_w(),
            memory_write_32: mk_arg_w(),
            memory_write_64: mk_arg_w(),
            memory_write_128: mk_arg_w(),
            call_supervisor: mk_arg(),
            interpreter_fallback: mk_arg(),
            exception_raised: mk_arg(),
            data_cache_operation: mk_arg(),
            instruction_cache_operation: mk_arg(),
            instruction_synchronization_barrier: mk_arg(),
            add_ticks: mk_arg(),
            get_ticks_remaining: mk_arg(),
            exclusive_clear: mk_arg(),
            exclusive_read_8: mk_arg(),
            exclusive_read_16: mk_arg(),
            exclusive_read_32: mk_arg(),
            exclusive_read_64: mk_arg(),
            exclusive_read_128: mk_arg(),
            get_cntpct: mk_arg(),
            exclusive_write_8: mk_arg(),
            exclusive_write_16: mk_arg(),
            exclusive_write_32: mk_arg(),
            exclusive_write_64: mk_arg(),
            exclusive_write_128: mk_arg(),
        }
    }

    fn dummy_raw_callbacks() -> RawExclusiveWriteCallbacks {
        let callback = || -> Box<dyn crate::backend::x64::callback::Callback> {
            Box::new(ArgCallback::new(dummy_raw_write as usize as u64, 0))
        };
        RawExclusiveWriteCallbacks {
            write_8: callback(),
            write_16: callback(),
            write_32: callback(),
            write_64: callback(),
            write_128: Box::new(ArgCallback::new(dummy_raw_write_128 as usize as u64, 0)),
        }
    }

    /// Verify the table is fully populated: every (ordered, bitsize,
    /// vaddr_idx, value_idx) combination in the valid space has both a
    /// read and a write entry.
    #[test]
    fn test_gen_fastmem_fallbacks_full_population() {
        // Need a large code buffer — 3136 stubs × ~50 bytes ≈ 150 KB.
        let mut asm = CodeAssembler::new(2 * 1024 * 1024).unwrap();
        let callbacks = dummy_callbacks();
        let table = gen_fastmem_fallbacks(&mut asm, &callbacks, None);

        let mut expected = 0;
        for &_o in &[false, true] {
            for &_v in &VALID_GPR_IDXES {
                for &_w in &VALID_GPR_IDXES {
                    for &_b in &[8usize, 16, 32, 64] {
                        expected += 1;
                    }
                }
            }
        }
        let expected_128 = 2 * VALID_GPR_IDXES.len() * 16;
        assert_eq!(table.read.len(), expected + expected_128);
        assert_eq!(table.write.len(), expected);
    }

    /// Verify offsets are unique and strictly monotonic — the stubs
    /// don't accidentally point at each other or overlap.
    #[test]
    fn test_gen_fastmem_fallbacks_unique_offsets() {
        let mut asm = CodeAssembler::new(2 * 1024 * 1024).unwrap();
        let callbacks = dummy_callbacks();
        let table = gen_fastmem_fallbacks(&mut asm, &callbacks, None);

        let mut all_offsets: Vec<usize> = table
            .read
            .values()
            .chain(table.write.values())
            .copied()
            .collect();
        all_offsets.sort();
        let unique_count = {
            let mut last = None;
            let mut n = 0usize;
            for &o in &all_offsets {
                if Some(o) != last {
                    n += 1;
                    last = Some(o);
                }
            }
            n
        };
        assert_eq!(
            unique_count,
            all_offsets.len(),
            "stub offsets must be unique"
        );
    }

    /// Verify each stub is non-empty (at minimum a `ret`, plus push/pop
    /// pairs, plus a call).
    #[test]
    fn test_gen_fastmem_fallbacks_stubs_nonempty() {
        let mut asm = CodeAssembler::new(2 * 1024 * 1024).unwrap();
        let callbacks = dummy_callbacks();
        let table = gen_fastmem_fallbacks(&mut asm, &callbacks, None);

        // Pick one specific stub and check it has reasonable size.
        let off_a = table.read_stub(false, 32, 0, 0);
        let off_b = table.write_stub(false, 32, 0, 0);
        // Both stubs in the same emit; the read fires first, write
        // immediately after. So write_offset > read_offset and the
        // gap is at least the size of a useful stub.
        assert!(off_b > off_a);
        assert!(
            off_b - off_a >= 16,
            "stubs should be at least one push+ret apart"
        );
    }

    #[test]
    fn test_fastmem_fallbacks_use_host_abi_parameter_registers() {
        let mut asm = CodeAssembler::new(2 * 1024 * 1024).unwrap();
        asm.set_protect_mode_rwe().unwrap();
        let mut callbacks = dummy_callbacks();
        callbacks.memory_read_32 = Box::new(ArgCallback::new(
            probe_read as *const () as usize as u64,
            PROBE_CONTEXT,
        ));
        callbacks.memory_write_32 = Box::new(ArgCallback::new(
            probe_write as *const () as usize as u64,
            PROBE_CONTEXT,
        ));
        let table = gen_fastmem_fallbacks(&mut asm, &callbacks, None);

        let vaddr_idx = abi::ABI_PARAMS[1].to_reg64().get_idx();
        let value_idx = abi::ABI_PARAMS[2].to_reg64().get_idx();
        let read_offset = table.read_stub(false, 32, vaddr_idx, RAX.get_idx());
        let write_offset = table.write_stub(false, 32, vaddr_idx, value_idx);
        let read: unsafe extern "C" fn(u64, u64) -> u64 =
            unsafe { core::mem::transmute(asm.top().add(read_offset)) };
        let write: unsafe extern "C" fn(u64, u64, u64) =
            unsafe { core::mem::transmute(asm.top().add(write_offset)) };

        let vaddr = 0xA1B2_C3D4;
        assert_eq!(
            unsafe { read(0, vaddr) },
            (PROBE_CONTEXT ^ vaddr) & u32::MAX as u64
        );

        let value = 0xFFFF_FFFF_7654_3210;
        unsafe { write(0, vaddr, value) };
        assert_eq!(WRITE_CONTEXT.load(Ordering::SeqCst), PROBE_CONTEXT);
        assert_eq!(WRITE_VADDR.load(Ordering::SeqCst), vaddr);
        assert_eq!(WRITE_VALUE.load(Ordering::SeqCst), value & u32::MAX as u64);
    }

    #[test]
    fn test_exclusive_fastmem_fallbacks_cover_all_widths() {
        let mut asm = CodeAssembler::new(4 * 1024 * 1024).unwrap();
        let callbacks = dummy_callbacks();
        let raw_callbacks = dummy_raw_callbacks();
        let table = gen_fastmem_fallbacks(&mut asm, &callbacks, Some(&raw_callbacks));

        let expected_scalar = 2 * VALID_GPR_IDXES.len() * VALID_GPR_IDXES.len() * 4;
        let expected_128 = 2 * VALID_GPR_IDXES.len() * 16;
        assert_eq!(table.exclusive_write.len(), expected_scalar + expected_128);
        for bitsize in [8usize, 16, 32, 64, 128] {
            let value_idx = if bitsize == 128 { 15 } else { 14 };
            assert!(
                table
                    .exclusive_write
                    .contains_key(&(true, bitsize, 0, value_idx)),
                "missing exclusive fallback for {} bits",
                bitsize
            );
        }
    }
}

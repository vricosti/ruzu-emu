use rxbyak::{byte_ptr, dword_ptr, qword_ptr};
use rxbyak::{CodeAssembler, JmpType, Label, RegExp};
use rxbyak::{R15, RAX, RBX, RCX, RSP};

use crate::backend::x64::block_of_code::{FORCE_RETURN, STACK_LAYOUT_RSP_OFFSET};
use crate::backend::x64::emit_context::EmitContext;
use crate::backend::x64::emit_data_processing::load_nzcv_into_flags;
use crate::backend::x64::patch_info::{
    PatchEntry, PatchType, A32_PATCH_JG_SIZE, A32_PATCH_JMP_SIZE, A32_PATCH_JZ_SIZE,
    A64_PATCH_JG_SIZE, A64_PATCH_JMP_SIZE, A64_PATCH_JZ_SIZE,
};
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::backend::x64::stack_layout::StackLayout;
use crate::ir::cond::Cond;
use crate::ir::terminal::Terminal;

// ---------------------------------------------------------------------------
// Terminal dispatch
// ---------------------------------------------------------------------------

/// Emit code for a block terminal.
///
/// Terminals define control flow at the end of a basic block.
/// When dispatcher_offsets is set in the context, terminals jump to the
/// appropriate return_from_run_code entry point. Otherwise (unit tests),
/// they emit inline add_ticks + ret.
pub fn emit_terminal(ctx: &EmitContext, ra: &mut RegAlloc, terminal: &Terminal) {
    match terminal {
        Terminal::Invalid => {
            // Should never reach an invalid terminal at runtime — emit int3
            ra.asm.int3().unwrap();
        }

        Terminal::ReturnToDispatch => {
            emit_terminal_return_to_dispatch(ctx, ra);
        }

        Terminal::LinkBlock { next } => {
            emit_terminal_link_block(ctx, ra, *next);
        }

        Terminal::LinkBlockFast { next } => {
            emit_terminal_link_block_fast(ctx, ra, *next);
        }

        Terminal::PopRSBHint => {
            // Check halt_reason before entering the RSB hot path.
            // Upstream wraps these terminals in CheckHalt at the IR level;
            // we enforce it at the emitter level as a safety net.
            let halt_offset = ctx.jit_state_info.offsetof_halt_reason;
            ra.asm
                .cmp(dword_ptr(RegExp::from(R15) + halt_offset as i32), 0i32)
                .unwrap();
            if let Some(offsets) = ctx.dispatcher_offsets {
                let halt_label = ra.asm.create_label();
                ra.asm.jnz(&halt_label, JmpType::Near).unwrap();
                emit_terminal_pop_rsb_hint(ctx, ra);
                ra.asm.bind(&halt_label).unwrap();
                emit_jmp_to_offset(ra.asm, offsets[FORCE_RETURN], ctx.code_base_ptr);
            } else {
                emit_terminal_pop_rsb_hint(ctx, ra);
            }
        }

        Terminal::FastDispatchHint => {
            let halt_offset = ctx.jit_state_info.offsetof_halt_reason;
            ra.asm
                .cmp(dword_ptr(RegExp::from(R15) + halt_offset as i32), 0i32)
                .unwrap();
            if let Some(offsets) = ctx.dispatcher_offsets {
                let halt_label = ra.asm.create_label();
                ra.asm.jnz(&halt_label, JmpType::Near).unwrap();
                emit_terminal_fast_dispatch_hint(ctx, ra);
                ra.asm.bind(&halt_label).unwrap();
                emit_jmp_to_offset(ra.asm, offsets[FORCE_RETURN], ctx.code_base_ptr);
            } else {
                emit_terminal_fast_dispatch_hint(ctx, ra);
            }
        }

        Terminal::If { cond, then_, else_ } => {
            emit_terminal_if(ctx, ra, *cond, then_, else_);
        }

        Terminal::CheckBit { then_, else_ } => {
            emit_terminal_check_bit(ctx, ra, then_, else_);
        }

        Terminal::CheckHalt { else_ } => {
            emit_terminal_check_halt(ctx, ra, else_);
        }
    }
}

// ---------------------------------------------------------------------------
// Architecture-aware helpers
// ---------------------------------------------------------------------------

/// Emit: store the target PC into JitState and (for A32) update
/// upper_location_descriptor if it differs from the current block's.
fn emit_set_pc(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    next: crate::ir::location::LocationDescriptor,
) {
    let pc = ctx.arch.extract_pc(next);
    let pc_offset = ctx.arch.pc_offset();

    if ctx.arch.pc_width() == 4 {
        // A32: 32-bit PC stored in reg[15]
        ra.asm
            .mov(dword_ptr(RegExp::from(R15) + pc_offset as i32), pc as i32)
            .unwrap();
    } else {
        // A64: 64-bit PC stored in JitState.pc
        ra.asm.mov(RAX, pc as i64).unwrap();
        ra.asm
            .mov(qword_ptr(RegExp::from(R15) + pc_offset as i32), RAX)
            .unwrap();
    }

    // A32: update upper_location_descriptor if changed
    if let Some(upper_offset) = ctx.arch.upper_location_descriptor_offset() {
        let new_masked = masked_a32_upper_location_descriptor(ctx, next);
        let old_masked = masked_a32_upper_location_descriptor(ctx, ctx.location);
        if new_masked != old_masked {
            ra.asm
                .mov(
                    dword_ptr(RegExp::from(R15) + upper_offset as i32),
                    new_masked as i32,
                )
                .unwrap();
        }
    }
}

fn emit_store_pc(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    next: crate::ir::location::LocationDescriptor,
) {
    let pc = ctx.arch.extract_pc(next);
    let pc_offset = ctx.arch.pc_offset();

    if ctx.arch.pc_width() == 4 {
        ra.asm
            .mov(dword_ptr(RegExp::from(R15) + pc_offset as i32), pc as i32)
            .unwrap();
    } else {
        ra.asm.mov(RAX, pc as i64).unwrap();
        ra.asm
            .mov(qword_ptr(RegExp::from(R15) + pc_offset as i32), RAX)
            .unwrap();
    }
}

fn emit_store_pc_raw(
    asm: &mut CodeAssembler,
    ctx: &EmitContext,
    next: crate::ir::location::LocationDescriptor,
) {
    let pc = ctx.arch.extract_pc(next);
    let pc_offset = ctx.arch.pc_offset();

    if ctx.arch.pc_width() == 4 {
        asm.mov(dword_ptr(RegExp::from(R15) + pc_offset as i32), pc as i32)
            .unwrap();
    } else {
        asm.mov(RAX, pc as i64).unwrap();
        asm.mov(qword_ptr(RegExp::from(R15) + pc_offset as i32), RAX)
            .unwrap();
    }
}

fn emit_set_upper_location_descriptor(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    next: crate::ir::location::LocationDescriptor,
) {
    let Some(upper_offset) = ctx.arch.upper_location_descriptor_offset() else {
        return;
    };

    let new_masked = masked_a32_upper_location_descriptor(ctx, next);
    let old_masked = masked_a32_upper_location_descriptor(ctx, ctx.location);
    if new_masked != old_masked {
        ra.asm
            .mov(
                dword_ptr(RegExp::from(R15) + upper_offset as i32),
                new_masked as i32,
            )
            .unwrap();
    }
}

fn masked_a32_upper_location_descriptor(
    ctx: &EmitContext,
    loc: crate::ir::location::LocationDescriptor,
) -> u32 {
    let upper = ctx.arch.extract_upper_location_descriptor(loc) & !4;
    if ctx.arch.is_a32() {
        // Upstream A32 x64 emitter masks E when always_little_endian is active.
        // ruzu only targets little-endian Switch execution, so match that behavior here.
        upper & !2
    } else {
        upper
    }
}

// ---------------------------------------------------------------------------
// ReturnToDispatch: return control to the host dispatcher
// ---------------------------------------------------------------------------

/// Emit: jump to return_from_run_code[0] (dispatcher re-entry).
///
/// When dispatcher_offsets is available, emits a jmp to the dispatcher.
/// Otherwise falls back to inline add_ticks + ret for unit tests.
fn emit_terminal_return_to_dispatch(ctx: &EmitContext, ra: &mut RegAlloc) {
    if let Some(offsets) = ctx.dispatcher_offsets {
        emit_jmp_to_offset(ra.asm, offsets[0], ctx.code_base_ptr);
    } else {
        // Fallback for unit tests (no dispatcher)
        if ctx.config.enable_cycle_counting {
            emit_add_ticks(ctx, ra);
        }
        ra.asm.ret().unwrap();
    }
}

// ---------------------------------------------------------------------------
// LinkBlock: set PC and return to dispatch
// ---------------------------------------------------------------------------

/// Emit: set PC to next, check cycles/halt inline, jump to dispatcher or direct link.
fn emit_terminal_link_block(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    next: crate::ir::location::LocationDescriptor,
) {
    if let Some(offsets) = ctx.dispatcher_offsets {
        let use_linking = ctx.enable_block_linking && !ctx.is_single_step;

        if ctx.arch.is_a32() {
            emit_set_upper_location_descriptor(ctx, ra, next);

            if !use_linking {
                emit_store_pc(ctx, ra, next);
                emit_jmp_to_offset(ra.asm, offsets[0], ctx.code_base_ptr);
                return;
            }

            if ctx.config.enable_cycle_counting {
                let cycles_offset =
                    STACK_LAYOUT_RSP_OFFSET + StackLayout::cycles_remaining_offset();
                ra.asm
                    .cmp(qword_ptr(RegExp::from(RSP) + cycles_offset as i32), 0i32)
                    .unwrap();

                let patch_offset = ra.asm.size();
                ctx.patch_entries.borrow_mut().push(PatchEntry {
                    target: next,
                    patch_type: PatchType::Jg,
                    code_offset: patch_offset,
                });

                let target_ptr = ctx.block_lookup.as_ref().and_then(|lookup| lookup(next));
                emit_patch_jg_a32(ra.asm, next, target_ptr, offsets, ctx.code_base_ptr, ctx);
            } else {
                let halt_offset = ctx.jit_state_info.offsetof_halt_reason;
                ra.asm
                    .cmp(dword_ptr(RegExp::from(R15) + halt_offset as i32), 0i32)
                    .unwrap();

                let patch_offset = ra.asm.size();
                ctx.patch_entries.borrow_mut().push(PatchEntry {
                    target: next,
                    patch_type: PatchType::Jz,
                    code_offset: patch_offset,
                });

                let target_ptr = ctx.block_lookup.as_ref().and_then(|lookup| lookup(next));
                emit_patch_jz_a32(ra.asm, next, target_ptr, offsets, ctx.code_base_ptr, ctx);
            }

            emit_store_pc(ctx, ra, next);
            emit_push_rsb_terminal(ctx, ra.asm, next);
            emit_jmp_to_offset(ra.asm, offsets[FORCE_RETURN], ctx.code_base_ptr);
            return;
        }

        // Upstream A64 `EmitTerminalImpl(LinkBlock)`: with block linking the PC is stored
        // only by the unlinked patch slot and by the fall-through to ForceReturnFromRunCode,
        // never on the linked path.
        if !use_linking {
            emit_set_pc(ctx, ra, next);
        }
        let target_pc = ctx.arch.extract_pc(next);
        let pc_offset = ctx.arch.pc_offset();

        if ctx.config.enable_cycle_counting {
            // Check cycles_remaining > 0
            let cycles_offset = STACK_LAYOUT_RSP_OFFSET + StackLayout::cycles_remaining_offset();
            ra.asm
                .cmp(qword_ptr(RegExp::from(RSP) + cycles_offset as i32), 0i32)
                .unwrap();

            if use_linking {
                // Record patch slot offset and emit patchable jg slot
                let patch_offset = ra.asm.size();
                ctx.patch_entries.borrow_mut().push(PatchEntry {
                    target: next,
                    patch_type: PatchType::Jg,
                    code_offset: patch_offset,
                });

                // Look up target in cache
                let target_ptr = ctx.block_lookup.as_ref().and_then(|lookup| lookup(next));

                emit_patch_jg(ra.asm, target_pc, pc_offset, target_ptr, offsets, ctx.code_base_ptr);
                emit_set_pc(ctx, ra, next);
            } else {
                let budget_exhausted = ra.asm.create_label();
                ra.asm.jle(&budget_exhausted, JmpType::Near).unwrap();

                // Cycles remain: return to dispatch for next block lookup
                emit_jmp_to_offset(ra.asm, offsets[0], ctx.code_base_ptr);

                // Budget exhausted: force return
                ra.asm.bind(&budget_exhausted).unwrap();
            }
            emit_jmp_to_offset(ra.asm, offsets[FORCE_RETURN], ctx.code_base_ptr);
        } else {
            // No cycle counting: check halt_reason
            let halt_offset = ctx.jit_state_info.offsetof_halt_reason;
            ra.asm
                .cmp(dword_ptr(RegExp::from(R15) + halt_offset as i32), 0i32)
                .unwrap();

            if use_linking {
                // Record patch slot offset and emit patchable jz slot
                let patch_offset = ra.asm.size();
                ctx.patch_entries.borrow_mut().push(PatchEntry {
                    target: next,
                    patch_type: PatchType::Jz,
                    code_offset: patch_offset,
                });

                // Look up target in cache
                let target_ptr = ctx.block_lookup.as_ref().and_then(|lookup| lookup(next));

                emit_patch_jz(ra.asm, target_pc, pc_offset, target_ptr, offsets, ctx.code_base_ptr);
                emit_set_pc(ctx, ra, next);
            } else {
                let halted = ra.asm.create_label();
                ra.asm.jnz(&halted, JmpType::Near).unwrap();

                // Not halted: normal dispatch
                emit_jmp_to_offset(ra.asm, offsets[0], ctx.code_base_ptr);

                // Halted: force return
                ra.asm.bind(&halted).unwrap();
            }
            emit_jmp_to_offset(ra.asm, offsets[FORCE_RETURN], ctx.code_base_ptr);
        }
    } else {
        // Fallback for unit tests
        if ctx.config.enable_cycle_counting {
            let cycles_offset = STACK_LAYOUT_RSP_OFFSET + StackLayout::cycles_remaining_offset();
            let halt_label = ra.asm.create_label();
            ra.asm
                .cmp(qword_ptr(RegExp::from(RSP) + cycles_offset as i32), 0i32)
                .unwrap();
            ra.asm.jle(&halt_label, JmpType::Near).unwrap();

            emit_add_ticks(ctx, ra);
            ra.asm.ret().unwrap();

            ra.asm.bind(&halt_label).unwrap();
            emit_add_ticks(ctx, ra);
            ra.asm.ret().unwrap();
        } else {
            let halt_offset = ctx.jit_state_info.offsetof_halt_reason;
            let halt_label = ra.asm.create_label();
            ra.asm
                .cmp(dword_ptr(RegExp::from(R15) + halt_offset as i32), 0i32)
                .unwrap();
            ra.asm.jnz(&halt_label, JmpType::Near).unwrap();

            ra.asm.ret().unwrap();

            ra.asm.bind(&halt_label).unwrap();
            ra.asm.ret().unwrap();
        }
    }
}

// ---------------------------------------------------------------------------
// LinkBlockFast: unconditional jump to next block
// ---------------------------------------------------------------------------

/// Emit: set PC to next, return to dispatch or direct link (unconditional).
fn emit_terminal_link_block_fast(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    next: crate::ir::location::LocationDescriptor,
) {
    if let Some(offsets) = ctx.dispatcher_offsets {
        let use_linking = ctx.enable_block_linking && !ctx.is_single_step;

        if ctx.arch.is_a32() {
            emit_set_upper_location_descriptor(ctx, ra, next);

            // When cycle counting is disabled (multicore/wall-clock mode), we must
            // check halt_reason even in fast-linked blocks. Without this, tight
            // loops (e.g. WFE spin loops) never check the external halt flag set by
            // the preemption timer, causing the JIT to spin forever and starve other
            // threads on the same core.
            if !ctx.config.enable_cycle_counting {
                let halt_offset = ctx.jit_state_info.offsetof_halt_reason;
                ra.asm
                    .cmp(dword_ptr(RegExp::from(R15) + halt_offset as i32), 0i32)
                    .unwrap();
                let not_halted = ra.asm.create_label();
                ra.asm.jz(&not_halted, JmpType::Near).unwrap();
                // Halted: force return to host
                emit_store_pc(ctx, ra, next);
                emit_jmp_to_offset(ra.asm, offsets[FORCE_RETURN], ctx.code_base_ptr);
                ra.asm.bind(&not_halted).unwrap();
            }

            if use_linking {
                let patch_offset = ra.asm.size();
                ctx.patch_entries.borrow_mut().push(PatchEntry {
                    target: next,
                    patch_type: PatchType::Jmp,
                    code_offset: patch_offset,
                });

                let target_ptr = ctx.block_lookup.as_ref().and_then(|lookup| lookup(next));
                emit_patch_jmp_a32(ra.asm, next, target_ptr, offsets, ctx.code_base_ptr, ctx);
            } else {
                emit_store_pc(ctx, ra, next);
                emit_jmp_to_offset(ra.asm, offsets[0], ctx.code_base_ptr);
            }
            return;
        }

        // Upstream A64 `EmitTerminalImpl(LinkBlockFast)` stores the PC only in the unlinked
        // patch slot. ruzu divergence (kept): without cycle counting the link also checks
        // halt_reason, so loops closed by unconditional B/BL links (e.g. `ldr; b loop`) still
        // see an external halt (test `external_halt_stops_a_running_direct_link_loop`); the
        // not-taken `jnz` keeps the linked path straight, and the PC is stored only when halted.
        let halted = ra.asm.create_label();
        let check_halt = use_linking && !ctx.config.enable_cycle_counting;
        if check_halt {
            let halt_offset = ctx.jit_state_info.offsetof_halt_reason;
            ra.asm
                .cmp(dword_ptr(RegExp::from(R15) + halt_offset as i32), 0i32)
                .unwrap();
            ra.asm.jnz(&halted, JmpType::Near).unwrap();
        }
        if use_linking {
            // Record patch slot offset and emit patchable jmp slot
            let patch_offset = ra.asm.size();
            ctx.patch_entries.borrow_mut().push(PatchEntry {
                target: next,
                patch_type: PatchType::Jmp,
                code_offset: patch_offset,
            });

            // Look up target in cache
            let target_ptr = ctx.block_lookup.as_ref().and_then(|lookup| lookup(next));

            emit_patch_jmp(
                ra.asm,
                ctx.arch.extract_pc(next),
                ctx.arch.pc_offset(),
                target_ptr,
                offsets,
                ctx.code_base_ptr,
            );
            if check_halt {
                ra.asm.bind(&halted).unwrap();
                emit_set_pc(ctx, ra, next);
                emit_jmp_to_offset(ra.asm, offsets[FORCE_RETURN], ctx.code_base_ptr);
            }
        } else {
            emit_set_pc(ctx, ra, next);
            emit_jmp_to_offset(ra.asm, offsets[0], ctx.code_base_ptr);
        }
    } else {
        if ctx.config.enable_cycle_counting {
            emit_add_ticks(ctx, ra);
        }
        ra.asm.ret().unwrap();
    }
}

// ---------------------------------------------------------------------------
// If: conditional branch between two sub-terminals
// ---------------------------------------------------------------------------

/// Emit: load NZCV, branch on ARM condition, emit both sub-terminals.
fn emit_terminal_if(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    cond: Cond,
    then_: &Terminal,
    else_: &Terminal,
) {
    match cond {
        Cond::AL | Cond::NV => {
            emit_terminal(ctx, ra, then_);
            return;
        }
        _ => {}
    }

    load_nzcv_into_flags(ra, cond, ctx.jit_state_info.offsetof_cpsr_nzcv);

    let pass_label = ra.asm.create_label();
    emit_jcc(ra.asm, cond, &pass_label);

    emit_terminal(ctx, ra, else_);

    ra.asm.bind(&pass_label).unwrap();
    emit_terminal(ctx, ra, then_);
}

// ---------------------------------------------------------------------------
// CheckBit: branch on stack check_bit value
// ---------------------------------------------------------------------------

/// Emit: check stack_layout.check_bit, branch on result.
fn emit_terminal_check_bit(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    then_: &Terminal,
    else_: &Terminal,
) {
    let check_bit_offset = STACK_LAYOUT_RSP_OFFSET + StackLayout::check_bit_offset();
    let fail_label = ra.asm.create_label();

    ra.asm
        .cmp(byte_ptr(RegExp::from(RSP) + check_bit_offset as i32), 0i32)
        .unwrap();
    ra.asm.jz(&fail_label, JmpType::Near).unwrap();

    emit_terminal(ctx, ra, then_);

    ra.asm.bind(&fail_label).unwrap();
    emit_terminal(ctx, ra, else_);
}

// ---------------------------------------------------------------------------
// CheckHalt: check halt_reason, force return if halted
// ---------------------------------------------------------------------------

/// Emit: if halt_reason != 0, force return to host; otherwise emit else_.
fn emit_terminal_check_halt(ctx: &EmitContext, ra: &mut RegAlloc, else_: &Terminal) {
    let halt_offset = ctx.jit_state_info.offsetof_halt_reason;
    let halt_label = ra.asm.create_label();

    ra.asm
        .cmp(dword_ptr(RegExp::from(R15) + halt_offset as i32), 0i32)
        .unwrap();
    ra.asm.jnz(&halt_label, JmpType::Near).unwrap();

    emit_terminal(ctx, ra, else_);

    ra.asm.bind(&halt_label).unwrap();
    if let Some(offsets) = ctx.dispatcher_offsets {
        emit_jmp_to_offset(ra.asm, offsets[FORCE_RETURN], ctx.code_base_ptr);
    } else {
        if ctx.config.enable_cycle_counting {
            emit_add_ticks(ctx, ra);
        }
        ra.asm.ret().unwrap();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Emit a conditional jump for an ARM condition code.
fn emit_jcc(asm: &mut CodeAssembler, cond: Cond, label: &Label) {
    let t = JmpType::Near;
    match cond {
        Cond::EQ => asm.jz(label, t),
        Cond::NE => asm.jnz(label, t),
        Cond::CS => asm.jc(label, t),
        Cond::CC => asm.jnc(label, t),
        Cond::MI => asm.js(label, t),
        Cond::PL => asm.jns(label, t),
        Cond::VS => asm.jo(label, t),
        Cond::VC => asm.jno(label, t),
        Cond::HI => asm.ja(label, t),
        Cond::LS => asm.jbe(label, t),
        Cond::GE => asm.jge(label, t),
        Cond::LT => asm.jl(label, t),
        Cond::GT => asm.jg(label, t),
        Cond::LE => asm.jle(label, t),
        Cond::AL | Cond::NV => asm.jmp(label, t),
    }
    .unwrap();
}

// ---------------------------------------------------------------------------
// PopRSBHint: jump to RSB handler or fall back to dispatch
// ---------------------------------------------------------------------------

fn emit_terminal_pop_rsb_hint(ctx: &EmitContext, ra: &mut RegAlloc) {
    if ctx.enable_rsb && !ctx.is_single_step {
        if let Some(handler_offset) = ctx.terminal_handler_pop_rsb_hint {
            if let Some(offsets) = ctx.dispatcher_offsets {
                let _ = offsets; // used indirectly by the handler
                emit_jmp_to_offset(ra.asm, handler_offset, ctx.code_base_ptr);
                return;
            }
        }
    }
    // Fallback: just dispatch normally
    emit_terminal_return_to_dispatch(ctx, ra);
}

// ---------------------------------------------------------------------------
// FastDispatchHint: jump to fast dispatch handler or fall back to dispatch
// ---------------------------------------------------------------------------

fn emit_terminal_fast_dispatch_hint(ctx: &EmitContext, ra: &mut RegAlloc) {
    if ctx.enable_fast_dispatch && !ctx.is_single_step {
        if let Some(handler_offset) = ctx.terminal_handler_fast_dispatch_hint {
            if let Some(offsets) = ctx.dispatcher_offsets {
                let _ = offsets;
                emit_jmp_to_offset(ra.asm, handler_offset, ctx.code_base_ptr);
                return;
            }
        }
    }
    // Fallback: just dispatch normally
    emit_terminal_return_to_dispatch(ctx, ra);
}

// ---------------------------------------------------------------------------
// Patch slot emitters for block linking
// ---------------------------------------------------------------------------

/// Emit an A64 patchable link slot, as upstream `A64EmitX64::EmitPatchJg/Jz/Jmp`.
///
/// Linked (`target_ptr` is Some): `jcc <target>`. Unlinked: `mov rax, pc;
/// mov [r15 + pc], rax; jcc return_from_run_code[0]`. Padded with NOPs to
/// `slot_size` (upstream `EnsurePatchLocationSize`). `jcc_opcode` is the second
/// byte of a `0F xx` rel32 conditional jump, or None for `jmp rel32`.
pub(crate) fn emit_patch_a64(
    asm: &mut CodeAssembler,
    jcc_opcode: Option<u8>,
    slot_size: usize,
    target_pc: u64,
    pc_offset: usize,
    target_ptr: Option<*const u8>,
    offsets: [usize; 4],
    code_base: *const u8,
) {
    let begin = asm.size();
    let target = match target_ptr {
        Some(ptr) => ptr as usize,
        None => {
            asm.mov(RAX, target_pc as i64).unwrap();
            asm.mov(qword_ptr(RegExp::from(R15) + pc_offset as i32), RAX)
                .unwrap();
            code_base as usize + offsets[0]
        }
    };
    let insn_len = if jcc_opcode.is_some() { 6 } else { 5 };
    let end_addr = code_base as usize + asm.size() + insn_len;
    let disp = (target as i64) - (end_addr as i64);
    match jcc_opcode {
        Some(op) => {
            asm.db(0x0F).unwrap();
            asm.db(op).unwrap();
        }
        None => asm.db(0xE9).unwrap(),
    }
    asm.dd(disp as u32).unwrap();
    let used = asm.size() - begin;
    assert!(used <= slot_size, "A64 patch slot overflow: {used} > {slot_size}");
    for _ in used..slot_size {
        asm.nop().unwrap();
    }
}

fn emit_patch_jg(
    asm: &mut CodeAssembler,
    target_pc: u64,
    pc_offset: usize,
    target_ptr: Option<*const u8>,
    offsets: [usize; 4],
    code_base: *const u8,
) {
    emit_patch_a64(asm, Some(0x8F), A64_PATCH_JG_SIZE, target_pc, pc_offset, target_ptr, offsets, code_base);
}

fn emit_patch_jz(
    asm: &mut CodeAssembler,
    target_pc: u64,
    pc_offset: usize,
    target_ptr: Option<*const u8>,
    offsets: [usize; 4],
    code_base: *const u8,
) {
    emit_patch_a64(asm, Some(0x84), A64_PATCH_JZ_SIZE, target_pc, pc_offset, target_ptr, offsets, code_base);
}

fn emit_patch_jmp(
    asm: &mut CodeAssembler,
    target_pc: u64,
    pc_offset: usize,
    target_ptr: Option<*const u8>,
    offsets: [usize; 4],
    code_base: *const u8,
) {
    emit_patch_a64(asm, None, A64_PATCH_JMP_SIZE, target_pc, pc_offset, target_ptr, offsets, code_base);
}

fn emit_patch_jg_a32(
    asm: &mut CodeAssembler,
    target_loc: crate::ir::location::LocationDescriptor,
    target_ptr: Option<*const u8>,
    offsets: [usize; 4],
    code_base: *const u8,
    ctx: &EmitContext,
) {
    let begin = asm.size();
    if let Some(ptr) = target_ptr {
        let target = ptr as usize;
        let jg_end = begin + 6;
        let jg_end_addr = code_base as usize + jg_end;
        let disp = (target as i64) - (jg_end_addr as i64);
        asm.db(0x0F).unwrap();
        asm.db(0x8F).unwrap();
        asm.dd(disp as u32).unwrap();
    } else {
        emit_store_pc_raw(asm, ctx, target_loc);
        emit_jcc_to_offset(asm, 0x8F, offsets[0], code_base);
    }
    let used = asm.size() - begin;
    for _ in used..A32_PATCH_JG_SIZE {
        asm.nop().unwrap();
    }
}

fn emit_patch_jz_a32(
    asm: &mut CodeAssembler,
    target_loc: crate::ir::location::LocationDescriptor,
    target_ptr: Option<*const u8>,
    offsets: [usize; 4],
    code_base: *const u8,
    ctx: &EmitContext,
) {
    let begin = asm.size();
    if let Some(ptr) = target_ptr {
        let target = ptr as usize;
        let jz_end = begin + 6;
        let jz_end_addr = code_base as usize + jz_end;
        let disp = (target as i64) - (jz_end_addr as i64);
        asm.db(0x0F).unwrap();
        asm.db(0x84).unwrap();
        asm.dd(disp as u32).unwrap();
    } else {
        emit_store_pc_raw(asm, ctx, target_loc);
        emit_jcc_to_offset(asm, 0x84, offsets[0], code_base);
    }
    let used = asm.size() - begin;
    for _ in used..A32_PATCH_JZ_SIZE {
        asm.nop().unwrap();
    }
}

fn emit_patch_jmp_a32(
    asm: &mut CodeAssembler,
    target_loc: crate::ir::location::LocationDescriptor,
    target_ptr: Option<*const u8>,
    offsets: [usize; 4],
    code_base: *const u8,
    ctx: &EmitContext,
) {
    let begin = asm.size();
    if let Some(ptr) = target_ptr {
        let target = ptr as usize;
        let jmp_end = begin + 5;
        let jmp_end_addr = code_base as usize + jmp_end;
        let disp = (target as i64) - (jmp_end_addr as i64);
        asm.db(0xE9).unwrap();
        asm.dd(disp as u32).unwrap();
    } else {
        emit_store_pc_raw(asm, ctx, target_loc);
        emit_jmp_to_offset(asm, offsets[0], code_base);
    }
    let used = asm.size() - begin;
    for _ in used..A32_PATCH_JMP_SIZE {
        asm.nop().unwrap();
    }
}

fn emit_jcc_to_offset(
    asm: &mut CodeAssembler,
    opcode: u8,
    target_offset: usize,
    code_base: *const u8,
) {
    let jcc_end = asm.size() + 6;
    let target_addr = code_base as usize + target_offset;
    let jcc_end_addr = code_base as usize + jcc_end;
    let disp = (target_addr as i64) - (jcc_end_addr as i64);
    asm.db(0x0F).unwrap();
    asm.db(opcode).unwrap();
    asm.dd(disp as u32).unwrap();
}

fn emit_push_rsb_terminal(
    ctx: &EmitContext,
    asm: &mut CodeAssembler,
    target_loc: crate::ir::location::LocationDescriptor,
) {
    let info = ctx.jit_state_info;
    let rsb_ptr_offset = info.offsetof_rsb_ptr;
    let rsb_loc_offset = info.offsetof_rsb_location_descriptors;
    let rsb_code_offset = info.offsetof_rsb_codeptrs;

    asm.mov(
        RBX.cvt32().unwrap(),
        dword_ptr(RegExp::from(R15) + rsb_ptr_offset as i32),
    )
    .unwrap();
    asm.mov(RAX, target_loc.value() as i64).unwrap();

    let patch_offset = asm.size();
    let fallback_code_ptr = ctx
        .dispatcher_offsets
        .map(|offsets| ctx.code_base_ptr as usize + offsets[0])
        .unwrap_or(0);
    let target_code_ptr = ctx
        .block_lookup
        .as_ref()
        .and_then(|lookup| lookup(target_loc))
        .map_or(fallback_code_ptr as u64, |ptr| ptr as u64);
    asm.mov(RCX, target_code_ptr as i64).unwrap();
    ctx.patch_entries.borrow_mut().push(PatchEntry {
        target: target_loc,
        patch_type: PatchType::MovRcx,
        code_offset: patch_offset,
    });

    asm.mov(
        qword_ptr(RegExp::from(R15) + RBX * 8u8 + rsb_loc_offset as i32),
        RAX,
    )
    .unwrap();
    asm.mov(
        qword_ptr(RegExp::from(R15) + RBX * 8u8 + rsb_code_offset as i32),
        RCX,
    )
    .unwrap();
    asm.add(RBX.cvt32().unwrap(), 1).unwrap();
    asm.and_(RBX.cvt32().unwrap(), info.rsb_ptr_mask as i32)
        .unwrap();
    asm.mov(
        dword_ptr(RegExp::from(R15) + rsb_ptr_offset as i32),
        RBX.cvt32().unwrap(),
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// Low-level jump helpers
// ---------------------------------------------------------------------------

/// Emit a raw `jmp rel32` to an absolute code buffer offset.
///
/// Computes the relative displacement from the end of the 5-byte jmp
/// instruction to the target offset, then emits `0xE9 <disp32>`.
pub(crate) fn emit_jmp_to_offset(
    asm: &mut CodeAssembler,
    target_offset: usize,
    code_base: *const u8,
) {
    // jmp rel32 is 5 bytes: 1 (opcode) + 4 (displacement)
    let jmp_end = asm.size() + 5;
    let target_addr = code_base as usize + target_offset;
    let jmp_end_addr = code_base as usize + jmp_end;
    let disp = (target_addr as i64) - (jmp_end_addr as i64);

    // Emit raw bytes: 0xE9 + 4-byte LE displacement
    asm.db(0xE9).unwrap();
    asm.dd(disp as u32).unwrap();
}

/// Emit: call add_ticks callback with (cycles_to_run - cycles_remaining).
///
/// Used only in the fallback (no-dispatcher) path for unit tests.
fn emit_add_ticks(ctx: &EmitContext, ra: &mut RegAlloc) {
    let cycles_to_run_off = STACK_LAYOUT_RSP_OFFSET + StackLayout::cycles_to_run_offset();
    let cycles_remaining_off = STACK_LAYOUT_RSP_OFFSET + StackLayout::cycles_remaining_offset();

    ctx.config
        .callbacks
        .add_ticks
        .emit_call(&mut *ra.asm, &|code, params| {
            code.mov(
                params[0],
                qword_ptr(RegExp::from(RSP) + cycles_to_run_off as i32),
            )?;
            code.sub(
                params[0],
                qword_ptr(RegExp::from(RSP) + cycles_remaining_off as i32),
            )
        })
        .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_emit_jcc_all_conditions() {
        let mut asm = rxbyak::CodeAssembler::new(4096).unwrap();
        let conditions = [
            Cond::EQ,
            Cond::NE,
            Cond::CS,
            Cond::CC,
            Cond::MI,
            Cond::PL,
            Cond::VS,
            Cond::VC,
            Cond::HI,
            Cond::LS,
            Cond::GE,
            Cond::LT,
            Cond::GT,
            Cond::LE,
            Cond::AL,
            Cond::NV,
        ];
        for cond in conditions {
            let label = asm.create_label();
            emit_jcc(&mut asm, cond, &label);
            asm.bind(&label).unwrap();
        }
        assert!(asm.size() > 0);
    }

    #[test]
    fn test_terminal_function_exists() {
        let _: fn(&EmitContext, &mut RegAlloc, &Terminal) = emit_terminal;
    }

    #[test]
    fn test_emit_jmp_to_offset() {
        let mut asm = rxbyak::CodeAssembler::new(4096).unwrap();
        let base = asm.top();
        let before = asm.size();
        emit_jmp_to_offset(&mut asm, 0, base);
        // Should emit 5 bytes (0xE9 + disp32)
        assert_eq!(asm.size() - before, 5);
    }

    #[test]
    fn test_emit_patch_jg_size() {
        let mut asm = rxbyak::CodeAssembler::new(4096).unwrap();
        let base = asm.top();
        let before = asm.size();
        emit_patch_jg(&mut asm, 0x1234_5678_9ABC, 8, None, [100, 200, 300, 400], base);
        assert_eq!(
            asm.size() - before,
            A64_PATCH_JG_SIZE,
            "jg patch slot should be exactly {} bytes",
            A64_PATCH_JG_SIZE
        );
    }

    #[test]
    fn test_emit_patch_jz_size() {
        let mut asm = rxbyak::CodeAssembler::new(4096).unwrap();
        let base = asm.top();
        let before = asm.size();
        emit_patch_jz(&mut asm, 0x1234_5678_9ABC, 8, None, [100, 200, 300, 400], base);
        assert_eq!(
            asm.size() - before,
            A64_PATCH_JZ_SIZE,
            "jz patch slot should be exactly {} bytes",
            A64_PATCH_JZ_SIZE
        );
    }

    #[test]
    fn test_emit_patch_jmp_size() {
        let mut asm = rxbyak::CodeAssembler::new(4096).unwrap();
        let base = asm.top();
        let before = asm.size();
        emit_patch_jmp(&mut asm, 0x1234_5678_9ABC, 8, None, [100, 200, 300, 400], base);
        assert_eq!(
            asm.size() - before,
            A64_PATCH_JMP_SIZE,
            "jmp patch slot should be exactly {} bytes",
            A64_PATCH_JMP_SIZE
        );
    }

    #[test]
    fn test_emit_patch_jmp_with_target() {
        let mut asm = rxbyak::CodeAssembler::new(4096).unwrap();
        let base = asm.top();
        // Emit some NOPs to create a "target" at a known offset
        for _ in 0..64 {
            asm.nop().unwrap();
        }
        let target_ptr = unsafe { base.add(64) };
        let before = asm.size();
        emit_patch_jmp(&mut asm, 0x1234_5678_9ABC, 8, Some(target_ptr), [100, 200, 300, 400], base);
        assert_eq!(asm.size() - before, A64_PATCH_JMP_SIZE);
        // First byte should be 0xE9 (jmp rel32)
        let code = unsafe { std::slice::from_raw_parts(base.add(before), A64_PATCH_JMP_SIZE) };
        assert_eq!(code[0], 0xE9, "First byte should be JMP opcode");
    }

    /// Upstream `A64EmitX64::EmitPatchJz` without a target: the unlinked slot stores the
    /// target PC (`mov rax, imm64; mov [r15 + pc], rax`) before `jz` to the dispatcher;
    /// with a target it is a bare `jz rel32`.
    #[test]
    fn test_emit_patch_jz_a64_unlinked_stores_pc_linked_is_bare_jz() {
        let mut asm = rxbyak::CodeAssembler::new(4096).unwrap();
        let base = asm.top();
        let pc: u64 = 0x1234_5678_9ABC;
        emit_patch_jz(&mut asm, pc, 8, None, [100, 200, 300, 400], base);
        let code = unsafe { std::slice::from_raw_parts(base, A64_PATCH_JZ_SIZE) };
        assert_eq!(&code[0..2], &[0x48, 0xB8], "mov rax, imm64");
        assert_eq!(u64::from_le_bytes(code[2..10].try_into().unwrap()), pc);
        assert_eq!(&code[10..14], &[0x49, 0x89, 0x47, 0x08], "mov [r15+8], rax");
        assert_eq!(&code[14..16], &[0x0F, 0x84], "jz rel32");
        let disp = i32::from_le_bytes(code[16..20].try_into().unwrap()) as i64;
        assert_eq!(20 + disp, 100, "jz targets return_from_run_code[0]");
        assert!(code[20..].iter().all(|&b| b == 0x90));

        let before = asm.size();
        let target_ptr = unsafe { base.add(64) };
        emit_patch_jz(&mut asm, pc, 8, Some(target_ptr), [100, 200, 300, 400], base);
        let code = unsafe { std::slice::from_raw_parts(base.add(before), A64_PATCH_JZ_SIZE) };
        assert_eq!(&code[0..2], &[0x0F, 0x84]);
        let disp = i32::from_le_bytes(code[2..6].try_into().unwrap()) as i64;
        assert_eq!(before as i64 + 6 + disp, 64);
        assert!(code[6..].iter().all(|&b| b == 0x90));
    }

    #[test]
    fn test_emit_patch_jg_with_target() {
        let mut asm = rxbyak::CodeAssembler::new(4096).unwrap();
        let base = asm.top();
        for _ in 0..64 {
            asm.nop().unwrap();
        }
        let target_ptr = unsafe { base.add(64) };
        let before = asm.size();
        emit_patch_jg(&mut asm, 0x1234_5678_9ABC, 8, Some(target_ptr), [100, 200, 300, 400], base);
        assert_eq!(asm.size() - before, A64_PATCH_JG_SIZE);
        let code = unsafe { std::slice::from_raw_parts(base.add(before), A64_PATCH_JG_SIZE) };
        // jg rel32: 0x0F 0x8F
        assert_eq!(code[0], 0x0F, "First byte should be 0x0F");
        assert_eq!(code[1], 0x8F, "Second byte should be 0x8F (jg)");
    }
}

use crate::backend::x64::a64_emit_x64_memory::{
    emit_a64_check_memory_abort, should_fastmem, FastmemFallbacksTable,
};
use crate::backend::x64::emit_context::EmitContext;
use crate::backend::x64::emit_x64_memory::{
    emit_call_to_offset, emit_fastmem_vaddr_a64, emit_read_memory_mov,
};
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::common::spin_lock_x64::{emit_spin_lock_lock, emit_spin_lock_unlock};
use crate::ir::inst::Inst;
use crate::ir::value::InstRef;
#[cfg(target_os = "windows")]
use rxbyak::RSP;
use rxbyak::{
    byte_ptr, dword_ptr, qword_ptr, word_ptr, xmmword_ptr, CodeAssembler, JmpType, Reg, RegExp,
    R15, RAX, RBX, RCX, RDX, XMM0,
};

use crate::backend::x64::a64_jitstate::A64JitState;
use crate::backend::x64::abi;
use crate::backend::x64::emit_context::DeferredEmitCtx;
use crate::backend::x64::exception_handler::{supports_fastmem, FastmemPatchInfo};
use crate::backend::x64::exclusive_monitor_friend::{
    get_exclusive_monitor_address_pointer, get_exclusive_monitor_lock_pointer,
    get_exclusive_monitor_processor_count, get_exclusive_monitor_value_pointer,
};
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::hostloc::HostLoc;

// ---------------------------------------------------------------------------
// EmitExclusiveLock / EmitExclusiveUnlock — inline acquire/release of the
// global exclusive monitor's spin lock.
//
// Port of upstream Dynarmic `EmitExclusiveLock` / `EmitExclusiveUnlock`
// (emit_x64_memory.h:341-359). The lock pointer is resolved at compile time
// via `GetExclusiveMonitorLockPointer` and burned into the emitted
// code as an absolute `mov reg, imm64`.
//
// Both helpers do nothing when no global_monitor is configured — i.e. when
// the JIT is single-core only and reservations don't need cross-core
// coordination (mirrors upstream's `Unsafe_IgnoreGlobalMonitor` short-circuit
// at the call sites).
// ---------------------------------------------------------------------------

/// Emit a take of the exclusive monitor's spin lock.
///
/// `ptr_reg` is loaded with the lock address (via `mov reg, imm64`) then
/// passed to `emit_spin_lock_lock`. `tmp32_reg` is a 32-bit scratch.
///
/// Returns true if any code was emitted (= a monitor was configured), false
/// otherwise. Callers must keep the lock balanced with `emit_exclusive_unlock`
/// only when this returns true.
pub fn emit_exclusive_lock(
    ctx: &EmitContext,
    asm: &mut CodeAssembler,
    ptr_reg: Reg,
    tmp32_reg: Reg,
) -> bool {
    let Some(monitor_ptr) = ctx.config.global_monitor else {
        return false;
    };
    // SAFETY: monitor_ptr was checked non-null when set in EmitConfig.
    let lock_storage_ptr = unsafe { get_exclusive_monitor_lock_pointer(monitor_ptr) };
    asm.mov(ptr_reg, lock_storage_ptr as u64 as i64).unwrap();
    emit_spin_lock_lock(
        asm,
        ptr_reg,
        tmp32_reg,
        ctx.has_host_feature(HostFeature::WAITPKG),
    );
    true
}

/// Emit a release of the exclusive monitor's spin lock.
///
/// Symmetric to `emit_exclusive_lock`. Must be paired only when the lock
/// helper returned true.
pub fn emit_exclusive_unlock(
    ctx: &EmitContext,
    asm: &mut CodeAssembler,
    ptr_reg: Reg,
    tmp32_reg: Reg,
) -> bool {
    let Some(monitor_ptr) = ctx.config.global_monitor else {
        return false;
    };
    let lock_storage_ptr = unsafe { get_exclusive_monitor_lock_pointer(monitor_ptr) };
    asm.mov(ptr_reg, lock_storage_ptr as u64 as i64).unwrap();
    emit_spin_lock_unlock(asm, ptr_reg, tmp32_reg);
    true
}

/// Resolve `&monitor.exclusive_addresses[index]` to an absolute host
/// pointer for use as an `imm64` load target.
pub fn exclusive_address_ptr(ctx: &EmitContext, index: usize) -> Option<*mut u64> {
    let monitor_ptr = ctx.config.global_monitor?;
    Some(unsafe { get_exclusive_monitor_address_pointer(monitor_ptr, index) })
}

/// Resolve `&monitor.exclusive_values[index]` to an absolute host pointer
/// (points to a `[u64; 2]` slot).
pub fn exclusive_value_ptr(
    ctx: &EmitContext,
    index: usize,
) -> Option<*mut crate::interface::exclusive_monitor::Vector> {
    let monitor_ptr = ctx.config.global_monitor?;
    Some(unsafe { get_exclusive_monitor_value_pointer(monitor_ptr, index) })
}

/// Number of guest processors the monitor was configured for. Used by
/// `EmitExclusiveTestAndClear` to iterate other processors and invalidate
/// their reservations when an exclusive store succeeds.
pub fn exclusive_monitor_processor_count(ctx: &EmitContext) -> usize {
    let Some(monitor_ptr) = ctx.config.global_monitor else {
        return 0;
    };
    unsafe { get_exclusive_monitor_processor_count(monitor_ptr) }
}

/// Emit the upstream `EmitExclusiveTestAndClear` sequence: for every other
/// processor in the monitor, if its reservation address equals `vaddr`,
/// invalidate it (overwrite with `INVALID_EXCLUSIVE_ADDRESS = 0xDEADDEADDEADDEAD`).
///
/// The current processor's own reservation is left intact and is cleared by
/// the caller's standard store path. `ptr_reg` and `tmp_reg` are 64-bit
/// scratches; both clobbered.
pub fn emit_exclusive_test_and_clear(
    ctx: &EmitContext,
    asm: &mut CodeAssembler,
    vaddr: Reg,
    ptr_reg: Reg,
    tmp_reg: Reg,
) {
    let processor_count = exclusive_monitor_processor_count(ctx);
    if processor_count == 0 {
        return;
    }
    let self_pid = ctx.config.memory.processor_id;

    // tmp = INVALID_EXCLUSIVE_ADDRESS sentinel (matches upstream's literal).
    asm.mov(tmp_reg, 0xDEAD_DEAD_DEAD_DEADu64 as i64).unwrap();

    for other_pid in 0..processor_count {
        if other_pid == self_pid {
            continue;
        }
        let Some(addr_ptr) = exclusive_address_ptr(ctx, other_pid) else {
            continue;
        };
        let ok = asm.create_label();
        asm.mov(ptr_reg, addr_ptr as u64 as i64).unwrap();
        asm.cmp(qword_ptr(RegExp::from(ptr_reg)), vaddr).unwrap();
        asm.jne(&ok, rxbyak::JmpType::Short).unwrap();
        asm.mov(qword_ptr(RegExp::from(ptr_reg)), tmp_reg).unwrap();
        asm.bind(&ok).unwrap();
    }
}

// ---------------------------------------------------------------------------
// A64ClearExclusive: clear the exclusive monitor
// ---------------------------------------------------------------------------

pub fn emit_a64_clear_exclusive(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    _inst: &Inst,
) {
    ra.host_call(None, &mut [None, None, None, None]);
    ctx.config
        .callbacks
        .exclusive_clear
        .emit_call_simple(&mut *ra.asm)
        .unwrap();
}

// ---------------------------------------------------------------------------
// Exclusive read operations (via host callbacks)
// ---------------------------------------------------------------------------

pub fn emit_a64_exclusive_read_memory_8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_exclusive_read(ctx, ra, inst_ref, inst, 8);
}

pub fn emit_a64_exclusive_read_memory_16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_exclusive_read(ctx, ra, inst_ref, inst, 16);
}

pub fn emit_a64_exclusive_read_memory_32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_exclusive_read(ctx, ra, inst_ref, inst, 32);
}

pub fn emit_a64_exclusive_read_memory_64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_exclusive_read(ctx, ra, inst_ref, inst, 64);
}

pub fn emit_a64_exclusive_read_memory_128(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_exclusive_read(ctx, ra, inst_ref, inst, 128);
}

fn emit_exclusive_read(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    if ctx.config.memory.fastmem_exclusive_access
        && ctx.config.global_monitor.is_some()
        && ctx.fastmem_available
        && supports_fastmem()
    {
        emit_a64_exclusive_read_inline(ctx, ra, inst_ref, inst, bitsize);
        return;
    }

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());

    // args[0] = location descriptor (upper), args[1] = vaddr, args[2] = acc_type
    // ArgCallback: position 0 = None (context), position 1 = vaddr

    if bitsize == 128 {
        let result = ra.scratch_xmm();
        ra.host_call(None, &mut [None, Some(&mut args[1]), None, None]);

        #[cfg(target_os = "windows")]
        {
            let frame_size = 16 + abi::ABI_SHADOW_SPACE;
            ra.alloc_stack_space(frame_size);
            ctx.config
                .callbacks
                .exclusive_read_128
                .emit_call(&mut *ra.asm, &|code, params| {
                    code.lea(
                        params[1],
                        qword_ptr(RegExp::from(RSP) + abi::ABI_SHADOW_SPACE as i32),
                    )
                })
                .unwrap();
            ra.asm
                .movups(
                    result,
                    xmmword_ptr(RegExp::from(RSP) + abi::ABI_SHADOW_SPACE as i32),
                )
                .unwrap();
            ra.release_stack_space(frame_size);
        }

        #[cfg(not(target_os = "windows"))]
        {
            ctx.config
                .callbacks
                .exclusive_read_128
                .emit_call_simple(&mut *ra.asm)
                .unwrap();
            ra.asm.movq(result, RAX).unwrap();
            ra.asm.pinsrq(result, RDX, 1).unwrap();
        }
        ra.define_value(inst_ref, result);
        emit_a64_check_memory_abort(ctx, ra, inst, None);
        return;
    }

    ra.host_call(Some(inst_ref), &mut [None, Some(&mut args[1]), None, None]);

    let callback = match bitsize {
        8 => &ctx.config.callbacks.exclusive_read_8,
        16 => &ctx.config.callbacks.exclusive_read_16,
        32 => &ctx.config.callbacks.exclusive_read_32,
        64 => &ctx.config.callbacks.exclusive_read_64,
        _ => unreachable!("Invalid exclusive read bitsize: {}", bitsize),
    };

    callback.emit_call_simple(&mut *ra.asm).unwrap();
    emit_a64_check_memory_abort(ctx, ra, inst, None);
}

// ---------------------------------------------------------------------------
// Exclusive write operations (via host callbacks)
// Returns U32: 0 = success, 1 = failure
// ---------------------------------------------------------------------------

pub fn emit_a64_exclusive_write_memory_8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_exclusive_write(ctx, ra, inst_ref, inst, 8);
}

pub fn emit_a64_exclusive_write_memory_16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_exclusive_write(ctx, ra, inst_ref, inst, 16);
}

pub fn emit_a64_exclusive_write_memory_32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_exclusive_write(ctx, ra, inst_ref, inst, 32);
}

pub fn emit_a64_exclusive_write_memory_64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_exclusive_write(ctx, ra, inst_ref, inst, 64);
}

pub fn emit_a64_exclusive_write_memory_128(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_exclusive_write(ctx, ra, inst_ref, inst, 128);
}

fn emit_exclusive_write(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    if ctx.config.memory.fastmem_exclusive_access
        && ctx.config.global_monitor.is_some()
        && ctx.config.raw_exclusive_write_callbacks.is_some()
        && ctx.fastmem_available
        && supports_fastmem()
    {
        emit_a64_exclusive_write_inline(ctx, ra, inst_ref, inst, bitsize);
        return;
    }

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());

    // args[0] = location descriptor (upper), args[1] = vaddr, args[2] = value, args[3] = acc_type
    // ArgCallback: position 0 = None (context), position 1 = vaddr, position 2 = value

    if bitsize == 128 {
        // Pin the address and vector value before releasing the allocation
        // scope. Windows passes the 16-byte value through a pointer, while
        // System V passes its two lanes in the third and fourth integer
        // parameter registers.
        let (first, rest) = args.split_at_mut(2);
        ra.use_loc(&mut first[1], abi::ABI_PARAMS[1]); // vaddr → RSI
        ra.use_loc(&mut rest[0], HostLoc::Xmm(1)); // value → XMM1
        ra.end_of_alloc_scope();
        ra.host_call(Some(inst_ref), &mut [None, None, None, None]);
        #[cfg(target_os = "windows")]
        {
            let frame_size = 16 + abi::ABI_SHADOW_SPACE;
            ra.alloc_stack_space(frame_size);
            ra.asm
                .movups(
                    xmmword_ptr(RegExp::from(RSP) + abi::ABI_SHADOW_SPACE as i32),
                    Reg::xmm(1),
                )
                .unwrap();
            ctx.config
                .callbacks
                .exclusive_write_128
                .emit_call(&mut *ra.asm, &|code, params| {
                    code.lea(
                        params[1],
                        qword_ptr(RegExp::from(RSP) + abi::ABI_SHADOW_SPACE as i32),
                    )
                })
                .unwrap();
            ra.release_stack_space(frame_size);
        }

        #[cfg(not(target_os = "windows"))]
        {
            let value_lo = abi::ABI_PARAMS[2].to_reg64();
            let value_hi = abi::ABI_PARAMS[3].to_reg64();
            ra.asm.movq(value_lo, Reg::xmm(1)).unwrap();
            if ctx.has_host_feature(HostFeature::SSE41) {
                ra.asm.pextrq(value_hi, Reg::xmm(1), 1).unwrap();
            } else {
                ra.asm.movaps(XMM0, Reg::xmm(1)).unwrap();
                ra.asm.punpckhqdq(XMM0, XMM0).unwrap();
                ra.asm.movq(value_hi, XMM0).unwrap();
            }
            ctx.config
                .callbacks
                .exclusive_write_128
                .emit_call_simple(&mut *ra.asm)
                .unwrap();
        }
        emit_a64_check_memory_abort(ctx, ra, inst, None);
        return;
    }

    let (first, rest) = args.split_at_mut(2);
    ra.host_call(
        Some(inst_ref), // Result (success/failure) in RAX
        &mut [None, Some(&mut first[1]), Some(&mut rest[0]), None],
    );

    let callback = match bitsize {
        8 => &ctx.config.callbacks.exclusive_write_8,
        16 => &ctx.config.callbacks.exclusive_write_16,
        32 => &ctx.config.callbacks.exclusive_write_32,
        64 => &ctx.config.callbacks.exclusive_write_64,
        _ => unreachable!("Invalid exclusive write bitsize: {}", bitsize),
    };

    callback.emit_call_simple(&mut *ra.asm).unwrap();
    emit_a64_check_memory_abort(ctx, ra, inst, None);
}

fn emit_a64_exclusive_read_inline(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    assert!(matches!(bitsize, 8 | 16 | 32 | 64 | 128));
    assert!(ctx.config.global_monitor.is_some() && ctx.fastmem_available);
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let pid = ctx.config.memory.processor_id;
    let addr_ptr = exclusive_address_ptr(ctx, pid).expect("global monitor required");
    let value_ptr = exclusive_value_ptr(ctx, pid).expect("global monitor required");

    if bitsize == 128 {
        ra.scratch_gpr_at(HostLoc::Gpr(RAX.get_idx()));
        ra.scratch_gpr_at(HostLoc::Gpr(RBX.get_idx()));
        ra.scratch_gpr_at(HostLoc::Gpr(RCX.get_idx()));
        ra.scratch_gpr_at(HostLoc::Gpr(RDX.get_idx()));
    }
    let vaddr = ra.use_gpr(&mut args[1]);
    let value = if bitsize == 128 {
        ra.scratch_xmm()
    } else {
        ra.scratch_gpr()
    };
    let tmp = ra.scratch_gpr();
    let tmp2 = ra.scratch_gpr();
    let vaddr_idx = vaddr.get_idx();
    let value_idx = value.get_idx();

    let fallbacks = unsafe {
        &*(ctx
            .fastmem_fallbacks
            .expect("exclusive fastmem requires fallback table")
            as *const FastmemFallbacksTable)
    };
    let wrapped_fn_off = fallbacks.read_stub(true, bitsize, vaddr_idx, value_idx);

    let locked = emit_exclusive_lock(ctx, &mut *ra.asm, tmp, tmp2.cvt32().unwrap());
    ra.asm
        .mov(
            byte_ptr(RegExp::from(R15) + A64JitState::offset_of_exclusive_state() as i32),
            1i32,
        )
        .unwrap();
    ra.asm.mov(tmp, addr_ptr as u64 as i64).unwrap();
    ra.asm.mov(qword_ptr(RegExp::from(tmp)), vaddr).unwrap();

    let marker = should_fastmem(ctx, inst_ref);
    if let Some(marker) = marker {
        let abort = ra.asm.create_label();
        let end = ra.asm.create_label();
        let mut require_abort = false;
        let src = emit_fastmem_vaddr_a64(ra, ctx, abort, vaddr, &mut require_abort, None);
        let mov_off = match bitsize {
            8 => emit_read_memory_mov::<8>(ra.asm, value_idx, src, true),
            16 => emit_read_memory_mov::<16>(ra.asm, value_idx, src, true),
            32 => emit_read_memory_mov::<32>(ra.asm, value_idx, src, true),
            64 => emit_read_memory_mov::<64>(ra.asm, value_idx, src, true),
            128 => emit_read_memory_mov::<128>(ra.asm, value_idx, src, true),
            _ => unreachable!(),
        };
        let resume_off = ra.asm.size();
        let recompile = ctx.config.memory.recompile_on_exclusive_fastmem_failure;
        ctx.deferred_emits
            .borrow_mut()
            .push(Box::new(move |dctx: &mut DeferredEmitCtx<'_>| {
                dctx.fastmem_patches.add(
                    dctx.code_base + mov_off as u64,
                    FastmemPatchInfo::new(
                        dctx.code_base + resume_off as u64,
                        dctx.code_base + wrapped_fn_off as u64,
                        Some(marker),
                        recompile,
                    ),
                );
                if require_abort {
                    let asm = &mut *dctx.asm;
                    asm.bind(&abort).unwrap();
                    emit_call_to_offset(asm, wrapped_fn_off);
                    asm.jmp(&end, JmpType::Near).unwrap();
                }
            }));
        ra.asm.bind(&end).unwrap();
    } else {
        emit_call_to_offset(ra.asm, wrapped_fn_off);
    }

    ra.asm.mov(tmp, value_ptr as u64 as i64).unwrap();
    match bitsize {
        8 => ra
            .asm
            .mov(byte_ptr(RegExp::from(tmp)), value.cvt8().unwrap()),
        16 => ra
            .asm
            .mov(word_ptr(RegExp::from(tmp)), value.cvt16().unwrap()),
        32 => ra
            .asm
            .mov(dword_ptr(RegExp::from(tmp)), value.cvt32().unwrap()),
        64 => ra.asm.mov(qword_ptr(RegExp::from(tmp)), value),
        128 => ra.asm.movups(xmmword_ptr(RegExp::from(tmp)), value),
        _ => unreachable!(),
    }
    .unwrap();
    if locked {
        emit_exclusive_unlock(ctx, &mut *ra.asm, tmp, tmp2.cvt32().unwrap());
    }
    ra.define_value(inst_ref, value);
    emit_a64_check_memory_abort(ctx, ra, inst, None);
}

fn emit_a64_exclusive_write_inline(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    assert!(matches!(bitsize, 8 | 16 | 32 | 64 | 128));
    assert!(ctx.config.global_monitor.is_some() && ctx.fastmem_available);
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let pid = ctx.config.memory.processor_id;
    let addr_ptr = exclusive_address_ptr(ctx, pid).expect("global monitor required");
    let value_ptr = exclusive_value_ptr(ctx, pid).expect("global monitor required");

    let rax = ra.scratch_gpr_at(HostLoc::Gpr(RAX.get_idx()));
    if bitsize == 128 {
        ra.scratch_gpr_at(HostLoc::Gpr(RBX.get_idx()));
        ra.scratch_gpr_at(HostLoc::Gpr(RCX.get_idx()));
        ra.scratch_gpr_at(HostLoc::Gpr(RDX.get_idx()));
    }
    let value = if bitsize == 128 {
        ra.use_xmm(&mut args[2])
    } else {
        ra.use_gpr(&mut args[2])
    };
    let vaddr = ra.use_gpr(&mut args[1]);
    let status = ra.scratch_gpr();
    let tmp = ra.scratch_gpr();
    let vaddr_idx = vaddr.get_idx();
    let value_idx = value.get_idx();

    let fallbacks = unsafe {
        &*(ctx
            .fastmem_fallbacks
            .expect("exclusive fastmem requires fallback table")
            as *const FastmemFallbacksTable)
    };
    let wrapped_fn_off = fallbacks.exclusive_write_stub(true, bitsize, vaddr_idx, value_idx);

    let locked = emit_exclusive_lock(ctx, &mut *ra.asm, tmp, rax.cvt32().unwrap());
    let end = ra.asm.create_label();
    ra.asm.mov(status.cvt32().unwrap(), 1u32).unwrap();
    ra.asm
        .cmp(
            byte_ptr(RegExp::from(R15) + A64JitState::offset_of_exclusive_state() as i32),
            0i32,
        )
        .unwrap();
    ra.asm.je(&end, JmpType::Near).unwrap();
    ra.asm.mov(tmp, addr_ptr as u64 as i64).unwrap();
    ra.asm.cmp(qword_ptr(RegExp::from(tmp)), vaddr).unwrap();
    ra.asm.jne(&end, JmpType::Near).unwrap();

    emit_exclusive_test_and_clear(ctx, &mut *ra.asm, vaddr, tmp, rax);
    ra.asm
        .mov(
            byte_ptr(RegExp::from(R15) + A64JitState::offset_of_exclusive_state() as i32),
            0i32,
        )
        .unwrap();
    ra.asm.mov(tmp, value_ptr as u64 as i64).unwrap();
    match bitsize {
        8 => ra
            .asm
            .movzx(rax.cvt32().unwrap(), byte_ptr(RegExp::from(tmp))),
        16 => ra
            .asm
            .movzx(rax.cvt32().unwrap(), word_ptr(RegExp::from(tmp))),
        32 => ra
            .asm
            .mov(rax.cvt32().unwrap(), dword_ptr(RegExp::from(tmp))),
        64 => ra.asm.mov(rax, qword_ptr(RegExp::from(tmp))),
        128 => {
            ra.asm.mov(rax, qword_ptr(RegExp::from(tmp))).unwrap();
            ra.asm.mov(RDX, qword_ptr(RegExp::from(tmp) + 8)).unwrap();
            if ctx.has_host_feature(HostFeature::SSE41) {
                ra.asm.movq(RBX, value).unwrap();
                ra.asm.pextrq(RCX, value, 1).unwrap();
            } else {
                ra.asm.movaps(XMM0, value).unwrap();
                ra.asm.movq(RBX, XMM0).unwrap();
                ra.asm.punpckhqdq(XMM0, XMM0).unwrap();
                ra.asm.movq(RCX, XMM0).unwrap();
            }
            Ok(())
        }
        _ => unreachable!(),
    }
    .unwrap();

    if let Some(marker) = should_fastmem(ctx, inst_ref) {
        let abort = ra.asm.create_label();
        let mut _require_abort = false;
        let dest = emit_fastmem_vaddr_a64(ra, ctx, abort, vaddr, &mut _require_abort, Some(tmp));
        let mov_off = ra.asm.size();
        ra.asm.lock().unwrap();
        match bitsize {
            8 => ra.asm.cmpxchg(byte_ptr(dest), value.cvt8().unwrap()),
            16 => ra.asm.cmpxchg(word_ptr(dest), value.cvt16().unwrap()),
            32 => ra.asm.cmpxchg(dword_ptr(dest), value.cvt32().unwrap()),
            64 => ra.asm.cmpxchg(qword_ptr(dest), value),
            128 => ra.asm.cmpxchg16b(xmmword_ptr(dest)),
            _ => unreachable!(),
        }
        .unwrap();
        ra.asm.setnz(status.cvt8().unwrap()).unwrap();
        let recompile = ctx.config.memory.recompile_on_exclusive_fastmem_failure;
        ctx.deferred_emits
            .borrow_mut()
            .push(Box::new(move |dctx: &mut DeferredEmitCtx<'_>| {
                let asm = &mut *dctx.asm;
                asm.bind(&abort).unwrap();
                emit_call_to_offset(asm, wrapped_fn_off);
                let resume_rip = dctx.code_base + asm.size() as u64;
                dctx.fastmem_patches.add(
                    dctx.code_base + mov_off as u64,
                    FastmemPatchInfo::new(
                        resume_rip,
                        dctx.code_base + wrapped_fn_off as u64,
                        Some(marker),
                        recompile,
                    ),
                );
                asm.cmp(RAX.cvt8().unwrap(), 0i32).unwrap();
                asm.setz(status.cvt8().unwrap()).unwrap();
                asm.movzx(status.cvt32().unwrap(), status.cvt8().unwrap())
                    .unwrap();
                asm.jmp(&end, JmpType::Near).unwrap();
            }));
    } else {
        emit_call_to_offset(ra.asm, wrapped_fn_off);
        ra.asm.cmp(RAX.cvt8().unwrap(), 0i32).unwrap();
        ra.asm.setz(status.cvt8().unwrap()).unwrap();
        ra.asm
            .movzx(status.cvt32().unwrap(), status.cvt8().unwrap())
            .unwrap();
    }

    ra.asm.bind(&end).unwrap();
    if locked {
        emit_exclusive_unlock(ctx, &mut *ra.asm, tmp, rax.cvt32().unwrap());
    }
    ra.define_value(inst_ref, status);
    emit_a64_check_memory_abort(ctx, ra, inst, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exclusive_memory_fn_signatures() {
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_a64_clear_exclusive;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_a64_exclusive_read_memory_8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_a64_exclusive_read_memory_128;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_a64_exclusive_write_memory_8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_a64_exclusive_write_memory_128;
    }
}

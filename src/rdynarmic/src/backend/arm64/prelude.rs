use std::any::Any;
use std::ffi::c_void;
use std::io::Write;

use crate::interface::halt_reason::HaltReason;
use crate::ir::cond::Cond;

use super::abi::{
    self, to_reg_list_gpr, to_reg_list_vec, ABI_CALLEE_SAVE, ABI_CALLER_SAVE, XFASTMEM, XHALT,
    XPAGETABLE, XSCRATCH0, XSCRATCH1, XSTATE, XTICKS,
};
#[cfg(test)]
use super::block_of_code::BlockOfCode;
#[cfg(test)]
use super::inst;
use super::jit_state::{A32JitState, A64JitState};
use super::stack_layout::{RSBEntry, StackLayout, RSB_COUNT};
use rhazel::{CodeGenerator, SystemReg, WReg, XReg};

pub type RunCodeFn = unsafe extern "C" fn(
    entry_point: *const u8,
    jit_state: *mut c_void,
    halt_reason: *mut u32,
) -> u32;

#[derive(Clone, Copy)]
pub struct DispatcherCallback {
    pub this_ptr: *const c_void,
    pub fn_ptr: *const c_void,
    pub ticks: Option<TickCallbacks>,
}

#[derive(Clone, Copy)]
pub struct TickCallbacks {
    pub this_ptr: *const c_void,
    pub add_ticks_fn_ptr: *const c_void,
    pub get_ticks_remaining_fn_ptr: *const c_void,
}

pub struct PreludeInfo {
    pub end_of_prelude: usize,
    pub run_code: RunCodeFn,
    pub step_code: RunCodeFn,
    pub return_to_dispatcher_offset: usize,
    pub return_to_dispatcher: *const u8,
    pub return_from_run_code_offset: usize,
    pub return_from_run_code: *const u8,
    pub read_memory_8: Option<*const u8>,
    pub read_memory_16: Option<*const u8>,
    pub read_memory_32: Option<*const u8>,
    pub read_memory_64: Option<*const u8>,
    pub read_memory_128: Option<*const u8>,
    pub wrapped_read_memory_8: Option<*const u8>,
    pub wrapped_read_memory_16: Option<*const u8>,
    pub wrapped_read_memory_32: Option<*const u8>,
    pub wrapped_read_memory_64: Option<*const u8>,
    pub wrapped_read_memory_128: Option<*const u8>,
    pub exclusive_read_memory_8: Option<*const u8>,
    pub exclusive_read_memory_16: Option<*const u8>,
    pub exclusive_read_memory_32: Option<*const u8>,
    pub exclusive_read_memory_64: Option<*const u8>,
    pub exclusive_read_memory_128: Option<*const u8>,
    pub write_memory_8: Option<*const u8>,
    pub write_memory_16: Option<*const u8>,
    pub write_memory_32: Option<*const u8>,
    pub write_memory_64: Option<*const u8>,
    pub write_memory_128: Option<*const u8>,
    pub wrapped_write_memory_8: Option<*const u8>,
    pub wrapped_write_memory_16: Option<*const u8>,
    pub wrapped_write_memory_32: Option<*const u8>,
    pub wrapped_write_memory_64: Option<*const u8>,
    pub wrapped_write_memory_128: Option<*const u8>,
    pub exclusive_write_memory_8: Option<*const u8>,
    pub exclusive_write_memory_16: Option<*const u8>,
    pub exclusive_write_memory_32: Option<*const u8>,
    pub exclusive_write_memory_64: Option<*const u8>,
    pub exclusive_write_memory_128: Option<*const u8>,
    pub call_svc: Option<*const u8>,
    pub exception_raised: Option<*const u8>,
    pub dc_raised: Option<*const u8>,
    pub ic_raised: Option<*const u8>,
    pub isb_raised: Option<*const u8>,
    pub get_cntpct: Option<*const u8>,
    pub add_ticks: Option<*const u8>,
    pub get_ticks_remaining: Option<*const u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreludeIsa {
    A32,
    A64,
}

impl Default for PreludeIsa {
    fn default() -> Self {
        Self::A32
    }
}

#[derive(Clone, Copy, Default)]
pub struct PreludeOptions {
    pub isa: PreludeIsa,
    pub dispatcher: Option<DispatcherCallback>,
    pub return_stack_buffer: bool,
    pub page_table_pointer: u64,
    pub fastmem_pointer: u64,
}

struct RunLikeEntryInfo {
    return_from_run_code_offset: usize,
    rsb_literal_load_offset: Option<usize>,
}

/// Emit bootstrap ARM64 run/step entry points.
///
/// This follows the first part of upstream `A32AddressSpace::EmitPrelude` and
/// `A64AddressSpace::EmitPrelude`: preserve callee-save registers, install the
/// state/halt registers, check halt before entering native code, and return the
/// previous halt reason while clearing it. When an ISA owner passes a
/// dispatcher callback, `return_to_dispatcher` follows upstream's generated
/// callback shape: check halt, call back into `GetOrEmit(context)`, then branch
/// to the returned block.
/// Format a `catch_unwind` payload so dispatcher failures reach `ruzu_log`
/// instead of aborting the GUI with an empty IPS backtrace.
pub(crate) fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else {
        "unknown panic payload".to_string()
    }
}

pub(crate) fn report_dispatcher_failure(label: &str, pc: u64, detail: &str) {
    let line = format!("{label} at pc={pc:#x}: {detail}");
    log::error!("{line}");
    eprintln!("{line}");
    log::logger().flush();
    let _ = std::io::stderr().flush();
}

pub fn emit_bootstrap_prelude(code: &mut CodeGenerator<'_>) -> Result<PreludeInfo, String> {
    emit_bootstrap_prelude_with_dispatcher(code, None)
}

pub fn emit_bootstrap_prelude_with_dispatcher(
    code: &mut CodeGenerator<'_>,
    dispatcher: Option<DispatcherCallback>,
) -> Result<PreludeInfo, String> {
    emit_bootstrap_prelude_with_options(
        code,
        PreludeOptions {
            isa: PreludeIsa::A32,
            dispatcher,
            return_stack_buffer: false,
            page_table_pointer: 0,
            fastmem_pointer: 0,
        },
    )
}

pub fn emit_bootstrap_prelude_with_options(
    code: &mut CodeGenerator<'_>,
    options: PreludeOptions,
) -> Result<PreludeInfo, String> {
    let dispatcher = options.dispatcher;
    let ticks = dispatcher.and_then(|dispatcher| dispatcher.ticks);
    let run_offset = code.code_size();
    let run_info = emit_run_like_entry(code, false, ticks, options)?;
    let return_from_run_code_offset = run_info.return_from_run_code_offset;
    let step_offset = code.code_size();
    let step_info = emit_run_like_entry(code, true, ticks, options)?;
    let return_to_dispatcher_offset = code.code_size();
    emit_return_to_dispatcher(code, return_from_run_code_offset, dispatcher)?;
    emit_and_patch_rsb_return_to_dispatcher_literal(
        code,
        return_to_dispatcher_offset,
        &[
            run_info.rsb_literal_load_offset,
            step_info.rsb_literal_load_offset,
        ],
    )?;

    code.seal();

    Ok(PreludeInfo {
        end_of_prelude: code.code_size(),
        run_code: unsafe { std::mem::transmute(code.code_base_ptr().add(run_offset)) },
        step_code: unsafe { std::mem::transmute(code.code_base_ptr().add(step_offset)) },
        return_to_dispatcher_offset,
        return_to_dispatcher: unsafe { code.code_base_ptr().add(return_to_dispatcher_offset) },
        return_from_run_code_offset,
        return_from_run_code: unsafe { code.code_base_ptr().add(return_from_run_code_offset) },
        read_memory_8: None,
        read_memory_16: None,
        read_memory_32: None,
        read_memory_64: None,
        read_memory_128: None,
        wrapped_read_memory_8: None,
        wrapped_read_memory_16: None,
        wrapped_read_memory_32: None,
        wrapped_read_memory_64: None,
        wrapped_read_memory_128: None,
        exclusive_read_memory_8: None,
        exclusive_read_memory_16: None,
        exclusive_read_memory_32: None,
        exclusive_read_memory_64: None,
        exclusive_read_memory_128: None,
        write_memory_8: None,
        write_memory_16: None,
        write_memory_32: None,
        write_memory_64: None,
        write_memory_128: None,
        wrapped_write_memory_8: None,
        wrapped_write_memory_16: None,
        wrapped_write_memory_32: None,
        wrapped_write_memory_64: None,
        wrapped_write_memory_128: None,
        exclusive_write_memory_8: None,
        exclusive_write_memory_16: None,
        exclusive_write_memory_32: None,
        exclusive_write_memory_64: None,
        exclusive_write_memory_128: None,
        call_svc: None,
        exception_raised: None,
        dc_raised: None,
        ic_raised: None,
        isb_raised: None,
        get_cntpct: None,
        add_ticks: None,
        get_ticks_remaining: None,
    })
}

fn emit_return_to_dispatcher(
    code: &mut CodeGenerator<'_>,
    return_from_run_code_offset: usize,
    dispatcher: Option<DispatcherCallback>,
) -> Result<(), String> {
    let Some(dispatcher) = dispatcher else {
        let return_to_dispatcher_offset = code.code_size();
        code.b_offset(return_from_run_code_offset as isize - return_to_dispatcher_offset as isize)?;
        return Ok(());
    };

    code.ldar(WReg::new(XSCRATCH0), rhazel::XRegSp::new(XHALT))?;
    let halt_branch_offset = {
        let offset = code.code_size();
        code.cbnz_offset(WReg::new(XSCRATCH0), 0)?;
        offset
    };
    let cycle_branch_offset = if dispatcher.ticks.is_some() {
        code.cmp_imm(XReg::new(XTICKS), 0)?;
        Some({
            let offset = code.code_size();
            code.b_cond_offset(Cond::LE, 0)?;
            offset
        })
    } else {
        None
    };
    let load_this_offset = {
        let offset = code.code_size();
        code.nop()?;
        offset
    };
    code.mov(XReg::new(X1), XReg::new(XSTATE))?;
    let load_fn_offset = {
        let offset = code.code_size();
        code.nop()?;
        offset
    };
    code.blr(XReg::new(XSCRATCH0))?;
    code.br(XReg::new(X0))?;

    let halt_branch_pc_offset =
        i32::try_from(return_from_run_code_offset as isize - halt_branch_offset as isize)
            .map_err(|_| "ARM64 return_to_dispatcher halt branch offset overflow".to_string())?;
    CodeGenerator::patch_at(code, halt_branch_offset, false)
        .cbnz_offset(WReg::new(XSCRATCH0), halt_branch_pc_offset)?;
    if let Some(cycle_branch_offset) = cycle_branch_offset {
        let cycle_branch_pc_offset =
            i32::try_from(return_from_run_code_offset as isize - cycle_branch_offset as isize)
                .map_err(|_| {
                    "ARM64 return_to_dispatcher cycle branch offset overflow".to_string()
                })?;
        CodeGenerator::patch_at(code, cycle_branch_offset, false)
            .b_cond_offset(Cond::LE, cycle_branch_pc_offset)?;
    }

    let pc_after_body = code.code_size();
    let this_data_offset = (pc_after_body + 7) & !7;
    let fn_data_offset = this_data_offset + 8;

    CodeGenerator::patch_at(code, load_this_offset, false).ldr_literal(
        XReg::new(X0),
        (this_data_offset as isize - load_this_offset as isize) as i32,
    )?;
    CodeGenerator::patch_at(code, load_fn_offset, false).ldr_literal(
        XReg::new(XSCRATCH0),
        (fn_data_offset as isize - load_fn_offset as isize) as i32,
    )?;
    code.align(8)?;
    let written_this_offset = code.write_u64(dispatcher.this_ptr as usize as u64)?;
    let written_fn_offset = code.write_u64(dispatcher.fn_ptr as usize as u64)?;

    if written_this_offset != this_data_offset || written_fn_offset != fn_data_offset {
        return Err("ARM64 return_to_dispatcher literal offsets diverged".to_string());
    }

    Ok(())
}

/// Emit the common upstream `EmitCallTrampoline` shape.
///
/// The generated code loads `this_ptr` into X0, loads `fn_ptr` into Xscratch0,
/// then branches to the function. The explicit data words mirror the literal
/// pool used by upstream oaknut code.
pub fn emit_call_trampoline(
    code: &mut CodeGenerator<'_>,
    this_ptr: *const c_void,
    fn_ptr: *const c_void,
) -> Result<*const u8, String> {
    let target_offset = code.code_size();
    let this_data_offset = (target_offset + 12 + 7) & !7;
    let fn_data_offset = this_data_offset + 8;

    code.ldr_literal(
        XReg::new(X0),
        (this_data_offset as isize - target_offset as isize) as i32,
    )?;
    code.ldr_literal(
        XReg::new(XSCRATCH0),
        (fn_data_offset as isize - (target_offset + 4) as isize) as i32,
    )?;
    code.br(XReg::new(XSCRATCH0))?;
    code.align(8)?;
    let written_this_offset = code.write_u64(this_ptr as usize as u64)?;
    let written_fn_offset = code.write_u64(fn_ptr as usize as u64)?;

    if written_this_offset != this_data_offset || written_fn_offset != fn_data_offset {
        return Err("ARM64 call trampoline literal offsets diverged".to_string());
    }

    Ok(unsafe { code.code_base_ptr().add(target_offset) })
}

/// Emit upstream A32/A64 `EmitWrappedReadCallTrampoline`.
///
/// Generated memory emitters use Xscratch0 to carry the guest address in the
/// wrapped path. The trampoline preserves caller-save state, calls the host
/// callback as `(this, Xscratch0)`, then moves the return value back into
/// Xscratch0 before returning to generated code.
pub fn emit_wrapped_read_call_trampoline(
    code: &mut CodeGenerator<'_>,
    this_ptr: *const c_void,
    fn_ptr: *const c_void,
) -> Result<*const u8, String> {
    let target_offset = code.code_size();
    let save_regs = ABI_CALLER_SAVE & !to_reg_list_gpr(XSCRATCH0);

    abi::emit_push_registers(code, save_regs, 0)?;
    emit_load_this_and_call(
        code,
        this_ptr,
        fn_ptr,
        |code| {
            code.mov(XReg::new(X1), XReg::new(XSCRATCH0))?;
            Ok(())
        },
        |code| {
            code.mov(XReg::new(XSCRATCH0), XReg::new(X0))?;
            abi::emit_pop_registers(code, save_regs, 0)?;
            code.ret()?;
            Ok(())
        },
    )?;

    Ok(unsafe { code.code_base_ptr().add(target_offset) })
}

/// Emit upstream A32/A64 `EmitWrappedWriteCallTrampoline`.
///
/// The wrapped write path passes the guest address/value through Xscratch0 and
/// Xscratch1, preserving generated-code caller-save state around the host call.
pub fn emit_wrapped_write_call_trampoline(
    code: &mut CodeGenerator<'_>,
    this_ptr: *const c_void,
    fn_ptr: *const c_void,
) -> Result<*const u8, String> {
    let target_offset = code.code_size();

    abi::emit_push_registers(code, ABI_CALLER_SAVE, 0)?;
    emit_load_this_and_call(
        code,
        this_ptr,
        fn_ptr,
        |code| {
            code.mov(XReg::new(X1), XReg::new(XSCRATCH0))?;
            code.mov(XReg::new(X2), XReg::new(XSCRATCH1))?;
            Ok(())
        },
        |code| {
            abi::emit_pop_registers(code, ABI_CALLER_SAVE, 0)?;
            code.ret()?;
            Ok(())
        },
    )?;

    Ok(unsafe { code.code_base_ptr().add(target_offset) })
}

pub fn emit_read128_call_trampoline(
    code: &mut CodeGenerator<'_>,
    this_ptr: *const c_void,
    fn_ptr: *const c_void,
) -> Result<*const u8, String> {
    let target_offset = code.code_size();
    let save_regs = to_reg_list_gpr(29) | to_reg_list_gpr(30);

    abi::emit_push_registers(code, save_regs, 0)?;
    emit_load_this_and_call(
        code,
        this_ptr,
        fn_ptr,
        |_| Ok(()),
        |code| {
            code.fmov_from_gp(rhazel::DReg::new(0), XReg::new(X0))?;
            code.fmov_high_from_gp(rhazel::VReg::new(0).d2(), XReg::new(X1))?;
            abi::emit_pop_registers(code, save_regs, 0)?;
            code.ret()?;
            Ok(())
        },
    )?;

    Ok(unsafe { code.code_base_ptr().add(target_offset) })
}

pub fn emit_wrapped_read128_call_trampoline(
    code: &mut CodeGenerator<'_>,
    this_ptr: *const c_void,
    fn_ptr: *const c_void,
) -> Result<*const u8, String> {
    let target_offset = code.code_size();
    let save_regs = ABI_CALLER_SAVE & !to_reg_list_vec(0);

    abi::emit_push_registers(code, save_regs, 0)?;
    emit_load_this_and_call(
        code,
        this_ptr,
        fn_ptr,
        |code| {
            code.mov(XReg::new(X1), XReg::new(XSCRATCH0))?;
            Ok(())
        },
        |code| {
            code.fmov_from_gp(rhazel::DReg::new(0), XReg::new(X0))?;
            code.fmov_high_from_gp(rhazel::VReg::new(0).d2(), XReg::new(X1))?;
            abi::emit_pop_registers(code, save_regs, 0)?;
            code.ret()?;
            Ok(())
        },
    )?;

    Ok(unsafe { code.code_base_ptr().add(target_offset) })
}

pub fn emit_write128_call_trampoline(
    code: &mut CodeGenerator<'_>,
    this_ptr: *const c_void,
    fn_ptr: *const c_void,
) -> Result<*const u8, String> {
    let target_offset = code.code_size();
    emit_load_this_and_branch(code, this_ptr, fn_ptr, |code| {
        code.fmov_to_gp(XReg::new(X2), rhazel::DReg::new(0))?;
        code.fmov_high_to_gp(XReg::new(X3), rhazel::VReg::new(0).d2())?;
        Ok(())
    })?;

    Ok(unsafe { code.code_base_ptr().add(target_offset) })
}

pub fn emit_wrapped_write128_call_trampoline(
    code: &mut CodeGenerator<'_>,
    this_ptr: *const c_void,
    fn_ptr: *const c_void,
) -> Result<*const u8, String> {
    let target_offset = code.code_size();

    abi::emit_push_registers(code, ABI_CALLER_SAVE, 0)?;
    emit_load_this_and_call(
        code,
        this_ptr,
        fn_ptr,
        |code| {
            code.mov(XReg::new(X1), XReg::new(XSCRATCH0))?;
            code.fmov_to_gp(XReg::new(X2), rhazel::DReg::new(0))?;
            code.fmov_high_to_gp(XReg::new(X3), rhazel::VReg::new(0).d2())?;
            Ok(())
        },
        |code| {
            abi::emit_pop_registers(code, ABI_CALLER_SAVE, 0)?;
            code.ret()?;
            Ok(())
        },
    )?;

    Ok(unsafe { code.code_base_ptr().add(target_offset) })
}

fn emit_load_this_and_call(
    code: &mut CodeGenerator<'_>,
    this_ptr: *const c_void,
    fn_ptr: *const c_void,
    emit_argument_moves: impl FnOnce(&mut CodeGenerator<'_>) -> Result<(), String>,
    emit_after_call: impl FnOnce(&mut CodeGenerator<'_>) -> Result<(), String>,
) -> Result<(), String> {
    let ldr_this_offset = code.code_size();
    code.nop()?;
    emit_argument_moves(code)?;
    let ldr_fn_offset = code.code_size();
    code.nop()?;
    code.blr(XReg::new(XSCRATCH0))?;
    emit_after_call(code)?;

    let pc_after_body = code.code_size();
    let this_data_offset = (pc_after_body + 7) & !7;
    let fn_data_offset = this_data_offset + 8;

    CodeGenerator::patch_at(code, ldr_this_offset, false).ldr_literal(
        XReg::new(X0),
        (this_data_offset as isize - ldr_this_offset as isize) as i32,
    )?;
    CodeGenerator::patch_at(code, ldr_fn_offset, false).ldr_literal(
        XReg::new(XSCRATCH0),
        (fn_data_offset as isize - ldr_fn_offset as isize) as i32,
    )?;
    code.align(8)?;
    let written_this_offset = code.write_u64(this_ptr as usize as u64)?;
    let written_fn_offset = code.write_u64(fn_ptr as usize as u64)?;

    if written_this_offset != this_data_offset || written_fn_offset != fn_data_offset {
        return Err("ARM64 wrapped call trampoline literal offsets diverged".to_string());
    }

    Ok(())
}

fn emit_load_this_and_branch(
    code: &mut CodeGenerator<'_>,
    this_ptr: *const c_void,
    fn_ptr: *const c_void,
    emit_argument_moves: impl FnOnce(&mut CodeGenerator<'_>) -> Result<(), String>,
) -> Result<(), String> {
    let ldr_this_offset = code.code_size();
    code.nop()?;
    emit_argument_moves(code)?;
    let ldr_fn_offset = code.code_size();
    code.nop()?;
    code.br(XReg::new(XSCRATCH0))?;

    let pc_after_body = code.code_size();
    let this_data_offset = (pc_after_body + 7) & !7;
    let fn_data_offset = this_data_offset + 8;

    CodeGenerator::patch_at(code, ldr_this_offset, false).ldr_literal(
        XReg::new(X0),
        (this_data_offset as isize - ldr_this_offset as isize) as i32,
    )?;
    CodeGenerator::patch_at(code, ldr_fn_offset, false).ldr_literal(
        XReg::new(XSCRATCH0),
        (fn_data_offset as isize - ldr_fn_offset as isize) as i32,
    )?;
    code.align(8)?;
    let written_this_offset = code.write_u64(this_ptr as usize as u64)?;
    let written_fn_offset = code.write_u64(fn_ptr as usize as u64)?;

    if written_this_offset != this_data_offset || written_fn_offset != fn_data_offset {
        return Err("ARM64 branch trampoline literal offsets diverged".to_string());
    }

    Ok(())
}

const X0: u8 = 0;
const X1: u8 = 1;
const X2: u8 = 2;
const X3: u8 = 3;
const X19: u8 = 19;
const WZR: u8 = 31;

fn emit_run_like_entry(
    code: &mut CodeGenerator<'_>,
    step: bool,
    ticks: Option<TickCallbacks>,
    options: PreludeOptions,
) -> Result<RunLikeEntryInfo, String> {
    // Args match upstream: X0=entry_point, X1=jit_state, X2=halt_reason.
    let saved_registers = ABI_CALLEE_SAVE | abi::to_reg_list_gpr(abi::LR);
    abi::emit_push_registers(code, saved_registers, core::mem::size_of::<StackLayout>())?;
    code.mov(XReg::new(X19), XReg::new(X0))?;
    code.mov(XReg::new(XSTATE), XReg::new(X1))?;
    code.mov(XReg::new(XHALT), XReg::new(X2))?;
    if options.page_table_pointer != 0 {
        emit_mov_x_imm(code, XPAGETABLE, options.page_table_pointer)?;
    }
    if options.fastmem_pointer != 0 {
        emit_mov_x_imm(code, XFASTMEM, options.fastmem_pointer)?;
    }
    let rsb_literal_load_offset = if options.return_stack_buffer {
        Some(emit_a32_rsb_init(code)?)
    } else {
        None
    };
    if let Some(ticks) = ticks {
        if step {
            code.movz(XReg::new(XTICKS), 1, 0)?;
        } else {
            emit_call_get_ticks_remaining(code, ticks)?;
        }
        code.str(
            XReg::new(XTICKS),
            rhazel::SP,
            StackLayout::cycles_to_run_offset() as u32,
        )?;
    }

    emit_guest_fpcr_setup(code, options.isa)?;

    if step {
        // Upstream uses an LDAXR/STLXR retry loop to set HaltReason::Step
        // without overwriting an external halt reason.
        code.ldaxr(WReg::new(XSCRATCH0), rhazel::XRegSp::new(XHALT))?;
        code.cbnz_offset(WReg::new(XSCRATCH0), 20)?;
        code.movz(WReg::new(XSCRATCH0), HaltReason::STEP.bits() as u16, 0)?;
        code.stlxr(
            WReg::new(XSCRATCH1),
            WReg::new(XSCRATCH0),
            rhazel::XRegSp::new(XHALT),
        )?;
        code.cbnz_offset(WReg::new(XSCRATCH1), -16)?;
    }

    if !step {
        let cbnz_offset = 8;
        code.ldar(WReg::new(XSCRATCH0), rhazel::XRegSp::new(XHALT))?;
        code.cbnz_offset(WReg::new(XSCRATCH0), cbnz_offset)?;
    }
    code.br(XReg::new(X19))?;

    let return_from_run_code_offset = code.code_size();
    code.nop()?;
    if let Some(ticks) = ticks {
        code.ldr(
            XReg::new(X1),
            rhazel::SP,
            StackLayout::cycles_to_run_offset() as u32,
        )?;
        code.sub(XReg::new(X1), XReg::new(X1), XReg::new(XTICKS))?;
        emit_mov_x_imm(code, X0, ticks.this_ptr as usize as u64)?;
        emit_mov_x_imm(code, XSCRATCH0, ticks.add_ticks_fn_ptr as usize as u64)?;
        code.blr(XReg::new(XSCRATCH0))?;
    }
    emit_restore_host_fpcr(code)?;
    code.ldaxr(WReg::new(X0), rhazel::XRegSp::new(XHALT))?;
    code.stlxr(
        WReg::new(XSCRATCH0),
        WReg::new(WZR),
        rhazel::XRegSp::new(XHALT),
    )?;
    code.cbnz_offset(WReg::new(XSCRATCH0), -8)?;
    abi::emit_pop_registers(code, saved_registers, core::mem::size_of::<StackLayout>())?;
    code.ret()?;
    Ok(RunLikeEntryInfo {
        return_from_run_code_offset,
        rsb_literal_load_offset,
    })
}

fn emit_a32_rsb_init(code: &mut CodeGenerator<'_>) -> Result<usize, String> {
    let load_offset = {
        let offset = code.code_size();
        code.nop()?;
        offset
    };
    for i in 0..RSB_COUNT {
        let code_ptr_offset =
            StackLayout::rsb_entry_offset(i) + core::mem::offset_of!(RSBEntry, code_ptr);
        code.str(XReg::new(XSCRATCH0), rhazel::SP, code_ptr_offset as u32)?;
    }
    Ok(load_offset)
}

fn emit_and_patch_rsb_return_to_dispatcher_literal(
    code: &mut CodeGenerator<'_>,
    return_to_dispatcher_offset: usize,
    load_offsets: &[Option<usize>],
) -> Result<(), String> {
    if load_offsets.iter().all(Option::is_none) {
        return Ok(());
    }

    code.align(8)?;
    let literal_value =
        unsafe { code.code_base_ptr().add(return_to_dispatcher_offset) as usize as u64 };
    let literal_offset = code.write_u64(literal_value)?;
    for &load_offset in load_offsets.iter().flatten() {
        let pc_offset = i32::try_from(literal_offset as isize - load_offset as isize)
            .map_err(|_| "ARM64 RSB return_to_dispatcher literal offset overflow".to_string())?;
        CodeGenerator::patch_at(code, load_offset, false)
            .ldr_literal(XReg::new(XSCRATCH0), pc_offset)?;
    }

    Ok(())
}

fn emit_guest_fpcr_setup(code: &mut CodeGenerator<'_>, isa: PreludeIsa) -> Result<(), String> {
    match isa {
        PreludeIsa::A32 => {
            code.ldr(
                WReg::new(XSCRATCH0),
                rhazel::XRegSp::new(XSTATE),
                core::mem::offset_of!(A32JitState, upper_location_descriptor) as u32,
            )?;
            code.and_imm(WReg::new(XSCRATCH0), WReg::new(XSCRATCH0), 0xffff_0000)?;
            code.mrs(XReg::new(XSCRATCH1), SystemReg::FPCR)?;
            code.str(
                WReg::new(XSCRATCH1),
                rhazel::XRegSp::new(31),
                StackLayout::save_host_fpcr_offset() as u32,
            )?;
            code.msr(SystemReg::FPCR, XReg::new(XSCRATCH0))?;
            Ok(())
        }
        PreludeIsa::A64 => {
            code.mrs(XReg::new(XSCRATCH1), SystemReg::FPCR)?;
            code.str(
                WReg::new(XSCRATCH1),
                rhazel::XRegSp::new(31),
                StackLayout::save_host_fpcr_offset() as u32,
            )?;
            code.ldr(
                WReg::new(XSCRATCH0),
                rhazel::XRegSp::new(XSTATE),
                core::mem::offset_of!(A64JitState, fpcr) as u32,
            )?;
            code.msr(SystemReg::FPCR, XReg::new(XSCRATCH0))?;
            Ok(())
        }
    }
}

fn emit_restore_host_fpcr(code: &mut CodeGenerator<'_>) -> Result<(), String> {
    code.ldr(
        WReg::new(XSCRATCH0),
        rhazel::XRegSp::new(31),
        StackLayout::save_host_fpcr_offset() as u32,
    )?;
    code.msr(SystemReg::FPCR, XReg::new(XSCRATCH0))?;
    Ok(())
}

fn emit_call_get_ticks_remaining(
    code: &mut CodeGenerator<'_>,
    ticks: TickCallbacks,
) -> Result<(), String> {
    emit_mov_x_imm(code, X0, ticks.this_ptr as usize as u64)?;
    emit_mov_x_imm(
        code,
        XSCRATCH0,
        ticks.get_ticks_remaining_fn_ptr as usize as u64,
    )?;
    code.blr(XReg::new(XSCRATCH0))?;
    code.mov(XReg::new(XTICKS), XReg::new(X0))?;
    Ok(())
}

fn emit_mov_x_imm(code: &mut CodeGenerator<'_>, rd: u8, imm: u64) -> Result<(), String> {
    code.movz(XReg::new(rd), imm as u16, 0)?;
    code.movk(XReg::new(rd), (imm >> 16) as u16, 16)?;
    code.movk(XReg::new(rd), (imm >> 32) as u16, 32)?;
    code.movk(XReg::new(rd), (imm >> 48) as u16, 48)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::arm64::jit_state::{A32JitState, A64JitState};

    fn read_u32(code: &BlockOfCode, offset: usize) -> u32 {
        unsafe {
            code.code_base_ptr()
                .add(offset)
                .cast::<u32>()
                .read_unaligned()
        }
    }

    fn read_u64(code: &BlockOfCode, offset: usize) -> u64 {
        unsafe {
            code.code_base_ptr()
                .add(offset)
                .cast::<u64>()
                .read_unaligned()
        }
    }

    #[cfg(target_arch = "aarch64")]
    fn write_branch_to_return_from_run_code(
        block: &mut CodeGenerator<'_>,
        return_from_run_code: *const u8,
    ) {
        let source = unsafe { block.code_base_ptr().add(block.code_size()) };
        let pc_offset = (return_from_run_code as isize)
            .checked_sub(source as isize)
            .expect("branch offset");
        block.write_u32(inst::b_imm(pc_offset)).unwrap();
    }

    #[test]
    fn panic_payload_message_reads_string_and_str() {
        let owned: Box<dyn Any + Send> = Box::new(String::from("realized W result"));
        assert_eq!(panic_payload_message(&*owned), "realized W result");
        let borrowed: Box<dyn Any + Send> = Box::new("location not set");
        assert_eq!(panic_payload_message(&*borrowed), "location not set");
        let other: Box<dyn Any + Send> = Box::new(1u32);
        assert_eq!(panic_payload_message(&*other), "unknown panic payload");
    }

    #[test]
    fn emits_distinct_run_and_step_entries() {
        let mut code_storage = BlockOfCode::with_size(4096).expect("code cache");
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let prelude = emit_bootstrap_prelude(&mut code).expect("prelude");
        assert_ne!(prelude.run_code as usize, prelude.step_code as usize);
        assert_eq!(prelude.return_from_run_code_offset, 92);
        assert_eq!(prelude.return_to_dispatcher_offset, 348);
        assert_eq!(
            prelude.return_from_run_code as usize,
            code.code_base_ptr() as usize + 92
        );
        assert_eq!(
            prelude.return_to_dispatcher as usize,
            code.code_base_ptr() as usize + 348
        );
        assert_eq!(code.code_size(), 352);
        assert_eq!(prelude.end_of_prelude, 352);
    }

    #[test]
    fn a32_run_entry_saves_guest_fpcr_and_restores_host_fpcr() {
        let mut code_storage = BlockOfCode::with_size(4096).expect("code cache");
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let prelude = emit_bootstrap_prelude(&mut code).expect("prelude");
        let upper_location_descriptor =
            core::mem::offset_of!(A32JitState, upper_location_descriptor) as u32;
        let save_host_fpcr = StackLayout::save_host_fpcr_offset() as u32;

        let mut words = Vec::new();
        for offset in (0..prelude.return_to_dispatcher_offset).step_by(4) {
            words.push(read_u32(&code, offset));
        }

        assert_eq!(
            words
                .iter()
                .filter(|&&word| word
                    == inst::ldr_w_unsigned(XSCRATCH0, XSTATE, upper_location_descriptor))
                .count(),
            2
        );
        assert_eq!(
            words
                .iter()
                .filter(|&&word| word == inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0xffff_0000))
                .count(),
            2
        );
        assert_eq!(
            words
                .iter()
                .filter(|&&word| word == inst::mrs_fpcr(XSCRATCH1))
                .count(),
            2
        );
        assert_eq!(
            words
                .iter()
                .filter(|&&word| word == inst::str_w_unsigned(XSCRATCH1, 31, save_host_fpcr))
                .count(),
            2
        );

        assert_eq!(
            read_u32(&code, prelude.return_from_run_code_offset + 4),
            inst::ldr_w_unsigned(XSCRATCH0, 31, save_host_fpcr)
        );
        assert_eq!(
            read_u32(&code, prelude.return_from_run_code_offset + 8),
            inst::msr_fpcr(XSCRATCH0)
        );
    }

    #[test]
    fn a32_rsb_option_seeds_entries_with_return_to_dispatcher_literal() {
        let mut code_storage = BlockOfCode::with_size(4096).expect("code cache");
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let prelude = emit_bootstrap_prelude_with_options(
            &mut code,
            PreludeOptions {
                isa: PreludeIsa::A32,
                dispatcher: None,
                return_stack_buffer: true,
                page_table_pointer: 0,
                fastmem_pointer: 0,
            },
        )
        .expect("prelude");
        let literal_offset = code.code_size() - core::mem::size_of::<u64>();

        assert_eq!(
            read_u64(&code, literal_offset),
            prelude.return_to_dispatcher as usize as u64
        );

        let mut ldr_count = 0;
        for offset in (0..prelude.return_to_dispatcher_offset).step_by(4) {
            let pc_offset = (literal_offset as isize - offset as isize) as i32;
            if read_u32(&code, offset) == inst::ldr_x_lit(XSCRATCH0, pc_offset) {
                ldr_count += 1;
            }
        }
        assert_eq!(ldr_count, 2);

        for i in 0..RSB_COUNT {
            let code_ptr_offset =
                StackLayout::rsb_entry_offset(i) + core::mem::offset_of!(RSBEntry, code_ptr);
            let expected = inst::str_x_unsigned_sp(XSCRATCH0, code_ptr_offset as u32);
            let count = (0..prelude.return_to_dispatcher_offset)
                .step_by(4)
                .filter(|&offset| read_u32(&code, offset) == expected)
                .count();
            assert_eq!(
                count, 2,
                "RSB entry {i} code_ptr should be seeded in run and step"
            );
        }
    }

    #[test]
    fn a32_run_entries_load_page_table_and_fastmem_before_rsb_setup() {
        let mut code_storage = BlockOfCode::with_size(4096).expect("code cache");
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let page_table = 0x1111_2222_3333_4444;
        let fastmem = 0x5555_6666_7777_8888;
        let prelude = emit_bootstrap_prelude_with_options(
            &mut code,
            PreludeOptions {
                isa: PreludeIsa::A32,
                dispatcher: None,
                return_stack_buffer: true,
                page_table_pointer: page_table,
                fastmem_pointer: fastmem,
            },
        )
        .expect("prelude");

        let expected_page_table = [
            inst::movz_x(XPAGETABLE, page_table as u16, 0),
            inst::movk_x(XPAGETABLE, (page_table >> 16) as u16, 16),
            inst::movk_x(XPAGETABLE, (page_table >> 32) as u16, 32),
            inst::movk_x(XPAGETABLE, (page_table >> 48) as u16, 48),
        ];
        let expected_fastmem = [
            inst::movz_x(XFASTMEM, fastmem as u16, 0),
            inst::movk_x(XFASTMEM, (fastmem >> 16) as u16, 16),
            inst::movk_x(XFASTMEM, (fastmem >> 32) as u16, 32),
            inst::movk_x(XFASTMEM, (fastmem >> 48) as u16, 48),
        ];
        let expected_first_rsb_store = inst::str_x_unsigned_sp(
            XSCRATCH0,
            (StackLayout::rsb_entry_offset(0) + core::mem::offset_of!(RSBEntry, code_ptr)) as u32,
        );

        let words: Vec<u32> = (0..prelude.return_to_dispatcher_offset)
            .step_by(4)
            .map(|offset| read_u32(&code, offset))
            .collect();
        let page_table_pos = words
            .windows(expected_page_table.len())
            .position(|window| window == expected_page_table)
            .expect("page-table load sequence");
        let fastmem_pos = words
            .windows(expected_fastmem.len())
            .position(|window| window == expected_fastmem)
            .expect("fastmem load sequence");
        let first_rsb_store_pos = words
            .iter()
            .position(|&word| word == expected_first_rsb_store)
            .expect("first RSB store");

        assert!(page_table_pos < fastmem_pos);
        assert!(fastmem_pos < first_rsb_store_pos);
    }

    #[test]
    fn a64_run_entry_loads_fpcr_from_a64_state() {
        let mut code_storage = BlockOfCode::with_size(4096).expect("code cache");
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let prelude = emit_bootstrap_prelude_with_options(
            &mut code,
            PreludeOptions {
                isa: PreludeIsa::A64,
                dispatcher: None,
                return_stack_buffer: false,
                page_table_pointer: 0,
                fastmem_pointer: 0,
            },
        )
        .expect("prelude");
        let a32_upper_location_descriptor =
            core::mem::offset_of!(A32JitState, upper_location_descriptor) as u32;
        let a64_fpcr = core::mem::offset_of!(A64JitState, fpcr) as u32;
        let save_host_fpcr = StackLayout::save_host_fpcr_offset() as u32;

        let words: Vec<u32> = (0..prelude.return_to_dispatcher_offset)
            .step_by(4)
            .map(|offset| read_u32(&code, offset))
            .collect();
        assert_eq!(
            words
                .iter()
                .filter(|&&word| word
                    == inst::ldr_w_unsigned(XSCRATCH0, XSTATE, a32_upper_location_descriptor))
                .count(),
            0
        );
        assert_eq!(
            words
                .iter()
                .filter(|&&word| word == inst::ldr_w_unsigned(XSCRATCH0, XSTATE, a64_fpcr))
                .count(),
            2
        );
        assert_eq!(
            words
                .iter()
                .filter(|&&word| word == inst::mrs_fpcr(XSCRATCH1))
                .count(),
            2
        );
        assert_eq!(
            words
                .iter()
                .filter(|&&word| word == inst::str_w_unsigned(XSCRATCH1, 31, save_host_fpcr))
                .count(),
            2
        );
    }

    #[test]
    fn call_trampoline_matches_upstream_literal_shape() {
        let mut code_storage = BlockOfCode::with_size(4096).expect("code cache");
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let this_ptr = 0x1111_2222usize as *const c_void;
        let fn_ptr = 0x3333_4444usize as *const c_void;
        let trampoline = emit_call_trampoline(&mut code, this_ptr, fn_ptr).unwrap();

        assert_eq!(trampoline, code.code_base_ptr());
        assert_eq!(read_u32(&code, 0), inst::ldr_x_lit(X0, 16));
        assert_eq!(read_u32(&code, 4), inst::ldr_x_lit(XSCRATCH0, 20));
        assert_eq!(read_u32(&code, 8), inst::br(XSCRATCH0));
        assert_eq!(read_u32(&code, 12), inst::nop());
        assert_eq!(read_u64(&code, 16), this_ptr as usize as u64);
        assert_eq!(read_u64(&code, 24), fn_ptr as usize as u64);
        assert_eq!(code.code_size(), 32);
    }

    #[test]
    fn wrapped_call_trampoline_keeps_literals_after_executable_body() {
        let mut code_storage = BlockOfCode::with_size(4096).expect("code cache");
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let this_ptr = 0x1111_2222usize as *const c_void;
        let fn_ptr = 0x3333_4444usize as *const c_void;
        let trampoline = emit_wrapped_read_call_trampoline(&mut code, this_ptr, fn_ptr).unwrap();

        assert_eq!(trampoline, code.code_base_ptr());
        assert_eq!(
            read_u32(&code, code.code_size() - 20),
            inst::ret_lr(),
            "RET must precede the literal pool"
        );
        assert_eq!(
            read_u64(&code, code.code_size() - 16),
            this_ptr as usize as u64
        );
        assert_eq!(
            read_u64(&code, code.code_size() - 8),
            fn_ptr as usize as u64
        );
    }

    #[test]
    fn read128_trampoline_packs_pair_return_into_q0_before_ret() {
        let mut code_storage = BlockOfCode::with_size(4096).expect("code cache");
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let this_ptr = 0x1111_2222usize as *const c_void;
        let fn_ptr = 0x3333_4444usize as *const c_void;
        let trampoline = emit_read128_call_trampoline(&mut code, this_ptr, fn_ptr).unwrap();

        assert_eq!(trampoline, code.code_base_ptr());
        assert_eq!(
            read_u32(&code, code.code_size() - 40),
            inst::fmov_d_from_x(0, X0)
        );
        assert_eq!(
            read_u32(&code, code.code_size() - 36),
            inst::fmov_v_d1_from_x(0, X1)
        );
        assert_eq!(
            read_u32(&code, code.code_size() - 20),
            inst::ret_lr(),
            "RET must precede the literal pool"
        );
        assert_eq!(
            read_u64(&code, code.code_size() - 16),
            this_ptr as usize as u64
        );
        assert_eq!(
            read_u64(&code, code.code_size() - 8),
            fn_ptr as usize as u64
        );
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn call_trampoline_branches_to_target_with_loaded_this_pointer() {
        unsafe extern "C" fn return_this(this_ptr: *const c_void) -> usize {
            this_ptr as usize
        }

        let mut code_storage = BlockOfCode::with_size(4096).expect("code cache");
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let this_ptr = 0x1234_5678usize as *const c_void;
        let trampoline = emit_call_trampoline(
            &mut code,
            this_ptr,
            return_this as *const () as *const c_void,
        )
        .unwrap();
        code.seal();

        let func: unsafe extern "C" fn() -> usize = unsafe { std::mem::transmute(trampoline) };
        assert_eq!(unsafe { func() }, this_ptr as usize);
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn run_code_calls_entry_and_clears_halt_reason() {
        let mut prelude_code_storage = BlockOfCode::with_size(4096).expect("prelude cache");
        let mut prelude_code = rhazel::CodeGenerator::new(&mut prelude_code_storage);
        let prelude = emit_bootstrap_prelude(&mut prelude_code).unwrap();

        let mut block_storage = BlockOfCode::with_size(4096).expect("block cache");
        let mut block = rhazel::CodeGenerator::new(&mut block_storage);
        block.write_u32(inst::movz_w(0, 0x77, 0)).unwrap();
        write_branch_to_return_from_run_code(&mut block, prelude.return_from_run_code);
        block.seal();

        let mut state = A32JitState::new();
        let mut halt_reason = 0u32;
        let result = unsafe {
            (prelude.run_code)(
                block.code_base_ptr(),
                (&mut state as *mut A32JitState).cast::<c_void>(),
                &mut halt_reason,
            )
        };
        assert_eq!(result, 0);
        assert_eq!(halt_reason, 0);
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn run_code_returns_existing_halt_without_calling_entry() {
        let mut block_storage = BlockOfCode::with_size(4096).expect("block cache");
        let mut block = rhazel::CodeGenerator::new(&mut block_storage);
        block.write_u32(inst::movz_w(0, 0x77, 0)).unwrap();
        block.write_u32(inst::ret_lr()).unwrap();
        block.seal();

        let mut prelude_code_storage = BlockOfCode::with_size(4096).expect("prelude cache");
        let mut prelude_code = rhazel::CodeGenerator::new(&mut prelude_code_storage);
        let prelude = emit_bootstrap_prelude(&mut prelude_code).unwrap();

        let mut state = A32JitState::new();
        let mut halt_reason = HaltReason::MEMORY_ABORT.bits();
        let result = unsafe {
            (prelude.run_code)(
                block.code_base_ptr(),
                (&mut state as *mut A32JitState).cast::<c_void>(),
                &mut halt_reason,
            )
        };
        assert_eq!(result, HaltReason::MEMORY_ABORT.bits());
        assert_eq!(halt_reason, 0);
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn step_code_returns_step_halt_reason() {
        let mut prelude_code_storage = BlockOfCode::with_size(4096).expect("prelude cache");
        let mut prelude_code = rhazel::CodeGenerator::new(&mut prelude_code_storage);
        let prelude = emit_bootstrap_prelude(&mut prelude_code).unwrap();

        let mut block_storage = BlockOfCode::with_size(4096).expect("block cache");
        let mut block = rhazel::CodeGenerator::new(&mut block_storage);
        block.write_u32(inst::movz_w(3, 0x77, 0)).unwrap();
        block
            .write_u32(inst::str_w_unsigned(
                3,
                XSTATE,
                core::mem::offset_of!(A32JitState, regs) as u32,
            ))
            .unwrap();
        write_branch_to_return_from_run_code(&mut block, prelude.return_from_run_code);
        block.seal();

        let mut state = A32JitState::new();
        let mut halt_reason = 0u32;
        let result = unsafe {
            (prelude.step_code)(
                block.code_base_ptr(),
                (&mut state as *mut A32JitState).cast::<c_void>(),
                &mut halt_reason,
            )
        };
        assert_eq!(result, HaltReason::STEP.bits());
        assert_eq!(halt_reason, 0);
        assert_eq!(state.regs[0], 0x77);
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn step_code_returns_existing_halt_without_overwriting_it() {
        let mut block_storage = BlockOfCode::with_size(4096).expect("block cache");
        let mut block = rhazel::CodeGenerator::new(&mut block_storage);
        block.write_u32(inst::movz_w(0, 0x77, 0)).unwrap();
        block.write_u32(inst::ret_lr()).unwrap();
        block.seal();

        let mut prelude_code_storage = BlockOfCode::with_size(4096).expect("prelude cache");
        let mut prelude_code = rhazel::CodeGenerator::new(&mut prelude_code_storage);
        let prelude = emit_bootstrap_prelude(&mut prelude_code).unwrap();

        let mut state = A32JitState::new();
        let mut halt_reason = HaltReason::MEMORY_ABORT.bits();
        let result = unsafe {
            (prelude.step_code)(
                block.code_base_ptr(),
                (&mut state as *mut A32JitState).cast::<c_void>(),
                &mut halt_reason,
            )
        };
        assert_eq!(result, HaltReason::MEMORY_ABORT.bits());
        assert_eq!(halt_reason, 0);
    }
}

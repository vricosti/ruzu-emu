//! SIGSEGV/Mach exception handler for fastmem fallback.
//!
//! On Linux: matches upstream `dynarmic/backend/exception_handler_posix.cpp`
//! using SIGSEGV + ucontext_t / gregs[REG_RIP].
//!
//! On macOS: upstream uses Mach exceptions (`exception_handler_macos.cpp`),
//! which requires a dedicated Mach port thread and x86_thread_state64_t /
//! arm_thread_state64_t access. Since rdynarmic only has an x64 backend and
//! macOS arm64 cannot run x64 JIT code natively, we provide a stub that
//! disables fastmem on macOS (matching upstream's `SupportsFastmem() = false`
//! path when the Mach handler is absent).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::ir::location::LocationDescriptor;

/// Identifies a microinstruction within a block, for the do-not-fastmem set.
///
/// Matches upstream `using DoNotFastmemMarker = std::tuple<IR::LocationDescriptor, unsigned>;`
/// where the second element is `inst->GetName()` — a unique id of the
/// microinstruction within its block. In rdynarmic we use the `InstRef`
/// index value (as `u32`).
pub type DoNotFastmemMarker = (LocationDescriptor, u32);

/// Information recorded for each fastmem memory instruction.
#[derive(Debug)]
pub struct FastmemPatchInfo {
    /// Address to resume after the fallback stub returns.
    pub resume_rip: u64,
    /// Address of the per-register fallback stub to call.
    pub callback: u64,
    /// Marker identifying the source microinstruction; inserted into
    /// `do_not_fastmem` if `recompile` is set and a fault occurs.
    pub marker: Option<DoNotFastmemMarker>,
    /// Whether to recompile the block without fastmem on repeated faults.
    pub recompile: bool,
    /// Set by the exception handler and drained after generated code returns.
    pending_recompile: AtomicBool,
}

impl FastmemPatchInfo {
    pub fn new(
        resume_rip: u64,
        callback: u64,
        marker: Option<DoNotFastmemMarker>,
        recompile: bool,
    ) -> Self {
        Self {
            resume_rip,
            callback,
            marker,
            recompile,
            pending_recompile: AtomicBool::new(false),
        }
    }
}

/// Redirected call information returned by the fastmem callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FakeCall {
    /// Address of fallback function to jump to.
    pub call_rip: u64,
    /// Address to return to after fallback completes (pushed on stack).
    pub ret_rip: u64,
}

/// Callback type: given faulting RIP, returns FakeCall or None.
type FastmemCallback = Box<dyn Fn(u64) -> Option<FakeCall> + Send + Sync>;

/// Per-emitter exception-handler registration.
///
/// Mirrors upstream `Dynarmic::Backend::ExceptionHandler`: `register` creates
/// the platform implementation, `set_fastmem_callback` publishes the code
/// range, and dropping the owner removes that range before its JIT storage is
/// released.
pub struct ExceptionHandler {
    code_range: Option<(u64, u64)>,
}

impl ExceptionHandler {
    pub const fn new() -> Self {
        Self { code_range: None }
    }

    pub fn register(&mut self, code_begin: *const u8, code_size: usize) {
        self.unregister();

        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        register_handler();

        self.code_range = Some((code_begin as u64, code_size as u64));
    }

    pub fn supports_fastmem(&self) -> bool {
        self.code_range.is_some() && platform_supports_fastmem()
    }

    pub fn set_fastmem_callback(&self, callback: FastmemCallback) {
        let Some((code_begin, code_size)) = self.code_range else {
            return;
        };
        set_code_block_callback(code_begin, code_size, callback);
    }

    fn unregister(&mut self) {
        let Some((code_begin, _)) = self.code_range.take() else {
            return;
        };
        unregister_code_block(code_begin as *const u8);
    }
}

impl Default for ExceptionHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ExceptionHandler {
    fn drop(&mut self) {
        self.unregister();
    }
}

/// Whether the process-global platform handler was installed successfully.
///
/// Low-level emit helpers use this after their owning `ExceptionHandler` has
/// registered. Per-emitter decisions should use
/// `ExceptionHandler::supports_fastmem`, matching Eden's ownership.
pub fn supports_fastmem() -> bool {
    platform_supports_fastmem()
}

// ── Linux-only: SIGSEGV-based fastmem handler ─────────────────────────────────
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
use std::sync::RwLock;

/// Code block range with its associated fastmem callback.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
struct CodeBlockInfo {
    size: u64,
    callback: FastmemCallback,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
struct MappedSignalStack {
    memory: *mut libc::c_void,
    size: usize,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
unsafe impl Send for MappedSignalStack {}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl MappedSignalStack {
    fn new() -> Option<Self> {
        let size = usize::max(libc::SIGSTKSZ, 2 * 1024 * 1024);
        let memory = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        (memory != libc::MAP_FAILED).then_some(Self { memory, size })
    }

    fn activate(&self) -> bool {
        let signal_stack = libc::stack_t {
            ss_sp: self.memory,
            ss_flags: 0,
            ss_size: self.size,
        };
        unsafe { libc::sigaltstack(&signal_stack, std::ptr::null_mut()) == 0 }
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl Drop for MappedSignalStack {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.memory, self.size);
        }
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
struct ThreadSignalStack(MappedSignalStack);

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl Drop for ThreadSignalStack {
    fn drop(&mut self) {
        unsafe {
            let mut current: libc::stack_t = std::mem::zeroed();
            if libc::sigaltstack(std::ptr::null(), &mut current) == 0
                && current.ss_sp == self.0.memory
                && current.ss_flags & libc::SS_DISABLE == 0
            {
                let disabled = libc::stack_t {
                    ss_sp: std::ptr::null_mut(),
                    ss_flags: libc::SS_DISABLE,
                    ss_size: 0,
                };
                libc::sigaltstack(&disabled, std::ptr::null_mut());
            }
        }
    }
}

/// Global signal handler state. There is one process-wide signal disposition,
/// while the code-block registry uses reader/writer locking like Eden's
/// `std::shared_mutex`.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
struct SigHandlerState {
    supports_fast_mem: bool,
    _signal_stack: Option<MappedSignalStack>,
    code_blocks: HashMap<u64, CodeBlockInfo>,
    old_sa_segv: libc::sigaction,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
unsafe impl Send for SigHandlerState {}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
unsafe impl Sync for SigHandlerState {}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
static SIG_HANDLER: RwLock<Option<SigHandlerState>> = RwLock::new(None);

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn register_handler() {
    let mut guard = SIG_HANDLER
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.is_none() {
        *guard = Some(install_signal_handler());
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn platform_supports_fastmem() -> bool {
    SIG_HANDLER
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .is_some_and(|state| state.supports_fast_mem)
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
fn platform_supports_fastmem() -> bool {
    true
}

#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "x86_64")
)))]
fn platform_supports_fastmem() -> bool {
    false
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn set_code_block_callback(code_begin: u64, code_size: u64, callback: FastmemCallback) {
    let mut guard = SIG_HANDLER
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(state) = guard.as_mut() {
        state.code_blocks.insert(
            code_begin,
            CodeBlockInfo {
                size: code_size,
                callback,
            },
        );
    }
}

#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "x86_64")
)))]
fn set_code_block_callback(_code_begin: u64, _code_size: u64, _callback: FastmemCallback) {}

/// Register a per-thread alternate signal stack for the current thread.
/// Linux only — no-op on macOS.
#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "x86_64")
)))]
pub fn register_thread_signal_stack() {}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn register_thread_signal_stack() {
    thread_local! {
        static SIGNAL_STACK: std::cell::RefCell<Option<ThreadSignalStack>> = const {
            std::cell::RefCell::new(None)
        };
    }
    SIGNAL_STACK.with(|slot| {
        if slot.borrow().is_some() {
            return;
        }
        if let Some(stack) = MappedSignalStack::new() {
            if stack.activate() {
                *slot.borrow_mut() = Some(ThreadSignalStack(stack));
            } else {
                eprintln!("dynarmic: POSIX SigHandler: init failure at sigaltstack");
            }
        } else {
            eprintln!("dynarmic: POSIX SigHandler: could not allocate signal stack");
        }
    });
}

/// Unregister a JIT code region. Linux only — no-op on macOS.
#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "x86_64")
)))]
pub fn unregister_code_block(_code_begin: *const u8) {}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn unregister_code_block(code_begin: *const u8) {
    let mut guard = SIG_HANDLER
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(state) = guard.as_mut() {
        state.code_blocks.remove(&(code_begin as u64));
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn install_signal_handler() -> SigHandlerState {
    unsafe {
        let signal_stack = MappedSignalStack::new();
        let mut old_sa_segv: libc::sigaction = std::mem::zeroed();
        let Some(signal_stack) = signal_stack else {
            eprintln!("dynarmic: POSIX SigHandler: could not allocate signal stack");
            return SigHandlerState {
                supports_fast_mem: false,
                _signal_stack: None,
                code_blocks: HashMap::new(),
                old_sa_segv,
            };
        };
        if !signal_stack.activate() {
            eprintln!("dynarmic: POSIX SigHandler: init failure at sigaltstack");
            return SigHandlerState {
                supports_fast_mem: false,
                _signal_stack: Some(signal_stack),
                code_blocks: HashMap::new(),
                old_sa_segv,
            };
        }

        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = sig_action as usize;
        sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);

        if libc::sigaction(libc::SIGSEGV, &sa, &mut old_sa_segv) != 0 {
            eprintln!("dynarmic: POSIX SigHandler: could not set SIGSEGV handler");
            return SigHandlerState {
                supports_fast_mem: false,
                _signal_stack: Some(signal_stack),
                code_blocks: HashMap::new(),
                old_sa_segv,
            };
        }

        SigHandlerState {
            supports_fast_mem: true,
            _signal_stack: Some(signal_stack),
            code_blocks: HashMap::new(),
            old_sa_segv,
        }
    }
}

/// SIGSEGV signal handler. Linux only.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
extern "C" fn sig_action(
    sig: libc::c_int,
    info: *mut libc::siginfo_t,
    raw_context: *mut libc::c_void,
) {
    unsafe {
        let ucontext = &mut *(raw_context as *mut libc::ucontext_t);
        let mctx = &mut ucontext.uc_mcontext;
        let rip = mctx.gregs[libc::REG_RIP as usize] as u64;
        let rsp_ref = &mut mctx.gregs[libc::REG_RSP as usize];

        // Match upstream `SigHandler::SigAction`: dispatch faults in registered JIT code and
        // otherwise chain to the handler that was installed before Dynarmic.
        let guard = SIG_HANDLER
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(state) = guard.as_ref() {
            for (&code_begin, block) in &state.code_blocks {
                if rip >= code_begin && rip < code_begin.wrapping_add(block.size) {
                    if let Some(fake_call) = (block.callback)(rip) {
                        // "Fake call": push ret_rip, set RIP to call_rip
                        *rsp_ref -= 8;
                        let stack_ptr = *rsp_ref as *mut u64;
                        *stack_ptr = fake_call.ret_rip;
                        mctx.gregs[libc::REG_RIP as usize] = fake_call.call_rip as i64;
                        return;
                    }
                }
            }
            eprintln!("Unhandled SIGSEGV at rip {rip:#018x}");
            let old_sa = std::ptr::read(&state.old_sa_segv);
            drop(guard);
            if old_sa.sa_flags & libc::SA_SIGINFO != 0 {
                let handler = std::mem::transmute::<
                    usize,
                    extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void),
                >(old_sa.sa_sigaction);
                handler(sig, info, raw_context);
                return;
            }
            if old_sa.sa_sigaction == libc::SIG_DFL {
                libc::signal(sig, libc::SIG_DFL);
                return;
            }
            if old_sa.sa_sigaction == libc::SIG_IGN {
                return;
            }
            let handler =
                std::mem::transmute::<usize, extern "C" fn(libc::c_int)>(old_sa.sa_sigaction);
            handler(sig);
            return;
        }

        // No handler — re-raise with default
        drop(guard);
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = libc::SIG_DFL;
        libc::sigaction(libc::SIGSEGV, &sa, std::ptr::null_mut());
    }
}

// ── Windows SEH-based fastmem handler ─────────────────────────────────────────
//
// Matches upstream `dynarmic/backend/x64/exception_handler_windows.cpp`.
//
// On Windows the OS uses Structured Exception Handling (SEH).  When a page
// fault fires inside JIT code the CPU unwinds through the registered
// RUNTIME_FUNCTION table and invokes our exception handler.  There is no
// signal mechanism.
//
// Implementation notes:
//  - `register_code_block` emits two small stubs into the code buffer and
//    writes UNWIND_INFO + RUNTIME_FUNCTION data after them.  Both structures
//    live inside the code buffer (RWX region), which is valid on Windows.
//  - The stubs are emitted as part of the initial setup before any blocks are
//    compiled; `EndAddress` covers the total capacity so we never need to
//    re-register as new blocks arrive.
//  - The `with_cb` stub calls the fixed Rust function `seh_fastmem_dispatch`
//    which looks up the faulting RIP in the global state and patches CONTEXT.
//  - UNWIND_INFO describes the prologue of `gen_run_code` (8 GPR pushes + sub
//    rsp 0xNNN + movaps for XMM6-15). Offsets and register codes match
//    exactly the instructions emitted by `block_of_code.rs`.
//
// References used for struct layouts:
//  Microsoft PE/COFF spec, chapter "x64 Exception Handling"
//  NT headers: RUNTIME_FUNCTION, UNWIND_INFO, UNWIND_CODE, CONTEXT

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
mod windows_seh {
    use std::sync::Mutex;

    use super::{FakeCall, FastmemCallback};

    // ── Win32 types (declared locally to avoid winapi/windows-sys dep) ─────────

    extern "system" {
        fn RtlAddFunctionTable(
            function_table: *mut RuntimeFunction,
            entry_count: u32,
            base_address: u64,
        ) -> u8;
        fn RtlDeleteFunctionTable(function_table: *mut RuntimeFunction) -> u8;
        #[cfg(test)]
        fn RtlVirtualUnwind(
            handler_type: u32,
            image_base: u64,
            control_pc: u64,
            function_entry: *mut RuntimeFunction,
            context_record: *mut u8,
            handler_data: *mut *mut core::ffi::c_void,
            establisher_frame: *mut u64,
            context_pointers: *mut core::ffi::c_void,
        ) -> *mut core::ffi::c_void;
    }

    #[repr(C)]
    struct RuntimeFunction {
        begin_address: u32,
        end_address: u32,
        unwind_data: u32,
    }

    // UNWIND_CODE field constants
    const UWOP_PUSH_NONVOL: u8 = 0;
    const UWOP_ALLOC_LARGE: u8 = 1;
    const UWOP_SAVE_XMM128: u8 = 8;

    // Register codes for UWOP_PUSH_NONVOL / UWOP_SAVE_XMM128
    const UWRC_RBX: u8 = 3;
    const UWRC_RBP: u8 = 5;
    const UWRC_RSI: u8 = 6;
    const UWRC_RDI: u8 = 7;
    const UWRC_R12: u8 = 12;
    const UWRC_R13: u8 = 13;
    const UWRC_R14: u8 = 14;
    const UWRC_R15: u8 = 15;

    // UNW_FLAG_EHANDLER — the UNWIND_INFO has an exception handler
    const UNW_FLAG_EHANDLER: u8 = 1;

    // ExceptionContinueSearch / ExceptionContinueExecution
    const EXCEPTION_CONTINUE_EXECUTION: i32 = 0;
    const EXCEPTION_CONTINUE_SEARCH: i32 = 1;

    // Windows CONTEXT struct offsets (x64, from WinNT.h).
    #[cfg(test)]
    const CTX_RBX_OFF: usize = 0x90;
    const CTX_RSP_OFF: usize = 0x98;
    #[cfg(test)]
    const CTX_RBP_OFF: usize = 0xA0;
    #[cfg(test)]
    const CTX_RSI_OFF: usize = 0xA8;
    #[cfg(test)]
    const CTX_RDI_OFF: usize = 0xB0;
    #[cfg(test)]
    const CTX_R12_OFF: usize = 0xD8;
    #[cfg(test)]
    const CTX_R13_OFF: usize = 0xE0;
    #[cfg(test)]
    const CTX_R14_OFF: usize = 0xE8;
    #[cfg(test)]
    const CTX_R15_OFF: usize = 0xF0;
    const CTX_RIP_OFF: usize = 0xF8;

    // ── Global state ────────────────────────────────────────────────────────────

    struct WinBlockInfo {
        code_begin: u64,
        code_end: u64,
        callback: FastmemCallback,
    }

    struct WinJitInfo {
        code_begin: u64,
        code_end: u64,
        code_blocks: Vec<WinBlockInfo>,
        runtime_fn_ptr: *mut RuntimeFunction,
        except_info_ptr: *mut u32,
        with_cb_rva: u32,
    }
    unsafe impl Send for WinJitInfo {}

    static WIN_SEH: Mutex<Vec<WinJitInfo>> = Mutex::new(Vec::new());

    // ── UNWIND_CODE helpers ─────────────────────────────────────────────────────

    fn push_nonvol(code_offset: u8, reg: u8) -> u16 {
        (code_offset as u16) | ((UWOP_PUSH_NONVOL as u16) << 8) | ((reg as u16) << 12)
    }
    fn alloc_large_op(code_offset: u8) -> u16 {
        (code_offset as u16) | ((UWOP_ALLOC_LARGE as u16) << 8) /* OpInfo=0 */
    }
    fn save_xmm128_op(code_offset: u8, xmm: u8) -> u16 {
        (code_offset as u16) | ((UWOP_SAVE_XMM128 as u16) << 8) | ((xmm as u16) << 12)
    }
    fn frame_entry(value: u16) -> u16 {
        value
    }

    /// Build the UNWIND_CODE array for our dispatcher prologue.
    ///
    /// Prologue sequence (must match `emit_push_callee_save_and_adjust_stack`):
    ///
    ///  push rbx    (1 byte,  offset 1)
    ///  push rsi    (1 byte,  offset 2)
    ///  push rdi    (1 byte,  offset 3)
    ///  push rbp    (1 byte,  offset 4)
    ///  push r12    (2 bytes, offset 6)
    ///  push r13    (2 bytes, offset 8)
    ///  push r14    (2 bytes, offset 10)
    ///  push r15    (2 bytes, offset 12)
    ///  sub  rsp, N (7 bytes, offset 19)
    ///  movaps [rsp+xmm_save_base+i*16], xmm6..xmm15
    ///
    /// `stack_allocation_size` is the exact amount subtracted from RSP by
    /// `emit_push_callee_save_and_adjust_stack`.
    fn build_unwind_codes(
        stack_allocation_size: usize,
        xmm_save_base: usize,
    ) -> (Vec<u16>, u8, u8) {
        let alloc_n = stack_allocation_size;
        // UWOP_ALLOC_LARGE OpInfo=0: next entry holds size / 8 as u16.
        assert!(alloc_n % 8 == 0, "alloc must be multiple of 8");
        let alloc_n8 = (alloc_n / 8) as u16;

        let mut prolog_offset = 19usize;
        let mut xmm_operations = Vec::with_capacity(10);
        for xmm in 6u8..=15 {
            let frame_offset = xmm_save_base + (xmm as usize - 6) * 16;
            let has_extended_register = xmm >= 8;
            let has_32_bit_displacement = frame_offset > i8::MAX as usize;
            prolog_offset += match (has_extended_register, has_32_bit_displacement) {
                (false, false) => 5,
                (true, false) => 6,
                (false, true) => 8,
                (true, true) => 9,
            };
            xmm_operations.push((prolog_offset as u8, xmm, frame_offset));
        }
        let prolog_size = prolog_offset as u8;

        let mut codes: Vec<u16> = Vec::with_capacity(30);
        for &(code_offset, xmm, frame_offset) in xmm_operations.iter().rev() {
            codes.push(save_xmm128_op(code_offset, xmm));
            codes.push(frame_entry((frame_offset / 16) as u16));
        }
        codes.extend_from_slice(&[
            // sub rsp, N (two-entry encoding: op then size)
            alloc_large_op(19),
            frame_entry(alloc_n8),
            // GPR pushes (CodeOffset = byte at which the instruction ends)
            push_nonvol(12, UWRC_R15),
            push_nonvol(10, UWRC_R14),
            push_nonvol(8, UWRC_R13),
            push_nonvol(6, UWRC_R12),
            push_nonvol(4, UWRC_RBP),
            push_nonvol(3, UWRC_RDI),
            push_nonvol(2, UWRC_RSI),
            push_nonvol(1, UWRC_RBX),
        ]);

        let count = codes.len() as u8;
        // CountOfCodes must be even for alignment (pad if needed).
        if codes.len() % 2 != 0 {
            codes.push(0);
        }
        (codes, count, prolog_size)
    }

    // ── Rust dispatch function (called from JIT stub) ───────────────────────────

    /// Called from the JIT-emitted `with_cb` stub when an SEH exception fires
    /// inside a registered JIT code range.
    ///
    /// `context_ptr` points to the Windows `CONTEXT` structure provided by the OS.
    /// Returns `EXCEPTION_CONTINUE_EXECUTION` (0) if the fault was handled,
    /// `EXCEPTION_CONTINUE_SEARCH` (1) otherwise.
    unsafe extern "system" fn seh_fastmem_dispatch(context_ptr: *mut u8) -> i32 {
        let rip = *(context_ptr.add(CTX_RIP_OFF) as *const u64);

        let guard = WIN_SEH.lock().unwrap();
        for jit in guard.iter() {
            for block in &jit.code_blocks {
                if rip >= block.code_begin && rip < block.code_end {
                    if let Some(FakeCall { call_rip, ret_rip }) = (block.callback)(rip) {
                        // Push ret_rip onto the guest stack (decrement Rsp, write value).
                        let rsp_ptr = context_ptr.add(CTX_RSP_OFF) as *mut u64;
                        *rsp_ptr -= 8;
                        let new_rsp = *rsp_ptr;
                        *(new_rsp as *mut u64) = ret_rip;
                        // Redirect execution to the fallback stub.
                        *(context_ptr.add(CTX_RIP_OFF) as *mut u64) = call_rip;
                        return EXCEPTION_CONTINUE_EXECUTION;
                    }
                }
            }
        }
        EXCEPTION_CONTINUE_SEARCH
    }

    // ── Code-buffer setup ────────────────────────────────────────────────────────

    /// Emit the two exception handler stubs and the UNWIND_INFO / RUNTIME_FUNCTION
    /// structures into the code buffer, then call `RtlAddFunctionTable`.
    ///
    /// Must be called after the dispatcher prelude is complete (so we know the
    /// current code size) but before the first user block is emitted.
    ///
    /// # Parameters
    /// - `code_buf_base`: base address of the code buffer (mmap'd RWX region)
    /// - `total_capacity`: total allocated size of the buffer (covers all future blocks)
    /// - `stack_allocation_size`: exact byte count subtracted from RSP
    ///
    /// # Returns
    pub fn setup_seh_in_code_buffer(
        code_buf_base: *mut u8,
        total_capacity: usize,
        stack_allocation_size: usize,
        xmm_save_base: usize,
        current_size: &mut usize,
    ) {
        // ── Helpers ───────────────────────────────────────────────────────────

        let write_bytes = |offset: &mut usize, bytes: &[u8]| {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    code_buf_base.add(*offset),
                    bytes.len(),
                );
            }
            *offset += bytes.len();
        };

        let align_to = |offset: &mut usize, align: usize| {
            let r = *offset % align;
            if r != 0 {
                *offset += align - r;
            }
        };

        // ── Stub 1: exception_handler_without_cb ─────────────────────────────
        // Returns ExceptionContinueSearch (1).
        //   mov eax, 1   → B8 01 00 00 00
        //   ret          → C3
        align_to(current_size, 16);
        let without_cb_rva = *current_size as u32;
        write_bytes(current_size, &[0xB8, 0x01, 0x00, 0x00, 0x00, 0xC3]);

        // ── Stub 2: exception_handler_with_cb ────────────────────────────────
        // Receives: RCX=EXCEPTION_RECORD*, RDX=Frame*, R8=CONTEXT*, R9=DISP_CTX*
        // We pass CONTEXT* (R8) as the single argument to `seh_fastmem_dispatch`.
        //
        // Windows x64 calling convention (the stub itself IS the handler so
        // it's entered with RCX/RDX/R8/R9 already set by the OS).
        //
        //   sub  rsp, 0x28        ; 48 83 EC 28  (4 bytes) shadow+align
        //   mov  rcx, r8          ; 4C 89 C1     (3 bytes) CONTEXT* → param
        //   mov  rax, <dispatch>  ; 48 B8 xx×8   (10 bytes)
        //   call rax              ; FF D0        (2 bytes)
        //   add  rsp, 0x28        ; 48 83 C4 28  (4 bytes)
        //   ret                   ; C3           (1 byte)
        align_to(current_size, 16);
        let with_cb_rva = *current_size as u32;

        let dispatch_addr = seh_fastmem_dispatch as *const () as usize as u64;
        let mut stub: Vec<u8> = vec![
            0x48, 0x83, 0xEC, 0x28, // sub rsp, 0x28
            0x4C, 0x89, 0xC1, // mov rcx, r8
            0x48, 0xB8, // mov rax, imm64 (prefix)
        ];
        stub.extend_from_slice(&dispatch_addr.to_le_bytes());
        stub.extend_from_slice(&[
            0xFF, 0xD0, // call rax
            0x48, 0x83, 0xC4, 0x28, // add rsp, 0x28
            0xC3, // ret
        ]);
        write_bytes(current_size, &stub);

        // ── UNWIND_INFO ───────────────────────────────────────────────────────
        align_to(current_size, 4);
        let unwind_info_rva = *current_size as u32;

        let (codes, count_codes, prolog_size) =
            build_unwind_codes(stack_allocation_size, xmm_save_base);

        // UNWIND_INFO header (4 bytes):
        //   byte 0: Version(3 bits)=1 | Flags(5 bits)=UNW_FLAG_EHANDLER
        //   byte 1: SizeOfProlog
        //   byte 2: CountOfCodes
        //   byte 3: FrameRegister(4 bits)=0 | FrameOffset(4 bits)=0
        let header = [
            1 | (UNW_FLAG_EHANDLER << 3), // Version=1, Flags=UNW_FLAG_EHANDLER
            prolog_size,
            count_codes,
            0u8, // no frame register
        ];
        write_bytes(current_size, &header);

        // UNWIND_CODE array (each entry is a u16, little-endian).
        for code in &codes {
            write_bytes(current_size, &code.to_le_bytes());
        }

        // UNW_EXCEPTION_INFO: ULONG ExceptionHandler (RVA of handler stub).
        // We start with without_cb; SetFastmemCallback updates it to with_cb.
        align_to(current_size, 4);
        let except_info_offset = *current_size; // we'll need to patch this later
        write_bytes(current_size, &without_cb_rva.to_le_bytes());

        // ── RUNTIME_FUNCTION ─────────────────────────────────────────────────
        align_to(current_size, 4);
        let rfunc_offset = *current_size;
        // BeginAddress = 0 (start of code buffer)
        // EndAddress   = total_capacity (covers all future compiled blocks)
        // UnwindData   = RVA of UNWIND_INFO
        let begin_addr: u32 = 0;
        let end_addr: u32 = total_capacity as u32;
        write_bytes(current_size, &begin_addr.to_le_bytes());
        write_bytes(current_size, &end_addr.to_le_bytes());
        write_bytes(current_size, &unwind_info_rva.to_le_bytes());

        // ── Register with Windows ────────────────────────────────────────────
        let rfunc_ptr = unsafe { code_buf_base.add(rfunc_offset) as *mut RuntimeFunction };
        unsafe { RtlAddFunctionTable(rfunc_ptr, 1, code_buf_base as u64) };

        // Store this code buffer's state for later callback updates and cleanup.
        let mut guard = WIN_SEH.lock().unwrap();
        guard.push(WinJitInfo {
            code_begin: code_buf_base as u64,
            code_end: code_buf_base as u64 + total_capacity as u64,
            code_blocks: Vec::new(),
            runtime_fn_ptr: rfunc_ptr,
            except_info_ptr: unsafe { code_buf_base.add(except_info_offset) as *mut u32 },
            with_cb_rva,
        });
    }

    // ── Public API ────────────────────────────────────────────────────────────

    /// Register a JIT code range with the SEH dispatcher and activate the
    /// with_cb handler stub.
    pub fn register_code_block_impl(
        code_begin: *const u8,
        code_end: *const u8,
        callback: FastmemCallback,
    ) {
        let mut guard = WIN_SEH.lock().unwrap();
        let address = code_begin as u64;
        let Some(jit) = guard
            .iter_mut()
            .find(|jit| address >= jit.code_begin && address < jit.code_end)
        else {
            return;
        };
        jit.code_blocks.push(WinBlockInfo {
            code_begin: code_begin as u64,
            code_end: code_end as u64,
            callback,
        });

        // Switch the UNWIND_INFO exception handler to the with_cb stub.
        if !jit.except_info_ptr.is_null() {
            unsafe { jit.except_info_ptr.write(jit.with_cb_rva) };
        }
    }

    pub fn unregister_code_block_impl(code_begin: *const u8) {
        let mut guard = WIN_SEH.lock().unwrap();
        if let Some(index) = guard
            .iter()
            .position(|jit| jit.code_begin == code_begin as u64)
        {
            let jit = guard.remove(index);
            if !jit.runtime_fn_ptr.is_null() {
                unsafe { RtlDeleteFunctionTable(jit.runtime_fn_ptr) };
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[repr(C, align(16))]
        struct ContextBuffer([u8; 1232]);

        fn code_offset(code: u16) -> u8 {
            code as u8
        }

        unsafe fn write_context_u64(context: &mut ContextBuffer, offset: usize, value: u64) {
            (context.0.as_mut_ptr().add(offset) as *mut u64).write_unaligned(value);
        }

        unsafe fn read_context_u64(context: &ContextBuffer, offset: usize) -> u64 {
            (context.0.as_ptr().add(offset) as *const u64).read_unaligned()
        }

        #[test]
        fn unwind_codes_match_windows_prologue_order_and_allocation() {
            let stack_allocation_size =
                crate::backend::x64::block_of_code::stack_frame_allocation_size(
                    core::mem::size_of::<crate::backend::x64::stack_layout::StackLayout>(),
                );
            let xmm_save_base =
                crate::backend::x64::block_of_code::xmm_save_base(core::mem::size_of::<
                    crate::backend::x64::stack_layout::StackLayout,
                >());
            let (codes, count, prolog_size) =
                build_unwind_codes(stack_allocation_size, xmm_save_base);

            assert_eq!(count, 30);
            assert_eq!(codes.len(), 30);
            assert_eq!(prolog_size, 107);

            let operation_indices = [
                0usize, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 22, 23, 24, 25, 26, 27, 28, 29,
            ];
            let operation_offsets: Vec<u8> = operation_indices
                .iter()
                .map(|index| code_offset(codes[*index]))
                .collect();
            assert_eq!(
                operation_offsets,
                vec![107, 98, 89, 80, 71, 62, 53, 44, 35, 27, 19, 12, 10, 8, 6, 4, 3, 2, 1]
            );
            assert_eq!(codes[21], (stack_allocation_size / 8) as u16);
        }

        #[test]
        fn registered_unwind_info_restores_dispatcher_stack_pointer() {
            let mut code =
                crate::backend::x64::block_of_code::BlockOfCode::with_size(4096).unwrap();
            let frame_size = core::mem::size_of::<crate::backend::x64::stack_layout::StackLayout>();
            let allocation =
                crate::backend::x64::block_of_code::stack_frame_allocation_size(frame_size);
            code.emit_push_callee_save_and_adjust_stack(frame_size)
                .unwrap();
            code.prelude_complete();

            let guard = WIN_SEH.lock().unwrap();
            let jit = guard
                .iter()
                .find(|jit| jit.code_begin == code.code_base_ptr() as u64)
                .unwrap();

            let mut stack = [0u64; 512];
            let caller_rsp = unsafe { stack.as_mut_ptr().add(384) } as u64;
            let return_rip = 0x0123_4567_89AB_CDEF;
            unsafe {
                (caller_rsp as *mut u64).write(return_rip);
            }

            let saved_registers = [
                0x0B0B_u64, 0x0606, 0x0707, 0x0505, 0x1212, 0x1313, 0x1414, 0x1515,
            ];
            for (index, value) in saved_registers.into_iter().enumerate() {
                unsafe {
                    ((caller_rsp - ((index + 1) * 8) as u64) as *mut u64).write(value);
                }
            }

            let mut context = ContextBuffer([0; 1232]);
            let dispatcher_rsp = caller_rsp - (saved_registers.len() * 8 + allocation) as u64;
            unsafe {
                write_context_u64(&mut context, CTX_RSP_OFF, dispatcher_rsp);
                write_context_u64(&mut context, CTX_RIP_OFF, jit.code_begin + 128);
            }

            let mut handler_data = core::ptr::null_mut();
            let mut establisher_frame = 0u64;
            unsafe {
                RtlVirtualUnwind(
                    0,
                    jit.code_begin,
                    jit.code_begin + 128,
                    jit.runtime_fn_ptr,
                    context.0.as_mut_ptr(),
                    &mut handler_data,
                    &mut establisher_frame,
                    core::ptr::null_mut(),
                );
            }

            assert_eq!(
                unsafe { read_context_u64(&context, CTX_RSP_OFF) },
                caller_rsp + 8
            );
            assert_eq!(
                unsafe { read_context_u64(&context, CTX_RIP_OFF) },
                return_rip
            );
            for (offset, expected) in [
                (CTX_RBX_OFF, saved_registers[0]),
                (CTX_RSI_OFF, saved_registers[1]),
                (CTX_RDI_OFF, saved_registers[2]),
                (CTX_RBP_OFF, saved_registers[3]),
                (CTX_R12_OFF, saved_registers[4]),
                (CTX_R13_OFF, saved_registers[5]),
                (CTX_R14_OFF, saved_registers[6]),
                (CTX_R15_OFF, saved_registers[7]),
            ] {
                assert_eq!(unsafe { read_context_u64(&context, offset) }, expected);
            }
        }
    }
}

// ── Windows public surface ─────────────────────────────────────────────────────

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
fn set_code_block_callback(code_begin: u64, code_size: u64, callback: FastmemCallback) {
    windows_seh::register_code_block_impl(
        code_begin as *const u8,
        (code_begin + code_size) as *const u8,
        callback,
    );
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub fn unregister_code_block(code_begin: *const u8) {
    windows_seh::unregister_code_block_impl(code_begin);
}

/// Called from `BlockOfCode` after the dispatcher prelude is complete to emit
/// SEH stubs + UNWIND_INFO + RUNTIME_FUNCTION into the code buffer.
///
/// `current_size` is a mutable reference to the assembler's current byte count;
/// it is advanced as data is written.
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub fn setup_seh_in_code_buffer(
    code_buf_base: *mut u8,
    total_capacity: usize,
    stack_allocation_size: usize,
    xmm_save_base: usize,
    current_size: &mut usize,
) {
    windows_seh::setup_seh_in_code_buffer(
        code_buf_base,
        total_capacity,
        stack_allocation_size,
        xmm_save_base,
        current_size,
    );
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub fn register_thread_signal_stack() {} // no-op on Windows

/// A per-emitter fastmem patch info table.
///
/// Records the RIP of each fastmem memory instruction and its fallback info.
/// Used by the SIGSEGV handler to redirect faulting instructions.
pub struct FastmemPatchTable {
    patches: HashMap<u64, FastmemPatchInfo>,
    /// Fast rejection for the overwhelmingly common no-fault path.
    ///
    /// The signal handler sets this together with the per-entry flag. The JIT
    /// checks it before scanning the patch table after generated code returns.
    pending_recompiles: AtomicBool,
}

impl FastmemPatchTable {
    pub fn new() -> Self {
        Self {
            patches: HashMap::new(),
            pending_recompiles: AtomicBool::new(false),
        }
    }

    /// Record a fastmem instruction at `rip` with its fallback info.
    pub fn add(&mut self, rip: u64, info: FastmemPatchInfo) {
        self.patches.insert(rip, info);
    }

    /// Look up a faulting RIP and return the FakeCall to redirect to.
    pub fn lookup(&self, rip: u64) -> Option<FakeCall> {
        self.patches.get(&rip).map(|info| FakeCall {
            call_rip: info.callback,
            ret_rip: info.resume_rip,
        })
    }

    /// Look up a fault and record upstream's recompile request without
    /// allocating or mutating JIT caches from an exception handler.
    ///
    /// Each emitter executes on one host thread at a time. The owning JIT
    /// drains this fixed-capacity queue immediately after generated code
    /// returns, then updates `do_not_fastmem` and invalidates the blocks.
    pub fn lookup_and_record_recompile(&self, rip: u64) -> Option<FakeCall> {
        let info = self.patches.get(&rip)?;
        if info.recompile && info.marker.is_some() {
            info.pending_recompile.store(true, Ordering::Release);
            self.pending_recompiles.store(true, Ordering::Release);
        }
        Some(FakeCall {
            call_rip: info.callback,
            ret_rip: info.resume_rip,
        })
    }

    /// Drain recompile requests after generated code has stopped executing.
    pub fn take_pending_recompiles(&self) -> Vec<DoNotFastmemMarker> {
        if !self.pending_recompiles.swap(false, Ordering::AcqRel) {
            return Vec::new();
        }

        self.patches
            .values()
            .filter_map(|info| {
                info.pending_recompile
                    .swap(false, Ordering::AcqRel)
                    .then_some(info.marker)
                    .flatten()
            })
            .collect()
    }

    /// Clear all patch info (called on cache clear).
    pub fn clear(&mut self) {
        self.patches.clear();
        self.pending_recompiles.store(false, Ordering::Relaxed);
    }

    pub fn len(&self) -> usize {
        self.patches.len()
    }
}

#[cfg(test)]
mod fastmem_patch_table_tests {
    use super::*;

    #[test]
    fn fastmem_support_requires_an_owner_and_uses_platform_state() {
        let mut handler = ExceptionHandler::new();
        assert!(!handler.supports_fastmem());

        handler.register(0x1000usize as *const u8, 0x1000);
        assert_eq!(handler.supports_fastmem(), supports_fastmem());
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn dropping_exception_handler_removes_owned_code_block() {
        let code_begin = 0x7fff_1234_0000usize as *const u8;
        let mut handler = ExceptionHandler::new();
        handler.register(code_begin, 0x2000);
        handler.set_fastmem_callback(Box::new(|_| None));

        assert!(SIG_HANDLER
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(|state| state.code_blocks.contains_key(&(code_begin as u64))));

        drop(handler);

        assert!(SIG_HANDLER
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(|state| !state.code_blocks.contains_key(&(code_begin as u64))));
    }

    #[test]
    fn recompile_lookup_records_marker() {
        let marker = (LocationDescriptor::new(0x1234), 7);
        let mut table = FastmemPatchTable::new();
        table.add(
            0x1000,
            FastmemPatchInfo::new(0x2000, 0x3000, Some(marker), true),
        );

        assert_eq!(
            table.lookup_and_record_recompile(0x1000),
            Some(FakeCall {
                call_rip: 0x3000,
                ret_rip: 0x2000,
            })
        );
        assert_eq!(table.take_pending_recompiles(), vec![marker]);
        assert!(table.take_pending_recompiles().is_empty());
    }

    #[test]
    fn non_recompiling_lookup_does_not_record_marker() {
        let mut table = FastmemPatchTable::new();
        table.add(
            0x1000,
            FastmemPatchInfo::new(
                0x2000,
                0x3000,
                Some((LocationDescriptor::new(0x1234), 7)),
                false,
            ),
        );

        assert!(table.lookup_and_record_recompile(0x1000).is_some());
        assert!(table.take_pending_recompiles().is_empty());
    }

    #[test]
    fn duplicate_faults_queue_one_recompile() {
        let marker = (LocationDescriptor::new(0x1234), 7);
        let mut table = FastmemPatchTable::new();
        table.add(
            0x1000,
            FastmemPatchInfo::new(0x2000, 0x3000, Some(marker), true),
        );

        for _ in 0..512 {
            assert!(table.lookup_and_record_recompile(0x1000).is_some());
        }
        assert_eq!(table.take_pending_recompiles(), vec![marker]);
    }

    #[test]
    fn no_fault_does_not_enter_patch_table_scan() {
        let mut table = FastmemPatchTable::new();
        for index in 0..1024 {
            table.add(
                0x1000 + index,
                FastmemPatchInfo::new(
                    0x2000 + index,
                    0x3000 + index,
                    Some((LocationDescriptor::new(0x4000 + index), index as u32)),
                    true,
                ),
            );
        }

        assert!(!table.pending_recompiles.load(Ordering::Relaxed));
        assert!(table.take_pending_recompiles().is_empty());
        assert!(!table.pending_recompiles.load(Ordering::Relaxed));
    }
}

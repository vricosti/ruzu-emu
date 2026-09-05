use rxbyak::dword_ptr;
use rxbyak::qword_ptr;
#[cfg(target_os = "windows")]
use rxbyak::xmmword_ptr;
use rxbyak::{CodeAssembler, JmpType, Reg, RegExp};
use rxbyak::{R14, R15, RAX, RBX, RSP};

use crate::backend::x64::a64_jitstate::A64JitState;
use crate::backend::x64::abi;
use crate::backend::x64::callback::Callback;
use crate::backend::x64::constant_pool::ConstantPool;
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::jitstate_info::JitStateInfo;
use crate::backend::x64::stack_layout::StackLayout;

pub(crate) fn get_host_features() -> HostFeature {
    use rxbyak::util::cpu;

    let cpu_info = cpu::Cpu::new();
    let mut features = HostFeature::empty();

    let mappings = [
        (cpu::SSSE3, HostFeature::SSSE3),
        (cpu::SSE41, HostFeature::SSE41),
        (cpu::SSE42, HostFeature::SSE42),
        (cpu::AVX, HostFeature::AVX),
        (cpu::AVX2, HostFeature::AVX2),
        (cpu::AVX512F, HostFeature::AVX512F),
        (cpu::AVX512CD, HostFeature::AVX512CD),
        (cpu::AVX512VL, HostFeature::AVX512VL),
        (cpu::AVX512BW, HostFeature::AVX512BW),
        (cpu::AVX512DQ, HostFeature::AVX512DQ),
        (cpu::AVX512_BITALG, HostFeature::AVX512BITALG),
        (cpu::AVX512_VBMI, HostFeature::AVX512VBMI),
        (cpu::PCLMULQDQ, HostFeature::PCLMULQDQ),
        (cpu::F16C, HostFeature::F16C),
        (cpu::FMA, HostFeature::FMA),
        (cpu::AESNI, HostFeature::AES),
        (cpu::SHA, HostFeature::SHA),
        (cpu::POPCNT, HostFeature::POPCNT),
        (cpu::BMI1, HostFeature::BMI1),
        (cpu::BMI2, HostFeature::BMI2),
        (cpu::LZCNT, HostFeature::LZCNT),
        (cpu::GFNI, HostFeature::GFNI),
        (cpu::WAITPKG, HostFeature::WAITPKG),
    ];
    for (cpu_feature, host_feature) in mappings {
        if cpu_info.has(cpu_feature) {
            features |= host_feature;
        }
    }

    if cpu_info.has(cpu::BMI2) {
        if cpu_info.has(cpu::AMD) {
            let family = cpu_info.family + cpu_info.ext_family;
            if family >= 0x19 {
                features |= HostFeature::FAST_BMI2;
            }
        } else {
            features |= HostFeature::FAST_BMI2;
        }
    }

    features
}

/// Default code cache size (128 MB).
pub const DEFAULT_CODE_SIZE: usize = 128 * 1024 * 1024;

/// Constant pool size (2 MB).
const CONSTANT_POOL_SIZE: usize = 2 * 1024 * 1024;

/// Callbacks invoked by the dispatcher loop.
pub struct RunCodeCallbacks {
    /// Called to look up the native code pointer for the current guest PC.
    pub lookup_block: Box<dyn Callback>,
    /// Called with (ticks_executed) when returning from JIT execution.
    pub add_ticks: Box<dyn Callback>,
    /// Called to get the remaining tick budget; returns ticks in RAX.
    pub get_ticks_remaining: Box<dyn Callback>,
    /// Whether cycle counting is enabled.
    pub enable_cycle_counting: bool,
    /// Fastmem pointer: base of 4GB host memory arena.
    /// When set, R13 is loaded with this pointer during JIT execution,
    /// and memory accesses use `[R13 + guest_addr]` directly.
    /// Matches upstream where R13 = fastmem_pointer.
    pub fastmem_pointer: Option<*const u8>,
    /// Page-table pointer loaded into R14 during JIT execution.
    pub page_table_pointer: Option<*const u8>,
}

/// Index bits for return_from_run_code variants.
pub const MXCSR_ALREADY_EXITED: usize = 1 << 0;
pub const FORCE_RETURN: usize = 1 << 1;

/// Dispatcher label offsets recorded during prelude generation.
///
/// These are absolute offsets into the code buffer. Emitted blocks jump
/// to these locations via raw `jmp rel32` instructions.
pub struct DispatcherLabels {
    /// Offsets for the 4 return_from_run_code entry points:
    /// - index 0: normal (MXCSR in guest mode, no force)
    /// - index 1 (MXCSR_ALREADY_EXITED): MXCSR already switched back to host
    /// - index 2 (FORCE_RETURN): force return (MXCSR in guest mode)
    /// - index 3 (MXCSR_ALREADY_EXITED | FORCE_RETURN): force return, MXCSR already host
    pub return_from_run_code: [usize; 4],
    /// Offset of the run_code entry point.
    pub run_code_offset: usize,
    /// Offset of the step_code entry point (same as run_code for Phase 12).
    pub step_code_offset: usize,
}

/// Stack offset from RSP to the start of StackLayout.
///
/// This is upstream `ABI_SHADOW_SPACE`: 32 bytes on Windows and zero on
/// System V. `CalculateFrameInfo` places callee-saved XMM registers after the
/// aligned caller frame, not before it.
pub const STACK_LAYOUT_RSP_OFFSET: usize = abi::ABI_SHADOW_SPACE;

#[cfg(target_os = "windows")]
pub(crate) fn xmm_save_base(frame_size: usize) -> usize {
    frame_size.next_multiple_of(16) + abi::ABI_SHADOW_SPACE
}

/// Calculate the amount subtracted from RSP after the callee-saved GPR pushes.
///
/// This is the Rust counterpart of upstream `CalculateFrameInfo`'s
/// `stack_subtraction` calculation in `backend/x64/abi.cpp`.
pub(crate) fn stack_frame_allocation_size(frame_size: usize) -> usize {
    let aligned_frame_size = frame_size.next_multiple_of(16);
    let rsp_alignment = if abi::CALLEE_SAVE_GPRS.len().is_multiple_of(2) {
        8
    } else {
        0
    };
    rsp_alignment + abi::CALLEE_SAVE_XMMS.len() * 16 + aligned_frame_size + abi::ABI_SHADOW_SPACE
}

pub(crate) fn emit_switch_mxcsr_on_entry(
    code: &mut CodeAssembler,
    guest_mxcsr_offset: usize,
) -> rxbyak::Result<()> {
    let host_mxcsr_offset =
        (STACK_LAYOUT_RSP_OFFSET + StackLayout::save_host_mxcsr_offset()) as i32;
    code.stmxcsr(dword_ptr(RegExp::from(RSP) + host_mxcsr_offset))?;
    code.ldmxcsr(dword_ptr(RegExp::from(R15) + guest_mxcsr_offset as i32))
}

pub(crate) fn emit_switch_mxcsr_on_exit(
    code: &mut CodeAssembler,
    guest_mxcsr_offset: usize,
) -> rxbyak::Result<()> {
    let host_mxcsr_offset =
        (STACK_LAYOUT_RSP_OFFSET + StackLayout::save_host_mxcsr_offset()) as i32;
    code.stmxcsr(dword_ptr(RegExp::from(R15) + guest_mxcsr_offset as i32))?;
    code.ldmxcsr(dword_ptr(RegExp::from(RSP) + host_mxcsr_offset))
}

/// Function pointer type for calling into JIT-generated dispatcher code.
///
/// Arguments: (jit_state: *mut A64JitState, code_ptr: *const u8) -> HaltReason bits
#[cfg(all(target_arch = "x86_64", not(target_os = "windows")))]
pub type RunCodeFn = unsafe extern "sysv64" fn(*mut A64JitState, *const u8) -> u32;

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
pub type RunCodeFn = unsafe extern "win64" fn(*mut A64JitState, *const u8) -> u32;

#[cfg(not(target_arch = "x86_64"))]
pub type RunCodeFn = unsafe extern "C" fn(*mut A64JitState, *const u8) -> u32;

/// BlockOfCode wraps the rxbyak code assembler and generates the
/// entry/exit stubs (dispatcher loop) for JIT execution.
///
/// During execution:
/// - R15 points to A64JitState
/// - RSP points to StackLayout on the stack
/// - Host callee-saved registers are preserved
/// - MXCSR is switched between host and guest values
pub struct BlockOfCode {
    /// The underlying x86-64 assembler.
    pub asm: CodeAssembler,
    /// Constant pool for 128-bit immediate values.
    pub constant_pool: ConstantPool,
    /// Whether the prelude (entry/exit stubs) has been generated.
    prelude_complete: bool,
    /// Code pointer where user-emitted blocks begin (after prelude).
    pub(crate) code_begin_offset: usize,
    /// Architecture-specific JIT-state layout used by shared x64 emission.
    jit_state_info: JitStateInfo,
    /// Immutable host-capability mask selected once when the code cache is built.
    host_features: HostFeature,
}

impl BlockOfCode {
    /// Create a new BlockOfCode with the default code cache size (A64 offsets).
    pub fn new() -> rxbyak::Result<Self> {
        Self::with_size(DEFAULT_CODE_SIZE)
    }

    /// Create a new BlockOfCode with a custom code cache size (A64 JIT state).
    pub fn with_size(total_size: usize) -> rxbyak::Result<Self> {
        Self::with_size_and_jit_state_info(total_size, JitStateInfo::from_a64())
    }

    /// Create a code cache for the supplied architecture-specific JIT state.
    pub fn with_size_and_jit_state_info(
        total_size: usize,
        jit_state_info: JitStateInfo,
    ) -> rxbyak::Result<Self> {
        let mut asm = CodeAssembler::new(total_size)?;
        #[cfg(not(feature = "no_execute_support"))]
        asm.set_protect_mode_rwe()?;
        Ok(Self {
            asm,
            constant_pool: ConstantPool::new(CONSTANT_POOL_SIZE),
            prelude_complete: false,
            code_begin_offset: 0,
            jit_state_info,
            host_features: get_host_features(),
        })
    }

    pub const fn jit_state_info(&self) -> JitStateInfo {
        self.jit_state_info
    }

    pub fn has_host_feature(&self, feature: HostFeature) -> bool {
        self.host_features.contains(feature)
    }

    pub fn host_features(&self) -> HostFeature {
        self.host_features
    }

    /// Make the code cache writable before emission.
    ///
    /// Matches upstream `BlockOfCode::EnableWriting`: this is only a
    /// protection transition when `DYNARMIC_ENABLE_NO_EXECUTE_SUPPORT` is
    /// enabled. The default x64 configuration keeps the cache RWX.
    pub fn enable_writing(&mut self) -> rxbyak::Result<()> {
        #[cfg(feature = "no_execute_support")]
        {
            return self.asm.set_protect_mode_rw();
        }
        #[cfg(not(feature = "no_execute_support"))]
        {
            Ok(())
        }
    }

    /// Make the code cache executable after emission.
    ///
    /// Matches upstream `BlockOfCode::DisableWriting`.
    pub fn disable_writing(&mut self) -> rxbyak::Result<()> {
        #[cfg(feature = "no_execute_support")]
        {
            return self.asm.set_protect_mode_re();
        }
        #[cfg(not(feature = "no_execute_support"))]
        {
            Ok(())
        }
    }

    /// Mark the prelude as complete, allocate the constant pool in the code
    /// cache, and record where user code begins.
    ///
    /// Matches upstream: after the prelude, an int3 + alignment padding is
    /// emitted, then CONSTANT_POOL_SIZE bytes are reserved for the pool.
    ///
    /// On Windows: also emits SEH stubs + UNWIND_INFO + RUNTIME_FUNCTION into
    /// the code buffer and registers them with `RtlAddFunctionTable`.
    pub fn prelude_complete(&mut self) {
        self.prelude_complete = true;

        // On Windows: emit SEH stubs and UNWIND_INFO before the constant pool.
        #[cfg(target_os = "windows")]
        {
            let frame_size = core::mem::size_of::<crate::backend::x64::stack_layout::StackLayout>();
            let stack_allocation_size = stack_frame_allocation_size(frame_size);
            let xmm_save_base = xmm_save_base(frame_size);
            let total_capacity = self.asm.capacity();
            let code_buf_base = self.asm.top() as *mut u8;
            let mut current_size = self.asm.size();
            crate::backend::x64::exception_handler::setup_seh_in_code_buffer(
                code_buf_base,
                total_capacity,
                stack_allocation_size,
                xmm_save_base,
                &mut current_size,
            );
            self.asm.set_size(current_size);
        }

        // Emit int3 separator and align to 16 bytes for the constant pool.
        self.asm.int3().unwrap();
        while self.asm.size() % 16 != 0 {
            self.asm.int3().unwrap();
        }

        // Reserve space for the constant pool in the code cache.
        let pool_offset = self.asm.size();
        let pool_base = unsafe { self.asm.top().add(pool_offset) as *mut u8 };
        let new_size = pool_offset + CONSTANT_POOL_SIZE;
        self.asm.set_size(new_size);

        self.constant_pool.set_pool_base(pool_base);

        self.code_begin_offset = self.asm.size();
    }

    /// Clear the code cache (resets to after prelude).
    ///
    /// Matches upstream `BlockOfCode::ClearCache()`: only rewinds the code
    /// pointer. The constant pool is NOT cleared — previously interned
    /// constants remain valid for the BlockOfCode lifetime.
    pub fn clear_cache(&mut self) {
        assert!(
            self.prelude_complete,
            "Cannot clear cache before prelude is complete"
        );
        // Reset code pointer back to where user code begins.
        // The prelude stubs and constant pool remain intact.
        self.asm.set_size(self.code_begin_offset);
    }

    /// Remaining bytes available for code generation.
    pub fn space_remaining(&self) -> usize {
        self.asm.capacity().saturating_sub(self.asm.size())
    }

    /// Get the base pointer of the code buffer.
    pub fn code_base_ptr(&self) -> *const u8 {
        self.asm.top()
    }

    /// Current code size in bytes.
    pub fn code_size(&self) -> usize {
        self.asm.size()
    }

    /// Total allocated buffer size in bytes.
    pub fn total_size(&self) -> usize {
        self.asm.capacity()
    }

    // ---- Code Emitters ----

    /// Emit: Push all callee-saved registers and allocate stack frame.
    ///
    /// On System V (Linux/macOS): saves RBX, RBP, R12-R15 (6 GPRs, 48 bytes).
    /// On Windows: saves RBX, RSI, RDI, RBP, R12-R15 (8 GPRs, 64 bytes) and
    /// also saves XMM6-XMM15 after the aligned StackLayout.
    ///
    /// Total frame allocation follows upstream `CalculateFrameInfo`.
    pub fn emit_push_callee_save_and_adjust_stack(
        &mut self,
        frame_size: usize,
    ) -> rxbyak::Result<()> {
        // Push callee-saved GPRs.
        for &loc in abi::CALLEE_SAVE_GPRS {
            self.asm.push(loc.to_reg64())?;
        }

        let alloc = stack_frame_allocation_size(frame_size);
        if alloc > 0 {
            self.asm.sub(RSP, alloc as i32)?;
        }

        // Windows: save XMM6-XMM15 after shadow space + StackLayout.
        #[cfg(target_os = "windows")]
        for &xmm_idx in abi::CALLEE_SAVE_XMMS {
            let i = (xmm_idx - 6) as usize;
            let off = (xmm_save_base(frame_size) + i * 16) as i32;
            self.asm
                .movaps(xmmword_ptr(RegExp::from(RSP) + off), Reg::xmm(xmm_idx))?;
        }

        Ok(())
    }

    /// Emit: Deallocate stack frame and pop callee-saved registers.
    pub fn emit_pop_callee_save_and_adjust_stack(
        &mut self,
        frame_size: usize,
    ) -> rxbyak::Result<()> {
        // Windows: restore XMM6-XMM15 from after shadow space + StackLayout.
        #[cfg(target_os = "windows")]
        for &xmm_idx in abi::CALLEE_SAVE_XMMS {
            let i = (xmm_idx - 6) as usize;
            let off = (xmm_save_base(frame_size) + i * 16) as i32;
            self.asm
                .movaps(Reg::xmm(xmm_idx), xmmword_ptr(RegExp::from(RSP) + off))?;
        }

        let alloc = stack_frame_allocation_size(frame_size);
        if alloc > 0 {
            self.asm.add(RSP, alloc as i32)?;
        }

        // Pop callee-saved GPRs in reverse order.
        for &loc in abi::CALLEE_SAVE_GPRS.iter().rev() {
            self.asm.pop(loc.to_reg64())?;
        }
        Ok(())
    }

    /// Emit: Switch MXCSR to guest mode on JIT entry.
    ///
    /// Saves host MXCSR to StackLayout, loads guest MXCSR from JitState.
    pub fn emit_switch_mxcsr_on_entry(&mut self) -> rxbyak::Result<()> {
        emit_switch_mxcsr_on_entry(&mut self.asm, self.jit_state_info.offsetof_guest_mxcsr)
    }

    /// Emit: Switch MXCSR back to host mode on JIT exit.
    ///
    /// Saves guest MXCSR to JitState, loads host MXCSR from StackLayout.
    pub fn emit_switch_mxcsr_on_exit(&mut self) -> rxbyak::Result<()> {
        emit_switch_mxcsr_on_exit(&mut self.asm, self.jit_state_info.offsetof_guest_mxcsr)
    }

    /// Emit: Enter standard ASIMD MXCSR mode.
    ///
    /// Saves guest MXCSR, loads ASIMD MXCSR.
    pub fn emit_enter_standard_asimd(&mut self) -> rxbyak::Result<()> {
        let guest_offset = self.jit_state_info.offsetof_guest_mxcsr;
        let asimd_offset = self.jit_state_info.offsetof_asimd_mxcsr;

        self.asm
            .stmxcsr(dword_ptr(RegExp::from(R15) + guest_offset as i32))?;
        self.asm
            .ldmxcsr(dword_ptr(RegExp::from(R15) + asimd_offset as i32))?;
        Ok(())
    }

    /// Emit: Leave standard ASIMD MXCSR mode.
    ///
    /// Saves ASIMD MXCSR, loads guest MXCSR.
    pub fn emit_leave_standard_asimd(&mut self) -> rxbyak::Result<()> {
        let guest_offset = self.jit_state_info.offsetof_guest_mxcsr;
        let asimd_offset = self.jit_state_info.offsetof_asimd_mxcsr;

        self.asm
            .stmxcsr(dword_ptr(RegExp::from(R15) + asimd_offset as i32))?;
        self.asm
            .ldmxcsr(dword_ptr(RegExp::from(R15) + guest_offset as i32))?;
        Ok(())
    }

    /// Emit: Call a function at the given absolute address.
    ///
    /// Uses `mov rax, imm64; call rax` for far calls.
    pub fn emit_call_function(&mut self, address: u64) -> rxbyak::Result<()> {
        self.asm.mov(RAX, address as i64)?;
        self.asm.call_reg(RAX)?;
        Ok(())
    }

    /// Emit: Zero-extend a register from the given bit size to 64 bits.
    pub fn emit_zero_extend_from(&mut self, bitsize: usize, reg: Reg) -> rxbyak::Result<()> {
        match bitsize {
            8 => {
                let r32 = Reg::gpr32(reg.index());
                // For idx 4..7 use new_ext8 (SPL/BPL/SIL/DIL) so the encoder
                // emits REX. `gpr8(4..7)` without REX = AH/CH/DH/BH. Same
                // bug class as host_call's U8 zero-extend fix.
                let idx = reg.index();
                let r8 = if (4..8).contains(&idx) {
                    Reg::new_ext8(idx)
                } else {
                    Reg::gpr8(idx)
                };
                self.asm.movzx(r32, r8)?;
            }
            16 => {
                let r32 = Reg::gpr32(reg.index());
                let r16 = Reg::gpr16(reg.index());
                self.asm.movzx(r32, r16)?;
            }
            32 => {
                // mov r32, r32 implicitly zero-extends to 64 bits
                let r32 = Reg::gpr32(reg.index());
                self.asm.mov(r32, r32)?;
            }
            64 => {
                // Already 64-bit, nothing to do
            }
            _ => panic!("Invalid bitsize for zero extend: {}", bitsize),
        }
        Ok(())
    }

    /// Emit: `int3` breakpoint instruction.
    pub fn emit_int3(&mut self) -> rxbyak::Result<()> {
        self.asm.int3()
    }

    /// Emit: `lock or dword [r15 + offset], value`
    ///
    /// Atomically OR a 32-bit value into a JitState field.
    /// Used by step_code to set the STEP halt reason bit.
    pub fn emit_lock_or_dword_r15(&mut self, offset: usize, value: u32) -> rxbyak::Result<()> {
        self.asm.lock()?;
        self.asm
            .or_(dword_ptr(RegExp::from(R15) + offset as i32), value as i32)?;
        Ok(())
    }

    /// Emit: N single-byte NOP instructions (0x90).
    pub fn emit_nop_pad(&mut self, count: usize) -> rxbyak::Result<()> {
        for _ in 0..count {
            self.asm.nop()?;
        }
        Ok(())
    }

    /// Generate the dispatcher prelude: run_code entry point and
    /// return_from_run_code exit stubs.
    ///
    /// This must be called before the architecture emitter generates its
    /// fallback tables and terminal handlers. The emitter calls
    /// `prelude_complete()` only after all of those permanent stubs have been
    /// emitted, matching the upstream constructor ordering.
    ///
    /// Calling convention:
    ///   System V (Linux/macOS): RDI = *mut A64JitState, RSI = *const u8
    ///   Windows x64:            RCX = *mut A64JitState, RDX = *const u8
    ///   Returns: u32 (HaltReason bits) in EAX
    pub fn gen_run_code(&mut self, cb: &RunCodeCallbacks) -> rxbyak::Result<DispatcherLabels> {
        assert!(
            !self.prelude_complete,
            "gen_run_code must be called before prelude_complete"
        );

        let frame_size = core::mem::size_of::<StackLayout>();
        let halt_offset = self.jit_state_info.offsetof_halt_reason;
        // StackLayout is at RSP+STACK_LAYOUT_RSP_OFFSET.
        let sl = STACK_LAYOUT_RSP_OFFSET;
        let cycles_remaining_off = sl + StackLayout::cycles_remaining_offset();
        let cycles_to_run_off = sl + StackLayout::cycles_to_run_offset();

        // ---- run_code entry ----
        let run_code_offset = self.asm.size();

        // Save callee-saved registers, allocate frame (+ XMM saves on Windows).
        self.emit_push_callee_save_and_adjust_stack(frame_size)?;

        // R15 = jit_state pointer (first ABI param).
        // RBX = initial code_ptr  (second ABI param).
        let param0 = abi::ABI_PARAMS[0].to_reg64();
        let param1 = abi::ABI_PARAMS[1].to_reg64();
        self.asm.mov(R15, param0)?;
        self.asm.mov(RBX, param1)?;

        // R13 = fastmem_pointer (callee-saved, for direct memory access).
        // Matches upstream: R13 holds the base of the 4GB host memory arena.
        // When fastmem_pointer is None, R13 stays as saved (unused for memory).
        if let Some(ptr) = cb.fastmem_pointer {
            self.asm.mov(rxbyak::R13, ptr as i64)?;
        }
        if let Some(ptr) = cb.page_table_pointer {
            self.asm.mov(R14, ptr as i64)?;
        }

        // If cycle counting: call get_ticks_remaining, store result
        if cb.enable_cycle_counting {
            cb.get_ticks_remaining.emit_call_simple(&mut self.asm)?;
            // RAX = ticks remaining
            self.asm
                .mov(qword_ptr(RegExp::from(RSP) + cycles_to_run_off as i32), RAX)?;
            self.asm.mov(
                qword_ptr(RegExp::from(RSP) + cycles_remaining_off as i32),
                RAX,
            )?;
        }

        // Check if already halted before we even enter
        let already_halted = self.asm.create_label();
        self.asm
            .cmp(dword_ptr(RegExp::from(R15) + halt_offset as i32), 0i32)?;
        self.asm.jnz(&already_halted, JmpType::Near)?;

        // Switch MXCSR to guest mode
        self.emit_switch_mxcsr_on_entry()?;

        // Jump to the first compiled block
        self.asm.jmp_reg(RBX)?;

        // ---- return_from_run_code[0]: normal return (MXCSR in guest mode) ----
        let rfrc_0_offset = self.asm.size();

        // Check halt_reason
        let force_return_label = self.asm.create_label();
        self.asm
            .cmp(dword_ptr(RegExp::from(R15) + halt_offset as i32), 0i32)?;
        self.asm.jnz(&force_return_label, JmpType::Near)?;

        // Check cycle budget
        if cb.enable_cycle_counting {
            self.asm.cmp(
                qword_ptr(RegExp::from(RSP) + cycles_remaining_off as i32),
                0i32,
            )?;
            self.asm.jle(&force_return_label, JmpType::Near)?;
        }

        // Look up next block: callback returns code pointer in RAX
        cb.lookup_block.emit_call_simple(&mut self.asm)?;

        // Jump to the next block
        self.asm.jmp_reg(RAX)?;

        // ---- return_from_run_code[MXCSR_ALREADY_EXITED]: MXCSR already host ----
        let rfrc_mxcsr_offset = self.asm.size();

        let return_mxcsr_already_exited_label = self.asm.create_label();
        self.asm
            .cmp(dword_ptr(RegExp::from(R15) + halt_offset as i32), 0i32)?;
        self.asm
            .jnz(&return_mxcsr_already_exited_label, JmpType::Near)?;

        if cb.enable_cycle_counting {
            self.asm.cmp(
                qword_ptr(RegExp::from(RSP) + cycles_remaining_off as i32),
                0i32,
            )?;
            self.asm
                .jle(&return_mxcsr_already_exited_label, JmpType::Near)?;
        }

        // Re-enter guest MXCSR mode and dispatch
        self.emit_switch_mxcsr_on_entry()?;
        cb.lookup_block.emit_call_simple(&mut self.asm)?;
        self.asm.jmp_reg(RAX)?;

        // ---- return_from_run_code[FORCE_RETURN]: force return, MXCSR still guest ----
        let rfrc_force_offset = self.asm.size();
        self.asm.bind(&force_return_label)?;

        // Switch MXCSR back to host
        self.emit_switch_mxcsr_on_exit()?;
        // Fall through to return_mxcsr_already_exited

        // ---- return_from_run_code[FORCE_RETURN | MXCSR_ALREADY_EXITED] ----
        let rfrc_force_mxcsr_offset = self.asm.size();
        self.asm.bind(&return_mxcsr_already_exited_label)?;
        self.asm.bind(&already_halted)?;

        // If cycle counting: compute ticks used and call add_ticks.
        // Ticks go in the second ABI param register (RSI on SysV, RDX on Windows).
        // ArgCallback occupies param[0] with its self-pointer, so ticks land in
        // param[1] which matches the ABI on both platforms.
        if cb.enable_cycle_counting {
            let tick_reg = abi::ABI_PARAMS[1].to_reg64(); // RSI on SysV, RDX on Win
            self.asm.mov(
                tick_reg,
                qword_ptr(RegExp::from(RSP) + cycles_to_run_off as i32),
            )?;
            self.asm.sub(
                tick_reg,
                qword_ptr(RegExp::from(RSP) + cycles_remaining_off as i32),
            )?;
            cb.add_ticks.emit_call_simple(&mut self.asm)?;
        }

        // Read halt_reason and atomically clear it.
        // xor eax, eax; xchg [r15 + halt_reason], eax
        // (xchg with memory is implicitly locked on x86 — no LOCK prefix needed)
        let eax = rxbyak::Reg::gpr32(0); // EAX
        self.asm.xor_(eax, eax)?;
        self.asm
            .xchg(eax, dword_ptr(RegExp::from(R15) + halt_offset as i32))?;

        // Deallocate stack frame and restore callee-saved registers
        self.emit_pop_callee_save_and_adjust_stack(frame_size)?;

        // Return HaltReason in EAX
        self.asm.ret()?;

        // Record all offsets
        // ---- step_code entry ----
        // Dedicated single-step entry point: sets cycle budget to 1,
        // atomically sets STEP in halt_reason, then jumps to the block.
        let step_code_offset = self.asm.size();

        // Save callee-saved registers and allocate StackLayout.
        self.emit_push_callee_save_and_adjust_stack(frame_size)?;

        // R15 = jit_state (param[0]), RBX = code_ptr (param[1])
        let param0 = abi::ABI_PARAMS[0].to_reg64();
        let param1 = abi::ABI_PARAMS[1].to_reg64();
        self.asm.mov(R15, param0)?;
        self.asm.mov(RBX, param1)?;

        // R13 = fastmem_pointer (same as run_code)
        if let Some(ptr) = cb.fastmem_pointer {
            self.asm.mov(rxbyak::R13, ptr as i64)?;
        }
        if let Some(ptr) = cb.page_table_pointer {
            self.asm.mov(R14, ptr as i64)?;
        }

        // Set cycle budget to 1 instruction
        if cb.enable_cycle_counting {
            self.asm.mov(
                qword_ptr(RegExp::from(RSP) + cycles_to_run_off as i32),
                1i32,
            )?;
            self.asm.mov(
                qword_ptr(RegExp::from(RSP) + cycles_remaining_off as i32),
                1i32,
            )?;
        }

        // Check if already halted — bail to force-return path if so
        let step_already_halted = self.asm.create_label();
        self.asm
            .cmp(dword_ptr(RegExp::from(R15) + halt_offset as i32), 0i32)?;
        self.asm.jnz(&step_already_halted, JmpType::Near)?;

        // Atomically set STEP bit in halt_reason
        self.emit_lock_or_dword_r15(
            halt_offset,
            crate::interface::halt_reason::HaltReason::STEP.bits(),
        )?;

        // Switch MXCSR to guest mode
        self.emit_switch_mxcsr_on_entry()?;

        // Jump to the compiled block
        self.asm.jmp_reg(RBX)?;

        // Already halted: go through the normal exit path
        self.asm.bind(&step_already_halted)?;

        // Compute ticks if cycle counting (same register selection as run_code).
        if cb.enable_cycle_counting {
            let tick_reg = abi::ABI_PARAMS[1].to_reg64();
            self.asm.mov(
                tick_reg,
                qword_ptr(RegExp::from(RSP) + cycles_to_run_off as i32),
            )?;
            self.asm.sub(
                tick_reg,
                qword_ptr(RegExp::from(RSP) + cycles_remaining_off as i32),
            )?;
            cb.add_ticks.emit_call_simple(&mut self.asm)?;
        }

        // Read halt_reason atomically and clear
        let eax_step = rxbyak::Reg::gpr32(0);
        self.asm.xor_(eax_step, eax_step)?;
        self.asm
            .xchg(eax_step, dword_ptr(RegExp::from(R15) + halt_offset as i32))?;

        // Restore and return
        self.emit_pop_callee_save_and_adjust_stack(frame_size)?;
        self.asm.ret()?;

        let labels = DispatcherLabels {
            return_from_run_code: [
                rfrc_0_offset,
                rfrc_mxcsr_offset,
                rfrc_force_offset,
                rfrc_force_mxcsr_offset,
            ],
            run_code_offset,
            step_code_offset,
        };

        Ok(labels)
    }
}

// Windows still emits and registers its unwind table from `prelude_complete`
// because the table occupies code-buffer space. The per-emitter
// `ExceptionHandler` normally removes it first; retain this Windows-only guard
// for standalone BlockOfCode owners that never construct an emitter.
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
impl Drop for BlockOfCode {
    fn drop(&mut self) {
        crate::backend::x64::exception_handler::unregister_code_block(self.asm.top());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::x64::callback::ArgCallback;
    use crate::backend::x64::jitstate_info::JitStateInfo;

    #[test]
    fn preserves_supplied_jit_state_info() {
        let info = JitStateInfo::from_a32();
        let boc = BlockOfCode::with_size_and_jit_state_info(4096, info).unwrap();
        assert_eq!(boc.jit_state_info(), info);
    }

    #[test]
    fn test_block_of_code_creation() {
        let boc = BlockOfCode::with_size(4096).unwrap();
        assert!(!boc.prelude_complete);
        assert_eq!(boc.code_begin_offset, 0);
    }

    #[test]
    fn test_prelude_complete() {
        let mut boc = BlockOfCode::with_size(4 * 1024 * 1024).unwrap();
        // Emit something to advance code pointer
        boc.asm.ret().unwrap();
        boc.prelude_complete();
        assert!(boc.prelude_complete);
        assert!(boc.code_begin_offset > 0);
    }

    #[test]
    fn test_constant_pool_integration() {
        let mut boc = BlockOfCode::with_size(4 * 1024 * 1024).unwrap();
        boc.prelude_complete();
        // get_constant returns a RegExp with rip_addr (auto-fixup at emit time)
        let addr1 = boc.constant_pool.get_constant(0x1234, 0x5678);
        let addr2 = boc.constant_pool.get_constant(0x1234, 0x5678);
        assert!(addr1.is_rip());
        assert!(addr2.is_rip());
        assert_eq!(
            boc.constant_pool.len(),
            1,
            "Deduplication should keep only 1 entry"
        );
        let _addr3 = boc.constant_pool.get_constant(0xAAAA, 0);
        assert_eq!(boc.constant_pool.len(), 2);
        // Emit a movaps using the pool constant to verify encoding works
        let xmm0 = rxbyak::Reg::xmm(0);
        boc.asm.movaps(xmm0, rxbyak::xmmword_ptr(addr1)).unwrap();
        assert!(boc.asm.size() > 0);
    }

    #[test]
    fn test_emit_int3() {
        let mut boc = BlockOfCode::with_size(4096).unwrap();
        boc.emit_int3().unwrap();
        assert!(boc.asm.size() > 0);
    }

    // Stub functions for testing dispatcher generation
    extern "C" fn stub_lookup(_arg: u64) -> u64 {
        0
    }
    extern "C" fn stub_add_ticks(_arg: u64, _ticks: u64) {}
    extern "C" fn stub_get_ticks(_arg: u64) -> u64 {
        1000
    }

    #[test]
    fn test_gen_run_code_no_cycles() {
        let mut boc = BlockOfCode::with_size(4 * 1024 * 1024).unwrap();
        let cb = RunCodeCallbacks {
            lookup_block: Box::new(ArgCallback::new(stub_lookup as *const () as u64, 0)),
            add_ticks: Box::new(ArgCallback::new(stub_add_ticks as *const () as u64, 0)),
            get_ticks_remaining: Box::new(ArgCallback::new(stub_get_ticks as *const () as u64, 0)),
            enable_cycle_counting: false,
            fastmem_pointer: None,
            page_table_pointer: None,
        };
        let labels = boc.gen_run_code(&cb).unwrap();
        boc.prelude_complete();
        assert!(boc.prelude_complete);
        assert!(boc.code_begin_offset > 0);
        assert!(labels.run_code_offset == 0);
        // All return_from_run_code offsets should be > 0
        for &off in &labels.return_from_run_code {
            assert!(off > 0);
        }
    }

    #[test]
    fn test_gen_run_code_with_cycles() {
        let mut boc = BlockOfCode::with_size(4 * 1024 * 1024).unwrap();
        let cb = RunCodeCallbacks {
            lookup_block: Box::new(ArgCallback::new(stub_lookup as *const () as u64, 0)),
            add_ticks: Box::new(ArgCallback::new(stub_add_ticks as *const () as u64, 0)),
            get_ticks_remaining: Box::new(ArgCallback::new(stub_get_ticks as *const () as u64, 0)),
            enable_cycle_counting: true,
            fastmem_pointer: None,
            page_table_pointer: None,
        };
        let labels = boc.gen_run_code(&cb).unwrap();
        boc.prelude_complete();
        assert!(boc.prelude_complete);
        // With cycle counting, the prelude should be larger
        assert!(boc.code_begin_offset > 50);
        assert!(labels.return_from_run_code[0] < labels.return_from_run_code[2]);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_dispatcher_prologue_matches_unwind_contract() {
        let mut boc = BlockOfCode::with_size(4096).unwrap();
        let frame_size = core::mem::size_of::<StackLayout>();
        let allocation = stack_frame_allocation_size(frame_size);

        boc.emit_push_callee_save_and_adjust_stack(frame_size)
            .unwrap();

        let code = unsafe { std::slice::from_raw_parts(boc.code_base_ptr(), boc.code_size()) };
        assert_eq!(code.len(), 107);
        assert_eq!(
            &code[..12],
            &[0x53, 0x56, 0x57, 0x55, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57]
        );
        assert_eq!(&code[12..15], &[0x48, 0x81, 0xEC]);
        assert_eq!(
            u32::from_le_bytes(code[15..19].try_into().unwrap()) as usize,
            allocation
        );
        assert_eq!(
            xmm_save_base(frame_size),
            abi::ABI_SHADOW_SPACE + frame_size
        );
    }

    #[test]
    fn test_clear_cache_preserves_prelude() {
        let mut boc = BlockOfCode::with_size(4 * 1024 * 1024).unwrap();
        let cb = RunCodeCallbacks {
            lookup_block: Box::new(ArgCallback::new(stub_lookup as *const () as u64, 0)),
            add_ticks: Box::new(ArgCallback::new(stub_add_ticks as *const () as u64, 0)),
            get_ticks_remaining: Box::new(ArgCallback::new(stub_get_ticks as *const () as u64, 0)),
            enable_cycle_counting: false,
            fastmem_pointer: None,
            page_table_pointer: None,
        };
        boc.gen_run_code(&cb).unwrap();
        boc.prelude_complete();
        let prelude_size = boc.code_begin_offset;

        // Emit some dummy code after prelude
        boc.asm.ret().unwrap();
        assert!(boc.asm.size() > prelude_size);

        // Clear cache — should reset to prelude size
        boc.clear_cache();
        assert_eq!(boc.asm.size(), prelude_size);
    }

    #[test]
    fn test_atomic_halt_reason_xchg() {
        // Verify that gen_run_code emits xchg (0x87) instead of two movs
        // for the halt_reason read-and-clear sequence.
        let mut boc = BlockOfCode::with_size(4 * 1024 * 1024).unwrap();
        let cb = RunCodeCallbacks {
            lookup_block: Box::new(ArgCallback::new(stub_lookup as *const () as u64, 0)),
            add_ticks: Box::new(ArgCallback::new(stub_add_ticks as *const () as u64, 0)),
            get_ticks_remaining: Box::new(ArgCallback::new(stub_get_ticks as *const () as u64, 0)),
            enable_cycle_counting: false,
            fastmem_pointer: None,
            page_table_pointer: None,
        };
        let labels = boc.gen_run_code(&cb).unwrap();
        boc.prelude_complete();
        let code = unsafe { std::slice::from_raw_parts(boc.code_base_ptr(), boc.code_size()) };
        // Search for xchg opcode (0x87) in the return path
        // It should appear after the return_from_run_code[FORCE_RETURN|MXCSR] offset
        let rfrc_last = labels.return_from_run_code[3];
        let search = &code[rfrc_last..];
        assert!(
            search.windows(1).any(|w| w[0] == 0x87),
            "Expected xchg (0x87) in the dispatcher return path"
        );
    }

    #[test]
    fn test_emit_lock_or_dword_r15() {
        let mut boc = BlockOfCode::with_size(4096).unwrap();
        let before = boc.asm.size();
        boc.emit_lock_or_dword_r15(0x10, 0x01).unwrap();
        let after = boc.asm.size();
        // lock prefix (1) + or with memory+imm should emit several bytes
        assert!(after - before > 3, "lock or should emit at least 4 bytes");
        // First byte should be LOCK prefix 0xF0
        let code =
            unsafe { std::slice::from_raw_parts(boc.code_base_ptr().add(before), after - before) };
        assert_eq!(code[0], 0xF0, "First byte should be LOCK prefix");
    }

    #[test]
    fn test_emit_nop_pad() {
        let mut boc = BlockOfCode::with_size(4096).unwrap();
        let before = boc.asm.size();
        boc.emit_nop_pad(5).unwrap();
        assert_eq!(boc.asm.size() - before, 5);
        let code = unsafe { std::slice::from_raw_parts(boc.code_base_ptr().add(before), 5) };
        for &b in code {
            assert_eq!(b, 0x90, "All bytes should be NOP");
        }
    }

    #[test]
    fn test_step_code_offset_differs_from_run_code() {
        let mut boc = BlockOfCode::with_size(4 * 1024 * 1024).unwrap();
        let cb = RunCodeCallbacks {
            lookup_block: Box::new(ArgCallback::new(stub_lookup as *const () as u64, 0)),
            add_ticks: Box::new(ArgCallback::new(stub_add_ticks as *const () as u64, 0)),
            get_ticks_remaining: Box::new(ArgCallback::new(stub_get_ticks as *const () as u64, 0)),
            enable_cycle_counting: true,
            fastmem_pointer: None,
            page_table_pointer: None,
        };
        let labels = boc.gen_run_code(&cb).unwrap();
        boc.prelude_complete();
        assert_ne!(
            labels.step_code_offset, labels.run_code_offset,
            "step_code should have its own entry point"
        );
        assert!(
            labels.step_code_offset > labels.return_from_run_code[3],
            "step_code should come after all return_from_run_code entries"
        );
    }

    #[test]
    fn test_step_code_contains_lock_or() {
        // step_code should contain LOCK (0xF0) prefix for atomic STEP set
        let mut boc = BlockOfCode::with_size(4 * 1024 * 1024).unwrap();
        let cb = RunCodeCallbacks {
            lookup_block: Box::new(ArgCallback::new(stub_lookup as *const () as u64, 0)),
            add_ticks: Box::new(ArgCallback::new(stub_add_ticks as *const () as u64, 0)),
            get_ticks_remaining: Box::new(ArgCallback::new(stub_get_ticks as *const () as u64, 0)),
            enable_cycle_counting: false,
            fastmem_pointer: None,
            page_table_pointer: None,
        };
        let labels = boc.gen_run_code(&cb).unwrap();
        boc.prelude_complete();
        let code = unsafe { std::slice::from_raw_parts(boc.code_base_ptr(), boc.code_size()) };
        // Search for LOCK prefix (0xF0) in the step_code region
        let step_region = &code[labels.step_code_offset..];
        assert!(
            step_region.windows(1).any(|w| w[0] == 0xF0),
            "step_code should contain LOCK prefix for atomic OR"
        );
    }
}

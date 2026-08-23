//! Port of zuyu/src/core/hle/kernel/physical_core.h/.cpp
//! Status: EN COURS
//! Derniere synchro: 2026-03-11
//!
//! PhysicalCore: represents a single emulated CPU core, responsible for
//! running guest threads and handling interrupts. Full implementation
//! requires KernelCore, KThread, KProcess, ArmInterface, Debugger.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Condvar, Mutex, OnceLock,
};

use crate::arm::arm_interface::{
    ArmInterface, HaltReason, KThread as OpaqueKThread, ThreadContext,
};
use crate::core::System;
use crate::hle::kernel::svc_dispatch::{self, SvcArgs, SvcId};

use super::k_process::ProcessLock;
#[cfg(feature = "debug-logs")]
use super::physical_core_log;
use super::{
    k_process::KProcess,
    k_scheduler::KScheduler,
    k_thread::{KThread, KThreadLock, StepState, SuspendType},
};

static ZERO_PC_SAVE_TRACE_COUNT: AtomicU64 = AtomicU64::new(0);

fn should_trace_ipi() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_IPI").is_some())
}

fn should_trace_ipi_async() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_IPI_ASYNC").is_some())
}

pub enum PhysicalCoreExecutionControl {
    Continue,
    Yield,
    Break,
}

/// Represents a single emulated physical CPU core.
pub struct PhysicalCore {
    m_core_index: usize,
    m_guard: Mutex<PhysicalCoreState>,
    m_on_interrupt: Condvar,
    m_runtime: Mutex<Option<PhysicalCoreRuntime>>,
}

struct PhysicalCoreState {
    m_is_interrupted: bool,
    m_is_single_core: bool,
    /// Raw pointer to the ArmInterface currently executing on this core.
    /// Set at the start of RunThread, cleared on exit.
    /// Upstream: `Core::ArmInterface* m_arm_interface{}`.
    m_arm_interface: Option<*mut dyn crate::arm::arm_interface::ArmInterface>,
    /// Raw pointer to the KThread currently executing on this core.
    /// Upstream: `KThread* m_current_thread{}`.
    m_current_thread: Option<*mut KThread>,
}

// Safety: Raw pointers in PhysicalCoreState are only accessed under m_guard mutex,
// matching upstream's std::scoped_lock lk{m_guard} pattern.
unsafe impl Send for PhysicalCoreState {}
unsafe impl Sync for PhysicalCoreState {}

struct PhysicalCoreRuntime {
    m_current_thread: Arc<KThreadLock>,
}

pub enum PhysicalCoreExecutionEvent {
    SupervisorCall { svc_num: u32, svc_args: SvcArgs },
    Halted(HaltReason),
}

impl PhysicalCore {
    fn drain_current_thread_termination(&self, scheduler: &Arc<Mutex<KScheduler>>) -> bool {
        let runtime_guard = self.m_runtime.lock().unwrap();
        let Some(runtime) = runtime_guard.as_ref() else {
            return false;
        };

        if {
            let thread = runtime.m_current_thread.lock().unwrap();
            thread.is_termination_requested() && !thread.is_signaled()
        } {
            runtime.m_current_thread.lock().unwrap().exit();
            scheduler.lock().unwrap().request_schedule();
            return true;
        }

        false
    }

    fn event_from_halt(
        &self,
        jit: &mut dyn ArmInterface,
        halt_reason: HaltReason,
    ) -> PhysicalCoreExecutionEvent {
        if halt_reason.contains(HaltReason::SUPERVISOR_CALL) {
            let svc_num = jit.get_svc_number();
            let mut svc_args = [0u64; 8];
            jit.get_svc_arguments(&mut svc_args);
            PhysicalCoreExecutionEvent::SupervisorCall { svc_num, svc_args }
        } else {
            PhysicalCoreExecutionEvent::Halted(halt_reason)
        }
    }

    pub fn new(core_index: usize, is_multicore: bool) -> Self {
        Self {
            m_core_index: core_index,
            m_guard: Mutex::new(PhysicalCoreState {
                m_is_interrupted: false,
                m_is_single_core: !is_multicore,
                m_arm_interface: None,
                m_current_thread: None,
            }),
            m_on_interrupt: Condvar::new(),
            m_runtime: Mutex::new(None),
        }
    }

    /// Execute guest code until the next runtime event.
    pub fn run_thread(
        &self,
        jit: &mut dyn ArmInterface,
        thread: &mut OpaqueKThread,
    ) -> PhysicalCoreExecutionEvent {
        let halt_reason = jit.run_thread(thread);
        self.event_from_halt(jit, halt_reason)
    }

    /// Execute the active process thread with Eden's debugger step-state
    /// ordering from `PhysicalCore::RunThread`.
    pub fn run_thread_with_step_state(
        &self,
        jit: &mut dyn ArmInterface,
        thread: &mut OpaqueKThread,
        thread_owner: &Arc<KThreadLock>,
    ) -> PhysicalCoreExecutionEvent {
        let step_pending = thread_owner.lock().unwrap().get_step_state() == StepState::StepPending;
        let halt_reason = if step_pending {
            jit.step_thread(thread)
        } else {
            jit.run_thread(thread)
        };

        if step_pending && halt_reason.contains(HaltReason::STEP_THREAD) {
            thread_owner
                .lock()
                .unwrap()
                .set_step_state(StepState::StepPerformed);
            // A completed step takes priority over every simultaneous halt
            // reason, including SVC and breakpoint.
            PhysicalCoreExecutionEvent::Halted(halt_reason)
        } else {
            self.event_from_halt(jit, halt_reason)
        }
    }

    /// Notify and suspend a thread whose previous single-step completed.
    /// This runs before entering the JIT when the thread is scheduled again.
    pub fn stop_after_completed_step(&self, system: &System, thread: &Arc<KThreadLock>) -> bool {
        if thread.lock().unwrap().get_step_state() != StepState::StepPerformed {
            return false;
        }

        system.notify_debugger_thread_stopped(Arc::clone(thread));
        thread.lock().unwrap().request_suspend(SuspendType::Debug);
        true
    }

    /// Apply Eden's step-priority rule to halt-reason classification.
    pub fn step_completed(&self, halt_reason: HaltReason, thread: &Arc<KThreadLock>) -> bool {
        halt_reason.contains(HaltReason::STEP_THREAD)
            && thread.lock().unwrap().get_step_state() == StepState::StepPerformed
    }

    /// Handle breakpoint, prefetch-abort and data-abort debugger stops.
    /// Returns true when the thread was suspended and execution must return to
    /// the scheduler.
    pub fn handle_debug_halt(
        &self,
        jit: &mut dyn ArmInterface,
        halt_reason: HaltReason,
        thread: &Arc<KThreadLock>,
        process: &Arc<ProcessLock>,
        system: &System,
    ) -> bool {
        let step_completed = self.step_completed(halt_reason, thread);
        let breakpoint =
            !step_completed && halt_reason.contains(HaltReason::INSTRUCTION_BREAKPOINT);
        let prefetch_abort = !step_completed && halt_reason.contains(HaltReason::PREFETCH_ABORT);
        let data_abort = !step_completed && halt_reason.contains(HaltReason::DATA_ABORT);

        if breakpoint || prefetch_abort {
            if breakpoint {
                jit.rewind_breakpoint_instruction();
                self.capture_debug_context(jit, thread);
            }
            if system.debugger_enabled() {
                system.notify_debugger_thread_stopped(Arc::clone(thread));
            } else {
                let mut context = ThreadContext::default();
                jit.get_context(&mut context);
                crate::arm::arm_interface::ArmInterfaceBase::new(false)
                    .log_backtrace(&process.lock().unwrap(), &context);
            }
            thread.lock().unwrap().request_suspend(SuspendType::Debug);
            return true;
        }

        if data_abort {
            if system.debugger_enabled() {
                if let Some(watchpoint) = jit.halted_watchpoint() {
                    system.notify_debugger_thread_watchpoint(Arc::clone(thread), watchpoint);
                } else {
                    log::error!("Dynarmic reported a data abort without a halted watchpoint");
                }
            }
            thread.lock().unwrap().request_suspend(SuspendType::Debug);
            return true;
        }

        false
    }

    fn capture_debug_context(&self, jit: &dyn ArmInterface, thread: &Arc<KThreadLock>) {
        let mut context = ThreadContext::default();
        jit.get_context(&mut context);
        thread.lock().unwrap().capture_guest_context(&context);
    }

    pub fn step_thread(
        &self,
        jit: &mut dyn ArmInterface,
        thread: &mut OpaqueKThread,
    ) -> PhysicalCoreExecutionEvent {
        let halt_reason = jit.step_thread(thread);
        self.event_from_halt(jit, halt_reason)
    }

    pub fn initialize_guest_runtime(
        &self,
        main_thread: Arc<KThreadLock>,
        jit: &mut dyn ArmInterface,
        thread_context: &mut ThreadContext,
    ) {
        let locked =
            KScheduler::lock_thread_context_for_runtime(&main_thread, self.m_core_index as i32);
        assert!(locked, "guest runtime thread context must be available");
        self.restore_thread_to_jit(jit, thread_context, &main_thread);
        *self.m_runtime.lock().unwrap() = Some(PhysicalCoreRuntime {
            m_current_thread: main_thread,
        });
    }

    pub fn finalize_guest_runtime(&self) {
        let runtime = self.m_runtime.lock().unwrap().take();
        if let Some(runtime) = runtime {
            let unlocked = KScheduler::unlock_thread_context_for_runtime(
                &runtime.m_current_thread,
                self.m_core_index as i32,
            );
            assert!(unlocked, "guest runtime must release its thread context");
        }
    }

    pub fn handoff_after_svc(
        &self,
        jit: &mut dyn ArmInterface,
        thread_context: &mut ThreadContext,
        current_thread: &Arc<KThreadLock>,
    ) {
        jit.get_context(thread_context);
        // Stash the PC so the SIGUSR1 dumper can report where the guest was
        // last seen on this core. Updated cheaply at every SVC return — good
        // enough to identify where a post-SVC guest-code spin begins.
        super::kernel::record_guest_pc(self.m_core_index, thread_context.pc);
        current_thread
            .lock()
            .unwrap()
            .capture_guest_context(thread_context);
    }

    pub fn dispatch_supervisor_call(
        &self,
        svc_num: u32,
        is_64bit: bool,
        svc_args: &mut SvcArgs,
        system: &System,
    ) -> bool {
        svc_dispatch::call(system, svc_num, is_64bit, svc_args);

        // `Svc::Call` reloads return arguments through
        // `kernel.CurrentPhysicalCore()` after the handler returns. A blocking
        // SVC may resume this fiber on another host core, so `self` and `jit`
        // can still refer to the entry core here. Upstream returns from
        // `PhysicalCore::RunThread` without touching either one.
        svc_num != SvcId::ExitThread as u32
    }

    pub fn run_loop<FSvc, FHalt>(
        &self,
        jit: &mut dyn ArmInterface,
        thread: &mut OpaqueKThread,
        thread_context: &mut ThreadContext,
        scheduler: &Arc<Mutex<KScheduler>>,
        _process: &Arc<ProcessLock>,
        is_64bit: bool,
        system: &System,
        mut on_supervisor_call: FSvc,
        mut on_halted: FHalt,
    ) -> (u32, u32, PhysicalCoreExecutionControl)
    where
        FSvc: FnMut(u32, &mut SvcArgs, &ThreadContext, u32, u32) -> PhysicalCoreExecutionControl,
        FHalt: FnMut(
            HaltReason,
            Option<u64>,
            &ThreadContext,
            u32,
            u32,
        ) -> PhysicalCoreExecutionControl,
    {
        let mut svc_count = 0u32;
        let mut iteration = 0u32;
        loop {
            let event = self.run_thread(jit, thread);
            iteration += 1;

            match event {
                PhysicalCoreExecutionEvent::SupervisorCall {
                    svc_num,
                    mut svc_args,
                } => {
                    svc_count += 1;
                    jit.get_context(thread_context);

                    // After SetHeapSize (SVC ~#89), dump module memory for comparison with zuyu
                    #[cfg(feature = "debug-logs")]
                    if svc_count == 90 {
                        physical_core_log::dump_module_memory(_process);
                    }

                    match on_supervisor_call(
                        svc_num,
                        &mut svc_args,
                        thread_context,
                        svc_count,
                        iteration,
                    ) {
                        PhysicalCoreExecutionControl::Break => {
                            return (iteration, svc_count, PhysicalCoreExecutionControl::Break);
                        }
                        PhysicalCoreExecutionControl::Yield => {
                            return (iteration, svc_count, PhysicalCoreExecutionControl::Yield);
                        }
                        PhysicalCoreExecutionControl::Continue => {}
                    }

                    let continue_thread =
                        self.dispatch_supervisor_call(svc_num, is_64bit, &mut svc_args, system);
                    let _ = continue_thread;
                    return (iteration, svc_count, PhysicalCoreExecutionControl::Yield);
                }
                PhysicalCoreExecutionEvent::Halted(halt_reason) => {
                    jit.get_context(thread_context);
                    let exception_address = jit.get_last_exception_address();
                    match on_halted(
                        halt_reason,
                        exception_address,
                        thread_context,
                        svc_count,
                        iteration,
                    ) {
                        PhysicalCoreExecutionControl::Break => {
                            return (iteration, svc_count, PhysicalCoreExecutionControl::Break);
                        }
                        PhysicalCoreExecutionControl::Yield => {
                            return (iteration, svc_count, PhysicalCoreExecutionControl::Yield);
                        }
                        PhysicalCoreExecutionControl::Continue => {}
                    }

                    // Upstream observes the same condition when returning from an
                    // interrupt/timer tick and re-entering scheduling. In this
                    // cooperative port, the dispatch quantum boundary is the
                    // equivalent place to drain a pending termination request.
                    if self.drain_current_thread_termination(scheduler) {
                        if let Some(current_thread) = super::kernel::get_current_thread_pointer() {
                            self.handoff_after_svc(jit, thread_context, &current_thread);
                        }
                    }

                    // Upstream `PhysicalCore::RunThread` returns to the CPU
                    // manager for an external interrupt, and after every JIT
                    // stop in single-core mode so CoreTiming can advance and
                    // the emulated cores can be preempted.
                    let is_single_core = self.m_guard.lock().unwrap().m_is_single_core;
                    let interrupt = !halt_reason.contains(HaltReason::STEP_THREAD)
                        && halt_reason.contains(HaltReason::BREAK_LOOP);
                    if interrupt || is_single_core {
                        return (iteration, svc_count, PhysicalCoreExecutionControl::Yield);
                    }
                }
            }
        }
    }

    /// Load context from thread to current core.
    /// Port of upstream `PhysicalCore::LoadContext(const KThread* thread)`.
    pub fn load_context(&self, thread: &KThread) {
        // Upstream: PhysicalCore::LoadContext (physical_core.cpp:148-167)
        // Gets process from thread, gets arm_interface from process,
        // calls interface->SetContext(thread->GetContext()),
        //       interface->SetTpidrroEl0(thread->GetTlsAddress()).
        let parent = match thread.parent.as_ref().and_then(|w| w.upgrade()) {
            Some(p) => p,
            None => return, // Kernel threads don't run on emulated CPU cores.
        };

        let mut process = parent.lock().unwrap();
        let watchpoints = std::ptr::addr_of!(process.watchpoints);
        if let Some(jit) = process.get_arm_interface_mut(self.m_core_index) {
            let k_ctx = &thread.thread_context;
            // Safety: both ThreadContext types have identical layout.
            let arm_ctx: &crate::arm::arm_interface::ThreadContext = unsafe {
                &*(k_ctx as *const super::k_thread::ThreadContext
                    as *const crate::arm::arm_interface::ThreadContext)
            };
            jit.set_context(arm_ctx);
            jit.set_tpidrro_el0(thread.get_tls_address().get());
            jit.set_watchpoint_array(watchpoints);
            trace_wrapper_context_event(3, self.m_core_index, thread, arm_ctx);
            log::info!(
                "PhysicalCore::load_context: core={} r15/PC=0x{:X} r13/SP=0x{:X} ctx.pc=0x{:X} ctx.sp=0x{:X}",
                self.m_core_index, k_ctx.r[15], k_ctx.r[13], k_ctx.pc, k_ctx.sp,
            );
        }
    }

    /// Load SVC arguments back into the current core's ARM interface.
    /// Port of upstream `PhysicalCore::LoadSvcArguments(const KProcess&, args)`.
    pub fn load_svc_arguments(&self, process: &mut KProcess, args: &[u64; 8]) {
        if let Some(jit) = process.get_arm_interface_mut(self.m_core_index) {
            jit.set_svc_arguments(args);
        }
    }

    /// Save context from current core to thread.
    /// Port of upstream `PhysicalCore::SaveContext(KThread* thread)`.
    pub fn save_context(&self, thread: &mut KThread) {
        // Upstream: PhysicalCore::SaveContext (physical_core.cpp:170-182)
        // Gets process from thread, gets arm_interface from process,
        // calls interface->GetContext(thread->GetContext()).
        let parent = match thread.parent.as_ref().and_then(|w| w.upgrade()) {
            Some(p) => p,
            None => return, // Kernel threads don't run on emulated CPU cores.
        };

        let process = parent.lock().unwrap();
        if let Some(jit) = process.get_arm_interface(self.m_core_index) {
            let k_ctx = &mut thread.thread_context;
            let arm_ctx: &mut crate::arm::arm_interface::ThreadContext = unsafe {
                &mut *(k_ctx as *mut super::k_thread::ThreadContext
                    as *mut crate::arm::arm_interface::ThreadContext)
            };
            jit.get_context(arm_ctx);
            trace_wrapper_context_event(2, self.m_core_index, thread, arm_ctx);
            if arm_ctx.pc == 0 && std::env::var_os("RUZU_TRACE_ZERO_PC_SAVE").is_some() {
                let n = ZERO_PC_SAVE_TRACE_COUNT.fetch_add(1, Ordering::Relaxed);
                if n < 64 || n.is_multiple_of(1024) {
                    eprintln!(
                        "[ZERO_PC_SAVE] n={} core={} tid={} type={:?} state={:?} prio={} active_core={} lr=0x{:08X} sp=0x{:08X} r0=0x{:08X} r1=0x{:08X}",
                        n,
                        self.m_core_index,
                        thread.get_thread_id(),
                        thread.thread_type,
                        thread.get_state(),
                        thread.get_priority(),
                        thread.get_active_core(),
                        arm_ctx.lr as u32,
                        arm_ctx.sp as u32,
                        arm_ctx.r[0] as u32,
                        arm_ctx.r[1] as u32,
                    );
                }
            }
        }
    }

    /// Save SVC arguments from the current core's ARM interface.
    /// Port of upstream `PhysicalCore::SaveSvcArguments(KProcess&, args)`.
    pub fn save_svc_arguments(&self, process: &KProcess, args: &mut [u64; 8]) {
        if let Some(jit) = process.get_arm_interface(self.m_core_index) {
            jit.get_svc_arguments(args);
        }
    }

    /// Log backtrace of current processor state.
    /// Port of upstream `PhysicalCore::LogBacktrace()`.
    pub fn log_backtrace(&self) {
        // Upstream: gets current process, gets arm_interface,
        // calls interface->LogBacktrace(process).
        let state = self.m_guard.lock().unwrap();
        let current_thread = state.m_current_thread;
        let arm_interface = state.m_arm_interface;
        drop(state);

        let Some(thread_ptr) = current_thread else {
            log::debug!(
                "PhysicalCore::log_backtrace: core={} no current thread",
                self.m_core_index
            );
            return;
        };

        let thread = unsafe { &*thread_ptr };
        let Some(parent) = thread.parent.as_ref().and_then(|w| w.upgrade()) else {
            log::debug!(
                "PhysicalCore::log_backtrace: core={} tid={} has no parent process",
                self.m_core_index,
                thread.get_thread_id()
            );
            return;
        };

        let mut ctx = crate::arm::arm_interface::ThreadContext::default();
        if let Some(jit_ptr) = arm_interface {
            let jit = unsafe { &*jit_ptr };
            jit.get_context(&mut ctx);
        } else {
            let k_ctx = &thread.thread_context;
            ctx.r = k_ctx.r;
            ctx.fp = k_ctx.fp;
            ctx.lr = k_ctx.lr;
            ctx.sp = k_ctx.sp;
            ctx.pc = k_ctx.pc;
            ctx.pstate = k_ctx.pstate;
            ctx.padding = k_ctx.padding;
            ctx.v = k_ctx.v;
            ctx.fpcr = k_ctx.fpcr;
            ctx.fpsr = k_ctx.fpsr;
            ctx.tpidr = k_ctx.tpidr;
        }

        let process = parent.lock().unwrap();
        log::error!(
            "PhysicalCore::log_backtrace: core={} tid={} pc=0x{:016X} lr=0x{:016X} sp=0x{:016X}",
            self.m_core_index,
            thread.get_thread_id(),
            ctx.pc,
            ctx.lr,
            ctx.sp
        );
        crate::arm::arm_interface::ArmInterfaceBase::new(false).log_backtrace(&process, &ctx);
    }

    /// Wait for an interrupt.
    pub fn idle(&self) {
        let mut state = self.m_guard.lock().unwrap();
        while !state.m_is_interrupted {
            state = self.m_on_interrupt.wait(state).unwrap();
        }
    }

    /// Enter the running context for this core.
    ///
    /// Upstream: `PhysicalCore::RunThread()` local `EnterContext` lambda.
    /// The interrupted check and publication of `(m_arm_interface,
    /// m_current_thread)` must happen under the same guard so an interrupt
    /// cannot arrive between "not interrupted" and "JIT is running".
    pub fn enter_running(
        &self,
        arm_interface: *mut dyn crate::arm::arm_interface::ArmInterface,
        thread: *mut KThread,
    ) -> bool {
        let mut state = self.m_guard.lock().unwrap();
        if state.m_is_interrupted {
            return false;
        }
        state.m_arm_interface = Some(arm_interface);
        state.m_current_thread = Some(thread);

        // Upstream calls `interface->LockThread(thread)` while the core
        // context is held. Dynarmic currently has a no-op implementation, but
        // preserving this order matters for backends that synchronize thread
        // parameter access with `Interrupt()`.
        unsafe {
            let thread_as_arm = &mut *(thread as *mut crate::arm::arm_interface::KThread);
            (*arm_interface).lock_thread(thread_as_arm);
        }

        true
    }

    /// Exit the running context for this core.
    ///
    /// Upstream: `PhysicalCore::RunThread()` local `ExitContext` lambda.
    /// Unlock the thread first, then clear the core's published running state
    /// under `m_guard`.
    pub fn exit_running(
        &self,
        arm_interface: *mut dyn crate::arm::arm_interface::ArmInterface,
        thread: *mut KThread,
        debug_thread: Option<&Arc<KThreadLock>>,
    ) {
        if let Some(debug_thread) = debug_thread {
            unsafe {
                self.capture_debug_context(&*arm_interface, debug_thread);
            }
        }

        unsafe {
            let thread_as_arm = &mut *(thread as *mut crate::arm::arm_interface::KThread);
            (*arm_interface).unlock_thread(thread_as_arm);
        }

        let mut state = self.m_guard.lock().unwrap();
        state.m_arm_interface = None;
        state.m_current_thread = None;
    }

    /// Interrupt this core.
    /// Port of upstream `PhysicalCore::Interrupt()`.
    #[track_caller]
    pub fn interrupt(&self) {
        let mut state = self.m_guard.lock().unwrap();

        // Load members.
        let arm_interface = state.m_arm_interface;
        let current_thread = state.m_current_thread;

        // Add interrupt flag.
        state.m_is_interrupted = true;

        if should_trace_ipi_async() && common::trace::is_enabled(common::trace::cat::SCHED_STATE) {
            static TRACE_COUNTS: [AtomicU64; crate::hardware_properties::NUM_CPU_CORES as usize] =
                [const { AtomicU64::new(0) }; crate::hardware_properties::NUM_CPU_CORES as usize];
            let trace_count = TRACE_COUNTS[self.m_core_index].fetch_add(1, Ordering::Relaxed);
            if trace_count < 512 || trace_count.is_multiple_of(1_000) {
                let caller = std::panic::Location::caller();
                let source = if caller.file().ends_with("k_scheduler.rs") {
                    1
                } else if caller.file().ends_with("k_interrupt_manager.rs") {
                    2
                } else if caller.file().ends_with("kernel.rs") {
                    3
                } else {
                    0
                };
                let running_tid = current_thread
                    .map(|thread| unsafe { (*thread).get_thread_id() })
                    .unwrap_or(0);
                common::trace::emit_raw(
                    common::trace::cat::SCHED_STATE,
                    &[
                        21,
                        self.m_core_index as u64,
                        source,
                        caller.line() as u64,
                        running_tid,
                        arm_interface.is_some() as u64,
                        trace_count,
                    ],
                );
            }
        }

        // Env-gated IPI trace: `RUZU_TRACE_IPI=1` logs every interrupt()
        // call with target core and the JIT/thread running there. Used to
        // diagnose cross-core wake latency for newly-RUNNABLE threads (the
        // elapsed_secs() prefix for inline correlation.
        if should_trace_ipi() {
            let t = crate::hle::kernel::trace_format::elapsed_secs();
            let running_tid = current_thread
                .map(|p| unsafe { (*p).get_thread_id() })
                .unwrap_or(0);
            log::warn!(
                "[{:>10.6}] [IPI] target_core={} running_tid={} running_jit={}",
                t,
                self.m_core_index,
                running_tid,
                arm_interface.is_some()
            );
        }

        // Interrupt ourselves (wake from idle).
        self.m_on_interrupt.notify_one();

        // If there is no thread running, we are done.
        let (Some(arm_ptr), Some(thread_ptr)) = (arm_interface, current_thread) else {
            return;
        };

        // Interrupt the CPU.
        // Safety: arm_interface and current_thread are valid while m_guard is held,
        // matching upstream's scoped_lock pattern.
        // arm_interface::KThread is an opaque placeholder for the real k_thread::KThread.
        unsafe {
            let thread_as_arm =
                &mut *(thread_ptr as *mut KThread as *mut crate::arm::arm_interface::KThread);
            (*arm_ptr).signal_interrupt(thread_as_arm);
        }
    }

    /// Clear this core's interrupt flag.
    pub fn clear_interrupt(&self) {
        let mut state = self.m_guard.lock().unwrap();
        state.m_is_interrupted = false;
    }

    /// Check if this core is interrupted.
    pub fn is_interrupted(&self) -> bool {
        let state = self.m_guard.lock().unwrap();
        state.m_is_interrupted
    }

    /// Get the core index.
    pub fn core_index(&self) -> usize {
        self.m_core_index
    }

    fn restore_thread_to_jit(
        &self,
        jit: &mut dyn ArmInterface,
        thread_context: &mut ThreadContext,
        thread: &Arc<KThreadLock>,
    ) {
        let thread = thread.lock().unwrap();
        thread.restore_guest_context(thread_context);
        jit.set_context(thread_context);
        jit.set_tpidrro_el0(thread.get_tls_address().get());
    }
}

fn trace_wrapper_context_event(
    stage: u64,
    core_index: usize,
    thread: &KThread,
    ctx: &crate::arm::arm_interface::ThreadContext,
) {
    if !common::trace::is_enabled(common::trace::cat::A64_EXCEPTION_CTX) {
        return;
    }
    if !is_animus_sdk_thread_wrapper_context(ctx.pc, ctx.lr) {
        return;
    }
    common::trace::emit_raw(
        common::trace::cat::A64_EXCEPTION_CTX,
        &[
            stage,
            core_index as u64,
            thread.get_thread_id(),
            ctx.pc,
            ctx.lr,
            ctx.sp,
            ctx.r[0],
            ctx.r[19],
            ctx.r[20],
            ctx.r[21],
            ctx.fp,
        ],
    );
}

fn is_animus_sdk_thread_wrapper_context(pc: u64, lr: u64) -> bool {
    const WRAPPER_START: u64 = 0x8467_74D0;
    const WRAPPER_END: u64 = 0x8467_7544;
    (WRAPPER_START..WRAPPER_END).contains(&pc) || (WRAPPER_START..WRAPPER_END).contains(&lr)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use crate::arm::arm_interface::{
        Architecture, DebugWatchpoint, KThread as OpaqueKThread, WatchpointArray,
    };
    use crate::core::System;
    use crate::hle::kernel::k_thread::ThreadState;
    use crate::hle::kernel::k_worker_task_manager::KWorkerTaskManager;

    use super::*;

    struct TestArmInterface {
        halt_reasons: VecDeque<HaltReason>,
        context: ThreadContext,
        tpidrro_el0: u64,
        set_svc_arguments_count: usize,
        run_count: usize,
        step_count: usize,
        rewind_count: usize,
    }

    impl TestArmInterface {
        fn new(halt_reasons: impl Into<VecDeque<HaltReason>>) -> Self {
            Self {
                halt_reasons: halt_reasons.into(),
                context: ThreadContext::default(),
                tpidrro_el0: 0,
                set_svc_arguments_count: 0,
                run_count: 0,
                step_count: 0,
                rewind_count: 0,
            }
        }
    }

    impl ArmInterface for TestArmInterface {
        fn run_thread(&mut self, _thread: &mut OpaqueKThread) -> HaltReason {
            self.run_count += 1;
            self.halt_reasons
                .pop_front()
                .unwrap_or(HaltReason::BREAK_LOOP)
        }

        fn step_thread(&mut self, _thread: &mut OpaqueKThread) -> HaltReason {
            self.step_count += 1;
            self.halt_reasons
                .pop_front()
                .unwrap_or(HaltReason::BREAK_LOOP)
        }

        fn clear_instruction_cache(&mut self) {}

        fn invalidate_cache_range(&mut self, _addr: u64, _size: usize) {}

        fn get_architecture(&self) -> Architecture {
            Architecture::AArch64
        }

        fn get_context(&self, ctx: &mut ThreadContext) {
            *ctx = self.context.clone();
        }

        fn set_context(&mut self, ctx: &ThreadContext) {
            self.context = ctx.clone();
        }

        fn set_tpidrro_el0(&mut self, value: u64) {
            self.tpidrro_el0 = value;
        }

        fn set_watchpoint_array(&mut self, _watchpoints: *const WatchpointArray) {}

        fn get_svc_arguments(&self, args: &mut [u64; 8]) {
            *args = [0; 8];
        }

        fn set_svc_arguments(&mut self, _args: &[u64; 8]) {
            self.set_svc_arguments_count += 1;
        }

        fn get_svc_number(&self) -> u32 {
            0
        }

        fn signal_interrupt(&mut self, _thread: &mut OpaqueKThread) {}

        fn halted_watchpoint(&self) -> Option<DebugWatchpoint> {
            None
        }

        fn rewind_breakpoint_instruction(&mut self) {
            self.rewind_count += 1;
        }
    }

    fn test_context() -> (
        PhysicalCore,
        Arc<ProcessLock>,
        Arc<Mutex<KScheduler>>,
        Arc<KThreadLock>,
        System,
    ) {
        let mut system = System::new_for_test();

        let mut process = KProcess::new();
        process.capabilities.core_mask = 0xF;
        process.capabilities.priority_mask = u64::MAX;
        process.flags = 0;
        process.allocate_code_memory(0x200000, 0x20000);
        process.initialize_handle_table();
        process.initialize_thread_local_region_base(0x240000);

        let process = Arc::new(ProcessLock::from_value(process));
        let current_thread = Arc::new(KThreadLock::new(KThread::new()));
        let other_thread = Arc::new(KThreadLock::new(KThread::new()));
        let scheduler = Arc::new(Mutex::new(KScheduler::new(0)));
        {
            let mut thread = current_thread.lock().unwrap();
            thread.initialize_main_thread(0x200000, 0x250000, 0, 0x23f000, &process, 1, 1, false);
            thread.set_priority(44);
            thread.set_base_priority(44);
        }
        {
            let mut thread = other_thread.lock().unwrap();
            thread.initialize_main_thread(0x201000, 0x260000, 0, 0x24f000, &process, 2, 2, false);
            thread.set_priority(44);
            thread.set_base_priority(44);
        }
        process
            .lock()
            .unwrap()
            .register_thread_object(current_thread.clone());
        process
            .lock()
            .unwrap()
            .register_thread_object(other_thread.clone());
        process.lock().unwrap().push_back_to_priority_queue(1);
        process.lock().unwrap().push_back_to_priority_queue(2);
        scheduler.lock().unwrap().initialize(1, 0, 0);

        let shared_memory = process.lock().unwrap().get_shared_memory();
        system.set_current_process_arc(process.clone());
        system.set_scheduler_arc(scheduler.clone());
        system.set_shared_process_memory(shared_memory);
        system.set_runtime_program_id(1);
        system.set_runtime_64bit(false);

        (
            PhysicalCore::new(0, false),
            process,
            scheduler,
            current_thread,
            system,
        )
    }

    #[test]
    fn run_loop_drains_current_thread_termination_on_halt_boundary() {
        let (physical_core, process, scheduler, current_thread, system) = test_context();
        // `initialize_main_thread` does not mark these fixture threads as
        // started. Model both runnable process threads so exiting the current
        // one decrements Eden's running-thread count without terminating the
        // whole process and its replacement thread.
        process.lock().unwrap().increment_running_thread_count();
        process.lock().unwrap().increment_running_thread_count();
        let mut thread_context = ThreadContext::default();
        let mut jit = TestArmInterface::new(VecDeque::from([HaltReason::BREAK_LOOP]));
        physical_core.initialize_guest_runtime(
            current_thread.clone(),
            &mut jit,
            &mut thread_context,
        );

        current_thread.lock().unwrap().request_terminate();

        let mut opaque_thread = unsafe { std::mem::zeroed::<OpaqueKThread>() };
        let (iterations, _svc_count, control) = physical_core.run_loop(
            &mut jit,
            &mut opaque_thread,
            &mut thread_context,
            &scheduler,
            &process,
            false,
            &system,
            |_svc_num, _svc_args, _thread_context, _svc_count, _iteration| {
                PhysicalCoreExecutionControl::Continue
            },
            |_halt_reason, _exception_address, _thread_context, _svc_count, _iteration| {
                PhysicalCoreExecutionControl::Continue
            },
        );
        KWorkerTaskManager::wait_for_global_idle();

        assert_eq!(iterations, 1);
        assert!(matches!(control, PhysicalCoreExecutionControl::Yield));
        let thread = current_thread.lock().unwrap();
        assert_eq!(thread.get_state(), ThreadState::TERMINATED);
        assert!(thread.is_signaled());
    }

    #[test]
    fn debugger_step_uses_step_thread_and_defers_the_stop_until_rescheduled() {
        let (physical_core, _process, _scheduler, current_thread, system) = test_context();
        current_thread
            .lock()
            .unwrap()
            .set_step_state(StepState::StepPending);
        let mut jit = TestArmInterface::new(VecDeque::from([
            HaltReason::STEP_THREAD | HaltReason::SUPERVISOR_CALL
        ]));
        let mut opaque_thread = unsafe { std::mem::zeroed::<OpaqueKThread>() };

        let event =
            physical_core.run_thread_with_step_state(&mut jit, &mut opaque_thread, &current_thread);

        assert!(matches!(
            event,
            PhysicalCoreExecutionEvent::Halted(reason)
                if reason.contains(HaltReason::STEP_THREAD)
        ));
        assert_eq!(jit.run_count, 0);
        assert_eq!(jit.step_count, 1);
        assert_eq!(
            current_thread.lock().unwrap().get_step_state(),
            StepState::StepPerformed
        );
        assert!(!current_thread
            .lock()
            .unwrap()
            .is_suspend_requested_type(SuspendType::Debug));

        assert!(physical_core.stop_after_completed_step(&system, &current_thread));
        assert!(current_thread
            .lock()
            .unwrap()
            .is_suspend_requested_type(SuspendType::Debug));
    }

    #[test]
    fn breakpoint_rewinds_and_suspends_without_handling_a_simultaneous_step_twice() {
        let (physical_core, process, _scheduler, current_thread, system) = test_context();
        let mut jit = TestArmInterface::new(VecDeque::new());

        assert!(physical_core.handle_debug_halt(
            &mut jit,
            HaltReason::INSTRUCTION_BREAKPOINT,
            &current_thread,
            &process,
            &system,
        ));
        assert_eq!(jit.rewind_count, 1);
        assert!(current_thread
            .lock()
            .unwrap()
            .is_suspend_requested_type(SuspendType::Debug));

        current_thread.lock().unwrap().resume(SuspendType::Debug);
        current_thread
            .lock()
            .unwrap()
            .set_step_state(StepState::StepPerformed);
        assert!(!physical_core.handle_debug_halt(
            &mut jit,
            HaltReason::STEP_THREAD | HaltReason::INSTRUCTION_BREAKPOINT,
            &current_thread,
            &process,
            &system,
        ));
        assert_eq!(jit.rewind_count, 1);
        assert!(!current_thread
            .lock()
            .unwrap()
            .is_suspend_requested_type(SuspendType::Debug));
    }

    #[test]
    fn run_loop_returns_after_any_jit_stop_in_single_core_mode() {
        let (physical_core, process, scheduler, _current_thread, system) = test_context();
        let mut thread_context = ThreadContext::default();
        let mut jit = TestArmInterface::new(VecDeque::from([HaltReason::empty()]));
        let mut opaque_thread = unsafe { std::mem::zeroed::<OpaqueKThread>() };

        let (iterations, svc_count, control) = physical_core.run_loop(
            &mut jit,
            &mut opaque_thread,
            &mut thread_context,
            &scheduler,
            &process,
            false,
            &system,
            |_svc_num, _svc_args, _thread_context, _svc_count, _iteration| {
                PhysicalCoreExecutionControl::Continue
            },
            |_halt_reason, _exception_address, _thread_context, _svc_count, _iteration| {
                PhysicalCoreExecutionControl::Continue
            },
        );

        assert_eq!(iterations, 1);
        assert_eq!(svc_count, 0);
        assert!(matches!(control, PhysicalCoreExecutionControl::Yield));
    }

    #[test]
    fn guest_runtime_releases_the_context_guard_it_acquires() {
        let (physical_core, _process, _scheduler, current_thread, _system) = test_context();
        let mut thread_context = ThreadContext::default();
        let mut jit = TestArmInterface::new(VecDeque::new());

        physical_core.initialize_guest_runtime(
            current_thread.clone(),
            &mut jit,
            &mut thread_context,
        );
        assert_eq!(
            current_thread
                .lock()
                .unwrap()
                .context_guard_owner
                .load(Ordering::SeqCst),
            0
        );

        physical_core.finalize_guest_runtime();
        assert_eq!(
            current_thread
                .lock()
                .unwrap()
                .context_guard_owner
                .load(Ordering::SeqCst),
            super::super::k_thread::CONTEXT_GUARD_UNOWNED
        );
    }

    #[test]
    fn run_loop_only_returns_for_non_step_break_loop_in_multicore_mode() {
        let (_single_core, process, scheduler, _current_thread, system) = test_context();
        let physical_core = PhysicalCore::new(0, true);
        let mut thread_context = ThreadContext::default();
        let mut jit = TestArmInterface::new(VecDeque::from([
            HaltReason::empty(),
            HaltReason::STEP_THREAD | HaltReason::BREAK_LOOP,
            HaltReason::BREAK_LOOP,
        ]));
        let mut opaque_thread = unsafe { std::mem::zeroed::<OpaqueKThread>() };

        let (iterations, svc_count, control) = physical_core.run_loop(
            &mut jit,
            &mut opaque_thread,
            &mut thread_context,
            &scheduler,
            &process,
            false,
            &system,
            |_svc_num, _svc_args, _thread_context, _svc_count, _iteration| {
                PhysicalCoreExecutionControl::Continue
            },
            |_halt_reason, _exception_address, _thread_context, _svc_count, _iteration| {
                PhysicalCoreExecutionControl::Continue
            },
        );

        assert_eq!(iterations, 3);
        assert_eq!(svc_count, 0);
        assert!(matches!(control, PhysicalCoreExecutionControl::Yield));
    }

    #[test]
    fn handoff_after_svc_captures_current_thread_context_without_switching() {
        let mut system = System::new_for_test();

        let mut process = KProcess::new();
        process.capabilities.core_mask = 0xF;
        process.capabilities.priority_mask = u64::MAX;
        process.flags = 0;
        process.allocate_code_memory(0x200000, 0x20000);
        process.initialize_handle_table();
        process.initialize_thread_local_region_base(0x240000);

        let process = Arc::new(ProcessLock::from_value(process));
        let current_thread = Arc::new(KThreadLock::new(KThread::new()));
        let scheduler = Arc::new(Mutex::new(KScheduler::new(0)));
        {
            let mut thread = current_thread.lock().unwrap();
            thread.initialize_main_thread(0x200000, 0x250000, 0, 0x23f000, &process, 1, 1, false);
            thread.set_priority(44);
            thread.set_base_priority(44);
        }
        process
            .lock()
            .unwrap()
            .register_thread_object(current_thread.clone());
        process.lock().unwrap().push_back_to_priority_queue(1);
        scheduler.lock().unwrap().initialize(1, 0, 0);

        let shared_memory = process.lock().unwrap().get_shared_memory();
        system.set_current_process_arc(process.clone());
        system.set_scheduler_arc(scheduler.clone());
        system.set_shared_process_memory(shared_memory);
        system.set_runtime_program_id(1);
        system.set_runtime_64bit(false);

        let physical_core = PhysicalCore::new(0, false);
        let mut thread_context = ThreadContext::default();
        let mut jit = TestArmInterface::new(VecDeque::new());
        physical_core.initialize_guest_runtime(
            current_thread.clone(),
            &mut jit,
            &mut thread_context,
        );

        jit.context.r[0] = 0xAA;
        jit.context.pc = 0x200100;

        let _ = scheduler;
        let _ = process;
        physical_core.handoff_after_svc(&mut jit, &mut thread_context, &current_thread);

        let thread = current_thread.lock().unwrap();
        assert_eq!(thread.thread_context.r[0], 0xAA);
        assert_eq!(thread.thread_context.r[15], 0x200100);
        assert_eq!(jit.context.r[0], 0xAA);
        assert_eq!(jit.context.pc, 0x200100);
        assert_eq!(
            scheduler.lock().unwrap().get_scheduler_current_thread_id(),
            Some(1)
        );
    }

    #[test]
    fn dispatch_supervisor_call_exit_thread_returns_false_without_handoff() {
        let (physical_core, process, scheduler, current_thread, _system) = test_context();
        // This test exercises ExitThread ownership, not the initialized
        // KernelCore/PhysicalCore graph used by `Svc::Call` to transfer
        // arguments. Use a kernel-less System so the minimal fixture does not
        // index the intentionally empty physical-core vector.
        let mut system = System::new();
        system.set_current_process_arc(process.clone());
        system.set_scheduler_arc(scheduler.clone());
        system.set_shared_process_memory(process.lock().unwrap().get_shared_memory());
        system.set_runtime_program_id(1);
        system.set_runtime_64bit(false);
        let mut thread_context = ThreadContext::default();
        let mut jit = TestArmInterface::new(VecDeque::new());
        physical_core.initialize_guest_runtime(
            current_thread.clone(),
            &mut jit,
            &mut thread_context,
        );
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&current_thread));

        let original_thread_pc = current_thread.lock().unwrap().thread_context.r[15];
        jit.context.pc = 0x200ABC;
        jit.context.r[0] = 0x1122;
        let mut svc_args: SvcArgs = [0xDEAD; 8];

        let continue_thread = physical_core.dispatch_supervisor_call(
            SvcId::ExitThread as u32,
            false,
            &mut svc_args,
            &system,
        );
        crate::hle::kernel::kernel::set_current_emu_thread(None);
        KWorkerTaskManager::wait_for_global_idle();

        assert!(!continue_thread);
        assert_eq!(jit.set_svc_arguments_count, 0);
        assert_eq!(
            current_thread.lock().unwrap().thread_context.r[15],
            original_thread_pc
        );
        assert_eq!(
            current_thread.lock().unwrap().get_state(),
            ThreadState::TERMINATED
        );
    }
}

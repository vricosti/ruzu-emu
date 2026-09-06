//! Port of zuyu/src/core/hle/kernel/k_thread.h / k_thread.cpp
//! Status: Partial (structural port, complex methods stubbed)
//! Derniere synchro: 2026-03-11
//!
//! KThread: The kernel thread object. Preserves all thread states, enums,
//! StackParameters, QueueEntry, NativeExecutionParameters, and all field
//! ownership matching upstream.

use bitflags::bitflags;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU16, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

use super::k_light_lock::{KLightLock, KScopedLightLock};
use super::k_process::KProcess;
use super::k_scheduler::KScheduler;
use super::k_scheduler_lock::KScopedSchedulerLock;
use super::k_scoped_scheduler_lock_and_sleep::KScopedSchedulerLockAndSleep;
use super::k_synchronization_object;
use super::k_synchronization_object::SynchronizationObjectState;
use super::k_synchronization_object::SynchronizationWaitContext;
use super::k_thread_queue::KThreadQueue;
use super::k_thread_queue::KThreadQueueWithoutEndWait;
use super::k_typed_address::{KProcessAddress, KVirtualAddress};
use super::k_worker_task_manager::{KWorkerTaskManager, WorkerType};
use crate::arm::arm_interface::ThreadContext as ArmThreadContext;
use crate::core::SystemRef;
use crate::hardware_properties::NUM_CPU_CORES;
use crate::hle::kernel::svc::svc_results::{
    RESULT_CANCELLED, RESULT_INVALID_COMBINATION, RESULT_INVALID_STATE,
    RESULT_NO_SYNCHRONIZATION_OBJECT, RESULT_OUT_OF_RESOURCE, RESULT_TERMINATION_REQUESTED,
    RESULT_TIMED_OUT,
};
use crate::hle::kernel::svc_types::THREAD_LOCAL_REGION_SIZE;
use crate::hle::result::RESULT_SUCCESS;
use crate::memory::memory::Memory;
// RBEntry kept for structural parity with upstream m_condvar_arbiter_tree_node.
// Currently unused: we use BTreeSet externally instead of an intrusive tree.

use super::k_process::ProcessLock;
use common::tree::RBEntry;

const TERMINATING_THREAD_PRIORITY: i32 =
    crate::hle::kernel::svc_types::SYSTEM_THREAD_PRIORITY_HIGHEST - 1;
pub(crate) const CONTEXT_GUARD_UNOWNED: i32 = -1;

// Step 5b of the upstream-faithful sync refactor: KThread storage moves
// from `Arc<KThreadLock>` to `Arc<KThreadLock>` where
// `KThreadLock = SyncCell<KThread>` (UnsafeCell + scheduler-spin-lock
// contract). The type alias name `KThreadLock` lets every
// `Arc<KThreadLock>` field/parameter declaration compile in place of the
// previous `Arc<KThreadLock>`.
//
// `SyncCell::lock` / `try_lock` / `lock_with` / `from_value` are
// API-compatible shims with `Mutex<T>` (see sync_cell.rs); they return
// guards that deref to `&mut KThread` without doing any real locking —
// serialization is the scheduler spin-lock's job. Mirrors step 5a's
// ProcessLock swap: `pub type ProcessLock = SyncCell<KProcess>`.
pub type KThreadLock = super::sync_cell::KThreadCell;

/// Return the process that owns the current emulated thread.
///
/// Upstream: `GetCurrentProcessPointer(KernelCore&)` in `k_thread.cpp`.
pub fn get_current_process_pointer() -> Option<Arc<ProcessLock>> {
    let current_thread = super::kernel::get_current_thread_pointer()?;
    let parent = current_thread.lock().ok()?.parent.clone()?;
    parent.upgrade()
}

/// Return the process that owns the current emulated thread.
///
/// Upstream: `GetCurrentProcess(KernelCore&)` in `k_thread.cpp`. Rust returns
/// the owning `Arc` rather than a C++ reference.
pub fn get_current_process() -> Option<Arc<ProcessLock>> {
    get_current_process_pointer()
}

/// Return the memory owned by the current emulated thread's process.
///
/// Upstream: `GetCurrentMemory(KernelCore&)` in `k_thread.cpp`.
pub fn get_current_memory() -> Option<Arc<Mutex<Memory>>> {
    let process = get_current_process_pointer()?;
    let process = process.lock().ok()?;
    process.get_memory()
}

/// Upstream anonymous `ThreadLocalRegion` in `k_thread.cpp`.
#[repr(C)]
struct ThreadLocalRegion {
    message_buffer: [u32; 0x100 / std::mem::size_of::<u32>()],
    disable_count: AtomicU16,
    interrupt_flag: AtomicU16,
}

const THREAD_LOCAL_DISABLE_COUNT_OFFSET: u64 =
    std::mem::offset_of!(ThreadLocalRegion, disable_count) as u64;
const THREAD_LOCAL_INTERRUPT_FLAG_OFFSET: u64 =
    std::mem::offset_of!(ThreadLocalRegion, interrupt_flag) as u64;

fn should_trace_wait_debug() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_WAIT_SYNC").is_some())
}

fn should_trace_priority_inheritance() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_PI").is_some())
}

fn should_trace_end_wait() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_END_WAIT").is_some())
}

fn should_trace_ct_fire() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_CT_FIRE").is_some())
}

/// Mirrors upstream anonymous `ThreadQueueImplForKThreadSetProperty` in
/// `k_thread.cpp`: the queue carries ownership of the target thread's pinned
/// waiter list so `CancelWait` can remove a cancelled waiter before delegating
/// to base `KThreadQueue::CancelWait`.
fn thread_queue_for_k_thread_set_property(owner: &Arc<KThreadLock>) -> KThreadQueue {
    KThreadQueue {
        hardware_timer: None,
        end_wait_allowed: true,
        notify_available_impl: None,
        cancel_wait_impl: None,
        pinned_wait_owner: Some(Arc::downgrade(owner)),
    }
}

// ---------------------------------------------------------------------------
// Enums matching upstream k_thread.h
// ---------------------------------------------------------------------------

/// Thread type.
/// Matches upstream `ThreadType` enum (k_thread.h).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadType {
    Main = 0,
    Kernel = 1,
    HighPriority = 2,
    User = 3,
    /// Special thread type for emulation purposes only.
    Dummy = 100,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConditionVariableTreeState {
    #[default]
    None,
    ConditionVariable,
    AddressArbiter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConditionVariableThreadKey {
    pub cv_key: u64,
    pub priority: i32,
    pub thread_id: u64,
}

/// Suspend type.
/// Matches upstream `SuspendType` enum (k_thread.h).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuspendType {
    Process = 0,
    Thread = 1,
    Debug = 2,
    Backtrace = 3,
    Init = 4,
    System = 5,
    // Count = 6, // not represented as a variant
}

impl SuspendType {
    pub const COUNT: u32 = 6;
}

bitflags! {
    /// Thread state flags.
    /// Matches upstream `ThreadState` enum (k_thread.h).
    /// Uses bitflags to support the combined suspend flags pattern.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ThreadState: u16 {
        const INITIALIZED = 0;
        const WAITING = 1;
        const RUNNABLE = 2;
        const TERMINATED = 3;

        /// Mask for the base state (lower 4 bits).
        const MASK = (1 << 4) - 1;

        const PROCESS_SUSPENDED  = 1 << (0 + 4);
        const THREAD_SUSPENDED   = 1 << (1 + 4);
        const DEBUG_SUSPENDED    = 1 << (2 + 4);
        const BACKTRACE_SUSPENDED = 1 << (3 + 4);
        const INIT_SUSPENDED     = 1 << (4 + 4);
        const SYSTEM_SUSPENDED   = 1 << (5 + 4);

        const SUSPEND_FLAG_MASK = ((1 << 6) - 1) << 4;
    }
}

impl ThreadState {
    pub const SUSPEND_SHIFT: u16 = 4;
}

bitflags! {
    /// DPC (Deferred Procedure Call) flags.
    /// Matches upstream `DpcFlag` enum (k_thread.h).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DpcFlag: u32 {
        const TERMINATING = 1 << 0;
        const TERMINATED  = 1 << 1;
    }
}

/// Reason a thread is waiting, for debugging purposes.
/// Matches upstream `ThreadWaitReasonForDebugging` enum (k_thread.h).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadWaitReasonForDebugging {
    None = 0,
    Sleep = 1,
    Ipc = 2,
    Synchronization = 3,
    ConditionVar = 4,
    Arbitration = 5,
    Suspended = 6,
}

impl Default for ThreadWaitReasonForDebugging {
    fn default() -> Self {
        Self::None
    }
}

/// Step state for debugging single-step.
/// Matches upstream `StepState` enum (k_thread.h).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    NotStepping = 0,
    StepPending = 1,
    StepPerformed = 2,
}

impl Default for StepState {
    fn default() -> Self {
        Self::NotStepping
    }
}

// ---------------------------------------------------------------------------
// Constants matching upstream KThread statics
// ---------------------------------------------------------------------------

/// Lowest thread priority from Svc.
pub const SVC_LOWEST_THREAD_PRIORITY: i32 = 63;
/// Highest thread priority from Svc.
pub const SVC_HIGHEST_THREAD_PRIORITY: i32 = 0;

/// Default thread priority.
pub const DEFAULT_THREAD_PRIORITY: i32 = 44;
/// Idle thread priority.
pub const IDLE_THREAD_PRIORITY: i32 = SVC_LOWEST_THREAD_PRIORITY + 1;
/// Dummy thread priority.
pub const DUMMY_THREAD_PRIORITY: i32 = SVC_LOWEST_THREAD_PRIORITY + 2;

/// Maximum count for priority inheritance.
pub const PRIORITY_INHERITANCE_COUNT_MAX: usize = 10;

// ---------------------------------------------------------------------------
// StackParameters — matches upstream KThread::StackParameters
// ---------------------------------------------------------------------------

/// Stack parameters stored per-thread.
/// Matches upstream `KThread::StackParameters` (k_thread.h).
pub struct StackParameters {
    pub svc_permission: [u8; 0x10],
    pub dpc_flags: AtomicU8,
    pub current_svc_id: u8,
    pub is_calling_svc: bool,
    pub is_in_exception_handler: bool,
    pub is_pinned: bool,
    pub disable_count: i32,
    // In C++ this is `KThread* cur_thread;`
    // We skip storing a self-pointer here; it serves no purpose in Rust.
}

impl Default for StackParameters {
    fn default() -> Self {
        Self {
            svc_permission: [0u8; 0x10],
            dpc_flags: AtomicU8::new(0),
            current_svc_id: 0,
            is_calling_svc: false,
            is_in_exception_handler: false,
            is_pinned: false,
            disable_count: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// QueueEntry — re-exported from k_priority_queue for KThread field.
// Matches upstream `KThread::QueueEntry` (k_thread.h).
// QueueEntry re-export removed: entries now stored inside KPriorityQueue.

// ---------------------------------------------------------------------------
// NativeExecutionParameters
// ---------------------------------------------------------------------------

/// Native execution parameters.
/// Matches upstream `KThread::NativeExecutionParameters` (k_thread.h).
pub struct NativeExecutionParameters {
    pub tpidr_el0: u64,
    pub tpidrro_el0: u64,
    // native_context omitted (void* — opaque platform-specific pointer)
    pub lock: std::sync::atomic::AtomicU32,
    pub is_running: bool,
    pub magic: u32,
}

impl Default for NativeExecutionParameters {
    fn default() -> Self {
        Self {
            tpidr_el0: 0,
            tpidrro_el0: 0,
            lock: std::sync::atomic::AtomicU32::new(1),
            is_running: false,
            // 'YUZU' in little-endian bytes: Y=0x59 U=0x55 Z=0x5A U=0x55
            magic: u32::from_le_bytes([b'Y', b'U', b'Z', b'U']),
        }
    }
}

// ---------------------------------------------------------------------------
// SyncObjectBuffer — matches upstream KThread::SyncObjectBuffer
// ---------------------------------------------------------------------------

/// Argument handle count max from Svc.
pub const SVC_ARGUMENT_HANDLE_COUNT_MAX: usize = 0x40;

// ---------------------------------------------------------------------------
// ThreadContext placeholder
// Upstream is Svc::ThreadContext with 29 GPRs, FP/SIMD regs, etc.
// ---------------------------------------------------------------------------

/// Placeholder for Svc::ThreadContext.
/// Mirrors the layout currently used by `arm_interface::ThreadContext`.
#[derive(Clone, Default)]
#[repr(C)]
pub struct ThreadContext {
    pub r: [u64; 29],
    pub fp: u64,
    pub lr: u64,
    pub sp: u64,
    pub pc: u64,
    pub pstate: u32,
    pub padding: u32,
    pub v: [u128; 32],
    pub fpcr: u32,
    pub fpsr: u32,
    pub tpidr: u64,
}

// ---------------------------------------------------------------------------
// KAffinityMask placeholder
// ---------------------------------------------------------------------------

/// Placeholder for KAffinityMask.
/// Upstream: k_affinity_mask.h. Simplified to a single u64 mask.
#[derive(Clone, Default)]
pub struct KAffinityMask {
    pub mask: u64,
}

impl KAffinityMask {
    pub fn get_affinity_mask(&self) -> u64 {
        self.mask
    }
    pub fn set_affinity_mask(&mut self, mask: u64) {
        self.mask = mask;
    }
}

// ---------------------------------------------------------------------------
// LockWithPriorityInheritanceInfo
// Matches upstream KThread::LockWithPriorityInheritanceInfo (k_thread.h:771-845).
// ---------------------------------------------------------------------------

/// Key for ordering waiters in the lock's thread tree.
/// Upstream uses LockWithPriorityInheritanceComparator which orders by
/// (condvar_key, priority, thread_id) — same as ConditionVariableComparator.
/// For lock waiters, condvar_key is effectively the address key, so we
/// order by (priority, thread_id) which is the meaningful ordering for
/// selecting the highest-priority waiter.
#[derive(Clone, Copy, Debug)]
pub struct LockWaiterKey {
    pub priority: i32,
    pub thread_id: u64,
    /// Stable address of the waiting `KThread`.
    ///
    /// Upstream's intrusive tree stores the `KThread` itself. Keeping the
    /// pointer here preserves that ownership model for kernel waiters which
    /// are intentionally absent from the schedulable-thread registry.
    pub thread_ptr: usize,
}

impl PartialEq for LockWaiterKey {
    fn eq(&self, other: &Self) -> bool {
        (self.priority, self.thread_id) == (other.priority, other.thread_id)
    }
}

impl Eq for LockWaiterKey {}

impl PartialOrd for LockWaiterKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LockWaiterKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.priority, self.thread_id).cmp(&(other.priority, other.thread_id))
    }
}

/// Per-address lock tracking structure.
/// Upstream: nested class inside KThread, slab-allocated, intrusive-list node.
/// In Rust: owned in a Vec on the holding thread.
///
/// Each instance tracks one address key (mutex/lock address) and the set
/// of threads waiting to acquire that lock.
pub struct LockWithPriorityInheritanceInfo {
    /// Waiters ordered by (priority, thread_id) — matches upstream red-black tree.
    tree: std::collections::BTreeSet<LockWaiterKey>,
    /// The address being locked.
    address_key: KProcessAddress,
    /// Owner thread ID (upstream: raw pointer to KThread).
    owner_thread_id: u64,
    /// Number of waiters.
    waiter_count: u32,
    /// Whether this is a kernel address key.
    is_kernel_address_key: bool,
}

impl LockWithPriorityInheritanceInfo {
    /// Create a new lock info for the given address key.
    /// Matches upstream `LockWithPriorityInheritanceInfo::Create()`.
    pub fn new(address_key: KProcessAddress, is_kernel_address_key: bool) -> Self {
        Self {
            tree: std::collections::BTreeSet::new(),
            address_key,
            owner_thread_id: 0,
            waiter_count: 0,
            is_kernel_address_key,
        }
    }

    pub fn set_owner(&mut self, owner_thread_id: u64) {
        self.owner_thread_id = owner_thread_id;
    }

    /// Add a waiter thread. The caller must provide the waiter's priority and thread_id.
    /// Matches upstream `AddWaiter(KThread*)`.
    pub fn add_waiter(&mut self, priority: i32, thread_id: u64, thread_ptr: usize) {
        let inserted = self.tree.insert(LockWaiterKey {
            priority,
            thread_id,
            thread_ptr,
        });
        if inserted {
            self.waiter_count += 1;
        } else {
            debug_assert!(
                false,
                "LockWithPriorityInheritanceInfo::add_waiter duplicate waiter priority={} thread_id={}",
                priority, thread_id
            );
        }
    }

    /// Remove a waiter thread. Returns true if the lock has no more waiters.
    /// Matches upstream `RemoveWaiter(KThread*)`.
    pub fn remove_waiter(&mut self, priority: i32, thread_id: u64) -> bool {
        let removed = self.tree.remove(&LockWaiterKey {
            priority,
            thread_id,
            thread_ptr: 0,
        });
        if removed {
            self.waiter_count -= 1;
        } else {
            debug_assert!(
                false,
                "LockWithPriorityInheritanceInfo::remove_waiter missing waiter priority={} thread_id={}",
                priority, thread_id
            );
            self.waiter_count = self.tree.len() as u32;
        }
        self.waiter_count == 0
    }

    /// Get the highest priority waiter's key.
    /// Matches upstream `GetHighestPriorityWaiter()` — front of tree = lowest
    /// (priority, thread_id) = highest priority.
    pub fn get_highest_priority_waiter(&self) -> Option<LockWaiterKey> {
        self.tree.iter().next().copied()
    }

    pub fn waiter_keys(&self) -> Vec<LockWaiterKey> {
        self.tree.iter().copied().collect()
    }

    pub fn get_address_key(&self) -> KProcessAddress {
        self.address_key
    }

    pub fn get_is_kernel_address_key(&self) -> bool {
        self.is_kernel_address_key
    }

    pub fn get_owner_thread_id(&self) -> u64 {
        self.owner_thread_id
    }

    pub fn get_waiter_count(&self) -> u32 {
        self.waiter_count
    }
}

/// Reference to a LockWithPriorityInheritanceInfo on another thread.
/// Upstream uses a raw pointer; we store enough info to find it.
#[derive(Clone, Debug)]
pub struct WaitingLockRef {
    /// The thread that owns the lock info.
    pub owner_thread_id: u64,
    /// The address key identifying the lock.
    pub address_key: KProcessAddress,
    /// Whether it's a kernel address key.
    pub is_kernel_address_key: bool,
    /// Raw pointer to the owner `KThread`. Matches upstream's `KThread*`.
    /// Valid while this wait is active: the owner is kept alive by the
    /// parent process's thread list, and `KThreadLock` never relocates
    /// its inner value. Used by `cancel_wait` paths running under the
    /// scheduler lock, where upstream dereferences the `KThread*` directly
    /// without any per-object mutex. 0 means "no pointer available".
    pub owner_thread_ptr: usize,
}

// ---------------------------------------------------------------------------
// KThread — the main thread structure
// ---------------------------------------------------------------------------

/// Diagnostic record for `KThread::context_guard` lock/unlock attribution.
/// Not part of upstream; read by the SIGUSR1 thread dump.
#[derive(Default)]
pub struct ContextGuardTrace {
    /// (call site, host thread name) of the last successful lock.
    pub last_lock: Option<(&'static str, String)>,
    /// (call site, host thread name) of the last unlock.
    pub last_unlock: Option<(&'static str, String)>,
}

/// The kernel thread object.
/// Matches upstream `KThread` class (k_thread.h).
///
/// Uses indices/IDs instead of raw pointers for references to other kernel
/// objects. Arc/Weak used where upstream uses shared_ptr/weak_ptr.
pub struct KThread {
    // -- Core KThread fields --
    pub object_id: u64,
    pub self_reference: Option<Weak<KThreadLock>>,
    pub thread_context: ThreadContext,
    pub condvar_arbiter_tree_node: RBEntry,
    pub priority: i32,

    // Condition variable / arbiter tree membership
    pub condvar_tree_state: ConditionVariableTreeState,
    pub condvar_key: u64,
    pub virtual_affinity_mask: u64,
    pub physical_affinity_mask: KAffinityMask,
    pub thread_id: u64,
    pub cpu_time: AtomicI64,
    pub address_key: KProcessAddress,
    // parent process — Weak reference matching upstream raw pointer + ref counting
    pub parent: Option<Weak<ProcessLock>>,
    /// Raw `*mut KProcess` cached at parenting time so scheduler-lock-protected
    /// code paths can access process fields (cond_var tree, etc.) without
    /// re-acquiring the KProcess mutex. Matches upstream's raw-pointer access
    /// to the owning process. 0 means "no pointer set"; fall back to the Weak.
    pub parent_raw_ptr: usize,
    pub scheduler: Option<Weak<Mutex<KScheduler>>>,
    /// Direct reference to the GlobalSchedulerContext for PQ updates.
    /// Matches upstream's access via `KernelCore&` in `KScheduler::OnThreadStateChanged`.
    pub global_scheduler_context:
        Option<Weak<Mutex<super::global_scheduler_context::GlobalSchedulerContext>>>,
    /// Non-owning pointer to the scheduler lock inside GlobalSchedulerContext.
    /// This avoids taking the outer GlobalSchedulerContext mutex on paths such as Sleep(),
    /// where upstream locks only the scheduler lock itself.
    pub scheduler_lock_ptr: usize,
    /// Process schedule count, cloned from the owning KProcess.
    /// Used by notify_state_transition to pass to PQ push without locking the process.
    pub process_schedule_count: Option<Arc<std::sync::atomic::AtomicI64>>,
    pub kernel_stack_top: KVirtualAddress,
    pub light_ipc_data: Option<Vec<u32>>,
    pub tls_address: KProcessAddress,
    /// Serializes thread activity, affinity, and context operations.
    /// Upstream: `KLightLock m_activity_pause_lock`.
    pub activity_pause_lock: Arc<KLightLock>,
    pub schedule_count: i64,
    pub last_scheduled_tick: i64,
    // per_core_priority_queue_entry removed: entries now stored inside KPriorityQueue.

    // Wait queue — upstream `m_wait_queue`
    pub wait_queue: Option<KThreadQueue>,

    // Lock with priority inheritance — matches upstream fields:
    // LockWithPriorityInheritanceInfoList m_held_lock_info_list{};
    // LockWithPriorityInheritanceInfo* m_waiting_lock_info{};
    pub held_lock_info_list: Vec<LockWithPriorityInheritanceInfo>,
    /// Index into the *owner* thread's held_lock_info_list that this thread
    /// is waiting on. None if not waiting on any lock.
    /// Upstream: raw pointer `m_waiting_lock_info`.
    /// We store (owner_thread_id, address_key, is_kernel_address_key) so we
    /// can find the lock info on the owner thread.
    pub waiting_lock_info: Option<WaitingLockRef>,
    /// Upstream: `WaiterList m_pinned_waiter_list`.
    /// Threads waiting for this thread to unpin are stored by guest thread id.
    pub pinned_waiter_list: Vec<u64>,

    pub address_key_value: u32,
    pub suspend_request_flags: u32,
    pub suspend_allowed_flags: u32,
    pub synced_index: i32,
    pub wait_result: u32, // Result code
    pub base_priority: i32,
    pub physical_ideal_core_id: i32,
    pub virtual_ideal_core_id: i32,
    pub num_kernel_waiters: i32,
    pub current_core_id: i32,
    pub core_id: i32,
    pub original_physical_affinity_mask: KAffinityMask,
    pub original_physical_ideal_core_id: i32,
    pub num_core_migration_disables: i32,
    pub thread_state: AtomicU16,
    pub termination_requested: AtomicBool,
    pub wait_cancelled: bool,
    pub cancellable: bool,
    pub signaled: bool,
    pub termination_wait_pair: Arc<(Mutex<bool>, Condvar)>,
    pub initialized: bool,
    pub debug_attached: bool,
    pub priority_inheritance_count: i8,
    pub resource_limit_release_hint: bool,
    pub is_kernel_address_key: bool,
    pub stack_parameters: StackParameters,

    // Emulation fields
    /// Host fiber context for this thread.
    /// Upstream: `std::shared_ptr<Common::Fiber> m_host_context`
    pub host_context: Option<Arc<common::fiber::Fiber>>,
    /// Context guard for fiber switching.
    ///
    /// Upstream uses `KSpinLock m_context_guard`. The owning core id is the
    /// lock word in Rust; `CONTEXT_GUARD_UNOWNED` is the unlocked state.
    /// This avoids retaining a host mutex guard across a fiber switch.
    pub(crate) context_guard_owner: AtomicI32,
    /// Diagnostic: last lock/unlock sites of `context_guard`
    /// (`site@host_thread`), shown by the SIGUSR1 dump to attribute leaked
    /// guards (a thread whose guard stays locked wedges the switch fiber's
    /// upstream-faithful `while (!context_guard.try_lock())` spin).
    pub context_guard_trace: parking_lot::Mutex<ContextGuardTrace>,
    pub thread_type: ThreadType,
    pub step_state: StepState,
    /// Upstream's dummy mutex, runnable predicate and condition variable.
    /// A separate allocation lets the host block without borrowing KThread
    /// while the signaling core mutates its kernel wait state.
    pub(crate) dummy_thread_wait: Arc<(Mutex<bool>, Condvar)>,

    // Debugging fields
    pub wait_reason_for_debugging: ThreadWaitReasonForDebugging,
    pub argument: usize,
    pub stack_top: KProcessAddress,
    pub native_execution_parameters: NativeExecutionParameters,
    /// Upstream: KTimerTask::m_time — absolute time in nanoseconds for
    /// the hardware timer. Set by KHardwareTimer::RegisterAbsoluteTask,
    /// cleared to 0 when the task fires or is cancelled.
    pub timer_task_time: i64,
    pub sync_wait_context: SynchronizationWaitContext,
    pub sync_object: SynchronizationObjectState,
}

impl KThread {
    pub fn restore_guest_context(&self, ctx: &mut ArmThreadContext) {
        ctx.r = self.thread_context.r;
        ctx.fp = self.thread_context.fp;
        ctx.lr = self.thread_context.lr;
        ctx.sp = self.thread_context.sp;
        ctx.pc = self.thread_context.pc;
        ctx.pstate = self.thread_context.pstate;
        ctx.v = self.thread_context.v;
        ctx.fpcr = self.thread_context.fpcr;
        ctx.fpsr = self.thread_context.fpsr;
        ctx.tpidr = self.thread_context.tpidr;
    }

    /// Diagnostic: record who locked/unlocked `context_guard` last
    /// (site + host thread), for the SIGUSR1 dump. Gated behind
    /// `RUZU_TRACE_CTX_GUARD` — the String allocation per context switch is
    /// not free on the scheduler hot path.
    pub fn record_context_guard_event(&self, locked: bool, site: &'static str) {
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !*ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_CTX_GUARD").is_some()) {
            return;
        }
        let host = std::thread::current().name().unwrap_or("?").to_string();
        let mut trace = self.context_guard_trace.lock();
        if locked {
            trace.last_lock = Some((site, host));
        } else {
            trace.last_unlock = Some((site, host));
        }
    }

    pub fn capture_guest_context(&mut self, ctx: &ArmThreadContext) {
        self.thread_context.r = ctx.r;
        self.thread_context.fp = ctx.fp;
        self.thread_context.lr = ctx.lr;
        self.thread_context.sp = ctx.sp;
        self.thread_context.pc = ctx.pc;
        self.thread_context.pstate = ctx.pstate;
        self.thread_context.v = ctx.v;
        self.thread_context.fpcr = ctx.fpcr;
        self.thread_context.fpsr = ctx.fpsr;
        self.thread_context.tpidr = ctx.tpidr;
    }

    /// Create a new KThread with default/zero-initialized state.
    pub fn new() -> Self {
        Self {
            object_id: 0,
            self_reference: None,
            thread_context: ThreadContext::default(),
            condvar_arbiter_tree_node: RBEntry::default(),
            priority: 0,
            condvar_tree_state: ConditionVariableTreeState::None,
            condvar_key: 0,
            virtual_affinity_mask: 0,
            physical_affinity_mask: KAffinityMask::default(),
            thread_id: 0,
            cpu_time: AtomicI64::new(0),
            address_key: KProcessAddress::default(),
            parent: None,
            parent_raw_ptr: 0,
            scheduler: None,
            global_scheduler_context: None,
            scheduler_lock_ptr: 0,
            process_schedule_count: None,
            kernel_stack_top: KVirtualAddress::default(),
            light_ipc_data: None,
            tls_address: KProcessAddress::default(),
            activity_pause_lock: Arc::new(KLightLock::new()),
            schedule_count: 0,
            last_scheduled_tick: 0,
            // per_core_priority_queue_entry removed: entries in KPriorityQueue
            wait_queue: None,
            held_lock_info_list: Vec::new(),
            waiting_lock_info: None,
            pinned_waiter_list: Vec::new(),
            address_key_value: 0,
            suspend_request_flags: 0,
            suspend_allowed_flags: ThreadState::SUSPEND_FLAG_MASK.bits() as u32,
            synced_index: 0,
            wait_result: RESULT_NO_SYNCHRONIZATION_OBJECT.get_inner_value(),
            base_priority: 0,
            physical_ideal_core_id: 0,
            virtual_ideal_core_id: 0,
            num_kernel_waiters: 0,
            current_core_id: 0,
            core_id: 0,
            original_physical_affinity_mask: KAffinityMask::default(),
            original_physical_ideal_core_id: 0,
            num_core_migration_disables: 0,
            thread_state: AtomicU16::new(0),
            termination_requested: AtomicBool::new(false),
            wait_cancelled: false,
            cancellable: false,
            signaled: false,
            termination_wait_pair: Arc::new((Mutex::new(false), Condvar::new())),
            initialized: false,
            debug_attached: false,
            priority_inheritance_count: 0,
            resource_limit_release_hint: false,
            is_kernel_address_key: false,
            stack_parameters: StackParameters::default(),
            host_context: None,
            context_guard_owner: AtomicI32::new(CONTEXT_GUARD_UNOWNED),
            context_guard_trace: parking_lot::Mutex::new(ContextGuardTrace::default()),
            thread_type: ThreadType::User,
            step_state: StepState::default(),
            dummy_thread_wait: Arc::new((Mutex::new(true), Condvar::new())),
            wait_reason_for_debugging: ThreadWaitReasonForDebugging::default(),
            argument: 0,
            stack_top: KProcessAddress::default(),
            native_execution_parameters: NativeExecutionParameters::default(),
            timer_task_time: 0,
            sync_wait_context: SynchronizationWaitContext::new(),
            sync_object: SynchronizationObjectState::new(),
        }
    }

    // -- Getters / setters matching upstream --

    pub fn get_priority(&self) -> i32 {
        self.priority
    }

    pub fn get_object_id(&self) -> u64 {
        self.object_id
    }

    pub fn bind_self_reference(&mut self, thread: &Arc<KThreadLock>) {
        self.self_reference = Some(Arc::downgrade(thread));
    }

    pub fn set_priority(&mut self, value: i32) {
        self.priority = value;
    }

    pub fn get_base_priority(&self) -> i32 {
        self.base_priority
    }

    pub fn get_thread_id(&self) -> u64 {
        self.thread_id
    }

    pub fn get_tls_address(&self) -> KProcessAddress {
        self.tls_address
    }

    pub fn get_tpidr_el0(&self) -> u64 {
        self.thread_context.tpidr
    }

    pub fn set_tpidr_el0(&mut self, value: u64) {
        self.thread_context.tpidr = value;
    }

    pub fn get_state(&self) -> ThreadState {
        let raw = self.thread_state.load(Ordering::Relaxed);
        ThreadState::from_bits_truncate(raw) & ThreadState::MASK
    }

    pub fn get_raw_state(&self) -> ThreadState {
        ThreadState::from_bits_truncate(self.thread_state.load(Ordering::Relaxed))
    }

    pub fn get_step_state(&self) -> StepState {
        self.step_state
    }

    pub fn set_step_state(&mut self, state: StepState) {
        self.step_state = state;
    }

    pub fn get_last_scheduled_tick(&self) -> i64 {
        self.last_scheduled_tick
    }

    pub fn set_last_scheduled_tick(&mut self, tick: i64) {
        self.last_scheduled_tick = tick;
    }

    pub fn add_cpu_time(&self, _core_id: i32, amount: i64) {
        self.cpu_time.fetch_add(amount, Ordering::Relaxed);
    }

    pub fn get_cpu_time(&self) -> i64 {
        self.cpu_time.load(Ordering::Relaxed)
    }

    pub fn get_active_core(&self) -> i32 {
        self.core_id
    }

    pub fn set_active_core(&mut self, core: i32) {
        self.core_id = core;
    }

    pub fn get_current_core(&self) -> i32 {
        self.current_core_id
    }

    pub fn set_current_core(&mut self, core: i32) {
        self.current_core_id = core;
    }

    pub fn is_user_thread(&self) -> bool {
        self.thread_type == ThreadType::User || self.parent.is_some()
    }

    /// Get the host fiber context for this thread.
    /// Upstream: `KThread::GetHostContext()` (k_thread.h:266).
    /// Used by CpuManager::RunThread and ShutdownThread for fiber switching.
    pub fn get_host_context(&self) -> Option<&Arc<common::fiber::Fiber>> {
        self.host_context.as_ref()
    }

    /// Set the host fiber context for this thread.
    pub fn set_host_context(&mut self, ctx: Arc<common::fiber::Fiber>) {
        self.host_context = Some(ctx);
    }

    fn initialize_host_context(&mut self, init_func: Option<Box<dyn FnOnce() + Send>>) {
        self.host_context = init_func.map(common::fiber::Fiber::new);
    }

    /// Shared owner-local initialization for ownerless kernel threads.
    ///
    /// Upstream owner boundary: `KThread::InitializeThread(...)` in `k_thread.cpp`
    /// for the `owner == nullptr` kernel-thread call sites
    /// (`InitializeMainThread`, `InitializeIdleThread`, `InitializeHighPriorityThread`).
    fn initialize_ownerless_kernel_thread(
        &mut self,
        virt_core: i32,
        thread_id: u64,
        object_id: u64,
        thread_type: ThreadType,
        priority: i32,
        initial_state: ThreadState,
        stack_parameters: StackParameters,
        init_func: Option<Box<dyn FnOnce() + Send>>,
    ) {
        let phys_core =
            crate::hardware_properties::VIRTUAL_TO_PHYSICAL_CORE_MAP[virt_core as usize];

        self.object_id = object_id;
        self.thread_type = thread_type;
        self.thread_id = thread_id;
        self.priority = priority;
        self.base_priority = priority;
        self.virtual_ideal_core_id = virt_core;
        self.physical_ideal_core_id = phys_core;
        self.virtual_affinity_mask = 1u64 << virt_core;
        self.physical_affinity_mask
            .set_affinity_mask(1u64 << phys_core);
        self.tls_address = KProcessAddress::default();
        self.parent = None;
        self.parent_raw_ptr = 0;
        self.scheduler = None;
        self.global_scheduler_context = None;
        self.scheduler_lock_ptr = 0;
        self.process_schedule_count = None;
        self.signaled = false;
        let (termination_lock, _) = &*self.termination_wait_pair;
        *termination_lock.lock().unwrap() = false;
        self.termination_requested.store(false, Ordering::Relaxed);
        self.wait_cancelled = false;
        self.cancellable = false;
        self.stack_top = KProcessAddress::default();
        self.argument = 0;
        self.core_id = phys_core;
        self.current_core_id = phys_core;
        self.thread_state
            .store(initial_state.bits(), Ordering::Relaxed);
        self.suspend_allowed_flags = ThreadState::SUSPEND_FLAG_MASK.bits() as u32;
        self.suspend_request_flags = 0;
        self.wait_result = RESULT_NO_SYNCHRONIZATION_OBJECT.get_inner_value();
        self.schedule_count = -1;
        self.last_scheduled_tick = 0;
        self.num_kernel_waiters = 0;
        self.resource_limit_release_hint = false;
        self.sync_wait_context.clear();
        self.stack_parameters = stack_parameters;
        self.initialized = true;

        // No owner → use 64-bit thread context (upstream: m_parent == nullptr → 64-bit path).
        self.reset_thread_context64(0, 0, 0);

        // Initialize emulation parameters.
        // Upstream: thread->m_host_context = std::make_shared<Common::Fiber>(std::move(init_func)).
        self.initialize_host_context(init_func);
    }

    /// Read the user-mode disable count from the thread's TLS region in guest memory.
    ///
    /// Upstream: `KThread::GetUserDisableCount()` (k_thread.cpp:552-560).
    /// The ThreadLocalRegion layout:
    ///   offset 0x000: message_buffer[0x100]
    ///   offset 0x100: disable_count (u16)
    ///   offset 0x102: interrupt_flag (u16)
    pub fn get_user_disable_count(&self) -> u16 {
        if !self.is_user_thread() {
            return 0;
        }
        let tls_addr = self.tls_address.get();
        if tls_addr == 0 {
            return 0;
        }
        let addr = tls_addr + THREAD_LOCAL_DISABLE_COUNT_OFFSET;

        if let Some(parent) = self.parent.as_ref().and_then(|w| w.upgrade()) {
            let memory = parent.lock().unwrap().get_memory();
            memory
                .map(|memory| memory.lock().unwrap().read_16(addr))
                .unwrap_or(0)
        } else {
            0
        }
    }

    /// Set the interrupt flag in the thread's TLS region in guest memory.
    ///
    /// Upstream: `KThread::SetInterruptFlag()` (k_thread.cpp:562-570).
    pub fn set_interrupt_flag(&self) {
        if !self.is_user_thread() {
            return;
        }
        let tls_addr = self.tls_address.get();
        if tls_addr == 0 {
            return;
        }
        let addr = tls_addr + THREAD_LOCAL_INTERRUPT_FLAG_OFFSET;

        if let Some(parent) = self.parent.as_ref().and_then(|w| w.upgrade()) {
            if let Some(memory) = parent.lock().unwrap().get_memory() {
                memory.lock().unwrap().write_16(addr, 1);
            }
        }
    }

    /// Clear the interrupt flag in the thread's TLS region in guest memory.
    ///
    /// Upstream: `KThread::ClearInterruptFlag()` (k_thread.cpp:572-580).
    pub fn clear_interrupt_flag(&self) {
        if !self.is_user_thread() {
            return;
        }
        let tls_addr = self.tls_address.get();
        if tls_addr == 0 {
            return;
        }
        let addr = tls_addr + THREAD_LOCAL_INTERRUPT_FLAG_OFFSET;

        if let Some(parent) = self.parent.as_ref().and_then(|w| w.upgrade()) {
            if let Some(memory) = parent.lock().unwrap().get_memory() {
                memory.lock().unwrap().write_16(addr, 0);
            }
        }
    }

    pub fn get_suspend_flags(&self) -> u32 {
        self.suspend_allowed_flags & self.suspend_request_flags
    }

    /// Get the user thread context for `svcGetThreadContext3`.
    ///
    /// Upstream: `KThread::GetThreadContext3` in `k_thread.cpp`.
    pub fn get_thread_context3(&self, out: &mut ThreadContext) -> u32 {
        let activity_pause_lock = self.activity_pause_lock.clone();
        let _activity_guard = KScopedLightLock::new(activity_pause_lock.as_ref());

        let _sl = if self.scheduler_lock_ptr != 0 {
            Some(KScopedSchedulerLock::new(unsafe {
                &*(self.scheduler_lock_ptr
                    as *const super::k_scheduler_lock::KAbstractSchedulerLock)
            }))
        } else {
            None
        };

        if !self.is_suspend_requested_type(SuspendType::Thread) {
            return RESULT_INVALID_STATE.get_inner_value();
        }

        if !self.is_termination_requested() {
            *out = self.thread_context.clone();

            // Upstream masks away mode bits, interrupt bits, IL bit, and other
            // reserved bits before copying the context to userspace.
            const EL0_AARCH64_PSR_MASK: u32 = 0xF000_0000;
            const EL0_AARCH32_PSR_MASK: u32 = 0xFE0F_FE20;

            let is_64bit = self
                .get_parent_raw_ptr()
                .map(|parent| unsafe { (&*parent).is_64bit() })
                .unwrap_or(true);
            if is_64bit {
                out.pstate &= EL0_AARCH64_PSR_MASK;
            } else {
                out.pstate &= EL0_AARCH32_PSR_MASK;
            }
        }

        RESULT_SUCCESS.get_inner_value()
    }

    pub fn is_suspended(&self) -> bool {
        self.get_suspend_flags() != 0
    }

    pub fn is_suspend_requested_type(&self, suspend_type: SuspendType) -> bool {
        (self.suspend_request_flags
            & (1u32 << (ThreadState::SUSPEND_SHIFT as u32 + suspend_type as u32)))
            != 0
    }

    pub fn is_suspend_requested(&self) -> bool {
        self.suspend_request_flags != 0
    }

    pub fn get_synced_index(&self) -> i32 {
        self.synced_index
    }

    pub fn set_synced_index(&mut self, index: i32) {
        self.synced_index = index;
    }

    pub fn get_wait_result(&self) -> u32 {
        self.wait_result
    }

    pub fn set_wait_result(&mut self, result: u32) {
        self.wait_result = result;
    }

    pub fn get_yield_schedule_count(&self) -> i64 {
        self.schedule_count
    }

    pub fn set_yield_schedule_count(&mut self, count: i64) {
        self.schedule_count = count;
    }

    pub fn is_wait_cancelled(&self) -> bool {
        self.wait_cancelled
    }

    pub fn clear_wait_cancelled(&mut self) {
        self.wait_cancelled = false;
    }

    pub fn is_cancellable(&self) -> bool {
        self.cancellable
    }

    pub fn set_cancellable(&mut self) {
        self.cancellable = true;
    }

    pub fn clear_cancellable(&mut self) {
        self.cancellable = false;
    }

    pub fn is_termination_requested(&self) -> bool {
        self.termination_requested.load(Ordering::Relaxed)
            || self.get_raw_state() == ThreadState::TERMINATED
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub fn get_thread_type(&self) -> ThreadType {
        self.thread_type
    }

    pub fn is_dummy_thread(&self) -> bool {
        self.thread_type == ThreadType::Dummy
    }

    pub fn get_disable_dispatch_count(&self) -> i32 {
        self.stack_parameters.disable_count
    }

    pub fn disable_dispatch(&mut self) {
        self.stack_parameters.disable_count += 1;
    }

    pub fn enable_dispatch(&mut self) {
        assert!(self.stack_parameters.disable_count > 0);
        self.stack_parameters.disable_count -= 1;
    }

    pub fn set_in_exception_handler(&mut self) {
        self.stack_parameters.is_in_exception_handler = true;
    }

    pub fn clear_in_exception_handler(&mut self) {
        self.stack_parameters.is_in_exception_handler = false;
    }

    pub fn is_in_exception_handler(&self) -> bool {
        self.stack_parameters.is_in_exception_handler
    }

    pub fn set_is_calling_svc(&mut self) {
        self.stack_parameters.is_calling_svc = true;
    }

    pub fn clear_is_calling_svc(&mut self) {
        self.stack_parameters.is_calling_svc = false;
    }

    pub fn is_calling_svc(&self) -> bool {
        self.stack_parameters.is_calling_svc
    }

    pub fn get_svc_id(&self) -> u8 {
        self.stack_parameters.current_svc_id
    }

    pub fn register_dpc(&self, flag: DpcFlag) {
        self.stack_parameters
            .dpc_flags
            .fetch_or(flag.bits() as u8, Ordering::Relaxed);
    }

    pub fn clear_dpc(&self, flag: DpcFlag) {
        self.stack_parameters
            .dpc_flags
            .fetch_and(!(flag.bits() as u8), Ordering::Relaxed);
    }

    pub fn get_dpc(&self) -> u8 {
        self.stack_parameters.dpc_flags.load(Ordering::Relaxed)
    }

    pub fn has_dpc(&self) -> bool {
        self.get_dpc() != 0
    }

    pub fn set_wait_reason_for_debugging(&mut self, reason: ThreadWaitReasonForDebugging) {
        self.wait_reason_for_debugging = reason;
    }

    pub fn get_wait_reason_for_debugging(&self) -> ThreadWaitReasonForDebugging {
        self.wait_reason_for_debugging
    }

    pub fn get_num_kernel_waiters(&self) -> i32 {
        self.num_kernel_waiters
    }

    pub fn add_transferred_kernel_waiters(&mut self, count: u32) {
        self.num_kernel_waiters += count as i32;
    }

    pub fn get_condition_variable_key(&self) -> u64 {
        self.condvar_key
    }

    pub fn get_condition_variable_tree(&self) -> Option<ConditionVariableTreeState> {
        match self.condvar_tree_state {
            ConditionVariableTreeState::None => None,
            state => Some(state),
        }
    }

    pub fn get_address_arbiter_key(&self) -> u64 {
        self.condvar_key
    }

    pub fn get_address_key(&self) -> KProcessAddress {
        self.address_key
    }

    pub fn get_address_key_value(&self) -> u32 {
        self.address_key_value
    }

    // -- LockWithPriorityInheritanceInfo methods --

    /// Find a held lock info by address key.
    /// Matches upstream `KThread::FindHeldLock()`.
    pub fn find_held_lock_index(
        &self,
        address_key: KProcessAddress,
        is_kernel_address_key: bool,
    ) -> Option<usize> {
        self.held_lock_info_list.iter().position(|info| {
            info.get_address_key() == address_key
                && info.get_is_kernel_address_key() == is_kernel_address_key
        })
    }

    /// Add a lock info to our held list and set ourselves as owner.
    /// Matches upstream `KThread::AddHeldLock()`.
    pub fn add_held_lock(&mut self, mut lock_info: LockWithPriorityInheritanceInfo) {
        lock_info.set_owner(self.thread_id);
        self.held_lock_info_list.push(lock_info);
    }

    /// Set the waiting lock info reference.
    /// Matches upstream `KThread::SetWaitingLockInfo()`.
    pub fn set_waiting_lock_info(&mut self, lock_ref: Option<WaitingLockRef>) {
        self.waiting_lock_info = lock_ref;
    }

    /// Get the waiting lock info reference.
    /// Matches upstream `KThread::GetWaitingLockInfo()`.
    pub fn get_waiting_lock_info(&self) -> Option<&WaitingLockRef> {
        self.waiting_lock_info.as_ref()
    }

    /// Add a waiter thread to the appropriate lock info.
    /// Matches upstream `KThread::AddWaiterImpl()` (k_thread.cpp:962-989).
    ///
    /// The waiter's address_key and is_kernel_address_key must already be set.
    pub fn add_waiter_impl(
        &mut self,
        waiter_thread_id: u64,
        waiter_priority: i32,
        waiter_thread_ptr: usize,
        waiter_address_key: KProcessAddress,
        waiter_is_kernel_address_key: bool,
    ) {
        // Keep track of kernel waiters.
        if waiter_is_kernel_address_key {
            self.num_kernel_waiters += 1;
        }

        // Find or create the lock info for this address.
        let lock_idx = self.find_held_lock_index(waiter_address_key, waiter_is_kernel_address_key);
        let lock_idx = match lock_idx {
            Some(idx) => idx,
            None => {
                let lock_info = LockWithPriorityInheritanceInfo::new(
                    waiter_address_key,
                    waiter_is_kernel_address_key,
                );
                self.add_held_lock(lock_info);
                self.held_lock_info_list.len() - 1
            }
        };

        // Add the waiter.
        self.held_lock_info_list[lock_idx].add_waiter(
            waiter_priority,
            waiter_thread_id,
            waiter_thread_ptr,
        );
    }

    /// Remove a waiter thread from its lock info.
    /// Matches upstream `KThread::RemoveWaiterImpl()` (k_thread.cpp:991-1009).
    ///
    /// `waiter_lock_ref` identifies which lock the waiter is on.
    pub fn remove_waiter_impl(
        &mut self,
        waiter_thread_id: u64,
        waiter_priority: i32,
        waiter_is_kernel_address_key: bool,
        waiter_address_key: KProcessAddress,
    ) {
        // Keep track of kernel waiters.
        if waiter_is_kernel_address_key {
            self.num_kernel_waiters -= 1;
        }

        // Find the lock info.
        let lock_idx = self
            .find_held_lock_index(waiter_address_key, waiter_is_kernel_address_key)
            .expect("RemoveWaiterImpl: lock info not found");

        // Remove the waiter; if the lock is now empty, remove it.
        // Bounds-checked against an unserialized concurrent shrink by the
        // hardware timer (see `remove_waiter_by_key`).
        let Some(lock_info) = self.held_lock_info_list.get_mut(lock_idx) else {
            return;
        };
        let is_empty = lock_info.remove_waiter(waiter_priority, waiter_thread_id);
        if is_empty && lock_idx < self.held_lock_info_list.len() {
            self.held_lock_info_list.remove(lock_idx);
        }
    }

    /// Public AddWaiter: adds waiter then triggers priority inheritance.
    /// Matches upstream `KThread::AddWaiter()` (k_thread.cpp:1063-1070).
    ///
    /// The waiter's lock-owner back-pointer is set automatically here, matching
    /// upstream `LockWithPriorityInheritanceInfo::AddWaiter` which internally
    /// calls `waiter->SetWaitingLockInfo(this)` (k_thread.h:801). Callers no
    /// longer need a separate `set_waiting_lock_owner_thread_id` step.
    ///
    /// Priority inheritance follows the complete lock-owner chain, matching
    /// upstream `RestorePriority`.
    pub fn add_waiter(
        &mut self,
        waiter: &Arc<KThreadLock>,
        waiter_thread_id: u64,
        waiter_priority: i32,
        waiter_address_key: KProcessAddress,
        waiter_is_kernel_address_key: bool,
    ) {
        if should_trace_priority_inheritance() {
            log::info!(
                "PI add_waiter owner_tid={} owner_prio={} owner_base={} waiter_tid={} waiter_prio={} addr=0x{:X} kernel={}",
                self.thread_id,
                self.priority,
                self.base_priority,
                waiter_thread_id,
                waiter_priority,
                waiter_address_key.get(),
                waiter_is_kernel_address_key,
            );
        }
        self.add_waiter_impl(
            waiter_thread_id,
            waiter_priority,
            waiter.as_ref().as_ptr() as usize,
            waiter_address_key,
            waiter_is_kernel_address_key,
        );

        // Set the waiter's lock-owner back-pointer. Matches the
        // `waiter->SetWaitingLockInfo(this)` step that upstream's
        // `LockWithPriorityInheritanceInfo::AddWaiter` performs.
        // `self` is the owner; compute its raw pointer for cancel_wait paths
        // that need a `*mut KThread` matching upstream's stored owner pointer.
        let owner_id = self.thread_id;
        let owner_ptr = (self as *mut KThread) as usize;
        waiter
            .lock()
            .unwrap()
            .set_waiting_lock_owner_thread_id(Some(owner_id), owner_ptr);

        // If the waiter has higher priority than us, inherit it.
        if waiter_priority < self.priority {
            self.restore_priority();
        }
    }

    /// Public AddWaiter with the full upstream priority-inheritance chain.
    /// Matches upstream `KThread::AddWaiter()` followed by
    /// `KThread::RestorePriority(kernel, this)`.
    pub fn add_waiter_with_process(
        _process_guard: &mut KProcess,
        owner_thread: &Arc<KThreadLock>,
        waiter: &Arc<KThreadLock>,
    ) {
        let (waiter_thread_id, waiter_priority, waiter_address_key, waiter_is_kernel_address_key) = {
            let waiter = waiter.lock().unwrap();
            (
                waiter.get_thread_id(),
                waiter.get_priority(),
                waiter.get_address_key(),
                waiter.get_is_kernel_address_key(),
            )
        };

        let should_restore = {
            let mut owner = owner_thread.lock().unwrap();
            if should_trace_priority_inheritance() {
                log::info!(
                    "PI add_waiter owner_tid={} owner_prio={} owner_base={} waiter_tid={} waiter_prio={} addr=0x{:X} kernel={}",
                    owner.thread_id,
                    owner.priority,
                    owner.base_priority,
                    waiter_thread_id,
                    waiter_priority,
                    waiter_address_key.get(),
                    waiter_is_kernel_address_key,
                );
            }

            owner.add_waiter_impl(
                waiter_thread_id,
                waiter_priority,
                waiter.as_ref().as_ptr() as usize,
                waiter_address_key,
                waiter_is_kernel_address_key,
            );

            let owner_thread_id = owner.thread_id;
            let owner_ptr = (&mut *owner as *mut KThread) as usize;
            waiter
                .lock()
                .unwrap()
                .set_waiting_lock_owner_thread_id(Some(owner_thread_id), owner_ptr);

            waiter_priority < owner.priority
        };

        if should_restore {
            owner_thread.lock().unwrap().restore_priority();
        }
    }

    /// Public RemoveWaiter: removes waiter then may restore priority.
    /// Matches upstream `KThread::RemoveWaiter()` (k_thread.cpp:1072-1081).
    pub fn remove_waiter(
        &mut self,
        waiter_thread_id: u64,
        waiter_priority: i32,
        waiter_is_kernel_address_key: bool,
        waiter_address_key: KProcessAddress,
    ) {
        self.remove_waiter_impl(
            waiter_thread_id,
            waiter_priority,
            waiter_is_kernel_address_key,
            waiter_address_key,
        );

        // If our priority equals the removed waiter's and we've inherited,
        // we may need to drop back.
        if self.priority == waiter_priority && self.priority < self.base_priority {
            self.restore_priority();
        }
    }

    /// Remove the highest priority waiter for a given address key and transfer
    /// lock ownership to it.
    /// Matches upstream `KThread::RemoveWaiterByKey()` (k_thread.cpp:1083-1142).
    pub fn remove_waiter_by_key(
        &mut self,
        address_key: KProcessAddress,
        is_kernel_address_key: bool,
        has_waiters: &mut bool,
    ) -> Option<(u64, i32, usize, Option<LockWithPriorityInheritanceInfo>)> {
        if should_trace_priority_inheritance() {
            log::info!(
                "PI remove_waiter_by_key owner_tid={} prio={} base={} addr=0x{:X} kernel={}",
                self.thread_id,
                self.priority,
                self.base_priority,
                address_key.get(),
                is_kernel_address_key,
            );
        }
        // Find the lock info for this address.
        let lock_idx = self.find_held_lock_index(address_key, is_kernel_address_key)?;

        // Remove the lock info from our held list. Bounds-checked: the hardware
        // timer (do_task) can mutate this owner's `held_lock_info_list`
        // unserialized (its gsc Weak is often dead, so it runs without the
        // scheduler lock), shrinking the list between `find_held_lock_index`
        // and here. Treat a stale index as "lock already gone" rather than
        // aborting the emulator.
        if lock_idx >= self.held_lock_info_list.len() {
            log::error!(
                "remove_waiter_by_key RACE: lock_idx={} >= len={} (owner_tid={} addr=0x{:X})",
                lock_idx,
                self.held_lock_info_list.len(),
                self.thread_id,
                address_key.get(),
            );
            return None;
        }
        let mut lock_info = self.held_lock_info_list.remove(lock_idx);

        // Adjust kernel waiter count.
        if lock_info.get_is_kernel_address_key() {
            self.num_kernel_waiters -= lock_info.get_waiter_count() as i32;
            assert!(self.num_kernel_waiters >= 0);
        }

        // Defensive: if the lock_info is in held_lock_info_list with 0 waiters,
        // it's a race window between add_waiter_impl creating the empty entry and
        // calling add_waiter on it. We've already detached it from the list; just
        // drop it silently and return None (same semantics as find returning None).
        // Triggers only under heavy tracing-induced timing perturbation. The root
        // race (add_waiter_impl not atomic w.r.t. remove_waiter_by_key) is a
        // separate scheduler-locking gap to be fixed later.
        if lock_info.get_waiter_count() == 0 {
            log::warn!(
                "remove_waiter_by_key: lock_info for tid={} addr=0x{:X} had 0 waiters in held list (race?); dropping",
                self.thread_id,
                address_key.get(),
            );
            return None;
        }

        // Remove the highest priority waiter to become the next owner.
        let next_owner_key = lock_info
            .get_highest_priority_waiter()
            .expect("RemoveWaiterByKey: lock has waiters but tree is empty");

        let next_owner_thread_id = next_owner_key.thread_id;
        let next_owner_priority = next_owner_key.priority;
        let next_owner_thread_ptr = next_owner_key.thread_ptr;

        if should_trace_priority_inheritance() {
            log::info!(
                "PI remove_waiter_by_key choose_next owner_tid={} next_tid={} next_prio={} remaining_waiters_before={}",
                self.thread_id,
                next_owner_thread_id,
                next_owner_priority,
                lock_info.get_waiter_count(),
            );
        }

        if lock_info.remove_waiter(next_owner_key.priority, next_owner_key.thread_id) {
            // The new owner was the only waiter — lock info is freed (dropped).
            *has_waiters = false;
            if self.priority == next_owner_priority && self.priority < self.base_priority {
                self.restore_priority();
            }
            return Some((
                next_owner_thread_id,
                next_owner_priority,
                next_owner_thread_ptr,
                None,
            ));
        } else {
            // There are additional waiters — transfer to new owner.
            *has_waiters = true;
        }

        // If our priority matched the next owner's and we've inherited, restore.
        if self.priority == next_owner_priority && self.priority < self.base_priority {
            self.restore_priority();
        }

        Some((
            next_owner_thread_id,
            next_owner_priority,
            next_owner_thread_ptr,
            Some(lock_info),
        ))
    }

    /// Restore inherited priority along the complete lock-owner chain.
    /// Matches upstream `KThread::RestorePriority()` (k_thread.cpp).
    pub fn restore_priority(&mut self) {
        let mut thread_ptr = self as *mut KThread;

        while !thread_ptr.is_null() {
            // SAFETY: callers hold the scheduler lock, which is the ownership
            // contract used by upstream for this intrusive owner-chain walk.
            let thread = unsafe { &mut *thread_ptr };
            let mut new_priority = thread.base_priority;
            for held_lock in &thread.held_lock_info_list {
                if let Some(highest) = held_lock.get_highest_priority_waiter() {
                    new_priority = new_priority.min(highest.priority);
                }
            }

            if new_priority == thread.priority {
                return;
            }

            let old_priority = thread.priority;
            let lock_ref = thread.waiting_lock_info.clone();
            let lock_owner_ptr = lock_ref.as_ref().map_or(std::ptr::null_mut(), |lock_ref| {
                lock_ref.owner_thread_ptr as *mut KThread
            });

            if let Some(lock_ref) = lock_ref.as_ref() {
                if !lock_owner_ptr.is_null() {
                    unsafe { &mut *lock_owner_ptr }.remove_waiter_impl(
                        thread.thread_id,
                        old_priority,
                        lock_ref.is_kernel_address_key,
                        lock_ref.address_key,
                    );
                }
            }

            let waiting_on_condition_variable = matches!(
                thread.get_condition_variable_tree(),
                Some(ConditionVariableTreeState::ConditionVariable)
            );
            let parent_ptr = thread.get_parent_raw_ptr();
            if waiting_on_condition_variable {
                if let Some(parent_ptr) = parent_ptr {
                    unsafe { &mut *parent_ptr }
                        .before_update_condition_variable_priority(thread.thread_id);
                }
            }

            thread.priority = new_priority;
            if should_trace_priority_inheritance() {
                log::info!(
                    "PI restore_priority tid={} old_prio={} new_prio={} base_prio={} held_locks={}",
                    thread.thread_id,
                    old_priority,
                    new_priority,
                    thread.base_priority,
                    thread.held_lock_info_list.len(),
                );
            }

            if waiting_on_condition_variable {
                if let Some(parent_ptr) = parent_ptr {
                    unsafe { &mut *parent_ptr }.after_update_condition_variable_priority(
                        thread.condition_variable_tree_key(),
                    );
                }
            }

            if let Some(lock_ref) = lock_ref.as_ref() {
                if !lock_owner_ptr.is_null() {
                    unsafe { &mut *lock_owner_ptr }.add_waiter_impl(
                        thread.thread_id,
                        new_priority,
                        thread_ptr as usize,
                        lock_ref.address_key,
                        lock_ref.is_kernel_address_key,
                    );
                }
            }

            thread.notify_priority_change(old_priority);
            thread_ptr = lock_owner_ptr;
        }
    }

    /// Collect all waiter thread IDs across all held locks.
    /// Replaces the old `waiter_thread_ids()` that returned the flat Vec.
    pub fn waiter_thread_ids(&self) -> Vec<u64> {
        let mut ids = Vec::new();
        for lock_info in &self.held_lock_info_list {
            for key in lock_info.tree.iter() {
                ids.push(key.thread_id);
            }
        }
        ids
    }

    /// Get waiter thread IDs for a specific address key.
    pub fn waiter_thread_ids_for_address(&self, address_key: KProcessAddress) -> Vec<u64> {
        for lock_info in &self.held_lock_info_list {
            if lock_info.get_address_key() == address_key {
                return lock_info.tree.iter().map(|k| k.thread_id).collect();
            }
        }
        Vec::new()
    }

    /// Legacy compatibility: remove_waiter with a thread_id.
    /// Searches all held locks for the given thread_id.
    pub fn remove_waiter_by_thread_id(&mut self, waiter_thread_id: u64) {
        // NOTE: this is reached from the hardware-timer's `on_timer` path
        // (timeout fires for a still-WAITING thread) which mutates the *owner*
        // thread through a raw `&mut` pointer, relying on the scheduler lock for
        // exclusion. Use bounds-checked access (`get`/`get_mut`) so a concurrent
        // shrink of `held_lock_info_list` cannot turn a stale index into an
        // out-of-bounds panic that aborts the whole emulator. A one-shot
        // diagnostic records the scheduler-lock ownership when the race is
        // observed so the missing serialization can be pinned down.
        let mut i = 0usize;
        while i < self.held_lock_info_list.len() {
            let key = match self.held_lock_info_list.get(i) {
                Some(lock_info) => lock_info
                    .tree
                    .iter()
                    .find(|k| k.thread_id == waiter_thread_id)
                    .copied(),
                None => break,
            };
            let Some(key) = key else {
                i += 1;
                continue;
            };

            if should_trace_priority_inheritance() {
                log::info!(
                    "PI remove_waiter_by_thread_id owner_tid={} owner_prio={} owner_base={} waiter_tid={} waiter_prio={} addr=0x{:X}",
                    self.thread_id,
                    self.priority,
                    self.base_priority,
                    waiter_thread_id,
                    key.priority,
                    self.held_lock_info_list
                        .get(i)
                        .map(|li| li.get_address_key().get())
                        .unwrap_or(0),
                );
            }

            let Some(lock_info) = self.held_lock_info_list.get_mut(i) else {
                // The list shrank underneath us between the find and the
                // mutation — a genuine concurrent access. Log who (if anyone)
                // owns the scheduler lock so the missing serialization can be
                // identified, then bail without panicking.
                let sched_held = super::kernel::scheduler_lock()
                    .map(|l| l.is_locked_by_current_thread())
                    .unwrap_or(false);
                log::error!(
                    "remove_waiter_by_thread_id RACE: held_lock_info_list shrank \
                     (owner_tid={} waiter_tid={} scheduler_lock_held_by_current={})",
                    self.thread_id,
                    waiter_thread_id,
                    sched_held,
                );
                return;
            };
            let is_kernel = lock_info.get_is_kernel_address_key();
            let is_empty = lock_info.remove_waiter(key.priority, key.thread_id);
            if is_kernel {
                self.num_kernel_waiters -= 1;
            }
            if is_empty {
                self.held_lock_info_list.remove(i);
            }
            if self.priority == key.priority && self.priority < self.base_priority {
                self.restore_priority();
            }
            return;
        }
    }

    pub fn get_is_kernel_address_key(&self) -> bool {
        self.is_kernel_address_key
    }

    pub fn set_user_address_key(&mut self, key: KProcessAddress, val: u32) {
        self.address_key = key;
        self.address_key_value = val;
        self.is_kernel_address_key = false;
    }

    pub fn set_kernel_address_key(&mut self, key: KProcessAddress) {
        self.address_key = key;
        self.is_kernel_address_key = true;
    }

    pub fn get_argument(&self) -> usize {
        self.argument
    }

    pub fn get_user_stack_top(&self) -> KProcessAddress {
        self.stack_top
    }

    pub fn get_affinity_mask(&self) -> &KAffinityMask {
        &self.physical_affinity_mask
    }

    pub fn get_native_execution_parameters(&mut self) -> &mut NativeExecutionParameters {
        &mut self.native_execution_parameters
    }

    pub fn is_waiting_on_synchronization(&self) -> bool {
        self.sync_wait_context.is_active()
    }

    pub fn complete_synchronization_wait(&mut self, synced_index: i32, result: u32) {
        self.synced_index = synced_index;
        // Upstream's `ThreadQueueImplForKSynchronizationObjectWait::NotifyAvailable`
        // calls `KThreadQueue::EndWait` (the BASE class, not the panicking
        // `KThreadQueueWithoutEndWait::EndWait`). Sync object wait queues are
        // KThreadQueueWithoutEndWait, so going through the regular `end_wait`
        // path (KThread::end_wait → wait_queue.end_wait) would hit the
        // "should never be called" assertion. We replicate the upstream
        // notify-available flow inline: clear cancellable + base_end_wait.
        let _scheduler_lock = self.lock_scheduler();
        if self.get_state() != ThreadState::WAITING {
            return;
        }
        let Some(wait_queue) = self.wait_queue.clone() else {
            log::error!("complete_synchronization_wait: wait_queue is None while state=Waiting");
            return;
        };
        self.clear_cancellable();
        wait_queue.base_end_wait(self, result);
        self.waiting_lock_info = None;
    }

    // -- Complex methods stubbed --

    fn reset_thread_context32(&mut self, stack_top: u64, entry_point: u64, arg: u64) {
        // Upstream: ctx = {}; ctx.r[0]=arg; ctx.r[15]=entry; ctx.r[13]=sp; ctx.fpcr=0; ctx.fpsr=0.
        // Do not also populate the AArch64 pc/sp fields: an A32-initialized
        // context loaded by an A64 backend must be visibly wrong during review.
        self.thread_context = ThreadContext::default();
        self.thread_context.r[0] = arg;
        self.thread_context.r[15] = entry_point;
        self.thread_context.r[13] = stack_top;
        self.thread_context.fpcr = 0;
        self.thread_context.fpsr = 0;
    }

    fn reset_thread_context64(&mut self, stack_top: u64, entry_point: u64, arg: u64) {
        self.thread_context = ThreadContext::default();
        self.thread_context.r[0] = arg;
        self.thread_context.r[18] = 1;
        self.thread_context.sp = stack_top;
        self.thread_context.pc = entry_point;
        self.thread_context.fpcr = 0;
        self.thread_context.fpsr = 0;
    }

    pub fn initialize_main_thread(
        &mut self,
        entry_point: u64,
        stack_top: u64,
        virt_core: i32,
        tls_address: u64,
        owner: &Arc<ProcessLock>,
        thread_id: u64,
        object_id: u64,
        is_64bit: bool,
    ) {
        self.initialize_main_thread_with_func(
            entry_point,
            stack_top,
            virt_core,
            tls_address,
            owner,
            thread_id,
            object_id,
            is_64bit,
            None,
        );
    }

    /// Initialize as a main thread with an optional host fiber init function.
    /// Upstream: `InitializeThread(thread, {}, {}, {}, IdleThreadPriority, virt_core, {},
    ///           ThreadType::Main, system.GetCpuManager().GetGuestActivateFunc())`
    pub fn initialize_main_thread_with_func(
        &mut self,
        entry_point: u64,
        stack_top: u64,
        virt_core: i32,
        tls_address: u64,
        owner: &Arc<ProcessLock>,
        thread_id: u64,
        object_id: u64,
        is_64bit: bool,
        init_func: Option<Box<dyn FnOnce() + Send>>,
    ) {
        let phys_core = virt_core;
        self.object_id = object_id;
        self.thread_type = ThreadType::Main;
        self.thread_id = thread_id;
        self.priority = IDLE_THREAD_PRIORITY;
        self.base_priority = IDLE_THREAD_PRIORITY;
        self.virtual_ideal_core_id = virt_core;
        self.physical_ideal_core_id = phys_core;
        self.virtual_affinity_mask = 1u64 << virt_core;
        self.physical_affinity_mask
            .set_affinity_mask(1u64 << phys_core);
        self.tls_address = KProcessAddress::new(tls_address);
        self.parent = Some(Arc::downgrade(owner));
        {
            let owner_guard = owner.lock().unwrap();
            self.scheduler = owner_guard.scheduler.clone();
            self.global_scheduler_context = owner_guard
                .global_scheduler_context
                .as_ref()
                .map(Arc::downgrade);
            self.process_schedule_count = Some(Arc::clone(&owner_guard.schedule_count));
        }
        self.stack_top = KProcessAddress::new(stack_top);
        self.argument = 0;
        self.core_id = phys_core;
        self.current_core_id = phys_core;
        self.thread_state
            .store(ThreadState::RUNNABLE.bits(), Ordering::Relaxed);
        self.suspend_allowed_flags = ThreadState::SUSPEND_FLAG_MASK.bits() as u32;
        self.suspend_request_flags = 0;
        self.wait_result = RESULT_NO_SYNCHRONIZATION_OBJECT.get_inner_value();
        self.schedule_count = -1;
        self.initialized = true;
        self.sync_wait_context.clear();
        self.stack_parameters.disable_count = 1;
        self.stack_parameters.is_in_exception_handler = true;

        if is_64bit {
            self.reset_thread_context64(stack_top, entry_point, 0);
        } else {
            self.reset_thread_context32(stack_top, entry_point, 0);
        }
        // Upstream does NOT set thread_context.tpidr here.
        // The TLS address is stored in m_tls_address and passed to the JIT
        // via SetTpidrroEl0 (CP15 URO) during LoadContext, not via ctx.tpidr (UPRW).

        // Initialize emulation parameters.
        // Upstream: thread->m_host_context = std::make_shared<Common::Fiber>(std::move(init_func));
        self.initialize_host_context(init_func);
    }

    /// Initialize as a kernel main thread (no process owner).
    ///
    /// Upstream: `KThread::InitializeMainThread(system, thread, virt_core)` (k_thread.cpp:279-282).
    /// Calls `InitializeThread(thread, {}, {}, {}, IdleThreadPriority, virt_core, {},
    ///         ThreadType::Main, system.GetCpuManager().GetGuestActivateFunc())`.
    ///
    /// The main thread is used by the scheduler as the initial current_thread for each core.
    /// Its host fiber context is set to the guest activate function from CpuManager.
    pub fn initialize_kernel_main_thread(
        &mut self,
        virt_core: i32,
        thread_id: u64,
        object_id: u64,
        init_func: Option<Box<dyn FnOnce() + Send>>,
    ) {
        let mut stack_parameters = StackParameters::default();
        stack_parameters.disable_count = 1;
        stack_parameters.is_in_exception_handler = true;

        self.initialize_ownerless_kernel_thread(
            virt_core,
            thread_id,
            object_id,
            ThreadType::Main,
            IDLE_THREAD_PRIORITY,
            ThreadState::RUNNABLE,
            stack_parameters,
            init_func,
        );
    }

    /// Initialize as a high-priority kernel thread (no process owner).
    ///
    /// Upstream: `KThread::InitializeHighPriorityThread(system, thread, func, arg, virt_core)`
    /// (k_thread.cpp:289-294).
    /// Calls `InitializeThread(thread, func, arg, {}, {}, virt_core, nullptr,
    ///         ThreadType::HighPriority, system.GetCpuManager().GetShutdownThreadStartFunc())`.
    pub fn initialize_high_priority_thread(
        &mut self,
        virt_core: i32,
        thread_id: u64,
        object_id: u64,
        init_func: Option<Box<dyn FnOnce() + Send>>,
    ) {
        self.initialize_ownerless_kernel_thread(
            virt_core,
            thread_id,
            object_id,
            ThreadType::HighPriority,
            SVC_HIGHEST_THREAD_PRIORITY,
            ThreadState::INITIALIZED,
            StackParameters::default(),
            init_func,
        );
    }

    /// Initialize as a kernel idle thread (no process owner).
    ///
    /// Upstream: `KThread::InitializeIdleThread(system, thread, virt_core)` (k_thread.cpp:284-287).
    /// Calls `InitializeThread(thread, {}, {}, {}, IdleThreadPriority, virt_core, {},
    ///         ThreadType::Main, system.GetCpuManager().GetIdleThreadStartFunc())`.
    ///
    /// Note: upstream idle threads also use `ThreadType::Main`, not a separate type.
    pub fn initialize_kernel_idle_thread(
        &mut self,
        virt_core: i32,
        thread_id: u64,
        object_id: u64,
        init_func: Option<Box<dyn FnOnce() + Send>>,
    ) {
        let mut stack_parameters = StackParameters::default();
        stack_parameters.disable_count = 1;
        stack_parameters.is_in_exception_handler = true;

        self.initialize_ownerless_kernel_thread(
            virt_core,
            thread_id,
            object_id,
            ThreadType::Main,
            IDLE_THREAD_PRIORITY,
            ThreadState::RUNNABLE,
            stack_parameters,
            init_func,
        );
    }

    pub fn initialize_user_thread(
        &mut self,
        entry_point: u64,
        arg: u64,
        stack_top: u64,
        prio: i32,
        virt_core: i32,
        owner: &Arc<ProcessLock>,
        thread_id: u64,
        object_id: u64,
        is_64bit: bool,
    ) -> u32 {
        self.initialize_user_thread_with_init_func(
            entry_point,
            arg,
            stack_top,
            prio,
            virt_core,
            owner,
            thread_id,
            object_id,
            is_64bit,
            None,
        )
    }

    /// Initialize as a user thread with an optional host fiber init function.
    /// Upstream: `InitializeUserThread(system, thread, func, arg, user_stack_top, prio,
    ///           virt_core, owner)` passes `system.GetCpuManager().GetGuestThreadFunc()`.
    pub fn initialize_user_thread_with_init_func(
        &mut self,
        entry_point: u64,
        arg: u64,
        stack_top: u64,
        prio: i32,
        virt_core: i32,
        owner: &Arc<ProcessLock>,
        thread_id: u64,
        object_id: u64,
        is_64bit: bool,
        init_func: Option<Box<dyn FnOnce() + Send>>,
    ) -> u32 {
        let tls_address = {
            let mut process = owner.lock().unwrap();
            match process.create_thread_local_region() {
                Some(address) => address,
                None => return RESULT_OUT_OF_RESOURCE.get_inner_value(),
            }
        };
        let owner_weak = Arc::downgrade(owner);
        let (scheduler, gsc, sched_count) = {
            let proc = owner.lock().unwrap();
            (
                proc.scheduler.clone(),
                proc.global_scheduler_context
                    .as_ref()
                    .map(|g| Arc::downgrade(g)),
                Some(Arc::clone(&proc.schedule_count)),
            )
        };

        self.initialize_user_thread_with_tls(
            entry_point,
            arg,
            stack_top,
            prio,
            virt_core,
            owner_weak,
            scheduler,
            gsc,
            sched_count,
            tls_address,
            thread_id,
            object_id,
            is_64bit,
            init_func,
        )
    }

    pub fn initialize_user_thread_with_tls(
        &mut self,
        entry_point: u64,
        arg: u64,
        stack_top: u64,
        prio: i32,
        virt_core: i32,
        owner: Weak<ProcessLock>,
        scheduler: Option<Weak<Mutex<KScheduler>>>,
        global_scheduler_context: Option<
            Weak<Mutex<super::global_scheduler_context::GlobalSchedulerContext>>,
        >,
        process_schedule_count: Option<Arc<std::sync::atomic::AtomicI64>>,
        tls_address: KProcessAddress,
        thread_id: u64,
        object_id: u64,
        is_64bit: bool,
        init_func: Option<Box<dyn FnOnce() + Send>>,
    ) -> u32 {
        let phys_core = virt_core;
        self.object_id = object_id;
        self.thread_type = ThreadType::User;
        self.thread_id = thread_id;
        self.priority = prio;
        self.base_priority = prio;
        self.virtual_ideal_core_id = virt_core;
        self.physical_ideal_core_id = phys_core;
        self.virtual_affinity_mask = 1u64 << virt_core;
        self.physical_affinity_mask
            .set_affinity_mask(1u64 << phys_core);
        self.thread_state
            .store(ThreadState::INITIALIZED.bits(), Ordering::Relaxed);
        self.suspend_allowed_flags = ThreadState::SUSPEND_FLAG_MASK.bits() as u32;
        self.suspend_request_flags = 0;
        if let Some(parent) = owner.upgrade() {
            if let Some(memory) = parent.lock().unwrap().get_memory() {
                let zero_tls = [0u8; THREAD_LOCAL_REGION_SIZE];
                memory
                    .lock()
                    .unwrap()
                    .write_block(tls_address.get(), &zero_tls);
            }
        }
        self.tls_address = tls_address;
        self.parent = Some(owner);
        self.scheduler = scheduler;
        self.global_scheduler_context = global_scheduler_context;
        self.scheduler_lock_ptr = self
            .global_scheduler_context
            .as_ref()
            .and_then(Weak::upgrade)
            .map(|gsc| {
                let guard = gsc.lock().unwrap();
                std::ptr::addr_of!(guard.m_scheduler_lock) as usize
            })
            .unwrap_or(0);
        self.process_schedule_count = process_schedule_count;
        self.signaled = false;
        let (termination_lock, _) = &*self.termination_wait_pair;
        *termination_lock.lock().unwrap() = false;
        self.termination_requested.store(false, Ordering::Relaxed);
        self.wait_cancelled = false;
        self.cancellable = false;
        self.core_id = phys_core;
        self.current_core_id = phys_core;
        self.wait_result = RESULT_NO_SYNCHRONIZATION_OBJECT.get_inner_value();
        self.schedule_count = -1;
        self.last_scheduled_tick = 0;
        self.num_kernel_waiters = 0;
        self.resource_limit_release_hint = false;
        self.sync_wait_context.clear();
        self.stack_top = KProcessAddress::new(stack_top);
        self.argument = arg as usize;
        self.stack_parameters = StackParameters::default();
        self.stack_parameters.disable_count = 1;
        self.stack_parameters.is_in_exception_handler = true;
        self.initialized = true;

        if is_64bit {
            self.reset_thread_context64(stack_top, entry_point, arg);
        } else {
            self.reset_thread_context32(stack_top, entry_point, arg);
        }
        // Upstream does NOT set thread_context.tpidr here.
        // The TLS address is passed via SetTpidrroEl0 (CP15 URO) during LoadContext.

        // Initialize emulation parameters.
        // Upstream: thread->m_host_context = std::make_shared<Common::Fiber>(std::move(init_func));
        self.initialize_host_context(init_func);

        RESULT_SUCCESS.get_inner_value()
    }

    /// Initialize as a dummy host thread.
    ///
    /// Port of upstream `KThread::InitializeDummyThread` (k_thread.cpp:269-276).
    /// Used for non-guest host threads that still need a current `KThread`
    /// owner in kernel code paths.
    pub fn initialize_dummy_thread(
        &mut self,
        owner: Option<&Arc<ProcessLock>>,
        thread_id: u64,
        object_id: u64,
    ) -> u32 {
        let core = (NUM_CPU_CORES as i32) - 1;

        self.object_id = object_id;
        self.thread_type = ThreadType::Dummy;
        self.thread_id = thread_id;
        self.priority = DUMMY_THREAD_PRIORITY;
        self.base_priority = DUMMY_THREAD_PRIORITY;
        self.virtual_ideal_core_id = core;
        self.physical_ideal_core_id = core;
        self.virtual_affinity_mask = 1u64 << core;
        self.physical_affinity_mask.set_affinity_mask(1u64 << core);
        self.thread_state
            .store(ThreadState::RUNNABLE.bits(), Ordering::Relaxed);
        self.suspend_allowed_flags = ThreadState::SUSPEND_FLAG_MASK.bits() as u32;
        self.suspend_request_flags = 0;
        self.parent = owner.map(Arc::downgrade);
        self.scheduler = owner.and_then(|process| process.lock().unwrap().scheduler.clone());
        self.global_scheduler_context = owner.and_then(|process| {
            process
                .lock()
                .unwrap()
                .global_scheduler_context
                .as_ref()
                .map(Arc::downgrade)
        }).or_else(|| {
            // A host dummy does not need a process or a per-core scheduler,
            // but its WAITING/RUNNABLE transitions still belong to the kernel's
            // global scheduler (upstream KThread always has KernelCore access).
            super::kernel::get_kernel_ref()
                .and_then(|kernel| kernel.global_scheduler_context())
                .map(Arc::downgrade)
        });
        // Upstream obtains the scheduler lock directly from KernelCore. A
        // parentless host identity can be created inside a scheduler callback
        // which already holds the GSC mutex; do not reacquire it here.
        self.scheduler_lock_ptr = if owner.is_none() {
            super::kernel::scheduler_lock()
                .map(|lock| lock as *const _ as usize)
                .unwrap_or(0)
        } else {
            self.global_scheduler_context
                .as_ref()
                .and_then(Weak::upgrade)
                .map(|gsc| {
                    let guard = gsc.lock().unwrap();
                    std::ptr::addr_of!(guard.m_scheduler_lock) as usize
                })
                .unwrap_or(0)
        };
        self.process_schedule_count =
            owner.map(|process| Arc::clone(&process.lock().unwrap().schedule_count));
        self.core_id = core;
        self.current_core_id = core;
        self.wait_result = RESULT_NO_SYNCHRONIZATION_OBJECT.get_inner_value();
        self.schedule_count = -1;
        self.initialized = true;
        self.stack_parameters = StackParameters::default();
        self.stack_parameters.disable_count = 0;
        self.stack_parameters.is_in_exception_handler = true;
        self.reset_thread_context64(0, 0, 0);

        RESULT_SUCCESS.get_inner_value()
    }

    /// Initialize as a service thread.
    ///
    /// Port of upstream `KThread::InitializeServiceThread` (k_thread.cpp:304-321).
    /// Creates a HighPriority thread with a host fiber context that runs the
    /// given function. The fiber wraps the function with OnThreadStart/ExitThread
    /// matching upstream's lambda in InitializeServiceThread.
    ///
    /// Used by `KernelCore::run_on_guest_core_process()` to create service threads
    /// that the scheduler runs on guest cores.
    pub fn initialize_service_thread(
        &mut self,
        system: SystemRef,
        thread_ref: &Arc<KThreadLock>,
        func: Box<dyn FnOnce() + Send>,
        priority: i32,
        core: i32,
        owner: &Arc<ProcessLock>,
        thread_id: u64,
        object_id: u64,
    ) {
        let owner_weak = Arc::downgrade(owner);
        let scheduler = owner.lock().unwrap().scheduler.clone();
        if let Some(gsc) = owner
            .lock()
            .unwrap()
            .global_scheduler_context
            .as_ref()
            .cloned()
        {
            // Use add_thread_with_id to avoid re-locking the thread mutex
            // (we're called under thread.lock() from run_on_guest_core_process).
            gsc.lock()
                .unwrap()
                .add_thread_with_id(thread_id, Arc::clone(thread_ref));
        }

        self.object_id = object_id;
        self.thread_type = ThreadType::HighPriority;
        self.thread_id = thread_id;
        self.priority = priority;
        self.base_priority = priority;
        self.virtual_ideal_core_id = core;
        self.physical_ideal_core_id = core;
        self.virtual_affinity_mask = 1u64 << core;
        self.physical_affinity_mask.set_affinity_mask(1u64 << core);
        self.thread_state
            .store(ThreadState::INITIALIZED.bits(), Ordering::Relaxed);
        self.suspend_allowed_flags = ThreadState::SUSPEND_FLAG_MASK.bits() as u32;
        self.suspend_request_flags = 0;
        self.parent = Some(owner_weak);
        self.scheduler = scheduler;
        // Extract process data WITHOUT holding the process lock during GSC lock,
        // to avoid AB/BA deadlock (process → GSC vs GSC → process).
        let (gsc_weak, scheduler_lock_ptr, schedule_count) = {
            let proc = owner.lock().unwrap();
            let gsc_ref = proc.global_scheduler_context.as_ref().cloned();
            let sc = Arc::clone(&proc.schedule_count);
            drop(proc); // Release process lock BEFORE locking GSC
            let ptr = gsc_ref
                .as_ref()
                .map(|gsc| {
                    let guard = gsc.lock().unwrap();
                    std::ptr::addr_of!(guard.m_scheduler_lock) as usize
                })
                .unwrap_or(0);
            let weak = gsc_ref.as_ref().map(Arc::downgrade);
            (weak, ptr, sc)
        };
        self.global_scheduler_context = gsc_weak;
        self.scheduler_lock_ptr = scheduler_lock_ptr;
        self.process_schedule_count = Some(schedule_count);
        self.core_id = core;
        self.current_core_id = core;
        self.wait_result = RESULT_NO_SYNCHRONIZATION_OBJECT.get_inner_value();
        self.schedule_count = -1;
        self.initialized = true;
        self.stack_parameters.disable_count = 1;
        self.stack_parameters.is_in_exception_handler = true;

        // Create host fiber context from the service function.
        // Upstream wraps with: OnThreadStart() -> func() -> ExitThread().
        let wrapped_func: Box<dyn FnOnce() + Send> = Box::new(move || {
            if !system.is_null() {
                if let Some(kernel) = system.get().kernel() {
                    if let Some(scheduler) = kernel.current_scheduler() {
                        if let Some(current_thread) = system.get().current_thread() {
                            scheduler.lock().unwrap().on_thread_start(&current_thread);
                        }
                    }
                }
            }

            func();

            if !system.is_null() {
                crate::hle::kernel::svc::svc_thread::exit_thread(system.get());
            }
        });
        self.initialize_host_context(Some(wrapped_func));

        log::trace!(
            "KThread::initialize_service_thread: thread_id={} priority={} core={}",
            thread_id,
            priority,
            core
        );
    }

    /// Refresh cached process-owned scheduler links from the owning process.
    ///
    /// Upstream resolves this state through kernel owners rather than cached weak
    /// references. The Rust port therefore needs an explicit backfill path when a
    /// process acquires its scheduler/GSC after a thread already exists.
    pub fn inherit_process_scheduler_state(&mut self, owner: &super::k_process::KProcess) {
        self.scheduler = owner.scheduler.clone();
        self.global_scheduler_context = owner.global_scheduler_context.as_ref().map(Arc::downgrade);
        self.scheduler_lock_ptr = owner
            .global_scheduler_context
            .as_ref()
            .map(|gsc| {
                let guard = gsc.lock().unwrap();
                std::ptr::addr_of!(guard.m_scheduler_lock) as usize
            })
            .unwrap_or(0);
        self.process_schedule_count = Some(Arc::clone(&owner.schedule_count));
    }

    pub fn clone_fpu_status_from(&mut self, current: &KThread) {
        self.thread_context.fpcr = current.thread_context.fpcr;
        self.thread_context.fpsr = current.thread_context.fpsr;
    }

    /// Set the thread's base priority with local priority-inheritance handling.
    ///
    /// Runtime paths that have process ownership should use
    /// `set_base_priority_with_process`, which ports upstream
    /// `KThread::SetBasePriority()` and walks the full lock-owner chain.
    /// This method is kept for tests and early setup paths where no registered
    /// process thread object exists yet.
    pub fn set_base_priority(&mut self, value: i32) {
        let old_priority = self.priority;
        let mut new_priority = value;
        for lock_info in &self.held_lock_info_list {
            if let Some(highest) = lock_info.get_highest_priority_waiter() {
                new_priority = new_priority.min(highest.priority);
            }
        }
        let waiting_on_condition_variable = matches!(
            self.get_condition_variable_tree(),
            Some(ConditionVariableTreeState::ConditionVariable)
        );
        let parent = if waiting_on_condition_variable {
            self.parent.as_ref().and_then(Weak::upgrade)
        } else {
            None
        };

        if old_priority != new_priority {
            if let Some(parent) = parent.as_ref() {
                parent
                    .lock()
                    .unwrap()
                    .before_update_condition_variable_priority(self.thread_id);
            }
        }

        self.base_priority = value;
        self.priority = new_priority;
        if old_priority != new_priority {
            let updated_thread_key = self.condition_variable_tree_key();
            if let Some(parent) = parent.as_ref() {
                parent
                    .lock()
                    .unwrap()
                    .after_update_condition_variable_priority(updated_thread_key);
            }
            self.notify_priority_change(old_priority);
        }
    }

    /// Process-owned `KThread::SetBasePriority()` port.
    ///
    /// Upstream takes `KScopedSchedulerLock`, stores `m_base_priority`, then
    /// calls `RestorePriority(m_kernel, this)`.
    pub fn set_base_priority_with_process(
        process_guard: &mut KProcess,
        thread_id: u64,
        value: i32,
    ) {
        let Some(thread) = process_guard.get_thread_by_thread_id(thread_id) else {
            return;
        };
        let mut thread = thread.lock().unwrap();
        thread.base_priority = value;
        thread.restore_priority();
    }

    /// Arc-backed entry used by runtime owners that must not hold the thread
    /// mutex across scheduler-lock release.
    ///
    /// Upstream `KThread::Run()` operates on a raw `KThread*` and does not have
    /// an outer object mutex. Keep the Rust runtime path as close as possible by
    /// taking the thread lock only for the actual state mutation, then dropping
    /// it before `KScopedSchedulerLock` unwinds.
    pub fn run_thread(thread: &Arc<KThreadLock>) -> u32 {
        loop {
            let scheduler_lock_ptr = {
                let thread = thread.lock().unwrap();
                thread.scheduler_lock_ptr
            };
            let _lk = if scheduler_lock_ptr != 0 {
                Some(KScopedSchedulerLock::new(unsafe {
                    &*(scheduler_lock_ptr as *const super::k_scheduler_lock::KAbstractSchedulerLock)
                }))
            } else {
                None
            };

            let current_termination_requested =
                super::kernel::with_current_thread_fast_mut(|t| t.is_termination_requested())
                    .unwrap_or(false);
            if current_termination_requested {
                return RESULT_TERMINATION_REQUESTED.get_inner_value();
            }

            {
                let mut thread = thread.lock().unwrap();

                if thread.termination_requested.load(Ordering::Relaxed) {
                    return RESULT_TERMINATION_REQUESTED.get_inner_value();
                }

                if thread.get_state() != ThreadState::INITIALIZED {
                    return RESULT_INVALID_STATE.get_inner_value();
                }

                let current_suspended =
                    super::kernel::with_current_thread_fast_mut(|t| t.is_suspended())
                        .unwrap_or(false);
                if current_suspended {
                    super::kernel::with_current_thread_fast_mut(|t| t.update_state());
                    continue;
                }

                if thread.is_user_thread() && thread.is_suspended() {
                    thread.update_state();
                }

                if let Some(parent) = thread.parent.as_ref().and_then(Weak::upgrade) {
                    if let Ok(process) = parent.try_lock() {
                        process.increment_running_thread_count();
                    }
                }

                thread.set_state(ThreadState::RUNNABLE);
            }

            return RESULT_SUCCESS.get_inner_value();
        }
    }

    /// Start the termination sequence.
    /// Matches upstream `KThread::StartTermination()` (k_thread.cpp:402-426).
    ///
    /// Upstream requires KScheduler::IsSchedulerLockedByCurrentThread().
    fn start_termination(&mut self) {
        // Release user exception and unpin, if relevant.
        if let Some(parent) = self.parent.as_ref().and_then(Weak::upgrade) {
            let mut process = parent.lock().unwrap();

            // Upstream: m_parent->ReleaseUserException(this)
            // Releases the exception thread if this thread holds it,
            // then wakes the next waiter via RemoveKernelWaiterByKey.
            if process.exception_thread_id == Some(self.thread_id) {
                process.exception_thread_id = None;
                // Wake the next kernel waiter that was blocked on the exception thread.
                // Upstream uses RemoveKernelWaiterByKey with the exception_thread
                // address | 1 as the key. We wake the first user waiter as a
                // simplified equivalent (kernel waiters are tracked only by count).
                if self.num_kernel_waiters > 0 {
                    self.num_kernel_waiters -= 1;
                    // The waiter would be woken via EndWait — in our model,
                    // the process-level sync will handle notification via
                    // FinishTermination's NotifyAvailable.
                }
            }

            // Upstream: if (m_parent->GetPinnedThread(GetCurrentCoreId(m_kernel)) == this)
            //               m_parent->UnpinCurrentThread();
            for core_id in 0..NUM_CPU_CORES as usize {
                if process.pinned_threads[core_id] == Some(self.thread_id) {
                    process.pinned_threads[core_id] = None;
                }
            }

            // Set state to terminated.
            self.set_state(ThreadState::TERMINATED);

            // Clear the thread's status as running in parent.
            // Upstream: m_parent->ClearRunningThread(this)
            process.clear_running_thread(self.thread_id);
        } else {
            // No parent — just set state.
            self.set_state(ThreadState::TERMINATED);
        }

        // Clear previous thread in KScheduler.
        // Upstream: KScheduler::ClearPreviousThread(m_kernel, this)
        // Upstream: KScheduler::ClearPreviousThread(m_kernel, this).

        // Register terminated DPC flag.
        self.register_dpc(DpcFlag::TERMINATED);
    }

    /// Finish the termination (called from worker task or inline).
    /// Matches upstream `KThread::FinishTermination()` (k_thread.cpp:428-448).
    ///
    /// Upstream spins until the thread is not executing on any core, then
    /// acquires the scheduler lock, signals the synchronization object,
    /// and closes the thread reference.
    pub fn finish_termination(&mut self) {
        // Upstream: Ensure the thread is not executing on any core.
        // Upstream spin-waits checking each core's scheduler current thread.
        // We check via the process's scheduler references if available.
        if self.parent.is_some() {
            // Spin-wait: upstream does a tight loop per core checking
            // scheduler.GetSchedulerCurrentThread() != this.
            // In our model, the fiber-based context switching ensures
            // that by the time the worker task runs, the thread has
            // already been unloaded from its core. The spin is a safety check.
            // We yield briefly to let any in-progress context switch complete.
            std::thread::yield_now();
        }

        // Upstream: KScopedSchedulerLock sl{m_kernel};
        // The scheduler lock ensures atomicity with thread state changes.
        // The caller (do_worker_task_impl or exit) should acquire this.

        // Signal.
        // Upstream: m_signaled = true; KSynchronizationObject::NotifyAvailable();
        self.signaled = true;
        {
            let (lock, cv) = &*self.termination_wait_pair;
            let mut termination_completed = lock.lock().unwrap();
            *termination_completed = true;
            cv.notify_all();
        }

        // Notify any waiters (matches KSynchronizationObject::NotifyAvailable).
        // Upstream: scheduler lock serializes; waiter nodes live on waiters'
        // stacks. Walk the thread's sync_object list and wake each waiter.
        unsafe {
            k_synchronization_object::notify_waiters_on_state(
                &self.sync_object,
                self.object_id,
                RESULT_SUCCESS.get_inner_value(),
            );
        }

        // Upstream: this->Close() — decrements reference count.
        // Reference counting is not yet implemented; the Arc<KThreadLock>
        // will be dropped when all references are released.
    }

    /// Exit the thread (called by the thread itself).
    /// Matches upstream `KThread::Exit()` (k_thread.cpp:1179-1208).
    ///
    /// Upstream flow:
    /// 1. Release resource limit hint (ThreadCountMax, 0, 1)
    /// 2. Decrement parent's running thread count
    /// 3. Under scheduler lock: disallow suspension, UpdateState, StartTermination
    /// 4. Register with KWorkerTaskManager::WorkerType::Exit
    /// 5. UNREACHABLE — the thread never returns from Exit()
    ///
    /// Our port queues `DoWorkerTaskImpl()` on KWorkerTaskManager like upstream,
    /// but still returns normally because guest thread exit is cooperative here.
    pub fn exit(&mut self) {
        // Release resource hint and decrement running count from parent.
        if let Some(parent) = self.parent.as_ref().and_then(Weak::upgrade) {
            let mut process = parent.lock().unwrap();
            // Upstream: m_parent->GetResourceLimit()->Release(ThreadCountMax, 0, 1)
            if let Some(ref rl) = process.resource_limit {
                rl.release_with_hint(
                    super::k_resource_limit::LimitableResource::ThreadCountMax,
                    0,
                    1,
                );
            }
            self.resource_limit_release_hint = true;
            process.decrement_running_thread_count();
        }

        // Perform termination under the scheduler lock.
        //
        // Upstream `KThread::Exit()` wraps UpdateState + StartTermination + the
        // `KWorkerTaskManager::AddTask(Exit)` in a single `KScopedSchedulerLock`.
        // The recursive scheduler lock MUST be held across the whole block: the
        // cooperative fiber scheduler only performs a context switch when the
        // outermost scheduler lock is released. Without this outer lock,
        // `start_termination()`'s inner `set_state(TERMINATED)` is the outermost
        // holder, so its release switches the fiber away from this (now
        // non-runnable) thread *before* the FinishTermination worker task is
        // queued. The task is then never registered, the thread's
        // synchronization object is never signaled, and any thread joining it
        // (`svcWaitSynchronization` on the thread handle) blocks forever. Holding
        // the lock here defers the switch until after the task is queued.
        {
            let scheduler_lock = super::kernel::scheduler_lock();
            let _scheduler_guard = scheduler_lock
                .as_ref()
                .map(|lock| super::k_scheduler_lock::KScopedSchedulerLock::new(lock));

            // Disallow all suspension.
            self.suspend_allowed_flags = 0;
            self.update_state();

            // Disallow all suspension (upstream sets this twice).
            self.suspend_allowed_flags = 0;

            // Start termination.
            self.start_termination();

            let worker_thread = self
                .self_reference
                .as_ref()
                .and_then(Weak::upgrade)
                .expect("KThread::exit requires a registered thread object");
            KWorkerTaskManager::add_task_static(
                0,
                WorkerType::Exit,
                Box::new(move || {
                    worker_thread.lock().unwrap().do_worker_task_impl();
                }),
            );
        }

        // Upstream: UNREACHABLE_MSG("KThread::Exit() would return").
        // The cooperative Rust execution bridge returns to its caller so
        // `PhysicalCore` can yield to the scheduler after SVC ExitThread.
        if std::env::var_os("RUZU_TRACE_THREAD_EXIT_RETURN").is_some() {
            log::error!(
                "KThread::Exit() returned — upstream marks this as unreachable tid={} object_id=0x{:X} core={} prio={}",
                self.thread_id,
                self.object_id,
                self.core_id,
                self.priority,
            );
        }
    }

    /// Terminate the thread (called by another thread).
    /// Matches upstream `KThread::Terminate()` (k_thread.cpp:1210-1223).
    ///
    /// Upstream flow:
    /// 1. ASSERT(this != GetCurrentThreadPointer(m_kernel))
    /// 2. RequestTerminate() — if already Terminated, succeed immediately
    /// 3. If not yet terminated: KSynchronizationObject::Wait(kernel, &index,
    ///    &[this], 1, WaitInfinite) — blocks until the thread signals
    ///
    /// Our `&mut self` variant cannot block safely because callers commonly
    /// hold the thread mutex while invoking it. Use `terminate_thread()` when
    /// a caller owns `Arc<KThreadLock>` and needs upstream-style waiting.
    pub fn terminate(&mut self) -> u32 {
        // Request termination.
        let new_state = self.request_terminate();
        if new_state == ThreadState::TERMINATED {
            return RESULT_SUCCESS.get_inner_value();
        }

        // Upstream: Wait on this thread as a synchronization object until it
        // signals (i.e., until FinishTermination sets m_signaled = true).
        //   s32 index;
        //   KSynchronizationObject* objects[] = {this};
        //   R_TRY(KSynchronizationObject::Wait(m_kernel, &index, objects, 1,
        //                                      Svc::WaitInfinite));
        //
        // Wait using the termination condvar. This blocks until
        // FinishTermination() sets the completion flag.
        let wait_pair = self.termination_wait_pair.clone();
        let (lock, cv) = &*wait_pair;
        let mut completed = lock.lock().unwrap();
        while !*completed {
            completed = cv.wait(completed).unwrap();
        }

        RESULT_SUCCESS.get_inner_value()
    }

    /// Rust-side helper for upstream-style `KThread::Terminate()` semantics.
    ///
    /// This variant can safely wait for `FinishTermination()` because it drops
    /// the thread mutex before blocking on the termination condition variable.
    pub fn terminate_thread(thread: &Arc<KThreadLock>) -> u32 {
        let wait_pair = {
            let mut guard = thread.lock().unwrap();
            let new_state = guard.request_terminate();
            if new_state == ThreadState::TERMINATED || guard.is_signaled() {
                return RESULT_SUCCESS.get_inner_value();
            }
            guard.termination_wait_pair.clone()
        };

        let (lock, cv) = &*wait_pair;
        let mut termination_completed = lock.lock().unwrap();
        while !*termination_completed {
            termination_completed = cv.wait(termination_completed).unwrap();
        }

        RESULT_SUCCESS.get_inner_value()
    }

    /// Request termination of the thread.
    /// Matches upstream `KThread::RequestTerminate()`.
    pub fn request_terminate(&mut self) -> ThreadState {
        // Atomic CAS: only proceed if this is the first request.
        let expected = false;
        let first_request = self
            .termination_requested
            .compare_exchange(expected, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();

        if first_request {
            // Fast path: if INITIALIZED, directly terminate.
            if self.get_state() == ThreadState::INITIALIZED {
                self.thread_state
                    .store(ThreadState::TERMINATED.bits(), Ordering::Relaxed);
                return ThreadState::TERMINATED;
            }

            // Register terminating DPC.
            self.register_dpc(DpcFlag::TERMINATING);

            // Unpin if pinned.
            if self.stack_parameters.is_pinned {
                // Upstream: self.GetOwnerProcess().UnpinThread(self)
                self.stack_parameters.is_pinned = false;
            }

            // Clear suspension.
            if self.is_suspended() {
                self.suspend_allowed_flags = 0;
                self.update_state();
            }

            // Raise priority to terminating priority.
            self.increase_base_priority(TERMINATING_THREAD_PRIORITY);

            // If RUNNABLE, request an interrupt-driven reschedule like upstream
            // sending a termination IPI to the candidate cores.
            if self.get_state() == ThreadState::RUNNABLE {
                if let Some(scheduler) = self.scheduler.as_ref().and_then(Weak::upgrade) {
                    scheduler.lock().unwrap().request_schedule_on_interrupt();
                } else {
                    self.request_schedule();
                }
            }

            // If WAITING, cancel the wait.
            if self.get_state() == ThreadState::WAITING {
                if let Some(wq) = self.wait_queue.clone() {
                    wq.cancel_wait(self, RESULT_TERMINATION_REQUESTED.get_inner_value(), true);
                    self.finalize_wait_transition();
                }
            }
        }

        self.get_state()
    }

    /// Increase base priority (only if the new priority is higher).
    /// Matches upstream `KThread::IncreaseBasePriority`.
    pub fn increase_base_priority(&mut self, priority: i32) {
        if self.base_priority > priority {
            self.base_priority = priority;
            // Upstream: RestorePriority(kernel, this)
            self.restore_priority();
        }
    }

    /// Request suspend of the given type.
    /// Matches upstream `KThread::RequestSuspend()`.
    pub fn request_suspend(&mut self, suspend_type: SuspendType) {
        let bit = 1u32 << (ThreadState::SUSPEND_SHIFT as u32 + suspend_type as u32);
        self.suspend_request_flags |= bit;
        self.try_suspend();
    }

    /// Resume from the given suspend type.
    /// Matches upstream `KThread::Resume()`.
    pub fn resume(&mut self, suspend_type: SuspendType) {
        let bit = 1u32 << (ThreadState::SUSPEND_SHIFT as u32 + suspend_type as u32);
        self.suspend_request_flags &= !bit;
        self.update_state();
    }

    /// Try to suspend the thread.
    /// Matches upstream `KThread::TrySuspend()`.
    pub fn try_suspend(&mut self) {
        if !self.is_suspend_requested() {
            return;
        }
        if self.get_num_kernel_waiters() > 0 {
            return;
        }
        self.update_state();
    }

    /// Update the thread state.
    /// Matches upstream `KThread::UpdateState()`.
    pub fn update_state(&mut self) {
        let old_state = self.get_raw_state();
        let base_state = old_state & ThreadState::MASK;
        let suspend_bits = ThreadState::from_bits_truncate(self.get_suspend_flags() as u16);
        let new_state = suspend_bits | base_state;
        if new_state != old_state
            && self.get_suspend_flags() != 0
            && base_state == ThreadState::WAITING
        {
            self.set_wait_reason_for_debugging(ThreadWaitReasonForDebugging::Suspended);
        }
        self.commit_state_transition(old_state, new_state, false);
    }

    /// Continue the thread.
    /// Matches upstream `KThread::ContinueThread()`.
    pub fn continue_thread(&mut self) {
        let old_state = self.get_raw_state();
        let continued_state = old_state & ThreadState::MASK;
        if continued_state != ThreadState::WAITING {
            self.set_wait_reason_for_debugging(ThreadWaitReasonForDebugging::None);
        }
        self.commit_state_transition(old_state, continued_state, false);
    }

    /// Set the thread state.
    /// Matches upstream `KThread::SetState()`.
    pub fn set_state(&mut self, state: ThreadState) {
        self.set_wait_reason_for_debugging(ThreadWaitReasonForDebugging::None);
        let old_state = self.get_raw_state();
        let new_state = (old_state & !ThreadState::MASK) | (state & ThreadState::MASK);
        self.commit_state_transition(old_state, new_state, true);
    }

    fn commit_state_transition(
        &mut self,
        old_state: ThreadState,
        new_state: ThreadState,
        reset_schedule_count: bool,
    ) {
        if old_state == new_state {
            return;
        }

        let old_base = old_state & ThreadState::MASK;
        let new_base = new_state & ThreadState::MASK;

        let scheduler_lock = super::kernel::scheduler_lock();
        let _scheduler_guard = scheduler_lock
            .as_ref()
            .map(|lock| super::k_scheduler_lock::KScopedSchedulerLock::new(lock));

        // Upstream: KScheduler::OnThreadStateChanged(kernel, this, old_state)
        // compares the raw state against ThreadState::Runnable. Suspend-bit
        // transitions like RUNNABLE -> DEBUG_SUSPENDED|RUNNABLE must therefore
        // be visible to the scheduler so it removes the thread from the PQ.
        // The raw state store and PQ update must happen while the scheduler
        // lock is held; its Unlock() callback then runs UpdateHighestPriorityThreads
        // and performs the core handoff, matching upstream ordering.
        let mut notified_gsc = false;
        if let Some(gsc_arc) = self
            .global_scheduler_context
            .as_ref()
            .and_then(Weak::upgrade)
        {
            let thread_id = self.thread_id;
            let priority = self.priority;
            let active_core = self.core_id;
            let affinity = self.physical_affinity_mask.get_affinity_mask();
            let is_dummy = self.thread_type == ThreadType::Dummy;
            let self_reference = self.self_reference.as_ref().and_then(Weak::upgrade);

            if should_trace_ct_fire() {
                log::info!(
                    "commit_state_transition tid={} {:?}->{:?} before_gsc_lock",
                    thread_id,
                    old_base,
                    new_base
                );
            }
            let mut gsc = gsc_arc.lock().unwrap();
            self.thread_state.store(new_state.bits(), Ordering::Relaxed);
            if reset_schedule_count {
                self.schedule_count = -1;
            }
            gsc.on_thread_state_changed(
                thread_id,
                old_state,
                new_state,
                priority,
                active_core,
                affinity,
                is_dummy,
                self_reference,
                self.process_schedule_count.clone(),
                self.last_scheduled_tick,
            );
            if should_trace_ct_fire() {
                log::info!(
                    "commit_state_transition tid={} {:?}->{:?} after_gsc",
                    thread_id,
                    old_base,
                    new_base
                );
            }
            notified_gsc = true;
        }

        // Fallback only when no global scheduler context owner exists.
        if !notified_gsc {
            self.thread_state.store(new_state.bits(), Ordering::Relaxed);
            if reset_schedule_count {
                self.schedule_count = -1;
            }
            if let Some(scheduler) = self.scheduler.as_ref().and_then(Weak::upgrade) {
                scheduler.lock().unwrap().on_thread_state_changed(
                    self.thread_id,
                    old_state,
                    new_state,
                );
            }
        }
    }

    fn notify_priority_change(&self, old_priority: i32) {
        // Match upstream KScheduler::OnThreadPriorityChanged: only runnable
        // threads are present in KPriorityQueue, and ChangePriority's
        // `is_running` argument means "current thread", not "runnable".
        if self.get_raw_state() != ThreadState::RUNNABLE {
            return;
        }
        let is_running = super::kernel::get_current_thread_id_fast() == Some(self.thread_id);

        // Pass already-owned thread properties so the GSC does not need to
        // lock this KThread again.
        if let Some(gsc_arc) = self
            .global_scheduler_context
            .as_ref()
            .and_then(Weak::upgrade)
        {
            gsc_arc.lock().unwrap().on_thread_priority_changed(
                self.thread_id,
                old_priority,
                self.priority,
                self.core_id,
                self.physical_affinity_mask.get_affinity_mask(),
                is_running,
                self.thread_type == ThreadType::Dummy,
                self.thread_id,
            );
        } else {
            if let Some(scheduler) = self.scheduler.as_ref().and_then(Weak::upgrade) {
                scheduler
                    .lock()
                    .unwrap()
                    .on_thread_priority_changed(self.thread_id, old_priority);
            }
        }
    }

    fn request_schedule(&self) {
        let Some(scheduler) = self.scheduler.as_ref().and_then(Weak::upgrade) else {
            return;
        };
        scheduler.lock().unwrap().request_schedule();
    }

    /// Pin the thread to a core.
    /// Matches upstream `KThread::Pin(s32 current_core)`.
    pub fn pin(&mut self, current_core: i32) {
        // Set pinned flag.
        self.stack_parameters.is_pinned = true;

        // Disable core migration.
        debug_assert!(self.num_core_migration_disables == 0);
        self.num_core_migration_disables += 1;

        // Save original state for unpinning.
        self.original_physical_ideal_core_id = self.physical_ideal_core_id;
        self.original_physical_affinity_mask = self.physical_affinity_mask.clone();

        // Bind to current core.
        let old_active_core = self.get_active_core();
        let old_affinity = self.physical_affinity_mask.get_affinity_mask();
        self.set_active_core(current_core);
        self.physical_ideal_core_id = current_core;
        self.physical_affinity_mask
            .set_affinity_mask(1u64 << current_core);

        if (old_active_core != current_core
            || self.physical_affinity_mask.get_affinity_mask() != old_affinity)
            && self.get_raw_state() == ThreadState::RUNNABLE
        {
            if let Some(gsc_arc) = self
                .global_scheduler_context
                .as_ref()
                .and_then(Weak::upgrade)
            {
                gsc_arc.lock().unwrap().on_thread_affinity_changed(
                    self.thread_id,
                    old_active_core,
                    old_affinity,
                    current_core,
                    self.physical_affinity_mask.get_affinity_mask(),
                    self.priority,
                    self.thread_type == ThreadType::Dummy,
                );
            }
        }

        // Disallow thread suspension.
        self.suspend_allowed_flags &=
            !(1u32 << (ThreadState::SUSPEND_SHIFT as u32 + SuspendType::Thread as u32));
        self.update_state();
    }

    /// Unpin the thread.
    /// Matches upstream `KThread::Unpin()`.
    pub fn unpin(&mut self) {
        // Clear pinned flag.
        self.stack_parameters.is_pinned = false;

        // Enable core migration.
        debug_assert!(self.num_core_migration_disables == 1);
        self.num_core_migration_disables -= 1;

        // Restore original affinity.
        let old_active_core = self.get_active_core();
        let old_mask = self.physical_affinity_mask.clone();
        let old_affinity = old_mask.get_affinity_mask();
        self.physical_ideal_core_id = self.original_physical_ideal_core_id;
        self.physical_affinity_mask = self.original_physical_affinity_mask.clone();

        if self.physical_affinity_mask.get_affinity_mask() != old_affinity {
            // Check if current core is still valid.
            if (self.physical_affinity_mask.get_affinity_mask() & (1u64 << old_active_core)) == 0 {
                if self.physical_ideal_core_id >= 0 {
                    self.set_active_core(self.physical_ideal_core_id);
                } else {
                    // Pick highest valid core.
                    let mask = self.physical_affinity_mask.get_affinity_mask();
                    if mask != 0 {
                        self.set_active_core((63 - mask.leading_zeros()) as i32);
                    }
                }
            }
            if self.get_raw_state() == ThreadState::RUNNABLE {
                if let Some(gsc_arc) = self
                    .global_scheduler_context
                    .as_ref()
                    .and_then(Weak::upgrade)
                {
                    gsc_arc.lock().unwrap().on_thread_affinity_changed(
                        self.thread_id,
                        old_active_core,
                        old_affinity,
                        self.get_active_core(),
                        self.physical_affinity_mask.get_affinity_mask(),
                        self.priority,
                        self.thread_type == ThreadType::Dummy,
                    );
                }
            }
        }

        // Allow thread suspension (if termination not requested).
        if !self.is_termination_requested() {
            self.suspend_allowed_flags |=
                1u32 << (ThreadState::SUSPEND_SHIFT as u32 + SuspendType::Thread as u32);
            self.update_state();
        }

        // Resume any threads that began waiting on us while we were pinned.
        // Upstream drains `m_pinned_waiter_list` and calls EndWait(ResultSuccess).
        let waiter_ids = std::mem::take(&mut self.pinned_waiter_list);
        for waiter_id in waiter_ids {
            if waiter_id == self.thread_id {
                self.end_wait(RESULT_SUCCESS.get_inner_value());
                continue;
            }

            let waiter = self
                .parent
                .as_ref()
                .and_then(Weak::upgrade)
                .and_then(|parent| parent.lock().unwrap().get_thread_by_thread_id(waiter_id))
                .or_else(|| {
                    self.global_scheduler_context
                        .as_ref()
                        .and_then(Weak::upgrade)
                        .and_then(|gsc| gsc.lock().unwrap().get_thread_by_thread_id(waiter_id))
                });

            if let Some(waiter) = waiter {
                waiter
                    .lock()
                    .unwrap()
                    .end_wait(RESULT_SUCCESS.get_inner_value());
            }
        }
    }

    /// Wait cancel.
    /// Matches upstream `KThread::WaitCancel()`.
    pub fn wait_cancel(&mut self) {
        if self.get_state() == ThreadState::WAITING && self.cancellable {
            self.wait_cancelled = false;
            self.synced_index = -1;
            self.cancel_wait(RESULT_CANCELLED.get_inner_value(), true);
        } else {
            self.wait_cancelled = true;
        }
    }

    /// Begin wait on a thread queue.
    ///
    /// Matches upstream `KThread::BeginWait(KThreadQueue* queue)`:
    /// sets state to Waiting and assigns the wait queue.
    pub fn begin_wait_with_queue(&mut self, wait_queue: KThreadQueue) {
        self.set_state(ThreadState::WAITING);
        self.wait_queue = Some(wait_queue);
    }

    /// Begin wait without a specialized queue implementation.
    pub fn begin_wait(&mut self) {
        self.begin_wait_with_queue(KThreadQueue::default());
    }

    pub fn unpark_wait(&self) {
        // Upstream does not block the host thread in BeginWait.
        // Retained as a no-op for compatibility with existing queue call sites.
    }

    /// Clear the thread's active wait queue.
    ///
    /// Matches upstream `KThread::ClearWaitQueue()` ownership.
    pub fn clear_wait_queue(&mut self) {
        self.wait_queue = None;
    }

    fn finalize_wait_transition(&mut self) {
        self.waiting_lock_info = None;
        self.set_wait_reason_for_debugging(ThreadWaitReasonForDebugging::None);
    }

    fn lock_scheduler(&self) -> Option<KScopedSchedulerLock<'static>> {
        let scheduler_lock = super::kernel::scheduler_lock()?;
        Some(KScopedSchedulerLock::new(scheduler_lock))
    }

    /// Mirrors upstream `KThread::NotifyAvailable`. Delegates to the thread's
    /// wait_queue `NotifyAvailable` under scheduler lock. Only touches `self`
    /// and its `sync_wait_context` — never the process.
    pub fn notify_available(&mut self, signaled_object: *const SynchronizationObjectState, result: u32) -> bool {
        let _scheduler_lock = self.lock_scheduler();
        let Some(wait_queue) = self.wait_queue.clone() else {
            return false;
        };
        wait_queue.notify_available(self, signaled_object, result)
    }

    /// End wait with a result.
    /// Matches upstream `KThread::EndWait()`.
    pub fn end_wait(&mut self, _wait_result: u32) {
        let _scheduler_lock = self.lock_scheduler();

        if self.get_state() != ThreadState::WAITING {
            // Upstream preserves this as a no-op when the target is not
            // actually waiting.
            return;
        }

        // RUZU_PROFILE_WAKE — capture wake-emit timestamp. The woken thread's
        // next supervisor-call entry will compute the latency from this point
        // until it actually runs. Captures every wake source (cv signal,
        // address arbiter unlock, synchronization-object signal, etc.) by
        // hooking at the kernel-level wake boundary, not per-call-site.
        crate::hle::kernel::svc_dispatch::record_wake_emit(self.thread_id);

        if should_trace_end_wait() {
            log::info!(
                "END_WAIT enter tid={} result=0x{:X} wait_reason={:?} wait_queue_present={} prio={} base={} addr=0x{:X}",
                self.thread_id,
                _wait_result,
                self.wait_reason_for_debugging,
                self.wait_queue.is_some(),
                self.priority,
                self.base_priority,
                self.address_key.get(),
            );
        }

        // Upstream: ASSERT_MSG(false, "wait_queue is nullptr!"); return;
        // Avoid a hard crash — log and return early like upstream.
        let Some(wait_queue) = self.wait_queue.clone() else {
            log::error!(
                "KThread::end_wait: wait_queue is None while state=Waiting (upstream ASSERT) tid={} reason={:?}",
                self.thread_id,
                self.wait_reason_for_debugging
            );
            return;
        };
        wait_queue.end_wait(self, _wait_result);
        self.finalize_wait_transition();

        if should_trace_end_wait() {
            log::info!(
                "END_WAIT exit tid={} state={:?} result=0x{:X} prio={} base={}",
                self.thread_id,
                self.get_state(),
                self.wait_result,
                self.priority,
                self.base_priority,
            );
        }
    }

    /// Cancel wait.
    /// Matches upstream `KThread::CancelWait()`.
    pub fn cancel_wait(&mut self, _wait_result: u32, _cancel_timer_task: bool) {
        let _scheduler_lock = self.lock_scheduler();

        if self.get_state() != ThreadState::WAITING {
            return;
        }

        let wait_queue = self
            .wait_queue
            .clone()
            .expect("KThread::cancel_wait requires wait_queue while waiting");
        wait_queue.cancel_wait(self, _wait_result, _cancel_timer_task);
        self.finalize_wait_transition();
    }

    /// Set the thread's activity (pause/resume).
    /// Matches upstream `KThread::SetActivity()`.
    pub fn set_activity(&mut self, activity: u32) -> u32 {
        let activity_pause_lock = self.activity_pause_lock.clone();
        let _activity_guard = KScopedLightLock::new(activity_pause_lock.as_ref());

        {
            let _scheduler_lock =
                super::kernel::scheduler_lock().map(|lock| KScopedSchedulerLock::new(lock));

            let cur_state = self.get_state();
            if cur_state != ThreadState::WAITING && cur_state != ThreadState::RUNNABLE {
                return RESULT_INVALID_STATE.get_inner_value();
            }

            match activity {
                0 => {
                    if !self.is_suspend_requested_type(SuspendType::Thread) {
                        return RESULT_INVALID_STATE.get_inner_value();
                    }
                    self.resume(SuspendType::Thread);
                }
                1 => {
                    if self.is_suspend_requested_type(SuspendType::Thread) {
                        return RESULT_INVALID_STATE.get_inner_value();
                    }
                    self.request_suspend(SuspendType::Thread);
                }
                _ => return RESULT_INVALID_STATE.get_inner_value(),
            }
        }

        // If the thread is now paused, update the pinned waiter list.
        // Matches upstream `KThread::SetActivity()` second block: while the
        // target remains current on any core, wait for it to unpin if pinned,
        // otherwise retry until it stops being current.
        if activity == 1 {
            let self_arc = self.self_reference.as_ref().and_then(Weak::upgrade);
            loop {
                let mut thread_is_current = false;

                {
                    let _scheduler_lock =
                        super::kernel::scheduler_lock().map(|lock| KScopedSchedulerLock::new(lock));

                    if self.is_termination_requested() {
                        return RESULT_SUCCESS.get_inner_value();
                    }

                    if self.stack_parameters.is_pinned {
                        let current_terminating =
                            super::kernel::with_current_thread_fast_mut(|thread| {
                                thread.is_termination_requested()
                            })
                            .unwrap_or(false);
                        if current_terminating {
                            return RESULT_TERMINATION_REQUESTED.get_inner_value();
                        }

                        if let (Some(waiter_id), Some(owner)) = (
                            super::kernel::get_current_thread_id_fast(),
                            self_arc.as_ref(),
                        ) {
                            self.pinned_waiter_list.push(waiter_id);

                            let wait_queue = thread_queue_for_k_thread_set_property(owner);
                            if waiter_id == self.thread_id {
                                self.begin_wait_with_queue(wait_queue);
                            } else if let Some(current_thread) =
                                super::kernel::get_current_thread_pointer()
                            {
                                current_thread
                                    .lock()
                                    .unwrap()
                                    .begin_wait_with_queue(wait_queue);
                            }
                        }
                    } else if let Some(kernel) = super::kernel::get_kernel_ref() {
                        for core_id in 0..NUM_CPU_CORES as usize {
                            let is_current = kernel.scheduler(core_id).and_then(|scheduler| {
                                scheduler.lock().unwrap().get_scheduler_current_thread_id()
                            }) == Some(self.thread_id);
                            if is_current {
                                thread_is_current = true;
                                break;
                            }
                        }
                    }
                }

                if !thread_is_current {
                    break;
                }
            }
        }

        self.request_schedule();
        RESULT_SUCCESS.get_inner_value()
    }

    /// Sleep for the given timeout.
    /// Matches upstream `KThread::Sleep()`.
    pub fn sleep(&mut self, timeout: i64) -> u32 {
        if timeout <= 0 {
            return RESULT_INVALID_STATE.get_inner_value();
        }

        log::trace!(
            "KThread::sleep enter tid={} timeout_tick={} disable_dispatch={}",
            self.thread_id,
            timeout,
            self.get_disable_dispatch_count()
        );
        self.wait_result = RESULT_SUCCESS.get_inner_value();
        let hardware_timer = super::kernel::get_hardware_timer_arc();
        let mut wait_queue = KThreadQueueWithoutEndWait::new();
        let Some(scheduler_lock) = super::kernel::scheduler_lock() else {
            return RESULT_INVALID_STATE.get_inner_value();
        };

        {
            let thread_ptr = self as *mut KThread as usize;
            let (mut sleep_guard, timer) = KScopedSchedulerLockAndSleep::new(
                scheduler_lock,
                hardware_timer.as_ref(),
                self.thread_id,
                thread_ptr,
                timeout,
            );

            if self.is_termination_requested() {
                sleep_guard.cancel_sleep();
                return RESULT_TERMINATION_REQUESTED.get_inner_value();
            }

            if let Some(timer) = timer {
                wait_queue.base.set_hardware_timer(timer);
            }

            self.set_timer_task_time(timeout);
            log::trace!(
                "KThread::sleep tid={} before begin_wait_with_queue",
                self.thread_id
            );
            self.begin_wait_with_queue(wait_queue.base);
            self.set_wait_reason_for_debugging(ThreadWaitReasonForDebugging::Sleep);
            if should_trace_wait_debug() {
                log::info!(
                    "KThread::sleep tid={} timeout_tick={} current_tick={:?}",
                    self.thread_id,
                    timeout,
                    super::kernel::get_current_hardware_tick()
                );
            }
            log::trace!("KThread::sleep tid={} wait armed", self.thread_id);
        }

        log::trace!("KThread::sleep exit tid={}", self.thread_id);
        RESULT_SUCCESS.get_inner_value()
    }

    /// Get core mask.
    /// Matches upstream `KThread::GetCoreMask()`.
    pub fn get_core_mask(&self) -> (i32, u64) {
        let _scheduler_lock =
            super::kernel::scheduler_lock().map(|lock| KScopedSchedulerLock::new(lock));
        (self.virtual_ideal_core_id, self.virtual_affinity_mask)
    }

    /// Get physical core mask.
    /// Matches upstream `KThread::GetPhysicalCoreMask()`.
    pub fn get_physical_core_mask(&self) -> (i32, u64) {
        let _scheduler_lock =
            super::kernel::scheduler_lock().map(|lock| KScopedSchedulerLock::new(lock));
        debug_assert!(self.num_core_migration_disables >= 0);
        if self.num_core_migration_disables == 0 {
            (
                self.physical_ideal_core_id,
                self.physical_affinity_mask.get_affinity_mask(),
            )
        } else {
            (
                self.original_physical_ideal_core_id,
                self.original_physical_affinity_mask.get_affinity_mask(),
            )
        }
    }

    /// Set core mask.
    /// Matches upstream `KThread::SetCoreMask()`.
    pub fn set_core_mask(&mut self, cpu_core_id: i32, affinity_mask: u64) -> u32 {
        debug_assert!(affinity_mask != 0);
        let activity_pause_lock = self.activity_pause_lock.clone();
        let _activity_guard = KScopedLightLock::new(activity_pause_lock.as_ref());

        let physical_affinity_mask_for_waiters;

        {
            let _scheduler_lock =
                super::kernel::scheduler_lock().map(|lock| KScopedSchedulerLock::new(lock));

            let mut core_id = cpu_core_id;
            if core_id != crate::hle::kernel::svc_types::IDEAL_CORE_NO_UPDATE {
                self.virtual_ideal_core_id = core_id;
            } else {
                core_id = self.virtual_ideal_core_id;
                if ((1u64 << core_id) & affinity_mask) == 0 {
                    return RESULT_INVALID_COMBINATION.get_inner_value();
                }
            }

            self.virtual_affinity_mask = affinity_mask;

            if core_id >= 0 {
                core_id =
                    crate::hardware_properties::VIRTUAL_TO_PHYSICAL_CORE_MAP[core_id as usize];
            }

            let physical_affinity_mask =
                crate::hardware_properties::convert_virtual_core_mask_to_physical(affinity_mask);
            physical_affinity_mask_for_waiters = physical_affinity_mask;

            if self.num_core_migration_disables == 0 {
                let old_mask = self.physical_affinity_mask.clone();
                let old_active_core = self.get_active_core();

                self.physical_ideal_core_id = core_id;
                self.physical_affinity_mask
                    .set_affinity_mask(physical_affinity_mask);

                if self.physical_affinity_mask.get_affinity_mask() != old_mask.get_affinity_mask() {
                    if old_active_core >= 0
                        && (self.physical_affinity_mask.get_affinity_mask()
                            & (1u64 << old_active_core))
                            == 0
                    {
                        let new_core = if self.physical_ideal_core_id >= 0 {
                            self.physical_ideal_core_id
                        } else {
                            let mask = self.physical_affinity_mask.get_affinity_mask();
                            (63 - mask.leading_zeros()) as i32
                        };
                        self.set_active_core(new_core);
                    }

                    if self.get_state() == ThreadState::RUNNABLE {
                        if let Some(gsc) = self
                            .global_scheduler_context
                            .as_ref()
                            .and_then(Weak::upgrade)
                        {
                            gsc.lock().unwrap().on_thread_affinity_changed(
                                self.thread_id,
                                old_active_core,
                                old_mask.get_affinity_mask(),
                                self.get_active_core(),
                                self.physical_affinity_mask.get_affinity_mask(),
                                self.priority,
                                self.is_dummy_thread(),
                            );
                        }
                    }
                }
            } else {
                self.original_physical_ideal_core_id = core_id;
                self.original_physical_affinity_mask
                    .set_affinity_mask(physical_affinity_mask);
            }
        }

        // Update the pinned waiter list.
        // Upstream retries while this thread is currently running on a core no
        // longer allowed by the new mask. If it is pinned, the current thread
        // waits on this thread's pinned waiter list until `Unpin()` resumes it.
        let self_arc = self.self_reference.as_ref().and_then(Weak::upgrade);
        if common::trace::is_enabled(common::trace::cat::THREAD_CORE_MASK) {
            common::trace::emit_raw(
                common::trace::cat::THREAD_CORE_MASK,
                &[
                    4,
                    super::kernel::get_current_thread_id_fast().unwrap_or(0),
                    self.thread_id,
                    0,
                    cpu_core_id as u32 as u64,
                    affinity_mask,
                    self.get_raw_state().bits() as u64,
                    self.get_active_core() as u64,
                    self.get_current_core() as u64,
                    RESULT_SUCCESS.get_inner_value() as u64,
                    self.stack_parameters.is_pinned as u64,
                    self.pinned_waiter_list.len() as u64,
                ],
            );
        }
        loop {
            let mut retry_update = false;

            {
                let _scheduler_lock =
                    super::kernel::scheduler_lock().map(|lock| KScopedSchedulerLock::new(lock));

                if self.is_termination_requested() {
                    return RESULT_SUCCESS.get_inner_value();
                }

                let mut current_core_for_thread = None;
                if let Some(kernel) = super::kernel::get_kernel_ref() {
                    for core_id in 0..NUM_CPU_CORES as usize {
                        let is_current = kernel.scheduler(core_id).and_then(|scheduler| {
                            scheduler.lock().unwrap().get_scheduler_current_thread_id()
                        }) == Some(self.thread_id);
                        if is_current {
                            current_core_for_thread = Some(core_id as i32);
                            break;
                        }
                    }
                }

                if let Some(thread_core) = current_core_for_thread {
                    let is_allowed =
                        (physical_affinity_mask_for_waiters & (1u64 << thread_core)) != 0;
                    if !is_allowed {
                        if self.stack_parameters.is_pinned {
                            let current_terminating =
                                super::kernel::with_current_thread_fast_mut(|thread| {
                                    thread.is_termination_requested()
                                })
                                .unwrap_or(false);
                            if current_terminating {
                                return RESULT_TERMINATION_REQUESTED.get_inner_value();
                            }

                            if let (Some(waiter_id), Some(owner)) = (
                                super::kernel::get_current_thread_id_fast(),
                                self_arc.as_ref(),
                            ) {
                                self.pinned_waiter_list.push(waiter_id);
                                if common::trace::is_enabled(common::trace::cat::THREAD_CORE_MASK) {
                                    common::trace::emit_raw(
                                        common::trace::cat::THREAD_CORE_MASK,
                                        &[
                                            6,
                                            waiter_id,
                                            self.thread_id,
                                            0,
                                            cpu_core_id as u32 as u64,
                                            affinity_mask,
                                            self.get_raw_state().bits() as u64,
                                            self.get_active_core() as u64,
                                            self.get_current_core() as u64,
                                            RESULT_SUCCESS.get_inner_value() as u64,
                                            self.stack_parameters.is_pinned as u64,
                                            self.pinned_waiter_list.len() as u64,
                                        ],
                                    );
                                }

                                let wait_queue = thread_queue_for_k_thread_set_property(owner);
                                if waiter_id == self.thread_id {
                                    self.begin_wait_with_queue(wait_queue);
                                } else if let Some(current_thread) =
                                    super::kernel::get_current_thread_pointer()
                                {
                                    current_thread
                                        .lock()
                                        .unwrap()
                                        .begin_wait_with_queue(wait_queue);
                                }
                            }
                        } else {
                            retry_update = true;
                            if common::trace::is_enabled(common::trace::cat::THREAD_CORE_MASK) {
                                common::trace::emit_raw(
                                    common::trace::cat::THREAD_CORE_MASK,
                                    &[
                                        5,
                                        super::kernel::get_current_thread_id_fast().unwrap_or(0),
                                        self.thread_id,
                                        0,
                                        cpu_core_id as u32 as u64,
                                        affinity_mask,
                                        self.get_raw_state().bits() as u64,
                                        self.get_active_core() as u64,
                                        self.get_current_core() as u64,
                                        RESULT_SUCCESS.get_inner_value() as u64,
                                        self.stack_parameters.is_pinned as u64,
                                        self.pinned_waiter_list.len() as u64,
                                    ],
                                );
                            }
                        }
                    }
                }
            }

            if !retry_update {
                break;
            }
        }

        if common::trace::is_enabled(common::trace::cat::THREAD_CORE_MASK) {
            common::trace::emit_raw(
                common::trace::cat::THREAD_CORE_MASK,
                &[
                    7,
                    super::kernel::get_current_thread_id_fast().unwrap_or(0),
                    self.thread_id,
                    0,
                    cpu_core_id as u32 as u64,
                    affinity_mask,
                    self.get_raw_state().bits() as u64,
                    self.get_active_core() as u64,
                    self.get_current_core() as u64,
                    RESULT_SUCCESS.get_inner_value() as u64,
                    self.stack_parameters.is_pinned as u64,
                    self.pinned_waiter_list.len() as u64,
                ],
            );
        }
        RESULT_SUCCESS.get_inner_value()
    }

    /// Finalize the thread.
    /// Matches upstream `KThread::Finalize()` (k_thread.cpp:333-387).
    pub fn finalize(&mut self) {
        // If the thread has an owner process, unregister it.
        if let Some(parent) = self.parent.as_ref().and_then(Weak::upgrade) {
            let mut process = parent.lock().unwrap();
            process.unregister_thread_object(self.thread_id, self.object_id);

            // If the thread has a local region, delete it.
            if self.tls_address.get() != 0 {
                process.delete_thread_local_region(self.tls_address);
            }
        }

        // Release any waiters.
        // Matches upstream KThread::Finalize() (k_thread.cpp:344-380).
        {
            debug_assert!(
                self.waiting_lock_info.is_none(),
                "thread {} has waiting_lock_info at finalize",
                self.thread_id
            );
            assert_eq!(
                self.num_kernel_waiters, 0,
                "thread {} has kernel waiters at finalize",
                self.thread_id
            );

            // Walk held_lock_info_list, cancel all waiters, free lock infos.
            while let Some(mut lock_info) = self.held_lock_info_list.pop() {
                debug_assert!(
                    !lock_info.get_is_kernel_address_key(),
                    "finalize: lock info should not have kernel address key"
                );

                // Remove all waiters from this lock.
                while lock_info.get_waiter_count() != 0 {
                    let Some(waiter_key) = lock_info.get_highest_priority_waiter() else {
                        break;
                    };
                    lock_info.remove_waiter(waiter_key.priority, waiter_key.thread_id);

                    // Cancel the waiter's wait.
                    if let Some(parent) = self.parent.as_ref().and_then(Weak::upgrade) {
                        let process = parent.lock().unwrap();
                        if let Some(waiter) = process.get_thread_by_thread_id(waiter_key.thread_id)
                        {
                            let mut waiter_guard = waiter.lock().unwrap();
                            waiter_guard.set_waiting_lock_info(None);
                            waiter_guard.cancel_wait(RESULT_INVALID_STATE.get_inner_value(), true);
                        }
                    }
                }
                // lock_info is dropped here (equivalent to upstream Free).
            }
        }

        // Release host emulation members.
        // Upstream: m_host_context.reset()
        self.host_context = None;

        // Perform inherited finalization.
        // Upstream: KSynchronizationObject::Finalize()
        // Clears the synchronization object state so no dangling waiters remain.
        self.sync_object = super::k_synchronization_object::SynchronizationObjectState::new();
    }

    /// Release the owner resources after the final thread reference closes.
    /// Matches upstream `KThread::PostDestroy()` (k_thread.cpp:337-345).
    pub fn post_destroy(parent: Option<Arc<ProcessLock>>, resource_limit_release_hint: bool) {
        let Some(parent) = parent else {
            return;
        };
        let resource_limit = parent.lock().unwrap().resource_limit.clone();
        if let Some(resource_limit) = resource_limit {
            let hint_value = if resource_limit_release_hint { 0 } else { 1 };
            resource_limit.release_with_hint(
                super::k_resource_limit::LimitableResource::ThreadCountMax,
                1,
                hint_value,
            );
        }
    }

    /// Is the thread signaled?
    /// Matches upstream `KThread::IsSignaled()` (k_thread.h).
    /// Returns true when the thread has completed termination (FinishTermination
    /// sets m_signaled = true). Used by KSynchronizationObject::Wait to determine
    /// if waiters should be woken.
    pub fn is_signaled(&self) -> bool {
        self.signaled
    }

    /// Worker task implementation.
    /// Matches upstream `KThread::DoWorkerTaskImpl()` (k_thread.cpp:450-453).
    /// Called by KWorkerTaskManager after Exit() registers the thread as a task.
    pub fn do_worker_task_impl(&mut self) {
        // Finish the termination that was begun by Exit().
        self.finish_termination();
    }

    /// OnTimer callback.
    pub fn on_timer(&mut self) {
        if should_trace_wait_debug() {
            log::info!(
                "KThread::on_timer tid={} state={:?} wait_queue={} wait_reason={:?} wait_result=0x{:x}",
                self.thread_id,
                self.get_state(),
                self.wait_queue.is_some(),
                self.get_wait_reason_for_debugging(),
                self.wait_result
            );
        }
        log::trace!(
            "KThread::on_timer tid={} state={:?} active_core={} current_core={} wait_queue={}",
            self.thread_id,
            self.get_state(),
            self.get_active_core(),
            self.get_current_core(),
            self.wait_queue.is_some()
        );
        let ct_trace = should_trace_ct_fire();
        if ct_trace {
            log::info!(
                "on_timer tid={} state={:?} has_wait_queue={} reason={:?}",
                self.thread_id,
                self.get_state(),
                self.wait_queue.is_some(),
                self.get_wait_reason_for_debugging()
            );
        }
        if self.get_state() == ThreadState::WAITING {
            if let Some(wait_queue) = self.wait_queue.clone() {
                if ct_trace {
                    log::info!("on_timer tid={} before_cancel_wait", self.thread_id);
                }
                wait_queue.cancel_wait(self, RESULT_TIMED_OUT.get_inner_value(), false);
                // Upstream leaves timeout completion to CancelWait(); keep only
                // the Rust-local cleanup that has no direct C++ field owner.
                self.waiting_lock_info = None;
                if ct_trace {
                    log::info!("on_timer tid={} after_cancel_wait", self.thread_id);
                }
            }
            log::trace!(
                "KThread::on_timer tid={} post-cancel state={:?} wait_result=0x{:x}",
                self.thread_id,
                self.get_state(),
                self.wait_result
            );
        }
    }

    /// Request that this dummy thread block on next DummyThreadBeginWait.
    /// Port of upstream `KThread::RequestDummyThreadWait`.
    pub fn request_dummy_thread_wait(&self) {
        *self.dummy_thread_wait.0.lock().unwrap() = false;
    }

    /// Block the dummy thread until DummyThreadEndWait is called.
    /// Port of upstream `KThread::DummyThreadBeginWait`.
    pub fn dummy_thread_begin_wait(thread: &Arc<KThreadLock>) {
        if super::kernel::get_kernel_ref()
            .is_some_and(|kernel| kernel.is_phantom_mode_for_single_core())
        {
            return;
        }
        let wait = {
            let thread = thread.lock().unwrap();
            if !thread.is_dummy_thread() {
                return;
            }
            Arc::clone(&thread.dummy_thread_wait)
        };
        // Do not retain a KThread reference across this host suspension.
        // Only the predicate mutex is held, and Condvar releases it to sleep.
        let guard = wait.0.lock().unwrap();
        let _guard = wait.1.wait_while(guard, |runnable| !*runnable).unwrap();
    }

    /// Wake the dummy thread from DummyThreadBeginWait.
    /// Port of upstream `KThread::DummyThreadEndWait`.
    pub fn dummy_thread_end_wait(&self) {
        *self.dummy_thread_wait.0.lock().unwrap() = true;
        self.dummy_thread_wait.1.notify_one();
    }

    /// Set condition variable state.
    /// Upstream: ASSERT(m_waiting_lock_info == nullptr).
    pub fn set_condition_variable(&mut self, address: KProcessAddress, cv_key: u64, value: u32) {
        debug_assert!(
            self.waiting_lock_info.is_none(),
            "set_condition_variable: m_waiting_lock_info must be null"
        );
        self.condvar_tree_state = ConditionVariableTreeState::ConditionVariable;
        self.condvar_key = cv_key;
        self.address_key = address;
        self.address_key_value = value;
        self.is_kernel_address_key = false;
    }

    /// Clear condition variable state.
    pub fn clear_condition_variable(&mut self) {
        self.condvar_tree_state = ConditionVariableTreeState::None;
    }

    pub fn is_waiting_for_condition_variable(&self) -> bool {
        self.condvar_tree_state == ConditionVariableTreeState::ConditionVariable
    }

    pub fn is_waiting_for_address_arbiter(&self) -> bool {
        self.condvar_tree_state == ConditionVariableTreeState::AddressArbiter
    }

    /// Set address arbiter state.
    /// Upstream: ASSERT(m_waiting_lock_info == nullptr).
    pub fn set_address_arbiter(&mut self, address: u64) {
        debug_assert!(
            self.waiting_lock_info.is_none(),
            "set_address_arbiter: m_waiting_lock_info must be null"
        );
        self.condvar_tree_state = ConditionVariableTreeState::AddressArbiter;
        self.condvar_key = address;
    }

    /// Clear address arbiter state.
    pub fn clear_address_arbiter(&mut self) {
        self.condvar_tree_state = ConditionVariableTreeState::None;
    }

    pub fn condition_variable_tree_key(&self) -> ConditionVariableThreadKey {
        ConditionVariableThreadKey {
            cv_key: self.condvar_key,
            priority: self.priority,
            thread_id: self.thread_id,
        }
    }

    /// Set the waiting lock info (which lock this thread is blocked on).
    /// Pass `None` to clear (thread is no longer waiting on a lock).
    /// `owner_thread_ptr` is an optional raw `*mut KThread` pointer to the
    /// owner; when supplied, `cancel_wait` paths running under the scheduler
    /// lock can dereference it directly (matching upstream's `KThread*`)
    /// instead of locking a Rust-only per-thread mutex.
    pub fn set_waiting_lock_owner_thread_id(
        &mut self,
        owner_thread_id: Option<u64>,
        owner_thread_ptr: usize,
    ) {
        match owner_thread_id {
            Some(id) => {
                self.waiting_lock_info = Some(WaitingLockRef {
                    owner_thread_id: id,
                    address_key: self.address_key,
                    is_kernel_address_key: self.is_kernel_address_key,
                    owner_thread_ptr,
                });
            }
            None => {
                self.waiting_lock_info = None;
            }
        }
    }

    /// Get the lock owner thread.
    /// Matches upstream `KThread::GetLockOwner()` (k_thread.cpp:732-734).
    pub fn get_lock_owner(&self) -> Option<Arc<KThreadLock>> {
        let lock_ref = self.waiting_lock_info.as_ref()?;
        let parent = self.parent.as_ref()?.upgrade()?;
        let process = parent.lock().unwrap();
        process.get_thread_by_thread_id(lock_ref.owner_thread_id)
    }

    /// Get the lock owner thread as a raw `*mut KThread`, matching upstream's
    /// `KThread* KThread::GetLockOwner()`. Callers must already be serialized
    /// by the scheduler lock (upstream's invariant). Returns `None` if no
    /// pointer was stored.
    pub fn get_lock_owner_raw(&self) -> Option<*mut KThread> {
        let ptr = self.waiting_lock_info.as_ref()?.owner_thread_ptr;
        if ptr == 0 {
            None
        } else {
            Some(ptr as *mut KThread)
        }
    }

    /// Get the lock owner thread ID (without looking up the thread).
    pub fn get_lock_owner_thread_id(&self) -> Option<u64> {
        self.waiting_lock_info.as_ref().map(|r| r.owner_thread_id)
    }

    /// Raw pointer to the owning `KProcess`, cached at parenting time so
    /// scheduler-lock-protected paths can read/write process fields without
    /// re-acquiring the process mutex (matches upstream's raw-pointer access).
    pub fn get_parent_raw_ptr(&self) -> Option<*mut KProcess> {
        if self.parent_raw_ptr == 0 {
            None
        } else {
            Some(self.parent_raw_ptr as *mut KProcess)
        }
    }

    pub fn set_parent_raw_ptr(&mut self, ptr: usize) {
        self.parent_raw_ptr = ptr;
    }

    pub fn has_wait_queue(&self) -> bool {
        self.wait_queue.is_some()
    }

    /// Get the timer task time (upstream KTimerTask::GetTime()).
    pub fn get_timer_task_time(&self) -> i64 {
        self.timer_task_time
    }

    /// Set the timer task time (upstream KTimerTask::SetTime()).
    pub fn set_timer_task_time(&mut self, time: i64) {
        self.timer_task_time = time;
    }

    /// Continue if has kernel waiters.
    pub fn continue_if_has_kernel_waiters(&mut self) {
        if self.get_num_kernel_waiters() > 0 {
            self.continue_thread();
        }
    }
}

impl Default for KThread {
    fn default() -> Self {
        Self::new()
    }
}

// KPriorityQueueMember impl removed: QueueEntry now stored inside KPriorityQueue.
// Thread properties are passed directly to PQ operations via
// GlobalSchedulerContext::on_thread_state_changed.

// HasRBEntry impl removed: condvar_arbiter_tree_node field is kept for
// structural parity with upstream m_condvar_arbiter_tree_node, but we use
// BTreeSet<ConditionVariableThreadKey> externally rather than an intrusive
// red-black tree through this node. The impl can be restored if/when we
// switch to an intrusive tree.

/// RAII guard that disables dispatch on construction and enables on destruction.
/// Matches upstream `KScopedDisableDispatch` (k_thread.h).
///
/// On construction: increments current thread's disable_dispatch_count.
/// On destruction: if count would reach 0, triggers RescheduleCurrentCore
/// (or RescheduleCurrentHLEThread for phantom/single-core mode).
/// Otherwise just decrements.
pub struct KScopedDisableDispatch {
    thread: Arc<KThreadLock>,
}

impl KScopedDisableDispatch {
    /// Create a new scoped disable dispatch guard.
    /// Upstream takes `KernelCore&` and uses `GetCurrentThread(kernel)`.
    /// We take the thread directly.
    pub fn new(thread: &Arc<KThreadLock>) -> Self {
        thread.lock().unwrap().disable_dispatch();
        Self {
            thread: thread.clone(),
        }
    }
}

impl Drop for KScopedDisableDispatch {
    fn drop(&mut self) {
        // Upstream: ~KScopedDisableDispatch() (k_thread.cpp:1429-1446)
        // If shutting down, do nothing.
        // Otherwise, if dispatch count is 1, reschedule; if > 1, just enable.

        let thread = self.thread.lock().unwrap();
        if thread.get_disable_dispatch_count() <= 1 {
            drop(thread);
            // Upstream: scheduler->RescheduleCurrentCore() if scheduler exists
            // and not phantom mode; otherwise RescheduleCurrentHLEThread.
            // Use the HLE reschedule path since we don't have kernel context here.
            KScheduler::reschedule_current_hle_thread();
        } else {
            drop(thread);
            self.thread.lock().unwrap().enable_dispatch();
        }
    }
}

/// Sleep the current emulated thread.
/// Mirrors the upstream owner boundary where `svc::SleepThread(...)`
/// forwards the positive-timeout path to `GetCurrentThread(kernel).Sleep(timeout)`.
pub fn sleep_current_thread(timeout: i64) -> Option<u32> {
    super::kernel::get_current_emu_thread().map(|thread| thread.lock().unwrap().sleep(timeout))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::SystemRef;
    use crate::hle::kernel::global_scheduler_context::GlobalSchedulerContext;
    use crate::hle::kernel::k_memory_block::PAGE_SIZE;
    use crate::hle::kernel::k_process::KProcess;
    use crate::hle::kernel::k_scheduler::KScheduler;
    use crate::hle::kernel::k_scheduler_lock;
    use crate::hle::result::RESULT_SUCCESS;
    use std::sync::{Arc, Mutex};

    #[test]
    fn current_process_and_memory_follow_the_current_threads_owner() {
        let mut system = crate::core::System::new();
        system.initialize();

        let mut application = KProcess::new();
        application.create_memory(&system);
        let application = Arc::new(ProcessLock::from_value(application));

        let mut applet = KProcess::new();
        applet.create_memory(&system);
        let applet = Arc::new(ProcessLock::from_value(applet));
        let applet_memory = applet.lock().unwrap().get_memory().unwrap();

        system.set_current_process_arc(Arc::clone(&application));

        let mut thread = KThread::new();
        thread.parent = Some(Arc::downgrade(&applet));
        let thread = Arc::new(KThreadLock::new(thread));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&thread));

        let resolved_process = get_current_process_pointer().unwrap();
        assert!(Arc::ptr_eq(&resolved_process, &applet));
        assert!(!Arc::ptr_eq(&resolved_process, &application));
        assert!(Arc::ptr_eq(&get_current_memory().unwrap(), &applet_memory));
        assert!(Arc::ptr_eq(&system.current_process_arc(), &applet));
        assert!(Arc::ptr_eq(
            &system.get_svc_memory().unwrap(),
            &applet_memory
        ));

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn test_thread_state_values() {
        assert_eq!(ThreadState::WAITING.bits(), 1);
        assert_eq!(ThreadState::RUNNABLE.bits(), 2);
        assert_eq!(ThreadState::TERMINATED.bits(), 3);
        assert_eq!(ThreadState::MASK.bits(), 0xF);
        assert_eq!(ThreadState::PROCESS_SUSPENDED.bits(), 1 << 4);
        assert_eq!(ThreadState::THREAD_SUSPENDED.bits(), 1 << 5);
        assert_eq!(ThreadState::DEBUG_SUSPENDED.bits(), 1 << 6);
        assert_eq!(ThreadState::BACKTRACE_SUSPENDED.bits(), 1 << 7);
        assert_eq!(ThreadState::INIT_SUSPENDED.bits(), 1 << 8);
        assert_eq!(ThreadState::SYSTEM_SUSPENDED.bits(), 1 << 9);
    }

    #[test]
    fn set_state_clears_wait_reason_like_upstream() {
        let mut thread = KThread::new();
        thread.set_wait_reason_for_debugging(ThreadWaitReasonForDebugging::Ipc);
        thread.set_state(ThreadState::WAITING);

        assert_eq!(
            thread.get_wait_reason_for_debugging(),
            ThreadWaitReasonForDebugging::None
        );
    }

    #[test]
    fn test_thread_type_values() {
        assert_eq!(ThreadType::Main as u32, 0);
        assert_eq!(ThreadType::Kernel as u32, 1);
        assert_eq!(ThreadType::HighPriority as u32, 2);
        assert_eq!(ThreadType::User as u32, 3);
        assert_eq!(ThreadType::Dummy as u32, 100);
    }

    #[test]
    fn test_suspend_type_values() {
        assert_eq!(SuspendType::Process as u32, 0);
        assert_eq!(SuspendType::Thread as u32, 1);
        assert_eq!(SuspendType::Debug as u32, 2);
        assert_eq!(SuspendType::Backtrace as u32, 3);
        assert_eq!(SuspendType::Init as u32, 4);
        assert_eq!(SuspendType::System as u32, 5);
    }

    #[test]
    fn test_dpc_flag_values() {
        assert_eq!(DpcFlag::TERMINATING.bits(), 1);
        assert_eq!(DpcFlag::TERMINATED.bits(), 2);
    }

    #[test]
    fn test_default_thread() {
        let thread = KThread::new();
        assert_eq!(thread.get_priority(), 0);
        assert_eq!(thread.get_thread_id(), 0);
        assert!(!thread.is_initialized());
        assert!(!thread.is_dummy_thread());
    }

    #[test]
    fn user_preemption_state_uses_process_memory_bridge() {
        assert_eq!(std::mem::size_of::<ThreadLocalRegion>(), 0x104);
        assert_eq!(THREAD_LOCAL_DISABLE_COUNT_OFFSET, 0x100);
        assert_eq!(THREAD_LOCAL_INTERRUPT_FLAG_OFFSET, 0x102);

        let mut system = crate::core::System::new_for_test();
        system.initialize();
        {
            let kernel = system
                .kernel_mut()
                .expect("test system must own an initialized kernel");
            kernel.initialize();
            kernel.initialize_memory_block_slab_manager(4096);
            kernel.memory_manager_mut().initialize_pool(
                crate::hle::kernel::k_memory_manager::Pool::Application,
                0x1_0000_0000,
                0x80000 * PAGE_SIZE,
            );
        }
        let mut process = KProcess::new();
        process.process_id = 100;
        process.capabilities.core_mask = 0xF;
        process.capabilities.priority_mask = u64::MAX;
        process.initialize_handle_table();
        process.resource_limit = Some(Arc::new(
            crate::hle::kernel::k_resource_limit::create_resource_limit_for_process(0x4000_0000),
        ));
        process.create_memory(&system);
        process.allocate_code_memory(0x20_0000, 0x40_000);
        process
            .page_table
            .set_heap_region(KProcessAddress::new(0x40_0000), 0x20_0000);
        let (result, tls_address) = process.set_heap_size(PAGE_SIZE);
        assert_eq!(result, RESULT_SUCCESS.get_inner_value());

        {
            let page_table = process.page_table.get_base_mut();
            let memory = page_table
                .m_memory
                .as_ref()
                .expect("test page table memory must be attached")
                .clone();
            let impl_page_table = page_table
                .m_impl
                .as_mut()
                .expect("test page table backend must be initialized");
            memory
                .lock()
                .unwrap()
                .set_current_page_table(impl_page_table.as_mut() as *mut _, true);
        }

        let tls_address = tls_address.get();
        let memory = process
            .get_memory()
            .expect("test process must own upstream-shaped Memory");
        memory
            .lock()
            .unwrap()
            .write_16(tls_address + THREAD_LOCAL_DISABLE_COUNT_OFFSET, 3);
        memory
            .lock()
            .unwrap()
            .write_16(tls_address + THREAD_LOCAL_INTERRUPT_FLAG_OFFSET, 0);

        // Keep contradictory values in the retired compatibility store. The
        // upstream KThread methods use KProcess::GetMemory(), never this store.
        process
            .process_memory
            .write()
            .unwrap()
            .write_16(tls_address + THREAD_LOCAL_DISABLE_COUNT_OFFSET, 0);
        process
            .process_memory
            .write()
            .unwrap()
            .write_16(tls_address + THREAD_LOCAL_INTERRUPT_FLAG_OFFSET, 0xBEEF);

        let process = Arc::new(ProcessLock::from_value(process));
        let mut thread = KThread::new();
        thread.parent = Some(Arc::downgrade(&process));
        thread.tls_address = KProcessAddress::new(tls_address);

        assert_eq!(thread.get_user_disable_count(), 3);

        thread.set_interrupt_flag();
        assert_eq!(
            memory
                .lock()
                .unwrap()
                .read_16(tls_address + THREAD_LOCAL_INTERRUPT_FLAG_OFFSET),
            1
        );
        assert_eq!(
            process
                .lock()
                .unwrap()
                .process_memory
                .read()
                .unwrap()
                .read_16(tls_address + THREAD_LOCAL_INTERRUPT_FLAG_OFFSET),
            0xBEEF
        );

        thread.clear_interrupt_flag();
        assert_eq!(
            memory
                .lock()
                .unwrap()
                .read_16(tls_address + THREAD_LOCAL_INTERRUPT_FLAG_OFFSET),
            0
        );
    }

    #[test]
    fn test_request_suspend_sets_suspend_bits_without_changing_base_state() {
        let mut thread = KThread::new();
        thread.set_state(ThreadState::RUNNABLE);

        thread.request_suspend(SuspendType::Thread);

        assert_eq!(thread.get_state(), ThreadState::RUNNABLE);
        assert!(thread.is_suspend_requested_type(SuspendType::Thread));
        assert!(thread.is_suspended());
        assert!(thread
            .get_raw_state()
            .contains(ThreadState::THREAD_SUSPENDED));
    }

    #[test]
    fn test_wait_cancel_marks_wait_cancelled_when_not_cancellable() {
        let mut thread = KThread::new();
        thread.begin_wait();

        thread.wait_cancel();

        assert!(thread.is_wait_cancelled());
        assert_eq!(thread.get_state(), ThreadState::WAITING);
    }

    #[test]
    fn test_wait_cancel_resumes_cancellable_waiting_thread() {
        let mut thread = KThread::new();
        thread.begin_wait();
        thread.set_cancellable();

        thread.wait_cancel();

        assert_eq!(thread.get_state(), ThreadState::RUNNABLE);
        assert!(!thread.is_wait_cancelled());
    }

    #[test]
    fn wait_cancel_pushes_runnable_thread_through_state_change_handler() {
        let gsc = Arc::new(Mutex::new(GlobalSchedulerContext::new()));
        let mut thread = KThread::new();
        thread.thread_id = 7;
        thread.priority = 12;
        thread.core_id = 0;
        thread.physical_affinity_mask.set_affinity_mask(0x1);
        thread.global_scheduler_context = Some(Arc::downgrade(&gsc));

        thread.begin_wait();
        thread.set_cancellable();

        thread.wait_cancel();

        let gsc = gsc.lock().unwrap();
        assert_eq!(thread.get_state(), ThreadState::RUNNABLE);
        assert_eq!(gsc.get_scheduled_front(0), Some(7));
    }

    #[test]
    fn test_on_timer_wakes_sleeping_thread() {
        let mut thread = KThread::new();

        thread.wait_result = RESULT_SUCCESS.get_inner_value();
        thread.begin_wait_with_queue(KThreadQueueWithoutEndWait::new().base);
        thread.set_wait_reason_for_debugging(ThreadWaitReasonForDebugging::Sleep);
        assert_eq!(thread.get_state(), ThreadState::WAITING);
        thread.waiting_lock_info = Some(WaitingLockRef {
            owner_thread_id: 99,
            address_key: KProcessAddress::new(0x1234),
            is_kernel_address_key: false,
            owner_thread_ptr: 0,
        });

        thread.on_timer();

        assert_eq!(thread.get_state(), ThreadState::RUNNABLE);
        assert!(thread.waiting_lock_info.is_none());
        assert_eq!(thread.get_wait_result(), RESULT_TIMED_OUT.get_inner_value());
    }

    #[test]
    fn test_run_thread_marks_thread_runnable_and_increments_parent_running_count() {
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut thread_guard = thread.lock().unwrap();
            thread_guard.parent = Some(Arc::downgrade(&process));
            thread_guard.thread_type = ThreadType::User;
        }

        let result = KThread::run_thread(&thread);

        assert_eq!(result, RESULT_SUCCESS.get_inner_value());
        assert_eq!(thread.lock().unwrap().get_state(), ThreadState::RUNNABLE);
        assert_eq!(
            process
                .lock()
                .unwrap()
                .num_running_threads
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn test_state_transition_requests_schedule_via_parent_process() {
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let scheduler = Arc::new(Mutex::new(KScheduler::new(0)));
        process.lock().unwrap().attach_scheduler(&scheduler);

        let mut thread = KThread::new();
        thread.thread_id = 1;
        thread.parent = Some(Arc::downgrade(&process));
        thread.scheduler = Some(Arc::downgrade(&scheduler));
        thread.set_state(ThreadState::RUNNABLE);
        scheduler
            .lock()
            .unwrap()
            .state
            .needs_scheduling
            .store(false, Ordering::Relaxed);

        thread.begin_wait();

        assert!(scheduler.lock().unwrap().needs_scheduling());
    }

    #[test]
    fn test_priority_change_requests_schedule_via_parent_process() {
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let scheduler = Arc::new(Mutex::new(KScheduler::new(0)));
        process.lock().unwrap().attach_scheduler(&scheduler);

        let mut thread = KThread::new();
        thread.thread_id = 1;
        thread.parent = Some(Arc::downgrade(&process));
        thread.scheduler = Some(Arc::downgrade(&scheduler));
        thread.priority = 44;
        thread.base_priority = 44;
        thread.set_state(ThreadState::RUNNABLE);
        scheduler
            .lock()
            .unwrap()
            .state
            .needs_scheduling
            .store(false, Ordering::Relaxed);

        thread.set_base_priority(30);

        assert!(scheduler.lock().unwrap().needs_scheduling());
    }

    #[test]
    fn priority_change_for_waiting_thread_does_not_touch_priority_queue() {
        let gsc = Arc::new(Mutex::new(GlobalSchedulerContext::new()));
        let mut thread = KThread::new();
        thread.thread_id = 52;
        thread.priority = 44;
        thread.base_priority = 44;
        thread.core_id = 2;
        thread.physical_affinity_mask.set_affinity_mask(0b0100);
        thread.global_scheduler_context = Some(Arc::downgrade(&gsc));

        thread.set_state(ThreadState::RUNNABLE);
        thread.begin_wait();
        {
            let gsc = gsc.lock().unwrap();
            assert_eq!(gsc.get_scheduled_front(2), None);
            gsc.m_scheduler_update_needed
                .store(false, Ordering::Relaxed);
        }

        thread.set_base_priority(30);

        let gsc = gsc.lock().unwrap();
        assert_eq!(thread.get_raw_state(), ThreadState::WAITING);
        assert_eq!(gsc.get_scheduled_front(2), None);
        assert!(!gsc.m_scheduler_update_needed.load(Ordering::Relaxed));
    }

    #[test]
    fn test_add_held_lock_only_sets_owner_and_links_lock() {
        let mut thread = KThread::new();
        thread.thread_id = 7;

        let mut lock_info =
            LockWithPriorityInheritanceInfo::new(KProcessAddress::new(0x4000), true);
        lock_info.add_waiter(3, 10, 0);
        lock_info.add_waiter(5, 11, 0);

        thread.add_held_lock(lock_info);

        assert_eq!(thread.get_num_kernel_waiters(), 0);
        assert_eq!(thread.held_lock_info_list.len(), 1);
        assert_eq!(thread.held_lock_info_list[0].get_owner_thread_id(), 7);
    }

    #[test]
    fn test_remove_waiter_by_thread_id_restores_priority() {
        let mut thread = KThread::new();
        thread.thread_id = 1;
        thread.base_priority = 10;
        thread.priority = 3;

        let mut lock_info =
            LockWithPriorityInheritanceInfo::new(KProcessAddress::new(0x5000), false);
        lock_info.add_waiter(3, 2, 0);
        thread.add_held_lock(lock_info);

        thread.remove_waiter_by_thread_id(2);

        assert_eq!(thread.priority, 10);
        assert!(thread.held_lock_info_list.is_empty());
    }

    #[test]
    fn restore_priority_walks_waiting_lock_owner_chain() {
        let mut process = KProcess::new();

        let owner = Arc::new(KThreadLock::new(KThread::new()));
        let middle = Arc::new(KThreadLock::new(KThread::new()));
        let high = Arc::new(KThreadLock::new(KThread::new()));

        for (thread, thread_id, object_id, priority) in [
            (&owner, 1, 101, 20),
            (&middle, 2, 102, 10),
            (&high, 3, 103, 3),
        ] {
            let mut guard = thread.lock().unwrap();
            guard.thread_id = thread_id;
            guard.object_id = object_id;
            guard.base_priority = priority;
            guard.priority = priority;
        }

        process.register_thread_object(owner.clone());
        process.register_thread_object(middle.clone());
        process.register_thread_object(high.clone());

        middle
            .lock()
            .unwrap()
            .set_user_address_key(KProcessAddress::new(0x1000), 2);
        KThread::add_waiter_with_process(&mut process, &owner, &middle);
        assert_eq!(owner.lock().unwrap().get_priority(), 10);

        high.lock()
            .unwrap()
            .set_user_address_key(KProcessAddress::new(0x2000), 3);
        KThread::add_waiter_with_process(&mut process, &middle, &high);

        assert_eq!(middle.lock().unwrap().get_priority(), 3);
        assert_eq!(owner.lock().unwrap().get_priority(), 3);
        assert_eq!(
            owner.lock().unwrap().held_lock_info_list[0]
                .get_highest_priority_waiter()
                .unwrap()
                .priority,
            3
        );
    }

    #[test]
    fn set_base_priority_with_process_preserves_inherited_priority() {
        let mut process = KProcess::new();

        let owner = Arc::new(KThreadLock::new(KThread::new()));
        let waiter = Arc::new(KThreadLock::new(KThread::new()));

        for (thread, thread_id, object_id, priority) in [(&owner, 1, 101, 20), (&waiter, 2, 102, 3)]
        {
            let mut guard = thread.lock().unwrap();
            guard.thread_id = thread_id;
            guard.object_id = object_id;
            guard.base_priority = priority;
            guard.priority = priority;
        }

        process.register_thread_object(owner.clone());
        process.register_thread_object(waiter.clone());

        waiter
            .lock()
            .unwrap()
            .set_user_address_key(KProcessAddress::new(0x3000), 2);
        KThread::add_waiter_with_process(&mut process, &owner, &waiter);
        assert_eq!(owner.lock().unwrap().get_priority(), 3);

        KThread::set_base_priority_with_process(&mut process, 1, 15);
        assert_eq!(owner.lock().unwrap().get_base_priority(), 15);
        assert_eq!(
            owner.lock().unwrap().get_priority(),
            3,
            "base priority changes must not discard inherited waiter priority"
        );

        KThread::set_base_priority_with_process(&mut process, 1, 1);
        assert_eq!(owner.lock().unwrap().get_base_priority(), 1);
        assert_eq!(
            owner.lock().unwrap().get_priority(),
            1,
            "effective priority follows the highest-priority source"
        );
    }

    #[test]
    fn test_remove_waiter_by_key_returns_transfer_lock_without_reinserting_on_old_owner() {
        let mut owner = KThread::new();
        owner.thread_id = 1;
        owner.base_priority = 10;
        owner.priority = 3;

        let mut lock_info =
            LockWithPriorityInheritanceInfo::new(KProcessAddress::new(0x5000), false);
        lock_info.add_waiter(3, 2, 0x1234);
        lock_info.add_waiter(5, 3, 0x5678);
        owner.add_held_lock(lock_info);

        let mut has_waiters = false;
        let result =
            owner.remove_waiter_by_key(KProcessAddress::new(0x5000), false, &mut has_waiters);

        let (next_owner_thread_id, next_owner_priority, next_owner_thread_ptr, transfer_lock_info) =
            result.expect("expected next owner");
        assert_eq!(next_owner_thread_id, 2);
        assert_eq!(next_owner_priority, 3);
        assert_eq!(next_owner_thread_ptr, 0x1234);
        assert!(has_waiters);
        assert!(transfer_lock_info.is_some());
        assert!(
            owner.held_lock_info_list.is_empty(),
            "old owner must not retain transferred lock info"
        );
        assert_eq!(owner.priority, 10);
    }

    #[test]
    fn initialize_service_thread_registers_with_global_scheduler_context() {
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let scheduler = Arc::new(Mutex::new(KScheduler::new(3)));
        let gsc = Arc::new(Mutex::new(GlobalSchedulerContext::new()));
        {
            let mut process_guard = process.lock().unwrap();
            process_guard.attach_scheduler(&scheduler);
            process_guard.global_scheduler_context = Some(gsc.clone());
        }

        let thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut thread_guard = thread.lock().unwrap();
            thread_guard.initialize_service_thread(
                SystemRef::null(),
                &thread,
                Box::new(|| {}),
                16,
                3,
                &process,
                0x1234,
                0x5678,
            );
        }

        let registered = gsc.lock().unwrap().get_thread_by_thread_id(0x1234);
        assert!(registered.is_some());
        assert_eq!(registered.unwrap().lock().unwrap().get_object_id(), 0x5678);
    }

    #[test]
    fn initialize_main_thread_inherits_scheduler_context_from_owner() {
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let scheduler = Arc::new(Mutex::new(KScheduler::new(0)));
        let gsc = Arc::new(Mutex::new(GlobalSchedulerContext::new()));
        {
            let mut process_guard = process.lock().unwrap();
            process_guard.attach_scheduler(&scheduler);
            process_guard.global_scheduler_context = Some(gsc.clone());
        }

        let mut thread = KThread::new();
        thread.initialize_main_thread_with_func(
            0x200000, 0x240000, 0, 0x23f000, &process, 17, 0x99, false, None,
        );

        let inherited_scheduler = thread
            .scheduler
            .as_ref()
            .and_then(Weak::upgrade)
            .expect("main thread should inherit scheduler");
        let inherited_gsc = thread
            .global_scheduler_context
            .as_ref()
            .and_then(Weak::upgrade)
            .expect("main thread should inherit global scheduler context");

        assert!(Arc::ptr_eq(&inherited_scheduler, &scheduler));
        assert!(Arc::ptr_eq(&inherited_gsc, &gsc));
        assert!(thread.process_schedule_count.is_some());
    }

    #[test]
    fn initialize_host_context_creates_fiber_eagerly() {
        let runs = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let runs_for_init = Arc::clone(&runs);

        let mut thread = KThread::new();
        thread.initialize_host_context(Some(Box::new(move || {
            runs_for_init.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })));

        let first = thread
            .get_host_context()
            .cloned()
            .expect("host context should be created eagerly");
        let second = thread
            .get_host_context()
            .cloned()
            .expect("host context should be reused");

        assert!(Arc::ptr_eq(&first, &second));
        assert!(thread.host_context.is_some());
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn test_finalize_unregisters_thread_object_from_owner_process() {
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let thread = Arc::new(KThreadLock::new(KThread::new()));

        {
            let mut guard = thread.lock().unwrap();
            guard.thread_id = 11;
            guard.object_id = 22;
            guard.parent = Some(Arc::downgrade(&process));
        }
        process
            .lock()
            .unwrap()
            .register_thread_object(thread.clone());

        thread.lock().unwrap().finalize();

        let process_guard = process.lock().unwrap();
        assert!(process_guard.get_thread_by_object_id(22).is_none());
        assert!(process_guard.get_thread_by_thread_id(11).is_none());
        assert!(!process_guard.thread_list.contains(&11));
    }

    #[test]
    fn terminate_thread_waits_for_finish_termination_signal() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::thread;
        use std::time::Duration;

        let target = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut guard = target.lock().unwrap();
            guard.object_id = 44;
            guard.thread_id = 7;
            guard.bind_self_reference(&target);
            guard.set_state(ThreadState::RUNNABLE);
        }

        let completed = Arc::new(AtomicBool::new(false));
        let completed_clone = completed.clone();
        let target_clone = target.clone();
        let waiter = thread::spawn(move || {
            let result = KThread::terminate_thread(&target_clone);
            assert_eq!(result, RESULT_SUCCESS.get_inner_value());
            completed_clone.store(true, Ordering::SeqCst);
        });

        thread::sleep(Duration::from_millis(10));
        assert!(!completed.load(Ordering::SeqCst));

        target.lock().unwrap().exit();

        waiter.join().unwrap();
        assert!(completed.load(Ordering::SeqCst));
        assert!(target.lock().unwrap().is_signaled());
    }

    #[test]
    fn request_terminate_runnable_thread_requests_interrupt_reschedule() {
        let scheduler = Arc::new(Mutex::new(KScheduler::new(0)));
        let mut thread = KThread::new();
        thread.thread_id = 9;
        thread.base_priority = 44;
        thread.priority = 44;
        thread.scheduler = Some(Arc::downgrade(&scheduler));
        thread.set_state(ThreadState::RUNNABLE);

        scheduler
            .lock()
            .unwrap()
            .state
            .needs_scheduling
            .store(false, Ordering::Relaxed);

        thread.request_terminate();

        assert_eq!(
            thread.get_priority(),
            crate::hle::kernel::svc_types::SYSTEM_THREAD_PRIORITY_HIGHEST - 1
        );
        assert_eq!(
            thread.get_base_priority(),
            crate::hle::kernel::svc_types::SYSTEM_THREAD_PRIORITY_HIGHEST - 1
        );
        assert!(scheduler.lock().unwrap().needs_scheduling());
    }

    #[test]
    fn test_core_mask_change_requests_schedule_via_thread_scheduler() {
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let scheduler = Arc::new(Mutex::new(KScheduler::new(0)));
        let gsc = Arc::new(Mutex::new(GlobalSchedulerContext::new()));
        scheduler.lock().unwrap().global_scheduler_context = Some(gsc.clone());
        process.lock().unwrap().attach_scheduler(&scheduler);

        let mut thread = KThread::new();
        thread.thread_id = 1;
        thread.parent = Some(Arc::downgrade(&process));
        thread.scheduler = Some(Arc::downgrade(&scheduler));
        thread.global_scheduler_context = Some(Arc::downgrade(&gsc));
        thread.virtual_ideal_core_id = 0;
        thread.virtual_affinity_mask = 0x1;
        thread.set_state(ThreadState::RUNNABLE);
        gsc.lock()
            .unwrap()
            .m_scheduler_update_needed
            .store(false, Ordering::Relaxed);

        assert_eq!(
            thread.set_core_mask(1, 0x2),
            RESULT_SUCCESS.get_inner_value()
        );
        assert!(gsc
            .lock()
            .unwrap()
            .m_scheduler_update_needed
            .load(Ordering::Relaxed));
    }

    #[test]
    fn unpin_resumes_threads_in_pinned_waiter_list() {
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let owner = Arc::new(KThreadLock::new(KThread::new()));
        let waiter = Arc::new(KThreadLock::new(KThread::new()));

        {
            let mut owner_guard = owner.lock().unwrap();
            owner_guard.thread_id = 1;
            owner_guard.object_id = 1;
            owner_guard.parent = Some(Arc::downgrade(&process));
            owner_guard.stack_parameters.is_pinned = true;
            owner_guard.num_core_migration_disables = 1;
            owner_guard.physical_ideal_core_id = 0;
            owner_guard.physical_affinity_mask.set_affinity_mask(0x1);
            owner_guard.original_physical_ideal_core_id = 0;
            owner_guard
                .original_physical_affinity_mask
                .set_affinity_mask(0x1);
            owner_guard.pinned_waiter_list.push(2);
        }
        {
            let mut waiter_guard = waiter.lock().unwrap();
            waiter_guard.thread_id = 2;
            waiter_guard.object_id = 2;
            waiter_guard.parent = Some(Arc::downgrade(&process));
            waiter_guard.begin_wait_with_queue(thread_queue_for_k_thread_set_property(&owner));
            assert_eq!(waiter_guard.get_state(), ThreadState::WAITING);
        }
        {
            let mut process_guard = process.lock().unwrap();
            process_guard.register_thread_object(owner.clone());
            process_guard.register_thread_object(waiter.clone());
        }

        owner.lock().unwrap().unpin();

        assert!(owner.lock().unwrap().pinned_waiter_list.is_empty());
        let waiter_guard = waiter.lock().unwrap();
        assert_eq!(waiter_guard.get_state(), ThreadState::RUNNABLE);
        assert_eq!(waiter_guard.wait_result, RESULT_SUCCESS.get_inner_value());
    }

    #[test]
    fn test_core_mask_dont_care_rehomes_active_core_to_allowed_affinity() {
        let mut thread = KThread::new();
        thread.thread_id = 1;
        thread.set_state(ThreadState::RUNNABLE);
        thread.virtual_ideal_core_id = crate::hle::kernel::svc_types::IDEAL_CORE_DONT_CARE;
        thread.virtual_affinity_mask = 0x1;
        thread.physical_ideal_core_id = crate::hle::kernel::svc_types::IDEAL_CORE_DONT_CARE;
        thread.physical_affinity_mask.set_affinity_mask(0x1);
        thread.set_active_core(0);
        thread.set_current_core(0);

        assert_eq!(
            thread.set_core_mask(crate::hle::kernel::svc_types::IDEAL_CORE_DONT_CARE, 0x2),
            RESULT_SUCCESS.get_inner_value()
        );
        assert_eq!(thread.get_active_core(), 1);
        assert_eq!(thread.physical_affinity_mask.get_affinity_mask(), 0x2);
    }

    #[test]
    fn test_get_physical_core_mask_selects_current_or_original_mask() {
        let mut thread = KThread::new();
        thread.physical_ideal_core_id = 1;
        thread.physical_affinity_mask.set_affinity_mask(0x2);
        thread.original_physical_ideal_core_id = 3;
        thread
            .original_physical_affinity_mask
            .set_affinity_mask(0x8);

        thread.num_core_migration_disables = 0;
        assert_eq!(thread.get_physical_core_mask(), (1, 0x2));

        thread.num_core_migration_disables = 1;
        assert_eq!(thread.get_physical_core_mask(), (3, 0x8));
    }

    #[test]
    fn test_activity_change_requests_schedule_via_thread_scheduler() {
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let scheduler = Arc::new(Mutex::new(KScheduler::new(0)));
        process.lock().unwrap().attach_scheduler(&scheduler);

        let mut thread = KThread::new();
        thread.thread_id = 1;
        thread.parent = Some(Arc::downgrade(&process));
        thread.scheduler = Some(Arc::downgrade(&scheduler));
        thread.set_state(ThreadState::RUNNABLE);
        scheduler
            .lock()
            .unwrap()
            .state
            .needs_scheduling
            .store(false, Ordering::Relaxed);

        assert_eq!(thread.set_activity(1), RESULT_SUCCESS.get_inner_value());
        assert!(scheduler.lock().unwrap().needs_scheduling());
    }

    #[test]
    fn capture_and_restore_guest_context_preserve_independent_registers() {
        let mut thread = KThread::new();
        let mut ctx = crate::arm::arm_interface::ThreadContext::default();
        ctx.fp = 0x1111;
        ctx.sp = 0x2222;
        ctx.lr = 0x3333;
        ctx.pc = 0x4444;
        ctx.r[11] = 0xAAAA;
        ctx.r[13] = 0xBBBB;
        ctx.r[14] = 0xCCCC;
        ctx.r[15] = 0xDDDD;

        thread.capture_guest_context(&ctx);

        assert_eq!(thread.thread_context.r[11], 0xAAAA);
        assert_eq!(thread.thread_context.r[13], 0xBBBB);
        assert_eq!(thread.thread_context.r[14], 0xCCCC);
        assert_eq!(thread.thread_context.r[15], 0xDDDD);
        assert_eq!(thread.thread_context.fp, 0x1111);
        assert_eq!(thread.thread_context.sp, 0x2222);
        assert_eq!(thread.thread_context.lr, 0x3333);
        assert_eq!(thread.thread_context.pc, 0x4444);

        let mut restored = crate::arm::arm_interface::ThreadContext::default();
        thread.restore_guest_context(&mut restored);

        assert_eq!(restored.fp, 0x1111);
        assert_eq!(restored.sp, 0x2222);
        assert_eq!(restored.lr, 0x3333);
        assert_eq!(restored.pc, 0x4444);
        assert_eq!(restored.r[11], 0xAAAA);
        assert_eq!(restored.r[13], 0xBBBB);
        assert_eq!(restored.r[14], 0xCCCC);
        assert_eq!(restored.r[15], 0xDDDD);
    }

    #[test]
    fn initialize_high_priority_thread_matches_shutdown_thread_contract() {
        let mut thread = KThread::new();

        thread.initialize_high_priority_thread(2, 11, 22, Some(Box::new(|| {})));

        assert_eq!(thread.thread_type, ThreadType::HighPriority);
        assert_eq!(thread.priority, SVC_HIGHEST_THREAD_PRIORITY);
        assert_eq!(thread.base_priority, SVC_HIGHEST_THREAD_PRIORITY);
        assert_eq!(thread.get_state(), ThreadState::INITIALIZED);
        assert!(thread.parent.is_none());
        assert!(thread.get_host_context().is_some());
    }

    #[test]
    fn internal_end_wait_does_not_overwrite_guest_registers() {
        let scheduler_lock = k_scheduler_lock::KAbstractSchedulerLock::new();
        let mut thread = KThread::new();
        thread.scheduler_lock_ptr =
            (&scheduler_lock as *const k_scheduler_lock::KAbstractSchedulerLock) as usize;
        thread.begin_wait();
        thread.thread_context.r[0] = 0x1701_D7;
        thread.thread_context.r[1] = 0x2105_4000_48;
        thread.synced_index = 3;

        let _outer = KScopedSchedulerLock::new(&scheduler_lock);
        thread.end_wait(RESULT_SUCCESS.get_inner_value());

        assert_eq!(thread.get_state(), ThreadState::RUNNABLE);
        assert!(!thread.has_wait_queue());
        assert_eq!(thread.get_wait_result(), RESULT_SUCCESS.get_inner_value());
        assert_eq!(thread.thread_context.r[0], 0x1701_D7);
        assert_eq!(thread.thread_context.r[1], 0x2105_4000_48);
    }
}

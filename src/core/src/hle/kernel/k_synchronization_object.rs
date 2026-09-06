//! Port of zuyu/src/core/hle/kernel/k_synchronization_object.{h,cpp}
//! Status: COMPLET (upstream-faithful raw-pointer intrusive list)
//! Derniere synchro: 2026-04-19
//!
//! KSynchronizationObject — base class for kernel objects that threads can wait
//! on. Extends KAutoObjectWithList.
//!
//! Waiter storage mirrors upstream: an intrusive linked list of
//! `ThreadListNode` structures is stored per sync object; each node is owned by
//! the waiting thread's `Wait()` stack frame. Traversal/signal paths only hold
//! the scheduler lock — they do NOT need `&KProcess`.

use std::marker::PhantomPinned;
use std::ptr;
use std::sync::{Arc, Mutex, Weak};

use super::k_auto_object::{KAutoObjectBase, KAutoObjectWithList, TypeObj};
use super::k_class_token;
use super::k_port::KPort;
use super::k_process::{KProcess, ProcessLock};
use super::k_readable_event::KReadableEvent;
use super::k_scoped_scheduler_lock_and_sleep::KScopedSchedulerLockAndSleep;
use super::k_server_session::KServerSession;
use super::k_thread::{KThread, KThreadLock};
use super::k_thread_queue::{KThreadQueue, KThreadQueueWithoutEndWait};
use crate::hle::kernel::svc::svc_results::{
    RESULT_CANCELLED, RESULT_INVALID_HANDLE, RESULT_TERMINATION_REQUESTED, RESULT_TIMED_OUT,
};
use crate::hle::result::ResultCode;

/// Maximum number of sync objects a single Wait() can target.
/// Upstream: `Svc::ArgumentHandleCountMax == 64`.
pub const ARGUMENT_HANDLE_COUNT_MAX: usize = 64;

/// Intrusive linked-list node used by `KSynchronizationObject` to track waiting
/// threads. Mirrors upstream `KSynchronizationObject::ThreadListNode`.
///
/// Each node is stored in a per-wait buffer owned by `KThread::wait_nodes`
/// (allocated by `wait()` before linking, cleared on wake). The signal path
/// dereferences `thread` as `Weak<KThreadLock>` and upgrades under the
/// scheduler lock.
pub struct ThreadListNode {
    pub next: *mut ThreadListNode,
    /// Weak ref to the waiter. Upgraded on signal to call notify_available.
    pub thread: Weak<KThreadLock>,
    /// Diagnostic ID of the object this node is linked into. Wait selection
    /// compares native synchronization-state pointers, not numeric IDs.
    pub object_id: u64,
    _pin: PhantomPinned,
}

impl ThreadListNode {
    pub fn new() -> Self {
        Self {
            next: ptr::null_mut(),
            thread: Weak::new(),
            object_id: 0,
            _pin: PhantomPinned,
        }
    }
}

// Safety: raw next pointer and object_id are only read/written under the
// scheduler lock; Weak<KThreadLock> is Send+Sync. Nodes are otherwise
// owned by a single waiter thread.
unsafe impl Send for ThreadListNode {}
unsafe impl Sync for ThreadListNode {}

/// Trait for objects that can be signaled and waited on.
/// Mirrors the pure virtual `IsSignaled()` in upstream.
pub trait KSynchronizable {
    fn is_signaled(&self) -> bool;
}

/// Waiter list state embedded in every waitable kernel object.
/// Upstream: the raw pointer fields on `KSynchronizationObject` itself.
///
/// Every mutation and traversal must happen with the scheduler lock held.
pub struct SynchronizationObjectState {
    head: *mut ThreadListNode,
    tail: *mut ThreadListNode,
}

impl SynchronizationObjectState {
    pub const fn new() -> Self {
        Self {
            head: ptr::null_mut(),
            tail: ptr::null_mut(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.head.is_null()
    }

    /// Diagnostic — pointer value of head, for cross-referencing wait/notify
    /// pairs in trace logs.
    pub fn head_addr(&self) -> *const ThreadListNode {
        self.head
    }

    /// Diagnostic — pointer value of tail.
    pub fn tail_addr(&self) -> *const ThreadListNode {
        self.tail
    }

    /// Link a ThreadListNode to the tail. Mirrors upstream `LinkNode`.
    ///
    /// # Safety
    /// - Caller holds the scheduler lock for the owning kernel.
    /// - `node` is a valid pointer that remains alive until `unlink_node`.
    pub unsafe fn link_node(&mut self, node: *mut ThreadListNode) {
        debug_assert!(!node.is_null());
        (*node).next = ptr::null_mut();
        if self.tail.is_null() {
            self.head = node;
        } else {
            (*self.tail).next = node;
        }
        self.tail = node;
    }

    /// Unlink a ThreadListNode. Mirrors upstream `UnlinkNode`
    /// (k_synchronization_object.h:47-66): the unlinked node's `next` field
    /// is left pointing at the following list element on purpose, because
    /// `KSynchronizationObject::NotifyAvailable`'s for-loop reads it after
    /// the body has unlinked the current node:
    ///   for (cur = head; cur != nullptr; cur = cur->next) { body(cur); }
    ///
    /// # Safety
    /// - Caller holds the scheduler lock for the owning kernel.
    /// - `node` is in this list.
    pub unsafe fn unlink_node(&mut self, node: *mut ThreadListNode) {
        debug_assert!(!node.is_null());
        let mut prev: *mut ThreadListNode = ptr::null_mut();
        let mut cur = self.head;
        while !cur.is_null() && cur != node {
            prev = cur;
            cur = (*cur).next;
        }
        if cur.is_null() {
            // Not found — upstream would UB, we no-op to stay safe.
            debug_assert!(false, "unlink_node: node not found in list");
            return;
        }
        let next = (*cur).next;
        if prev.is_null() {
            self.head = next;
        } else {
            (*prev).next = next;
        }
        if self.tail == cur {
            self.tail = prev;
        }
        // Intentionally do NOT clear `(*cur).next`. Upstream preserves it so
        // that iteration loops can continue past an unlinked-from-list node.
    }

    /// Walk the list under the scheduler lock and collect strong refs to the
    /// waiters. Dead (dropped) threads are filtered out.
    ///
    /// # Safety
    /// Caller must hold the scheduler lock.
    pub unsafe fn waiter_snapshot(&self) -> Vec<Arc<KThreadLock>> {
        let mut v = Vec::new();
        let mut cur = self.head;
        while !cur.is_null() {
            if let Some(t) = (*cur).thread.upgrade() {
                v.push(t);
            }
            cur = (*cur).next;
        }
        v
    }
}

// Safety: list pointers are only mutated under the scheduler lock; nodes
// themselves are Send+Sync.
unsafe impl Send for SynchronizationObjectState {}
unsafe impl Sync for SynchronizationObjectState {}

impl Default for SynchronizationObjectState {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-thread record of which objects a Wait() is targeting plus the raw
/// pointers needed by the wake callback to unlink nodes from other objects.
///
/// The `nodes` buffer owns the `ThreadListNode`s linked into sync objects;
/// pointers into it must remain stable, so it's a `Box<[ThreadListNode]>`.
///
/// `object_states` holds raw pointers to each `SynchronizationObjectState` the
/// thread is linked into. The referenced states live inside kernel objects
/// held by `Arc`s elsewhere; the pointers are valid only while the scheduler
/// lock is held AND the wait is active (i.e. thread is in WAITING state).
pub struct SynchronizationWaitContext {
    pub nodes: Box<[ThreadListNode]>,
    pub object_ids: Vec<u64>,
    /// Keep native objects alive until all nodes have been unlinked, including
    /// when an IPC transaction defers the guest fiber switch past Wait's return.
    objects: Vec<WaitableObject>,
    /// Raw pointers to the SynchronizationObjectState each node is linked into.
    /// SAFETY: only dereferenced under the scheduler lock.
    pub object_states: Vec<*mut SynchronizationObjectState>,
    pub active: bool,
}

impl SynchronizationWaitContext {
    pub fn new() -> Self {
        Self {
            nodes: Box::new([]),
            object_ids: Vec::new(),
            objects: Vec::new(),
            object_states: Vec::new(),
            active: false,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn object_ids(&self) -> &[u64] {
        &self.object_ids
    }

    pub fn clear(&mut self) {
        self.nodes = Box::new([]);
        self.object_ids.clear();
        self.object_states.clear();
        self.active = false;
        self.objects.clear();
    }

    /// Compare native object identity, as upstream compares object pointers.
    /// IDs from process/session/event namespaces need not be distinct.
    pub fn synced_index_for(&self, signaled_object: *const SynchronizationObjectState) -> Option<usize> {
        self.object_states
            .iter()
            .position(|&state| std::ptr::eq(state, signaled_object))
    }
}

// Safety: the raw pointers are only dereferenced under the scheduler lock;
// the struct is otherwise owned by one KThread.
unsafe impl Send for SynchronizationWaitContext {}
unsafe impl Sync for SynchronizationWaitContext {}

impl Default for SynchronizationWaitContext {
    fn default() -> Self {
        Self::new()
    }
}

/// The queue callback that runs when any of the wait-targeted sync objects
/// signals the waiting thread. Mirrors upstream
/// `ThreadQueueImplForKSynchronizationObjectWait`.
pub(crate) struct ThreadQueueImplForKSynchronizationObjectWait;

impl ThreadQueueImplForKSynchronizationObjectWait {
    pub(crate) fn queue() -> KThreadQueue {
        KThreadQueueWithoutEndWait::with_callbacks(
            Some(Self::notify_available),
            Some(Self::cancel_wait),
        )
        .base
    }

    /// Upstream: iterate the wait's nodes, find synced_index, unlink all nodes,
    /// set synced_index on thread, clear cancellable, base EndWait.
    fn notify_available(
        wait_queue: &KThreadQueue,
        thread: &mut KThread,
        signaled_object: *const SynchronizationObjectState,
        wait_result: u32,
    ) -> bool {
        if !thread.sync_wait_context.is_active() {
            return false;
        }

        // Compute synced_index.
        let synced_index = thread
            .sync_wait_context
            .synced_index_for(signaled_object)
            .map(|i| i as i32)
            .unwrap_or(-1);

        // Unlink every node from its object, under scheduler lock.
        unsafe {
            let ctx = &mut thread.sync_wait_context;
            debug_assert_eq!(ctx.nodes.len(), ctx.object_states.len());
            for i in 0..ctx.nodes.len() {
                let state_ptr = ctx.object_states[i];
                if state_ptr.is_null() {
                    continue;
                }
                let node_ptr = &mut ctx.nodes[i] as *mut ThreadListNode;
                (*state_ptr).unlink_node(node_ptr);
            }
            ctx.clear();
        }

        thread.synced_index = synced_index;
        thread.clear_cancellable();
        wait_queue.base_end_wait(thread, wait_result);
        true
    }

    fn cancel_wait(thread: &mut KThread) {
        if !thread.sync_wait_context.is_active() {
            thread.clear_cancellable();
            return;
        }
        unsafe {
            let ctx = &mut thread.sync_wait_context;
            for i in 0..ctx.nodes.len() {
                let state_ptr = ctx.object_states[i];
                if state_ptr.is_null() {
                    continue;
                }
                let node_ptr = &mut ctx.nodes[i] as *mut ThreadListNode;
                (*state_ptr).unlink_node(node_ptr);
            }
            ctx.clear();
        }
        thread.clear_cancellable();
    }
}

/// Enum wrapping the possible sync-object sources so wait() can query the
/// right state_ptr + signaled check per object_id.
pub(crate) enum WaitableObject {
    ReadableEvent {
        _event: Arc<Mutex<KReadableEvent>>,
        is_signaled: *const std::sync::atomic::AtomicBool,
        sync_state: *mut SynchronizationObjectState,
    },
    ClientPort {
        _port: Arc<Mutex<KPort>>,
        port: *mut KPort,
    },
    ServerPort {
        _port: Arc<Mutex<KPort>>,
        port: *mut KPort,
    },
    ServerSession {
        _session: Arc<Mutex<KServerSession>>,
        session: *mut KServerSession,
    },
    Thread(Arc<KThreadLock>),
    Process(Arc<ProcessLock>),
}

impl WaitableObject {
    pub(crate) fn from_readable_event(event: Arc<Mutex<KReadableEvent>>) -> Self {
        let (is_signaled, sync_state) = {
            let mut guard = event.lock().unwrap();
            (
                &guard.is_signaled as *const std::sync::atomic::AtomicBool,
                &mut guard.sync_object as *mut SynchronizationObjectState,
            )
        };
        Self::ReadableEvent {
            _event: event,
            is_signaled,
            sync_state,
        }
    }

    pub(crate) fn from_server_port(port: Arc<Mutex<KPort>>) -> Self {
        let port_ptr = {
            let mut guard = port.lock().unwrap();
            &mut *guard as *mut KPort
        };
        Self::ServerPort {
            _port: port,
            port: port_ptr,
        }
    }

    pub(crate) fn from_server_session(session: Arc<Mutex<KServerSession>>) -> Self {
        let session_ptr = {
            let mut guard = session.lock().unwrap();
            &mut *guard as *mut KServerSession
        };
        Self::ServerSession {
            _session: session,
            session: session_ptr,
        }
    }

    pub(crate) fn from_process(process: Arc<ProcessLock>) -> Self {
        Self::Process(process)
    }

    pub(crate) fn is_signaled(&self) -> bool {
        match self {
            Self::ReadableEvent { is_signaled, .. } => unsafe {
                (**is_signaled).load(std::sync::atomic::Ordering::Relaxed)
            },
            Self::ClientPort { port, .. } => unsafe { (&(**port).client).is_signaled() },
            Self::ServerPort { port, .. } => unsafe { (&(**port).server).is_signaled() },
            Self::ServerSession { session, .. } => unsafe { (&**session).is_signaled() },
            Self::Thread(thread) => thread.lock().unwrap().is_signaled(),
            Self::Process(process) => process.lock().unwrap().is_signaled(),
        }
    }

    /// Acquire a raw pointer to the object's `SynchronizationObjectState`.
    ///
    /// # Safety
    /// The caller must guarantee the state lives at least as long as any node
    /// that is linked into it — i.e. the pointer is used while the underlying
    /// `Arc` is held and the scheduler lock is acquired on touch.
    fn sync_state_ptr(&self) -> *mut SynchronizationObjectState {
        match self {
            Self::ReadableEvent { sync_state, .. } => *sync_state,
            Self::ClientPort { port, .. } => unsafe {
                &mut (**port).client.sync_object as *mut SynchronizationObjectState
            },
            Self::ServerPort { port, .. } => unsafe {
                &mut (**port).server.sync_object as *mut SynchronizationObjectState
            },
            Self::ServerSession { session, .. } => unsafe {
                &mut (**session).sync_object as *mut SynchronizationObjectState
            },
            Self::Thread(thread) => {
                let mut guard = thread.lock().unwrap();
                &mut guard.sync_object as *mut SynchronizationObjectState
            }
            Self::Process(process) => {
                let mut guard = process.lock().unwrap();
                &mut guard.sync_object as *mut SynchronizationObjectState
            }
        }
    }
}

fn resolve_waitable_object(
    process: &Arc<ProcessLock>,
    process_guard: &KProcess,
    object_id: u64,
) -> Option<WaitableObject> {
    if let Some(port) = process_guard.get_server_port_by_object_id(object_id) {
        return Some(WaitableObject::from_server_port(port));
    }
    if let Some(port) = process_guard.get_client_port_by_object_id(object_id) {
        let port_ptr = {
            let mut guard = port.lock().unwrap();
            &mut *guard as *mut KPort
        };
        return Some(WaitableObject::ClientPort {
            _port: port,
            port: port_ptr,
        });
    }
    if let Some(event) = process_guard.get_readable_event_by_object_id(object_id) {
        return Some(WaitableObject::from_readable_event(event));
    }
    if let Some(session) = process_guard.get_server_session_by_object_id(object_id) {
        return Some(WaitableObject::from_server_session(session));
    }
    if let Some(thread) = process_guard.get_thread_by_object_id(object_id) {
        return Some(WaitableObject::Thread(thread));
    }
    if process_guard.process_id == object_id {
        return Some(WaitableObject::from_process(process.clone()));
    }
    None
}

pub fn is_object_signaled(process: &KProcess, object_id: u64) -> bool {
    // Compatibility helper for older call sites: resolve through a temporary
    // process Arc is not possible here, so keep the previous direct behavior
    // for non-process objects and handle process inline.
    if let Some(port) = process.get_server_port_by_object_id(object_id) {
        return port.lock().unwrap().server.is_signaled();
    }
    if let Some(port) = process.get_client_port_by_object_id(object_id) {
        return port.lock().unwrap().client.is_signaled();
    }
    if let Some(event) = process.get_readable_event_by_object_id(object_id) {
        return event.lock().unwrap().is_signaled();
    }
    if let Some(session) = process.get_server_session_by_object_id(object_id) {
        return session.lock().unwrap().is_signaled();
    }
    if let Some(thread) = process.get_thread_by_object_id(object_id) {
        return thread.lock().unwrap().is_signaled();
    }
    if process.process_id == object_id {
        return process.is_signaled();
    }
    false
}

fn first_signaled_waitable_index(objects: &[WaitableObject]) -> Option<usize> {
    objects.iter().position(|object| object.is_signaled())
}

fn resolve_waitable_objects(
    process: &Arc<ProcessLock>,
    object_ids: &[u64],
) -> Option<Vec<WaitableObject>> {
    let process_guard = process.lock().unwrap();
    object_ids
        .iter()
        .map(|&oid| resolve_waitable_object(process, &process_guard, oid))
        .collect()
}

/// KSynchronizationObject — kernel object that threads can wait on.
///
/// Mirrors upstream `KSynchronizationObject : public KAutoObjectWithList`.
///
/// In ruzu each waitable kernel type keeps its own embedded
/// `SynchronizationObjectState` field instead of inheriting this base directly
/// (Rust has no multiple inheritance). This type is retained for the
/// type-token plumbing expected by KAutoObject.
pub struct KSynchronizationObject {
    pub base: KAutoObjectWithList,
    pub sync_object: SynchronizationObjectState,
}

impl KSynchronizationObject {
    pub fn new(kernel: usize) -> Self {
        Self {
            base: KAutoObjectWithList::new(kernel),
            sync_object: SynchronizationObjectState::new(),
        }
    }

    pub fn finalize(&self) {
        self.on_finalize_synchronization_object();
    }

    pub fn on_finalize_synchronization_object(&self) {}

    /// Mirror of upstream `GetWaitingThreadsForDebugging()`. Caller is
    /// responsible for scheduler-lock scoping.
    ///
    /// # Safety
    /// Call under scheduler lock only.
    pub unsafe fn get_waiting_threads_for_debugging(&self) -> Vec<Arc<KThreadLock>> {
        self.sync_object.waiter_snapshot()
    }
}

impl KAutoObjectBase for KSynchronizationObject {
    fn get_type_obj(&self) -> TypeObj {
        Self::get_static_type_obj()
    }

    fn get_type_name(&self) -> &'static str {
        Self::get_static_type_name()
    }
}

impl KSynchronizationObject {
    pub fn get_static_type_obj() -> TypeObj {
        TypeObj::new(
            "KSynchronizationObject",
            k_class_token::class_token(k_class_token::ObjectType::KSynchronizationObject),
        )
    }

    pub fn get_static_type_name() -> &'static str {
        "KSynchronizationObject"
    }
}

/// Walk a sync object's waiter list and call `notify_available` on each
/// thread. Mirrors upstream `KSynchronizationObject::NotifyAvailable`.
///
/// Collects strong waiter references before invoking queue callbacks, which
/// unlink their nodes. The caller's recursive scheduler lock stays held during
/// both traversal and notification, as upstream requires.
///
/// # Safety
/// The caller must guarantee `state` remains live for the duration of this
/// call (it's behind an Arc held by the signaler), and hold the scheduler lock.
pub unsafe fn notify_waiters_on_state(
    state: &SynchronizationObjectState,
    signaled_object_id: u64,
    result: u32,
) -> bool {
    let waiters = state.waiter_snapshot();
    if std::env::var_os("RUZU_TRACE_NOTIFY_WAITERS").is_some() {
        let n = waiters.len();
        log::info!(
            "[NOTIFY] object_id={} waiters={} state_addr={:p} head={:?} tail={:?}",
            signaled_object_id,
            n,
            state as *const _,
            state.head_addr(),
            state.tail_addr(),
        );
    }
    let mut woke_any = false;
    for thread in waiters {
        if common::trace::is_enabled(common::trace::cat::HOST_THREAD_IPC) {
            common::trace::emit_raw(
                common::trace::cat::HOST_THREAD_IPC,
                &[33, signaled_object_id],
            );
        }
        let mut guard = thread.lock().unwrap();
        if common::trace::is_enabled(common::trace::cat::HOST_THREAD_IPC) {
            common::trace::emit_raw(
                common::trace::cat::HOST_THREAD_IPC,
                &[34, signaled_object_id],
            );
        }
        if guard.notify_available(state, result) {
            woke_any = true;
        }
    }
    woke_any
}

/// Wait on a set of synchronization objects identified by object_id.
///
/// Mirrors upstream `KSynchronizationObject::Wait`. Key differences:
/// - Ruzu resolves object_ids through the process's object tables (upstream
///   passes `KSynchronizationObject**` directly).
/// - The `ThreadListNode` buffer lives in the wait() function's stack frame
///   (via a `Box<[ThreadListNode]>` owned by the thread's `sync_wait_context`).
/// - The fiber switch driven by `KScopedSchedulerLockAndSleep` matches upstream.
pub fn wait(
    process: &Arc<ProcessLock>,
    current_thread: &Arc<KThreadLock>,
    _scheduler: &Arc<Mutex<super::k_scheduler::KScheduler>>,
    out_index: &mut i32,
    object_ids: Vec<u64>,
    timeout_ns: i64,
) -> ResultCode {
    let Some(waitable_objects) = resolve_waitable_objects(process, &object_ids) else {
        return RESULT_INVALID_HANDLE;
    };

    wait_on_objects(
        current_thread,
        out_index,
        object_ids,
        waitable_objects,
        timeout_ns,
    )
}

/// Wait on synchronization objects already owned by the caller.
///
/// This is the direct counterpart of upstream
/// `KSynchronizationObject::Wait(kernel, ..., KSynchronizationObject** objects,
/// ...)`. SVC entry points first resolve guest handles through their process
/// table and call `wait`; `Service::MultiWait` already owns native objects and
/// calls this function without a second process-table lookup.
pub(crate) fn wait_on_objects(
    current_thread: &Arc<KThreadLock>,
    out_index: &mut i32,
    object_ids: Vec<u64>,
    waitable_objects: Vec<WaitableObject>,
    timeout_ns: i64,
) -> ResultCode {
    debug_assert_eq!(object_ids.len(), waitable_objects.len());

    let current_thread_id = current_thread.lock().unwrap().thread_id;
    // Upstream's `KSynchronizationObject::Wait` opens
    // `KScopedSchedulerLockAndSleep slp(kernel, ...)` unconditionally.
    // Use the kernel singleton's scheduler_lock so the lock scope matches
    // upstream even at lifecycle points where the per-thread cache was zero.
    let scheduler_lock = super::kernel::scheduler_lock()
        .expect("scheduler_lock must exist — kernel not initialized?");
    let hardware_timer = super::kernel::get_hardware_timer_arc();
    let thread_ptr = {
        let guard = current_thread.lock().unwrap();
        &*guard as *const KThread as usize
    };
    let _result = {
        let (mut sleep_guard, timer) = KScopedSchedulerLockAndSleep::new(
            scheduler_lock,
            hardware_timer.as_ref(),
            current_thread_id,
            thread_ptr,
            timeout_ns,
        );

        if current_thread.lock().unwrap().is_termination_requested() {
            sleep_guard.cancel_sleep();
            return RESULT_TERMINATION_REQUESTED;
        }

        if let Some(index) = first_signaled_waitable_index(&waitable_objects) {
            if std::env::var_os("RUZU_TRACE_NOTIFY_WAITERS").is_some() {
                log::info!(
                    "[WAIT_EARLY] tid={} object_ids={:?} signaled_index={} short-circuit (no link)",
                    current_thread_id,
                    object_ids,
                    index,
                );
            }
            *out_index = index as i32;
            sleep_guard.cancel_sleep();
            return crate::hle::result::RESULT_SUCCESS;
        }

        *out_index = -1;
        if timeout_ns == 0 {
            sleep_guard.cancel_sleep();
            return RESULT_TIMED_OUT;
        }

        {
            let mut guard = current_thread.lock().unwrap();
            if guard.is_wait_cancelled() {
                sleep_guard.cancel_sleep();
                guard.clear_wait_cancelled();
                return RESULT_CANCELLED;
            }
        }

        // Allocate the node buffer and resolve sync-state pointers for every
        // object. Once linked these pointers must stay valid until wake.
        let n = object_ids.len();
        let mut nodes: Box<[ThreadListNode]> = (0..n)
            .map(|_| ThreadListNode::new())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let mut state_ptrs: Vec<*mut SynchronizationObjectState> = Vec::with_capacity(n);

        let current_thread_weak = Arc::downgrade(current_thread);
        for (i, oid) in object_ids.iter().enumerate() {
            let state_ptr = waitable_objects[i].sync_state_ptr();
            nodes[i].thread = current_thread_weak.clone();
            nodes[i].object_id = *oid;
            unsafe {
                (*state_ptr).link_node(&mut nodes[i] as *mut ThreadListNode);
            }
            if std::env::var_os("RUZU_TRACE_NOTIFY_WAITERS").is_some() {
                log::info!(
                    "[LINK] tid={} object_id={} state_addr={:p} node={:p}",
                    current_thread_id,
                    *oid,
                    state_ptr,
                    &nodes[i] as *const ThreadListNode,
                );
            }
            state_ptrs.push(state_ptr);
        }

        // Stash the node buffer + state_ptrs on the thread so the queue
        // callback can unlink on wake.
        {
            let mut guard = current_thread.lock().unwrap();
            guard.sync_wait_context = SynchronizationWaitContext {
                nodes,
                object_ids: object_ids.clone(),
                objects: waitable_objects,
                object_states: state_ptrs,
                active: true,
            };
            guard.synced_index = -1;
            guard.wait_result = crate::hle::result::RESULT_SUCCESS.get_inner_value();
            guard.set_cancellable();

            let mut wait_queue = ThreadQueueImplForKSynchronizationObjectWait::queue();
            if let Some(timer) = timer {
                wait_queue.set_hardware_timer(timer);
            }
            guard.begin_wait_with_queue(wait_queue);
            guard.set_wait_reason_for_debugging(
                super::k_thread::ThreadWaitReasonForDebugging::Synchronization,
            );
        }

        crate::hle::result::RESULT_SUCCESS
    };
    // KScopedSchedulerLockAndSleep drops here. Upstream returns to the
    // physical-core scheduler naturally after BeginWait. In ruzu, the
    // CPU-manager post-SVC path observes ThreadState::WAITING and performs
    // the core handoff; spinning here would change the scheduler's current
    // thread while leaving this host core stuck inside the old SVC.
    let thread = current_thread.lock().unwrap();
    *out_index = thread.get_synced_index();
    ResultCode::new(thread.get_wait_result())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn await_kernel_registration(thread: &Arc<KThreadLock>) {
        use super::super::{kernel, k_scheduler_lock::KScopedSchedulerLock};
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let linked = {
                let _lock = KScopedSchedulerLock::new(kernel::scheduler_lock().unwrap());
                thread.lock().unwrap().sync_wait_context.is_active()
            };
            if linked { return; }
            assert!(std::time::Instant::now() < deadline, "waiter did not link");
            std::thread::yield_now();
        }
    }

    #[test]
    fn host_early_signals_preserve_order_and_pending_cancellation() {
        let mut kernel = Box::new(super::super::kernel::KernelCore::new());
        kernel.initialize();
        let thread = kernel.get_current_emu_thread().unwrap();
        let first = Arc::new(Mutex::new(KReadableEvent::new()));
        let second = Arc::new(Mutex::new(KReadableEvent::new()));
        first.lock().unwrap().initialize(0, 1);
        second.lock().unwrap().initialize(0, 2);
        second.lock().unwrap().signal();
        first.lock().unwrap().signal();
        thread.lock().unwrap().wait_cancel();
        let mut index = -1;
        let result = wait_on_objects(&thread, &mut index, vec![1, 2], vec![
            WaitableObject::from_readable_event(Arc::clone(&first)),
            WaitableObject::from_readable_event(Arc::clone(&second)),
        ], -1);
        assert_eq!((result, index), (crate::hle::result::RESULT_SUCCESS, 0));
        assert!(thread.lock().unwrap().is_wait_cancelled());
        first.lock().unwrap().clear();
        second.lock().unwrap().clear();
        index = -1;
        let result = wait_on_objects(&thread, &mut index, vec![1],
            vec![WaitableObject::from_readable_event(Arc::clone(&first))], -1);
        assert_eq!((result, index), (RESULT_CANCELLED, -1));
        assert!(!thread.lock().unwrap().is_wait_cancelled());
        assert!(first.lock().unwrap().sync_object.is_empty());
        assert!(second.lock().unwrap().sync_object.is_empty());
        kernel.shutdown();
    }

    #[test]
    fn dummy_wakeup_before_condition_variable_sleep_is_remembered() {
        let mut kernel = Box::new(super::super::kernel::KernelCore::new());
        kernel.initialize();
        let thread = kernel.get_current_emu_thread().unwrap();
        {
            let _lock = super::super::k_scheduler_lock::KScopedSchedulerLock::new(
                super::super::kernel::scheduler_lock().unwrap());
            let thread = thread.lock().unwrap();
            thread.request_dummy_thread_wait();
            thread.dummy_thread_end_wait();
        }
        KThread::dummy_thread_begin_wait(&thread);
        assert!(*thread.lock().unwrap().dummy_thread_wait.0.lock().unwrap());
        kernel.shutdown();
    }

    #[test]
    fn native_thread_completion_wakes_parentless_host() {
        use super::super::{kernel, k_scheduler_lock::KScopedSchedulerLock};
        use std::sync::mpsc;
        use std::time::Duration;
        let mut kernel = Box::new(kernel::KernelCore::new());
        kernel.initialize();
        let target = Arc::new(KThreadLock::new(KThread::new()));
        let worker_target = Arc::clone(&target);
        let (ready_tx, ready_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let thread = kernel::get_current_emu_thread().unwrap();
            ready_tx.send(Arc::clone(&thread)).unwrap();
            let mut index = -1;
            let result = wait_on_objects(&thread, &mut index, vec![42],
                vec![WaitableObject::Thread(worker_target)], -1);
            done_tx.send((result, index)).unwrap();
        });
        let waiter = ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        await_kernel_registration(&waiter);
        {
            let _lock = KScopedSchedulerLock::new(kernel::scheduler_lock().unwrap());
            target.lock().unwrap().finish_termination();
        }
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            (crate::hle::result::RESULT_SUCCESS, 0));
        worker.join().unwrap();
        assert!(target.lock().unwrap().sync_object.is_empty());
        kernel.shutdown();
    }

    #[test]
    fn host_deadline_delivery_pauses_but_explicit_stop_signal_does_not() {
        use super::super::kernel;
        use std::sync::mpsc;
        use std::time::{Duration, Instant};
        let mut kernel = Box::new(kernel::KernelCore::new());
        kernel.initialize();
        let timing = Arc::new(crate::core_timing::CoreTiming::new());
        timing.set_multicore(true);
        kernel.wire_hardware_timer(Arc::clone(&timing));
        timing.initialize(|| kernel::get_kernel_ref().unwrap().register_host_thread());
        let start_deadline = Instant::now() + Duration::from_secs(2);
        while !timing.has_started() {
            assert!(Instant::now() < start_deadline);
            std::thread::yield_now();
        }
        for stop_signal in [false, true] {
            timing.sync_pause(true);
            let event = Arc::new(Mutex::new(KReadableEvent::new()));
            event.lock().unwrap().initialize(0, 42);
            let worker_event = Arc::clone(&event);
            let (ready_tx, ready_rx) = mpsc::channel();
            let (done_tx, done_rx) = mpsc::channel();
            let deadline = kernel.hardware_timer().unwrap().get_tick() + 20_000_000;
            let worker = std::thread::spawn(move || {
                let thread = kernel::get_current_emu_thread().unwrap();
                ready_tx.send(Arc::clone(&thread)).unwrap();
                let mut index = -1;
                let result = wait_on_objects(&thread, &mut index, vec![42],
                    vec![WaitableObject::from_readable_event(worker_event)], deadline);
                done_tx.send((result, index)).unwrap();
            });
            let thread = ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            await_kernel_registration(&thread);
            assert!(matches!(done_rx.recv_timeout(Duration::from_millis(40)), Err(mpsc::RecvTimeoutError::Timeout)));
            assert!(kernel.hardware_timer().unwrap().get_tick() >= deadline);
            let expected = if stop_signal {
                event.lock().unwrap().signal();
                (crate::hle::result::RESULT_SUCCESS, 0)
            } else {
                timing.sync_pause(false);
                (RESULT_TIMED_OUT, -1)
            };
            assert_eq!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap(), expected);
            worker.join().unwrap();
            assert!(event.lock().unwrap().sync_object.is_empty());
        }
        timing.reset();
        kernel.shutdown();
    }

    #[test]
    fn guest_wait_context_retains_native_object_until_unlink() {
        // Exercise the guest queue lifetime independently of the host
        // suspension primitive. A deferred IPC return can drop the caller's
        // object references while this context is still WAITING.
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        let event = Arc::new(Mutex::new(KReadableEvent::new()));
        event.lock().unwrap().initialize(0, 42);
        let weak_event = Arc::downgrade(&event);
        let object = WaitableObject::from_readable_event(event);
        let state = object.sync_state_ptr();
        let mut nodes = vec![ThreadListNode::new()].into_boxed_slice();
        nodes[0].thread = Arc::downgrade(&thread);
        unsafe { (*state).link_node(&mut nodes[0]); }
        {
            let mut thread = thread.lock().unwrap();
            assert!(!thread.is_dummy_thread());
            thread.sync_wait_context = SynchronizationWaitContext {
                nodes, object_ids: vec![42], objects: vec![object],
                object_states: vec![state], active: true,
            };
            thread.set_cancellable();
            thread.begin_wait_with_queue(ThreadQueueImplForKSynchronizationObjectWait::queue());
        }
        let event = weak_event.upgrade().expect("guest queue must retain its object");
        event.lock().unwrap().signal();
        assert!(event.lock().unwrap().sync_object.is_empty());
        assert_eq!(thread.lock().unwrap().get_synced_index(), 0);
        assert!(!thread.lock().unwrap().sync_wait_context.is_active());
        drop(event);
        assert!(weak_event.upgrade().is_none(), "completed guest wait retained the object");
    }

    #[test]
    fn native_server_port_arrival_wakes_host_by_object_identity() {
        use super::super::kernel;
        use std::sync::mpsc;
        use std::time::Duration;
        let mut kernel = Box::new(kernel::KernelCore::new());
        kernel.initialize();
        for light in [false, true] {
            let port = Arc::new(Mutex::new(KPort::new()));
            port.lock().unwrap().initialize(2, light, 0);
            let worker_port = Arc::clone(&port);
            let (ready_tx, ready_rx) = mpsc::channel();
            let (done_tx, done_rx) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let thread = kernel::get_current_emu_thread().unwrap();
                let event = Arc::new(Mutex::new(KReadableEvent::new()));
                event.lock().unwrap().initialize(0, 42);
                ready_tx.send(Arc::clone(&thread)).unwrap();
                let mut index = -1;
                // Distinct native objects may have equal numeric IDs. Only
                // the port at index 1 becomes signaled, not the event at 0.
                let result = wait_on_objects(&thread, &mut index, vec![42, 42], vec![
                    WaitableObject::from_readable_event(event),
                    WaitableObject::from_server_port(worker_port),
                ], -1);
                done_tx.send((result, index)).unwrap();
            });
            let thread = ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            await_kernel_registration(&thread);
            let result = if light {
                KPort::enqueue_light_session_arc(&port, 123)
            } else {
                KPort::enqueue_session_arc(&port, 123)
            };
            assert_eq!(result, crate::hle::result::RESULT_SUCCESS);
            assert_eq!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
                (crate::hle::result::RESULT_SUCCESS, 1));
            worker.join().unwrap();
            assert!(port.lock().unwrap().server.sync_object.is_empty());
        }
        kernel.shutdown();
    }

    #[test]
    fn parentless_host_cancellation_and_hardware_deadline_unlink_nodes() {
        use super::super::{kernel, k_scheduler_lock::KScopedSchedulerLock};
        use std::sync::mpsc;
        use std::time::Duration;

        let mut kernel = Box::new(kernel::KernelCore::new());
        kernel.initialize();
        // Deterministic guest clock: advance explicitly, no host sleeps/timer
        // polling. The same hardware callback is used by the runtime timer.
        let timing = Arc::new(crate::core_timing::CoreTiming::new());
        kernel.wire_hardware_timer(Arc::clone(&timing));
        for expected in [RESULT_CANCELLED, RESULT_TIMED_OUT, RESULT_TERMINATION_REQUESTED] {
            let event = Arc::new(Mutex::new(KReadableEvent::new()));
            event.lock().unwrap().initialize(0, 0x1234);
            let worker_event = Arc::clone(&event);
            let (ready_tx, ready_rx) = mpsc::channel();
            let (done_tx, done_rx) = mpsc::channel();
            let deadline = kernel.hardware_timer().unwrap().get_tick() + 1_000_000;
            let worker = std::thread::spawn(move || {
                let thread = kernel::get_current_emu_thread().unwrap();
                ready_tx.send(Arc::clone(&thread)).unwrap();
                let mut index = -1;
                let result = wait_on_objects(&thread, &mut index, vec![0x1234],
                    vec![WaitableObject::from_readable_event(worker_event)], deadline);
                done_tx.send((result, index)).unwrap();
            });
            let thread = ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            await_kernel_registration(&thread);
            {
                let _lock = KScopedSchedulerLock::new(kernel::scheduler_lock().unwrap());
                assert_eq!(thread.lock().unwrap().get_timer_task_time(), deadline);
                if expected == RESULT_CANCELLED {
                    thread.lock().unwrap().wait_cancel();
                } else if expected == RESULT_TERMINATION_REQUESTED {
                    thread.lock().unwrap().request_terminate();
                }
            }
            if expected == RESULT_TIMED_OUT {
                assert!(done_rx.try_recv().is_err());
                timing.add_ticks(10_000_000);
                timing.advance();
            }
            assert_eq!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap(), (expected, -1));
            worker.join().unwrap();
            {
                let _lock = KScopedSchedulerLock::new(kernel::scheduler_lock().unwrap());
                assert!(event.lock().unwrap().sync_object.is_empty());
                assert_eq!(thread.lock().unwrap().get_timer_task_time(), 0);
                assert!(!thread.lock().unwrap().is_cancellable());
            }
        }
        kernel.shutdown();
    }

    #[test]
    fn parentless_host_dummy_wait_uses_kernel_queue_and_wakeup() {
        use super::super::kernel::{self, KernelCore};
        use super::super::k_scheduler_lock::KScopedSchedulerLock;
        use super::super::k_thread::ThreadState;
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        // KernelCore's scheduler callbacks retain its address.
        let mut kernel = Box::new(KernelCore::new());
        kernel.initialize();
        kernel.register_host_thread();
        let event = Arc::new(Mutex::new(KReadableEvent::new()));
        event.lock().unwrap().initialize(0, 0x1234);
        let (registered_tx, registered_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker_event = Arc::clone(&event);
        let worker = std::thread::spawn(move || {
            let thread = kernel::get_current_emu_thread().unwrap();
            {
                let thread = thread.lock().unwrap();
                assert!(thread.is_dummy_thread());
                assert!(thread.parent.is_none());
                assert!(thread.scheduler.is_none());
                assert!(thread.global_scheduler_context.as_ref().unwrap().upgrade().is_some());
            }
            registered_tx.send(Arc::clone(&thread)).unwrap();
            let mut index = -1;
            let result = wait_on_objects(&thread, &mut index, vec![0x1234],
                vec![WaitableObject::from_readable_event(worker_event)], -1);
            done_tx.send((result, index)).unwrap();
        });
        let thread = registered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let sleeping = {
                let _lock = KScopedSchedulerLock::new(kernel::scheduler_lock().unwrap());
                let thread = thread.lock().unwrap();
                thread.get_state() == ThreadState::WAITING && thread.sync_wait_context.is_active()
            };
            if sleeping { break; }
            assert!(Instant::now() < deadline, "host never entered the kernel wait queue");
            std::thread::yield_now();
        }
        assert!(done_rx.try_recv().is_err(), "wait returned without a signal");
        event.lock().unwrap().signal();
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            (crate::hle::result::RESULT_SUCCESS, 0));
        worker.join().unwrap();
        {
            let _lock = KScopedSchedulerLock::new(kernel::scheduler_lock().unwrap());
            assert!(event.lock().unwrap().sync_object.is_empty());
            let thread = thread.lock().unwrap();
            assert!(!thread.sync_wait_context.is_active());
            assert_eq!(thread.get_disable_dispatch_count(), 0);
        }
        kernel.shutdown();
    }

    #[test]
    fn link_and_unlink_single_node() {
        let mut state = SynchronizationObjectState::new();
        let mut node = ThreadListNode::new();
        unsafe {
            state.link_node(&mut node);
            assert!(!state.is_empty());
            state.unlink_node(&mut node);
            assert!(state.is_empty());
        }
    }

    #[test]
    fn server_session_waitable_access_does_not_relock_wrapper() {
        let session = Arc::new(Mutex::new(KServerSession::new()));
        let session_ptr = {
            let mut guard = session.lock().unwrap();
            &mut *guard as *mut KServerSession
        };
        let waitable = WaitableObject::ServerSession {
            _session: Arc::clone(&session),
            session: session_ptr,
        };

        // Upstream accesses IsSignaled and the synchronization-object state
        // under the scheduler lock without reacquiring KServerSession::m_lock.
        // Holding the Rust wrapper here catches a regression to the former
        // scheduler -> session-mutex lock inversion.
        let _wrapper_guard = session.lock().unwrap();
        assert!(!waitable.is_signaled());
        assert_eq!(waitable.sync_state_ptr(), unsafe {
            &mut (*session_ptr).sync_object as *mut SynchronizationObjectState
        });
    }

    #[test]
    fn link_and_unlink_preserves_order() {
        let mut state = SynchronizationObjectState::new();
        let mut a = ThreadListNode::new();
        let mut b = ThreadListNode::new();
        let mut c = ThreadListNode::new();
        a.object_id = 1;
        b.object_id = 2;
        c.object_id = 3;
        unsafe {
            state.link_node(&mut a);
            state.link_node(&mut b);
            state.link_node(&mut c);
            state.unlink_node(&mut b);
            // head → a → c
            let mut ids = Vec::new();
            let mut cur = state.head;
            while !cur.is_null() {
                ids.push((*cur).object_id);
                cur = (*cur).next;
            }
            assert_eq!(ids, vec![1, 3]);

            state.unlink_node(&mut a);
            state.unlink_node(&mut c);
            assert!(state.is_empty());
        }
    }

    #[test]
    fn synchronization_wait_queue_cancel_unlinks_every_node() {
        let mut first_state = SynchronizationObjectState::new();
        let mut second_state = SynchronizationObjectState::new();
        let mut nodes: Box<[ThreadListNode]> =
            vec![ThreadListNode::new(), ThreadListNode::new()].into_boxed_slice();

        unsafe {
            first_state.link_node(&mut nodes[0]);
            second_state.link_node(&mut nodes[1]);
        }

        let mut thread = KThread::new();
        thread.sync_wait_context = SynchronizationWaitContext {
            nodes,
            object_ids: vec![1, 2],
            objects: Vec::new(),
            object_states: vec![&mut first_state, &mut second_state],
            active: true,
        };
        thread.set_cancellable();

        ThreadQueueImplForKSynchronizationObjectWait::cancel_wait(&mut thread);

        assert!(first_state.is_empty());
        assert!(second_state.is_empty());
        assert!(!thread.sync_wait_context.is_active());
        assert!(!thread.is_cancellable());
    }
}

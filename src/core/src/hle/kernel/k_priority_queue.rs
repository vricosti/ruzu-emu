//! Port of zuyu/src/core/hle/kernel/k_priority_queue.h
//! Status: EN COURS
//! Derniere synchro: 2026-03-27
//!
//! KPriorityQueue — multi-core priority queue for thread scheduling.
//!
//! Upstream uses intrusive linked lists: `QueueEntry` nodes live inside
//! `KThread` (`m_per_core_priority_queue_entry`) and the queue manipulates
//! `member->GetPriorityQueueEntry(core)` directly. A thread and its scheduler
//! linkage are therefore the same object and cannot disagree.
//!
//! The Rust port cannot put the links inside `KThread` without locking each
//! `Arc<Mutex<KThread>>` during queue traversal, which would invert the
//! scheduler/thread lock order. Instead it keeps the linkage outside the thread,
//! in a map keyed by thread id, together with a cache of the thread properties
//! the scheduler needs — so PQ operations still require no thread locking.
//!
//! The links and the properties live in **one** [`MemberSlot`], inserted and
//! removed as a unit. That is the property upstream gets for free from
//! intrusiveness: a member is either in the queue with both, or absent with
//! neither. They were previously two independent maps, and the one holding the
//! links was never pruned, so the two could disagree — a split-brain state the
//! port had to detect and repair, and which could leave a core idle while a
//! Runnable thread waited.

use std::collections::HashMap;

use crate::hardware_properties::MAX_NUM_CPU_CORES;

/// Number of priority levels (0..=63).
pub const NUM_PRIORITY: usize = 64;
/// Number of CPU cores.
pub const NUM_CORES: usize = MAX_NUM_CPU_CORES as usize;

/// Lowest thread priority.
pub const LOWEST_PRIORITY: i32 = 63;
/// Highest thread priority.
pub const HIGHEST_PRIORITY: i32 = 0;

pub const fn is_valid_core(core: i32) -> bool {
    core >= 0 && (core as usize) < NUM_CORES
}

pub const fn is_valid_priority(priority: i32) -> bool {
    priority >= HIGHEST_PRIORITY && priority <= LOWEST_PRIORITY + 1
}

// ---------------------------------------------------------------------------
// BitSet64
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
pub struct BitSet64 {
    bits: u64,
}

impl BitSet64 {
    pub const fn new() -> Self {
        Self { bits: 0 }
    }

    /// Set bit at position `bit`. Upstream stores in reversed order:
    /// bit 0 = MSB (bit 63 of u64), bit 63 = LSB (bit 0 of u64).
    /// This way, `leading_zeros()` directly returns the priority number.
    pub fn set_bit(&mut self, bit: i32) {
        debug_assert!(bit >= 0 && bit < 64);
        self.bits |= 1u64 << (63 - bit);
    }

    pub fn clear_bit(&mut self, bit: i32) {
        debug_assert!(bit >= 0 && bit < 64);
        self.bits &= !(1u64 << (63 - bit));
    }

    /// Returns the index of the highest-priority (numerically lowest) set bit.
    /// Returns 64 if no bits are set.
    pub const fn count_leading_zero(&self) -> u32 {
        if self.bits == 0 {
            64
        } else {
            self.bits.leading_zeros()
        }
    }

    /// Returns the next set bit after `bit`, or 64 if none.
    pub const fn get_next_set(&self, bit: i32) -> u32 {
        // Mask out bit `bit` and all higher-priority (lower-numbered) bits.
        // In reversed storage, bit N is at position (63 - N).
        // We want to clear all bits at positions >= (63 - bit), i.e., keep bits < (63 - bit).
        let shift = 63 - bit;
        let masked = if shift <= 0 {
            0
        } else {
            self.bits & ((1u64 << shift) - 1)
        };
        if masked == 0 {
            64
        } else {
            masked.leading_zeros()
        }
    }
}

// ---------------------------------------------------------------------------
// QueueEntry — per-core linked list node using thread_id
// ---------------------------------------------------------------------------

/// Intrusive linked list entry for priority queue membership.
/// Stored internally in the PQ, keyed by (thread_id, core).
///
/// Upstream uses raw `Member*` pointers; we use `Option<u64>` thread_ids.
#[derive(Debug, Clone, Default)]
pub struct QueueEntry {
    prev: Option<u64>,
    next: Option<u64>,
}

impl QueueEntry {
    pub fn new() -> Self {
        Self {
            prev: None,
            next: None,
        }
    }

    pub fn get_prev(&self) -> Option<u64> {
        self.prev
    }

    pub fn get_next(&self) -> Option<u64> {
        self.next
    }

    pub fn set_prev(&mut self, prev: Option<u64>) {
        self.prev = prev;
    }

    pub fn set_next(&mut self, next: Option<u64>) {
        self.next = next;
    }
}

// ---------------------------------------------------------------------------
// MemberMap — internal storage for per-thread, per-core queue entries
// ---------------------------------------------------------------------------

/// Internal storage: thread_id → [QueueEntry; NUM_CORES].
/// Shared between scheduled and suggested queues (a thread is in at most
/// one list per core, so the entries don't conflict).
type MemberMap = HashMap<u64, MemberSlot>;

/// Everything the queue owns for one member: its per-core list links and the
/// cached properties that decide which lists it belongs to.
///
/// This is the port's stand-in for upstream storing `QueueEntry` inside
/// `KThread`. Indexing a slot by core yields the link node, so link code reads
/// the same as it did against a bare `[QueueEntry; NUM_CORES]`.
#[derive(Debug, Clone)]
pub struct MemberSlot {
    entries: [QueueEntry; NUM_CORES],
    props: ThreadProps,
}

impl MemberSlot {
    fn new(props: ThreadProps) -> Self {
        Self {
            entries: std::array::from_fn(|_| QueueEntry::new()),
            props,
        }
    }
}

impl std::ops::Index<usize> for MemberSlot {
    type Output = QueueEntry;
    fn index(&self, core: usize) -> &QueueEntry {
        &self.entries[core]
    }
}

impl std::ops::IndexMut<usize> for MemberSlot {
    fn index_mut(&mut self, core: usize) -> &mut QueueEntry {
        &mut self.entries[core]
    }
}

// ---------------------------------------------------------------------------
// ThreadProps — cached thread properties for lock-free scheduler access
// ---------------------------------------------------------------------------

/// Cached thread properties stored in the PQ.
/// Updated on push/remove/change operations.
/// Allows the scheduler migration loop to read thread properties
/// without locking individual KThread mutexes.
#[derive(Debug, Clone)]
pub struct ThreadProps {
    pub priority: i32,
    pub active_core: i32,
    pub affinity: u64,
    pub is_dummy: bool,
    /// Cached `KThread::m_last_scheduled_tick`.
    ///
    /// Upstream reads this while holding only the scheduler lock. Keeping it
    /// beside the other queue-owned thread properties preserves that access
    /// ordering without introducing a per-thread lock into migration scans.
    pub last_scheduled_tick: i64,
    /// Shared reference to the owning process's schedule_count.
    /// Allows IncrementScheduledCount without any lock acquisition.
    pub process_schedule_count: Option<std::sync::Arc<std::sync::atomic::AtomicI64>>,
}

// ---------------------------------------------------------------------------
// KPerCoreQueue — per-priority doubly-linked list using thread_ids
// ---------------------------------------------------------------------------

/// Per-priority, per-core doubly-linked list.
/// Matches upstream `KPriorityQueue::KPerCoreQueue`.
///
/// The root entries act as sentinel nodes:
/// root.next = head of list, root.prev = tail of list.
#[derive(Debug, Clone)]
pub struct KPerCoreQueue {
    roots: [QueueEntry; NUM_CORES],
}

impl Default for KPerCoreQueue {
    fn default() -> Self {
        Self {
            roots: std::array::from_fn(|_| QueueEntry::new()),
        }
    }
}

impl KPerCoreQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a thread to the back of the queue for a core.
    /// Returns true if the queue was previously empty (first element).
    pub fn push_back(&mut self, core: i32, member_id: u64, entries: &mut MemberMap) -> bool {
        let c = core as usize;
        let tail_id = self.roots[c].get_prev();

        // Link: member.prev = tail, member.next = None
        {
            let Some(e) = entries.get_mut(&member_id) else {
                // Upstream cannot reach this: the entry is part of the thread.
                // Here it means a caller linked a member it never inserted.
                debug_assert!(false, "push_back for member {member_id} with no slot");
                return false;
            };
            e[c].set_prev(tail_id);
            e[c].set_next(None);
        }

        // Link: tail.next = member (or root.next if empty)
        if let Some(tid) = tail_id {
            if let Some(te) = entries.get_mut(&tid) {
                te[c].set_next(Some(member_id));
            }
        } else {
            self.roots[c].set_next(Some(member_id));
        }
        self.roots[c].set_prev(Some(member_id));

        tail_id.is_none()
    }

    /// Push a thread to the front of the queue for a core.
    /// Returns true if the queue was previously empty.
    pub fn push_front(&mut self, core: i32, member_id: u64, entries: &mut MemberMap) -> bool {
        let c = core as usize;
        let head_id = self.roots[c].get_next();

        // Link: member.prev = None, member.next = head
        {
            let Some(e) = entries.get_mut(&member_id) else {
                debug_assert!(false, "push_front for member {member_id} with no slot");
                return false;
            };
            e[c].set_prev(None);
            e[c].set_next(head_id);
        }

        // Link: head.prev = member (or root.prev if empty)
        if let Some(hid) = head_id {
            if let Some(he) = entries.get_mut(&hid) {
                he[c].set_prev(Some(member_id));
            }
        } else {
            self.roots[c].set_prev(Some(member_id));
        }
        self.roots[c].set_next(Some(member_id));

        head_id.is_none()
    }

    /// Remove a thread from the queue for a core.
    /// Returns true if the queue is now empty.
    pub fn remove(&mut self, core: i32, member_id: u64, entries: &mut MemberMap) -> bool {
        let c = core as usize;
        let root_next = self.roots[c].get_next();
        let root_prev = self.roots[c].get_prev();
        let (prev_id, next_id) = entries
            .get(&member_id)
            .map(|e| (e[c].get_prev(), e[c].get_next()))
            .unwrap_or((None, None));

        // Upstream uses intrusive queue entries and only calls Remove for a
        // member that is in this list. The Rust port can reach this function
        // after stale/cache-repair paths, where the per-core entry exists but
        // is not linked in the requested queue. Treat that as a no-op; otherwise
        // an unlinked entry with prev=None,next=None looks like the sole list
        // element and incorrectly clears the queue root.
        let prev_links_to_member = prev_id
            .and_then(|pid| {
                entries
                    .get(&pid)
                    .map(|e| e[c].get_next() == Some(member_id))
            })
            .unwrap_or(false);
        let next_links_to_member = next_id
            .and_then(|nid| {
                entries
                    .get(&nid)
                    .map(|e| e[c].get_prev() == Some(member_id))
            })
            .unwrap_or(false);
        let member_is_linked = (prev_id.is_none() && root_next == Some(member_id)
            || prev_id.is_some() && prev_links_to_member)
            && (next_id.is_none() && root_prev == Some(member_id)
                || next_id.is_some() && next_links_to_member);
        if !member_is_linked {
            return root_next.is_none();
        }

        // Unlink prev -> next
        if let Some(pid) = prev_id {
            if let Some(pe) = entries.get_mut(&pid) {
                pe[c].set_next(next_id);
            }
        } else {
            self.roots[c].set_next(next_id);
        }

        // Unlink next -> prev
        if let Some(nid) = next_id {
            if let Some(ne) = entries.get_mut(&nid) {
                ne[c].set_prev(prev_id);
            }
        } else {
            self.roots[c].set_prev(prev_id);
        }

        self.get_front(core).is_none()
    }

    /// Remove every root-reachable occurrence of `member_id` from this per-core
    /// list.
    ///
    /// Upstream's intrusive `KThread::QueueEntry` makes duplicate membership
    /// impossible: a thread has one entry object per core. The Rust port uses
    /// thread ids as links, so a doubled transition can leave the same id
    /// reachable more than once while only one `QueueEntry` stores links. Rebuild
    /// the root-visible list without that id instead of trusting the stale member
    /// entry.
    pub fn remove_all_matching(
        &mut self,
        core: i32,
        priority: i32,
        scheduled: bool,
        member_id: u64,
        entries: &mut MemberMap,
    ) -> bool {
        let c = core as usize;
        let mut kept = Vec::new();
        let mut seen = Vec::new();
        let mut current = self.roots[c].get_next();
        let old_tail = self.roots[c].get_prev();
        let limit = entries.len().saturating_add(1);

        for _ in 0..limit {
            let Some(id) = current else {
                break;
            };
            if seen.contains(&id) {
                break;
            }
            seen.push(id);

            current = entries.get(&id).and_then(|e| e[c].get_next());
            if id != member_id
                && entries.get(&id).is_some_and(|slot| {
                    slot.props.priority == priority
                        && slot.props.affinity & (1u64 << core) != 0
                        && (slot.props.active_core == core) == scheduled
                })
            {
                kept.push(id);
            }
        }

        for (&id, slot) in entries.iter() {
            let props = &slot.props;
            let belongs_to_queue = props.priority == priority
                && props.affinity & (1u64 << core) != 0
                && (props.active_core == core) == scheduled;
            if id == member_id || kept.contains(&id) || !belongs_to_queue {
                continue;
            }
            let entry = &slot[c];
            if old_tail == Some(id)
                || entry.get_prev() == Some(member_id)
                || entry.get_next() == Some(member_id)
            {
                kept.push(id);
            }
        }

        self.roots[c].set_next(kept.first().copied());
        self.roots[c].set_prev(kept.last().copied());

        for (idx, &id) in kept.iter().enumerate() {
            if let Some(e) = entries.get_mut(&id) {
                e[c].set_prev(idx.checked_sub(1).and_then(|prev| kept.get(prev)).copied());
                e[c].set_next(kept.get(idx + 1).copied());
            }
        }

        if let Some(e) = entries.get_mut(&member_id) {
            e[c].set_prev(None);
            e[c].set_next(None);
        }

        self.get_front(core).is_none()
    }
    pub fn get_front(&self, core: i32) -> Option<u64> {
        self.roots[core as usize].get_next()
    }
}

// ---------------------------------------------------------------------------
// KPriorityQueueImpl
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct KPriorityQueueImpl {
    queues: Vec<KPerCoreQueue>,
    available_priorities: [BitSet64; NUM_CORES],
}

impl KPriorityQueueImpl {
    pub fn new() -> Self {
        let mut queues = Vec::with_capacity(NUM_PRIORITY);
        for _ in 0..NUM_PRIORITY {
            queues.push(KPerCoreQueue::new());
        }
        Self {
            queues,
            available_priorities: [BitSet64::new(); NUM_CORES],
        }
    }

    pub fn push_back(&mut self, priority: i32, core: i32, member_id: u64, entries: &mut MemberMap) {
        debug_assert!(is_valid_core(core));
        debug_assert!(is_valid_priority(priority));
        if priority > LOWEST_PRIORITY {
            return;
        }

        let was_empty = self.queues[priority as usize].push_back(core, member_id, entries);
        if was_empty {
            self.available_priorities[core as usize].set_bit(priority);
        }
    }

    pub fn push_front(
        &mut self,
        priority: i32,
        core: i32,
        member_id: u64,
        entries: &mut MemberMap,
    ) {
        debug_assert!(is_valid_core(core));
        debug_assert!(is_valid_priority(priority));
        if priority > LOWEST_PRIORITY {
            return;
        }

        if self.queues[priority as usize].push_front(core, member_id, entries) {
            self.available_priorities[core as usize].set_bit(priority);
        }
    }

    pub fn remove(&mut self, priority: i32, core: i32, member_id: u64, entries: &mut MemberMap) {
        debug_assert!(is_valid_core(core));
        debug_assert!(is_valid_priority(priority));
        if priority > LOWEST_PRIORITY {
            return;
        }

        if self.queues[priority as usize].remove(core, member_id, entries) {
            self.available_priorities[core as usize].clear_bit(priority);
        }
    }

    pub fn get_front(&self, core: i32) -> Option<u64> {
        debug_assert!(is_valid_core(core));
        let priority = self.available_priorities[core as usize].count_leading_zero() as i32;
        if priority <= LOWEST_PRIORITY {
            self.queues[priority as usize].get_front(core)
        } else {
            None
        }
    }

    pub fn get_front_at_priority(&self, priority: i32, core: i32) -> Option<u64> {
        debug_assert!(is_valid_core(core));
        debug_assert!(is_valid_priority(priority));
        if priority <= LOWEST_PRIORITY {
            self.queues[priority as usize].get_front(core)
        } else {
            None
        }
    }

    /// Get the next thread after `member_id` in the queue for `core`.
    /// If no next in current priority, jump to the front of the next priority.
    pub fn get_next(
        &self,
        core: i32,
        member_id: u64,
        member_priority: i32,
        entries: &MemberMap,
    ) -> Option<u64> {
        debug_assert!(is_valid_core(core));

        let next = entries
            .get(&member_id)
            .and_then(|e| e[core as usize].get_next());

        if next.is_some() {
            return next;
        }

        // Jump to the next priority level
        let next_priority =
            self.available_priorities[core as usize].get_next_set(member_priority) as i32;
        if next_priority <= LOWEST_PRIORITY {
            self.queues[next_priority as usize].get_front(core)
        } else {
            None
        }
    }

    pub fn move_to_front(
        &mut self,
        priority: i32,
        core: i32,
        member_id: u64,
        entries: &mut MemberMap,
    ) {
        debug_assert!(is_valid_core(core));
        debug_assert!(is_valid_priority(priority));
        if priority <= LOWEST_PRIORITY {
            self.queues[priority as usize].remove(core, member_id, entries);
            self.queues[priority as usize].push_front(core, member_id, entries);
        }
    }

    pub fn move_to_back(
        &mut self,
        priority: i32,
        core: i32,
        member_id: u64,
        entries: &mut MemberMap,
    ) -> Option<u64> {
        debug_assert!(is_valid_core(core));
        debug_assert!(is_valid_priority(priority));
        if priority <= LOWEST_PRIORITY {
            self.queues[priority as usize].remove(core, member_id, entries);
            self.queues[priority as usize].push_back(core, member_id, entries);
            self.queues[priority as usize].get_front(core)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// KPriorityQueue — the full priority queue
// ---------------------------------------------------------------------------

/// The full priority queue with scheduled and suggested queues.
/// Matches upstream `Kernel::KPriorityQueue<Member, NumCores_, LowestPriority, HighestPriority>`.
///
/// Entries and thread properties are stored internally, so PQ operations
/// do not require locking any KThread mutex.
#[derive(Debug, Clone)]
pub struct KPriorityQueue {
    scheduled_queue: KPriorityQueueImpl,
    suggested_queue: KPriorityQueueImpl,
    /// Every member the queue knows about: its per-core list links and the
    /// cached properties deciding which lists it belongs to, inserted and
    /// removed as one unit so the two can never disagree.
    ///
    /// A thread is in at most one list per core (either scheduled or
    /// suggested), so the per-core links don't conflict.
    members: MemberMap,
}

fn clear_affinity_bit(affinity: &mut u64, core: i32) {
    *affinity &= !(1u64 << core);
}

fn get_next_core(affinity: &mut u64) -> i32 {
    let core = affinity.trailing_zeros() as i32;
    clear_affinity_bit(affinity, core);
    core
}

impl KPriorityQueue {
    pub fn new() -> Self {
        Self {
            scheduled_queue: KPriorityQueueImpl::new(),
            suggested_queue: KPriorityQueueImpl::new(),
            members: HashMap::new(),
        }
    }

    // -- Property cache access (for migration loop) --

    /// Get cached thread properties. Returns None if thread not in PQ.
    pub fn get_thread_props(&self, thread_id: u64) -> Option<&ThreadProps> {
        self.members.get(&thread_id).map(|slot| &slot.props)
    }

    /// Upstream: IncrementScheduledCount(thread) — increments the owning process's
    /// schedule_count via the cached Arc<AtomicI64>. No locks needed.
    pub fn increment_scheduled_count(&self, thread_id: u64) {
        if let Some(props) = self.get_thread_props(thread_id) {
            if let Some(ref counter) = props.process_schedule_count {
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    pub fn set_last_scheduled_tick(&mut self, thread_id: u64, tick: i64) {
        if let Some(slot) = self.members.get_mut(&thread_id) {
            slot.props.last_scheduled_tick = tick;
        }
    }

    // -- Getters --

    pub fn get_scheduled_front(&self, core: i32) -> Option<u64> {
        self.scheduled_queue.get_front(core)
    }

    pub fn get_scheduled_front_at_priority(&self, core: i32, priority: i32) -> Option<u64> {
        self.scheduled_queue.get_front_at_priority(priority, core)
    }

    pub fn get_suggested_front(&self, core: i32) -> Option<u64> {
        self.suggested_queue.get_front(core)
    }

    pub fn get_suggested_front_at_priority(&self, core: i32, priority: i32) -> Option<u64> {
        self.suggested_queue.get_front_at_priority(priority, core)
    }

    pub fn get_scheduled_next(
        &self,
        core: i32,
        member_id: u64,
        member_priority: i32,
    ) -> Option<u64> {
        self.scheduled_queue
            .get_next(core, member_id, member_priority, &self.members)
    }

    pub fn get_suggested_next(
        &self,
        core: i32,
        member_id: u64,
        member_priority: i32,
    ) -> Option<u64> {
        self.suggested_queue
            .get_next(core, member_id, member_priority, &self.members)
    }

    pub fn get_same_priority_next(&self, core: i32, member_id: u64) -> Option<u64> {
        self.members
            .get(&member_id)
            .and_then(|e| e[core as usize].get_next())
    }

    // -- Private push/remove with priority --

    fn push_back_impl(
        &mut self,
        priority: i32,
        member_id: u64,
        active_core: i32,
        affinity_mask: u64,
    ) {
        debug_assert!(is_valid_priority(priority));

        let Self {
            scheduled_queue,
            suggested_queue,
            members: entries,
        } = self;
        let mut affinity = affinity_mask;
        if active_core >= 0 {
            scheduled_queue.push_back(priority, active_core, member_id, entries);
            clear_affinity_bit(&mut affinity, active_core);
        }

        while affinity != 0 {
            suggested_queue.push_back(priority, get_next_core(&mut affinity), member_id, entries);
        }
    }

    fn push_front_impl(
        &mut self,
        priority: i32,
        member_id: u64,
        active_core: i32,
        affinity_mask: u64,
    ) {
        debug_assert!(is_valid_priority(priority));

        let Self {
            scheduled_queue,
            suggested_queue,
            members: entries,
        } = self;
        let mut affinity = affinity_mask;
        if active_core >= 0 {
            scheduled_queue.push_front(priority, active_core, member_id, entries);
            clear_affinity_bit(&mut affinity, active_core);
        }

        // Note: Nintendo pushes onto the back of the suggested queue, not the front.
        while affinity != 0 {
            suggested_queue.push_back(priority, get_next_core(&mut affinity), member_id, entries);
        }
    }

    fn remove_impl(&mut self, priority: i32, member_id: u64, active_core: i32, affinity_mask: u64) {
        debug_assert!(is_valid_priority(priority));

        let Self {
            scheduled_queue,
            suggested_queue,
            members: entries,
        } = self;
        let mut affinity = affinity_mask;
        if active_core >= 0 {
            scheduled_queue.remove(priority, active_core, member_id, entries);
            clear_affinity_bit(&mut affinity, active_core);
        }

        while affinity != 0 {
            suggested_queue.remove(priority, get_next_core(&mut affinity), member_id, entries);
        }
    }

    fn remove_all_expected_impl(
        &mut self,
        priority: i32,
        member_id: u64,
        active_core: i32,
        affinity_mask: u64,
    ) {
        debug_assert!(is_valid_priority(priority));

        let Self {
            scheduled_queue,
            suggested_queue,
            members,
        } = self;
        let entries = members;
        let mut affinity = affinity_mask;
        if active_core >= 0 {
            if scheduled_queue.queues[priority as usize].remove_all_matching(
                active_core,
                priority,
                true,
                member_id,
                entries,
            ) {
                scheduled_queue.available_priorities[active_core as usize].clear_bit(priority);
            }
            clear_affinity_bit(&mut affinity, active_core);
        }

        while affinity != 0 {
            let core = get_next_core(&mut affinity);
            if suggested_queue.queues[priority as usize]
                .remove_all_matching(core, priority, false, member_id, entries)
            {
                suggested_queue.available_priorities[core as usize].clear_bit(priority);
            }
        }
    }

    // -- Public mutators (properties passed directly) --

    /// Push a thread to the back of its queues (scheduled for active core, suggested for others).
    /// Matches upstream `KPriorityQueue::PushBack(member)`.
    pub fn push_back(
        &mut self,
        member_id: u64,
        priority: i32,
        active_core: i32,
        affinity: u64,
        is_dummy: bool,
        process_schedule_count: Option<std::sync::Arc<std::sync::atomic::AtomicI64>>,
    ) {
        if is_dummy {
            return;
        }
        if let Some(existing) = self.get_thread_props(member_id).cloned() {
            self.remove_impl(
                existing.priority,
                member_id,
                existing.active_core,
                existing.affinity,
            );
        }
        // Links and properties enter together, so a member is never half-known.
        let props = ThreadProps {
            priority,
            active_core,
            affinity,
            is_dummy,
            last_scheduled_tick: 0,
            process_schedule_count,
        };
        match self.members.get_mut(&member_id) {
            Some(slot) => slot.props = props,
            None => {
                self.members.insert(member_id, MemberSlot::new(props));
            }
        }
        self.push_back_impl(priority, member_id, active_core, affinity);
    }

    /// Push a thread to the front of its scheduled queue and back of suggested queues.
    /// Matches upstream `KPriorityQueue::PushFront(member)`.
    pub fn push_front(
        &mut self,
        member_id: u64,
        priority: i32,
        active_core: i32,
        affinity: u64,
        is_dummy: bool,
        process_schedule_count: Option<std::sync::Arc<std::sync::atomic::AtomicI64>>,
    ) {
        if is_dummy {
            return;
        }
        if let Some(existing) = self.get_thread_props(member_id).cloned() {
            self.remove_impl(
                existing.priority,
                member_id,
                existing.active_core,
                existing.affinity,
            );
        }
        // Links and properties enter together, so a member is never half-known.
        let props = ThreadProps {
            priority,
            active_core,
            affinity,
            is_dummy,
            last_scheduled_tick: 0,
            process_schedule_count,
        };
        match self.members.get_mut(&member_id) {
            Some(slot) => slot.props = props,
            None => {
                self.members.insert(member_id, MemberSlot::new(props));
            }
        }
        self.push_front_impl(priority, member_id, active_core, affinity);
    }

    /// Remove a thread from all its queues.
    /// Matches upstream `KPriorityQueue::Remove(member)`.
    pub fn remove(
        &mut self,
        member_id: u64,
        priority: i32,
        active_core: i32,
        affinity: u64,
        is_dummy: bool,
    ) {
        if is_dummy {
            return;
        }
        let (priority, active_core, affinity) =
            if let Some(existing) = self.get_thread_props(member_id) {
                (existing.priority, existing.active_core, existing.affinity)
            } else {
                (priority, active_core, affinity)
            };
        // Unlink from every list first — that fixes up the neighbours' links —
        // then drop the slot. Dropping links and properties together is what
        // makes the split-brain state unrepresentable, and it also stops the
        // link storage growing without bound as threads come and go.
        self.remove_impl(priority, member_id, active_core, affinity);
        self.remove_all_expected_impl(priority, member_id, active_core, affinity);
        self.members.remove(&member_id);
    }
    pub fn move_to_scheduled_front(
        &mut self,
        member_id: u64,
        priority: i32,
        active_core: i32,
        is_dummy: bool,
    ) {
        if is_dummy {
            return;
        }
        let (priority, active_core) = self
            .get_thread_props(member_id)
            .map(|props| (props.priority, props.active_core))
            .unwrap_or((priority, active_core));
        self.scheduled_queue
            .move_to_front(priority, active_core, member_id, &mut self.members);
    }

    pub fn move_to_scheduled_back(
        &mut self,
        member_id: u64,
        priority: i32,
        active_core: i32,
        is_dummy: bool,
    ) -> Option<u64> {
        if is_dummy {
            return None;
        }
        let (priority, active_core) = self
            .get_thread_props(member_id)
            .map(|props| (props.priority, props.active_core))
            .unwrap_or((priority, active_core));
        self.scheduled_queue
            .move_to_back(priority, active_core, member_id, &mut self.members)
    }

    /// Change a thread's priority in the queue.
    /// Matches upstream `KPriorityQueue::ChangePriority(prev_priority, is_running, member)`.
    pub fn change_priority(
        &mut self,
        prev_priority: i32,
        is_running: bool,
        member_id: u64,
        new_priority: i32,
        active_core: i32,
        affinity: u64,
        is_dummy: bool,
    ) {
        if is_dummy {
            return;
        }
        debug_assert!(is_valid_priority(prev_priority));

        let (active_core, affinity) = self
            .get_thread_props(member_id)
            .map(|props| (props.active_core, props.affinity))
            .unwrap_or((active_core, affinity));

        self.remove_impl(prev_priority, member_id, active_core, affinity);

        if is_running {
            self.push_front_impl(new_priority, member_id, active_core, affinity);
        } else {
            self.push_back_impl(new_priority, member_id, active_core, affinity);
        }

        // Update cached priority
        if let Some(props) = self.members.get_mut(&member_id).map(|s| &mut s.props) {
            props.priority = new_priority;
        }
    }

    /// Change a thread's affinity mask.
    /// Matches upstream `KPriorityQueue::ChangeAffinityMask(prev_core, prev_affinity, member)`.
    pub fn change_affinity_mask(
        &mut self,
        prev_core: i32,
        prev_affinity: u64,
        member_id: u64,
        new_core: i32,
        new_affinity: u64,
        priority: i32,
        is_dummy: bool,
    ) {
        if is_dummy {
            return;
        }

        let Self {
            scheduled_queue,
            suggested_queue,
            members,
        } = self;
        let entries = members;

        // Remove from all old queues
        for core in 0..NUM_CORES as i32 {
            if prev_affinity & (1u64 << core) != 0 {
                if core == prev_core {
                    scheduled_queue.remove(priority, core, member_id, entries);
                } else {
                    suggested_queue.remove(priority, core, member_id, entries);
                }
            }
        }

        // Add to all new queues
        for core in 0..NUM_CORES as i32 {
            if new_affinity & (1u64 << core) != 0 {
                if core == new_core {
                    scheduled_queue.push_back(priority, core, member_id, entries);
                } else {
                    suggested_queue.push_back(priority, core, member_id, entries);
                }
            }
        }

        // Update cached properties
        if let Some(slot) = entries.get_mut(&member_id) {
            slot.props.active_core = new_core;
            slot.props.affinity = new_affinity;
        }
    }

    /// Change a thread's active core.
    /// Matches upstream `KPriorityQueue::ChangeCore(prev_core, member, to_front)`.
    ///
    /// The caller must have already set the thread's active_core to `new_core`
    /// (or this function updates the cache — the actual KThread.core_id must
    /// be updated separately if needed).
    pub fn change_core(
        &mut self,
        prev_core: i32,
        member_id: u64,
        new_core: i32,
        priority: i32,
        is_dummy: bool,
        to_front: bool,
    ) {
        if is_dummy {
            return;
        }

        if prev_core != new_core {
            let Self {
                scheduled_queue,
                suggested_queue,
                members: entries,
            } = self;
            if prev_core >= 0 {
                if scheduled_queue.queues[priority as usize]
                    .remove_all_matching(prev_core, priority, true, member_id, entries)
                {
                    scheduled_queue.available_priorities[prev_core as usize].clear_bit(priority);
                }
            }
            if new_core >= 0 {
                suggested_queue.remove(priority, new_core, member_id, entries);
                if to_front {
                    scheduled_queue.push_front(priority, new_core, member_id, entries);
                } else {
                    scheduled_queue.push_back(priority, new_core, member_id, entries);
                }
            }
            if prev_core >= 0 {
                suggested_queue.push_back(priority, prev_core, member_id, entries);
            }

            // Update cached active_core
            if let Some(slot) = entries.get_mut(&member_id) {
                let props = &mut slot.props;
                props.active_core = new_core;
            }
        }
    }
}

impl Default for KPriorityQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn assert_queue_invariants(pq: &KPriorityQueue, iteration: usize, operation: &str) {
        let mut scheduled_memberships = HashSet::new();
        let mut suggested_memberships = HashSet::new();

        for core in 0..NUM_CORES as i32 {
            for priority in HIGHEST_PRIORITY..=LOWEST_PRIORITY {
                for (scheduled, queue) in
                    [(true, &pq.scheduled_queue), (false, &pq.suggested_queue)]
                {
                    let root = &queue.queues[priority as usize].roots[core as usize];
                    let mut current = root.get_next();
                    let priority_available = queue.available_priorities[core as usize].bits
                        & (1u64 << (63 - priority))
                        != 0;
                    assert_eq!(
                        priority_available,
                        current.is_some(),
                        "iteration {iteration}: availability bit disagrees with core {core} \
                         priority {priority} after {operation}"
                    );
                    let mut previous = None;
                    let mut seen = HashSet::new();

                    while let Some(thread_id) = current {
                        assert!(
                            seen.insert(thread_id),
                            "cycle in core {core} priority {priority}"
                        );
                        let props = pq.get_thread_props(thread_id).unwrap_or_else(|| {
                            panic!(
                                "iteration {iteration}: queue member {thread_id} on core \
                                     {core} priority {priority} must have cached properties after \
                                     {operation}"
                            )
                        });
                        assert_eq!(
                            props.priority, priority,
                            "iteration {iteration}: thread {thread_id} is linked in the wrong \
                             priority bucket on core {core} after {operation}"
                        );
                        assert_ne!(props.affinity & (1u64 << core), 0);
                        assert_eq!(scheduled, props.active_core == core);

                        let entry = &pq.members[&thread_id][core as usize];
                        assert_eq!(entry.get_prev(), previous);
                        let memberships = if scheduled {
                            &mut scheduled_memberships
                        } else {
                            &mut suggested_memberships
                        };
                        assert!(
                            memberships.insert((thread_id, core)),
                            "duplicate membership for thread {thread_id} on core {core}"
                        );

                        previous = Some(thread_id);
                        current = entry.get_next();
                    }

                    assert_eq!(root.get_prev(), previous);
                }
            }
        }

        for (&thread_id, slot) in &pq.members {
            let props = &slot.props;
            for core in 0..NUM_CORES as i32 {
                let in_affinity = props.affinity & (1u64 << core) != 0;
                assert_eq!(
                    scheduled_memberships.contains(&(thread_id, core)),
                    in_affinity && props.active_core == core
                );
                assert_eq!(
                    suggested_memberships.contains(&(thread_id, core)),
                    in_affinity && props.active_core != core
                );
            }
        }
    }

    fn next_random(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        *state
    }

    #[test]
    fn test_bitset64() {
        let mut bs = BitSet64::new();
        assert_eq!(bs.count_leading_zero(), 64);

        bs.set_bit(5);
        assert_eq!(bs.count_leading_zero(), 5); // Highest priority (lowest number)

        bs.set_bit(3);
        assert_eq!(bs.count_leading_zero(), 3); // 3 < 5, so 3 is highest priority

        bs.clear_bit(3);
        assert_eq!(bs.count_leading_zero(), 5); // Back to 5
    }

    #[test]
    fn test_bitset64_get_next_set() {
        let mut bs = BitSet64::new();
        bs.set_bit(5);
        bs.set_bit(10);
        bs.set_bit(20);

        assert_eq!(bs.get_next_set(3), 5); // Next after 3 is 5
        assert_eq!(bs.get_next_set(5), 10); // Next after 5 is 10
        assert_eq!(bs.get_next_set(10), 20); // Next after 10 is 20
        assert_eq!(bs.get_next_set(20), 64); // Nothing after 20
    }

    #[test]
    fn test_priority_queue_creation() {
        let pq = KPriorityQueue::new();
        for core in 0..NUM_CORES as i32 {
            assert!(pq.get_scheduled_front(core).is_none());
            assert!(pq.get_suggested_front(core).is_none());
        }
    }

    #[test]
    fn test_valid_core_priority() {
        assert!(is_valid_core(0));
        assert!(is_valid_core(3));
        assert!(!is_valid_core(-1));
        assert!(!is_valid_core(NUM_CORES as i32));

        assert!(is_valid_priority(0));
        assert!(is_valid_priority(63));
        assert!(is_valid_priority(64));
        assert!(!is_valid_priority(-1));
        assert!(!is_valid_priority(65));
    }

    #[test]
    fn test_push_back_and_get_front() {
        let mut pq = KPriorityQueue::new();
        // Thread 100: priority 16, active core 3, affinity = cores 0-3
        pq.push_back(100, 16, 3, 0b1111, false, None);
        assert_eq!(pq.get_scheduled_front(3), Some(100));
        // Should be suggested on cores 0, 1, 2
        assert_eq!(pq.get_suggested_front(0), Some(100));
        assert_eq!(pq.get_suggested_front(1), Some(100));
        assert_eq!(pq.get_suggested_front(2), Some(100));
    }

    #[test]
    fn test_push_and_remove() {
        let mut pq = KPriorityQueue::new();
        pq.push_back(100, 16, 3, 0b1111, false, None);
        pq.remove(100, 16, 3, 0b1111, false);
        for core in 0..NUM_CORES as i32 {
            assert!(pq.get_scheduled_front(core).is_none());
            assert!(pq.get_suggested_front(core).is_none());
        }
    }

    #[test]
    fn test_multiple_threads_ordering() {
        let mut pq = KPriorityQueue::new();
        // Two threads on same core, same priority
        pq.push_back(100, 16, 3, 0b1000, false, None);
        pq.push_back(200, 16, 3, 0b1000, false, None);
        // First pushed should be front
        assert_eq!(pq.get_scheduled_front(3), Some(100));
        // Next should be 200
        assert_eq!(pq.get_scheduled_next(3, 100, 16), Some(200));
    }

    #[test]
    fn remove_unlinked_entry_does_not_clear_queue_root() {
        let mut pq = KPriorityQueue::new();
        pq.push_back(100, 16, 0, 0b0001, false, None);
        pq.push_back(200, 20, 1, 0b0010, false, None);

        // Thread 200 has an entry for core 0, but it is not linked in core 0's
        // scheduled queue. Removing it from that queue must be a no-op.
        pq.scheduled_queue.remove(20, 0, 200, &mut pq.members);

        assert_eq!(pq.get_scheduled_front(0), Some(100));
        assert_eq!(pq.get_scheduled_front(1), Some(200));
    }

    #[test]
    fn test_change_core() {
        let mut pq = KPriorityQueue::new();
        pq.push_back(100, 16, 3, 0b1111, false, None);
        assert_eq!(pq.get_scheduled_front(3), Some(100));
        // Migrate to core 0
        pq.change_core(3, 100, 0, 16, false, true);
        assert!(pq.get_scheduled_front(3).is_none());
        assert_eq!(pq.get_scheduled_front(0), Some(100));
        // Should now be suggested on core 3
        assert_eq!(pq.get_suggested_front(3), Some(100));
        // Cached props should reflect new core
        assert_eq!(pq.get_thread_props(100).unwrap().active_core, 0);
    }

    #[test]
    fn test_priority_change_uses_cached_core_after_migration() {
        let mut pq = KPriorityQueue::new();
        pq.push_back(100, 44, 0, 0b0011, false, None);
        pq.change_core(0, 100, 1, 44, false, false);

        // The Rust scheduler applies KThread.core_id migrations after dropping
        // the GSC lock. If a priority change observes the old KThread core
        // during that window, the PQ still has to behave like upstream, where
        // SetActiveCore() already happened before ChangeCore().
        pq.change_priority(44, false, 100, 20, 0, 0b0011, false);

        assert!(pq.get_scheduled_front(0).is_none());
        assert_eq!(pq.get_scheduled_front(1), Some(100));
        assert_eq!(pq.get_thread_props(100).unwrap().priority, 20);
        assert_eq!(pq.get_thread_props(100).unwrap().active_core, 1);

        pq.remove(100, 20, 0, 0b0011, false);
        assert!(pq.get_scheduled_front(0).is_none());
        assert!(pq.get_scheduled_front(1).is_none());
    }

    #[test]
    fn test_change_core_purges_duplicate_old_scheduled_membership() {
        let mut pq = KPriorityQueue::new();
        pq.push_back(82, 44, 2, 0b0110, false, None);
        pq.push_back(90, 44, 2, 0b0100, false, None);

        // Rust-only split-brain shape observed in ANIMUS: the same id remains
        // root-visible more than once on the old scheduled core. Upstream's
        // intrusive QueueEntry cannot represent this, but the id-linked port
        // must purge it before scheduling the thread on the new core.
        pq.scheduled_queue.push_back(44, 2, 82, &mut pq.members);

        pq.change_core(2, 82, 1, 44, false, false);

        assert_eq!(pq.get_scheduled_front(1), Some(82));
        assert_eq!(pq.get_scheduled_front(2), Some(90));
        assert_ne!(pq.get_scheduled_next(2, 90, 44), Some(82));
    }

    #[test]
    fn test_dummy_thread_ignored() {
        let mut pq = KPriorityQueue::new();
        pq.push_back(100, 16, 3, 0b1111, true, None); // is_dummy = true
        assert!(pq.get_scheduled_front(3).is_none());
    }

    #[test]
    fn test_reinserting_same_thread_does_not_leave_stale_membership() {
        let mut pq = KPriorityQueue::new();
        pq.push_back(100, 16, 3, 0b1111, false, None);
        pq.push_back(100, 16, 3, 0b1111, false, None);

        assert_eq!(pq.get_scheduled_front(3), Some(100));

        pq.remove(100, 16, 3, 0b1111, false);

        for core in 0..NUM_CORES as i32 {
            assert!(pq.get_scheduled_front(core).is_none());
            assert!(pq.get_suggested_front(core).is_none());
        }
    }

    #[test]
    fn test_remove_purges_duplicate_root_reachable_membership() {
        let mut pq = KPriorityQueue::new();
        pq.push_back(100, 16, 3, 0b1000, false, None);
        pq.push_back(200, 16, 3, 0b1000, false, None);

        // Force the Rust-only corruption observed in ANIMUS: the same thread id
        // is visible twice in one queue, but only one QueueEntry stores links.
        pq.scheduled_queue.push_back(16, 3, 100, &mut pq.members);

        pq.remove(100, 16, 3, 0b1000, false);

        assert_eq!(pq.get_scheduled_front(3), Some(200));
        assert_eq!(pq.get_scheduled_next(3, 200, 16), None);
    }

    /// Links and properties are one slot, so the split-brain state the port
    /// used to repair — an id still linked in a list after its properties were
    /// dropped — cannot be built any more. `remove` must take both away
    /// together, which is what upstream gets for free by storing the
    /// `QueueEntry` inside `KThread`.
    ///
    /// This replaces four tests that each constructed that state by hand and
    /// asserted a repair path cleaned it up; the repair paths are gone with it.
    #[test]
    fn remove_drops_links_and_properties_together() {
        let mut pq = KPriorityQueue::new();
        pq.push_back(86, 44, 1, 0b0010, false, None);
        pq.push_back(100, 44, 1, 0b0010, false, None);
        assert_eq!(pq.get_scheduled_front(1), Some(86));
        assert!(pq.members.contains_key(&86));

        pq.remove(86, 44, 1, 0b0010, false);

        // Neither half survives: no properties, and no link storage to go
        // stale or to be resurrected by a later rebuild of the list.
        assert!(pq.get_thread_props(86).is_none());
        assert!(
            !pq.members.contains_key(&86),
            "removing a member must drop its link storage too, or it leaks and \
             can be relinked later"
        );
        assert_eq!(pq.get_scheduled_front(1), Some(100));

        pq.remove(100, 44, 1, 0b0010, false);
        assert!(pq.get_scheduled_front(1).is_none());
        assert!(pq.members.is_empty(), "no member outlives its removal");
    }

    #[test]
    fn remove_does_not_resurrect_detached_member_from_another_priority() {
        let mut pq = KPriorityQueue::new();
        pq.push_back(10, 14, 0, 0b0001, false, None);
        pq.push_back(22, 49, 0, 0b0001, false, None);

        // Upstream leaves the removed intrusive QueueEntry's links stale, but
        // its queue identity is fixed by the owning thread's current priority.
        // Reproduce such a detached link into the member being removed from a
        // different bucket. The Rust repair path must not treat that link alone
        // as proof that thread 10 belongs to priority 49.
        pq.members.get_mut(&10).unwrap()[0].set_next(Some(22));

        pq.remove(22, 49, 0, 0b0001, false);

        assert_eq!(pq.get_scheduled_front_at_priority(0, 14), Some(10));
        assert_eq!(pq.get_scheduled_front_at_priority(0, 49), None);
        assert_eq!(pq.get_thread_props(10).unwrap().priority, 14);
    }

    #[test]
    fn valid_upstream_operation_sequences_preserve_queue_membership() {
        let mut pq = KPriorityQueue::new();
        let mut random = 0x4D4B_3844_5051_5545;

        for iteration in 0..50_000 {
            let value = next_random(&mut random);
            let thread_id = 1 + ((value >> 8) % 64);
            let existing = pq.get_thread_props(thread_id).cloned();

            let operation = match (value % 6, existing) {
                (0, None) => {
                    let priority = ((value >> 16) % 64) as i32;
                    let affinity = 1u64 << ((value >> 24) % NUM_CORES as u64);
                    let active_core = affinity.trailing_zeros() as i32;
                    pq.push_back(thread_id, priority, active_core, affinity, false, None);
                    format!(
                        "push tid={thread_id} priority={priority} core={active_core} \
                         affinity={affinity:#x}"
                    )
                }
                (1, Some(props)) => {
                    pq.remove(
                        thread_id,
                        props.priority,
                        props.active_core,
                        props.affinity,
                        false,
                    );
                    format!(
                        "remove tid={thread_id} priority={} core={} affinity={:#x}",
                        props.priority, props.active_core, props.affinity
                    )
                }
                (2, Some(props)) => {
                    let new_priority = ((value >> 16) % 64) as i32;
                    pq.change_priority(
                        props.priority,
                        false,
                        thread_id,
                        new_priority,
                        props.active_core,
                        props.affinity,
                        false,
                    );
                    format!(
                        "change_priority tid={thread_id} {}->{new_priority} core={} affinity={:#x}",
                        props.priority, props.active_core, props.affinity
                    )
                }
                (3, Some(props)) => {
                    let new_affinity = ((value >> 24) & 0xF).max(1);
                    let new_core = new_affinity.trailing_zeros() as i32;
                    pq.change_affinity_mask(
                        props.active_core,
                        props.affinity,
                        thread_id,
                        new_core,
                        new_affinity,
                        props.priority,
                        false,
                    );
                    format!(
                        "change_affinity tid={thread_id} core={}->{new_core} affinity={:#x}->{new_affinity:#x} priority={}",
                        props.active_core, props.affinity, props.priority
                    )
                }
                (4, Some(props)) => {
                    let allowed_cores: Vec<i32> = (0..NUM_CORES as i32)
                        .filter(|core| props.affinity & (1u64 << core) != 0)
                        .collect();
                    let new_core = allowed_cores[((value >> 32) as usize) % allowed_cores.len()];
                    pq.change_core(
                        props.active_core,
                        thread_id,
                        new_core,
                        props.priority,
                        false,
                        value & (1 << 40) != 0,
                    );
                    format!(
                        "change_core tid={thread_id} {}->{new_core} priority={} affinity={:#x}",
                        props.active_core, props.priority, props.affinity
                    )
                }
                (5, Some(props)) => {
                    if value & (1 << 40) == 0 {
                        pq.move_to_scheduled_front(
                            thread_id,
                            props.priority,
                            props.active_core,
                            false,
                        );
                        format!(
                            "move_front tid={thread_id} priority={} core={}",
                            props.priority, props.active_core
                        )
                    } else {
                        pq.move_to_scheduled_back(
                            thread_id,
                            props.priority,
                            props.active_core,
                            false,
                        );
                        format!(
                            "move_back tid={thread_id} priority={} core={}",
                            props.priority, props.active_core
                        )
                    }
                }
                _ => format!("noop tid={thread_id}"),
            };

            assert_queue_invariants(&pq, iteration, &operation);
        }

        assert_queue_invariants(&pq, 50_000, "final validation");
    }
}

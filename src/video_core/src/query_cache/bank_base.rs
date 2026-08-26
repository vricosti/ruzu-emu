// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's `src/video_core/query_cache/bank_base.h`.
//!
//! Provides the base bank allocation type (`BankBase`) and a pool manager (`BankPool`)
//! used by the query cache to manage slots for host queries.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Base class for query banks. Manages a fixed-size block of query slots.
///
/// Maps to C++ `BankBase`.
pub struct BankBase {
    /// Original bank size (immutable after construction).
    base_bank_size: usize,
    /// Current effective bank size (may shrink when closed).
    bank_size: usize,
    /// Number of active references to slots in this bank.
    references: AtomicUsize,
    /// Next slot to allocate.
    current_slot: usize,
}

impl BankBase {
    /// Create a new bank with the given capacity.
    pub fn new(bank_size: usize) -> Self {
        Self {
            base_bank_size: bank_size,
            bank_size,
            references: AtomicUsize::new(0),
            current_slot: 0,
        }
    }

    /// Try to reserve a slot. Returns `(true, slot_index)` on success,
    /// or `(false, bank_size)` if the bank is closed.
    pub fn reserve(&mut self) -> (bool, usize) {
        if self.is_closed() {
            return (false, self.bank_size);
        }
        let result = self.current_slot;
        self.current_slot += 1;
        (true, result)
    }

    /// Reset the bank to its initial state for reuse.
    pub fn reset(&mut self) {
        self.current_slot = 0;
        self.references.store(0, Ordering::SeqCst);
        self.bank_size = self.base_bank_size;
    }

    /// Return the current effective bank size.
    pub fn size(&self) -> usize {
        self.bank_size
    }

    /// Add reference count.
    pub fn add_reference(&self, how_many: usize) {
        self.references.fetch_add(how_many, Ordering::Relaxed);
    }

    /// Close (release) references. Panics if releasing more than held.
    pub fn close_reference(&self, how_many: usize) {
        let current = self.references.load(Ordering::Relaxed);
        assert!(
            how_many <= current,
            "BankBase::close_reference: releasing {} but only {} held",
            how_many,
            current
        );
        self.references.fetch_sub(how_many, Ordering::Relaxed);
    }

    /// Close the bank: freeze its size at the current allocation point.
    pub fn close(&mut self) {
        self.bank_size = self.current_slot;
    }

    /// Returns true if no more slots can be reserved.
    pub fn is_closed(&self) -> bool {
        self.current_slot >= self.bank_size
    }

    /// Returns true if the bank is closed and has no remaining references.
    pub fn is_dead(&self) -> bool {
        self.is_closed() && self.references.load(Ordering::SeqCst) == 0
    }
}

/// A pool of banks that recycles dead banks.
///
/// Maps to C++ `BankPool<BankType>`. In the Rust port, the bank type is
/// parameterized via the `BankLike` trait rather than a C++ template parameter.
pub struct BankPool<B> {
    /// Storage for all banks ever created.
    bank_pool: VecDeque<B>,
    /// Indices of banks in allocation order (front = oldest).
    bank_indices: VecDeque<usize>,
}

/// Trait abstracting the bank interface so `BankPool` can be generic.
pub trait BankLike {
    fn is_dead(&self) -> bool;
    fn reset(&mut self);
}

impl BankLike for BankBase {
    fn is_dead(&self) -> bool {
        BankBase::is_dead(self)
    }
    fn reset(&mut self) {
        BankBase::reset(self);
    }
}

impl<B: BankLike> BankPool<B> {
    /// Create an empty bank pool.
    pub fn new() -> Self {
        Self {
            bank_pool: VecDeque::new(),
            bank_indices: VecDeque::new(),
        }
    }

    /// Reserve a bank from the pool, recycling a dead one if possible.
    /// If no dead bank is available, `builder` is called to construct a new one.
    ///
    /// Maps to C++ `BankPool::ReserveBank`.
    pub fn reserve_bank<F, E>(&mut self, builder: F) -> Result<usize, E>
    where
        F: FnOnce(&mut VecDeque<B>, usize) -> Result<(), E>,
    {
        if let Some(&front_idx) = self.bank_indices.front() {
            if self.bank_pool[front_idx].is_dead() {
                let new_index = self.bank_indices.pop_front().unwrap();
                self.bank_pool[new_index].reset();
                self.bank_indices.push_back(new_index);
                return Ok(new_index);
            }
        }
        let new_index = self.bank_pool.len();
        builder(&mut self.bank_pool, new_index)?;
        self.bank_indices.push_back(new_index);
        Ok(new_index)
    }

    /// Get a mutable reference to a bank by index.
    pub fn get_bank(&mut self, index: usize) -> &mut B {
        &mut self.bank_pool[index]
    }

    /// Return the total number of banks in the pool.
    pub fn bank_count(&self) -> usize {
        self.bank_pool.len()
    }
}

impl<B: BankLike> Default for BankPool<B> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Upstream `BankBase::Close` freezes the bank at its allocation point, so
    /// a closed bank hands out no further slots (`bank_base.h:55`).
    #[test]
    fn close_freezes_the_bank_at_the_current_slot() {
        let mut bank = BankBase::new(4);
        assert_eq!(bank.reserve(), (true, 0));
        assert_eq!(bank.reserve(), (true, 1));

        bank.close();

        assert!(bank.is_closed());
        assert_eq!(bank.size(), 2);
        assert_eq!(bank.reserve(), (false, 2));
    }

    /// `IsDead` requires both conditions: closed *and* unreferenced.
    #[test]
    fn is_dead_requires_closed_and_unreferenced() {
        let mut bank = BankBase::new(2);
        let (_, _) = bank.reserve();
        bank.add_reference(1);
        bank.close();
        assert!(!bank.is_dead());

        bank.close_reference(1);
        assert!(bank.is_dead());
    }

    #[test]
    fn reserve_bank_only_calls_builder_when_no_dead_bank_exists() {
        let mut pool: BankPool<BankBase> = BankPool::new();

        let first = pool
            .reserve_bank(|queue, _index| {
                queue.push_back(BankBase::new(1));
                Ok::<(), ()>(())
            })
            .unwrap();
        assert_eq!(pool.bank_count(), 1);

        // The only bank is still open, so nothing can be recycled and
        // `reserve_bank` must build a second one.
        let second = pool
            .reserve_bank(|queue, _index| {
                queue.push_back(BankBase::new(1));
                Ok::<(), ()>(())
            })
            .unwrap();
        assert_ne!(first, second);
        assert_eq!(pool.bank_count(), 2);

        // Exhaust the first bank so it becomes dead, and it is recycled instead.
        let bank = pool.get_bank(first);
        let (_, _) = bank.reserve();
        assert!(bank.is_dead());
        let recycled: Result<usize, ()> =
            pool.reserve_bank(|_queue, _index| panic!("builder must not run"));
        let recycled = recycled.unwrap();
        assert_eq!(recycled, first);
        assert_eq!(pool.bank_count(), 2);
    }

    #[test]
    fn reserve_bank_propagates_builder_failure_without_indexing_a_bank() {
        let mut pool: BankPool<BankBase> = BankPool::new();
        let result = pool.reserve_bank(|_queue, _index| Err("creation failed"));
        assert_eq!(result, Err("creation failed"));
        assert_eq!(pool.bank_count(), 0);
    }
}

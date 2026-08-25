// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `vk_resource_pool.h` / `vk_resource_pool.cpp`.
//!
//! Generic pool of GPU resources protected by timeline tick fences.
//! Automatically grows when all resources are in use.

use std::sync::Arc;

use super::master_semaphore::MasterSemaphore;

// ---------------------------------------------------------------------------
// ResourcePool
// ---------------------------------------------------------------------------

/// Port of `ResourcePool` class.
///
/// Base type for managing a growable pool of GPU resources where each
/// resource slot is tagged with a timeline tick. When a slot's tick
/// has been completed by the GPU, the slot can be reused.
pub struct ResourcePool {
    /// Reference to the master semaphore for tick queries.
    master_semaphore: Option<Arc<MasterSemaphore>>,

    /// Number of new resources created on overflow.
    grow_step: usize,

    /// Hint iterator pointing to the likely next free resource.
    hint_iterator: usize,

    /// Timeline tick for each resource slot.
    ticks: Vec<u64>,
}

impl ResourcePool {
    /// Port of `ResourcePool::ResourcePool` (default).
    pub fn new_default() -> Self {
        ResourcePool {
            master_semaphore: None,
            grow_step: 0,
            hint_iterator: 0,
            ticks: Vec::new(),
        }
    }

    /// Port of `ResourcePool::ResourcePool(MasterSemaphore&, size_t)`.
    pub fn new(master_semaphore: Arc<MasterSemaphore>, grow_step: usize) -> Self {
        ResourcePool {
            master_semaphore: Some(master_semaphore),
            grow_step,
            hint_iterator: 0,
            ticks: Vec::new(),
        }
    }

    /// Port of `ResourcePool::CommitResource`.
    ///
    /// Finds and returns the index of a free resource slot, growing
    /// the pool if necessary. Calls `allocate_fn(begin, end)` when new
    /// resources must be created.
    pub fn commit_resource(&mut self, allocate_fn: &mut dyn FnMut(usize, usize)) -> usize {
        let found = {
            let ms = self
                .master_semaphore
                .as_deref()
                .expect("ResourcePool: master_semaphore not set");
            let found =
                Self::find_free(&mut self.ticks, self.hint_iterator, ms.known_gpu_tick(), ms);
            found.or_else(|| {
                ms.refresh();
                Self::find_free(&mut self.ticks, self.hint_iterator, ms.known_gpu_tick(), ms)
            })
        };
        let found = found.unwrap_or_else(|| {
            let free_resource = self.manage_overflow(allocate_fn);
            self.ticks[free_resource] = self
                .master_semaphore
                .as_deref()
                .expect("ResourcePool: master_semaphore not set")
                .current_tick();
            free_resource
        });
        self.hint_iterator = (found + 1) % self.ticks.len();
        found
    }

    /// Fallible Rust adaptation of `CommitResource`.
    ///
    /// Upstream propagates allocation failures through exceptions from the
    /// virtual `Allocate` call. Rust callers that allocate Vulkan resources
    /// need the same behavior through `Result`.
    pub fn try_commit_resource<E>(
        &mut self,
        allocate_fn: &mut dyn FnMut(usize, usize) -> Result<(), E>,
    ) -> Result<usize, E> {
        let found = {
            let ms = self
                .master_semaphore
                .as_deref()
                .expect("ResourcePool: master_semaphore not set");
            let found =
                Self::find_free(&mut self.ticks, self.hint_iterator, ms.known_gpu_tick(), ms);
            found.or_else(|| {
                ms.refresh();
                Self::find_free(&mut self.ticks, self.hint_iterator, ms.known_gpu_tick(), ms)
            })
        };
        let found = match found {
            Some(found) => found,
            None => {
                let free_resource = self.try_manage_overflow(allocate_fn)?;
                self.ticks[free_resource] = self
                    .master_semaphore
                    .as_deref()
                    .expect("ResourcePool: master_semaphore not set")
                    .current_tick();
                free_resource
            }
        };

        self.hint_iterator = (found + 1) % self.ticks.len();
        Ok(found)
    }

    // --- Private ---

    fn find_free(
        ticks: &mut [u64],
        hint_iterator: usize,
        gpu_tick: u64,
        master_semaphore: &MasterSemaphore,
    ) -> Option<usize> {
        let search = |ticks: &mut [u64], begin: usize, end: usize| -> Option<usize> {
            for iterator in begin..end {
                if gpu_tick >= ticks[iterator] {
                    ticks[iterator] = master_semaphore.current_tick();
                    return Some(iterator);
                }
            }
            None
        };
        let ticks_len = ticks.len();
        search(ticks, hint_iterator, ticks_len).or_else(|| search(ticks, 0, hint_iterator))
    }

    /// Port of `ResourcePool::ManageOverflow`.
    fn manage_overflow(&mut self, allocate_fn: &mut dyn FnMut(usize, usize)) -> usize {
        let old_capacity = self.ticks.len();
        self.grow(allocate_fn);

        // The last entry is guaranteed to be free, since it's the first element
        // of the freshly allocated resources.
        old_capacity
    }

    /// Fallible Rust adaptation of `ResourcePool::ManageOverflow`.
    fn try_manage_overflow<E>(
        &mut self,
        allocate_fn: &mut dyn FnMut(usize, usize) -> Result<(), E>,
    ) -> Result<usize, E> {
        let old_capacity = self.ticks.len();
        self.try_grow(allocate_fn)?;
        Ok(old_capacity)
    }

    /// Port of `ResourcePool::Grow`.
    fn grow(&mut self, allocate_fn: &mut dyn FnMut(usize, usize)) {
        let old_capacity = self.ticks.len();
        self.ticks.resize(old_capacity + self.grow_step, 0);
        allocate_fn(old_capacity, old_capacity + self.grow_step);
    }

    /// Fallible Rust adaptation of `ResourcePool::Grow`.
    fn try_grow<E>(
        &mut self,
        allocate_fn: &mut dyn FnMut(usize, usize) -> Result<(), E>,
    ) -> Result<(), E> {
        let old_capacity = self.ticks.len();
        self.ticks.resize(old_capacity + self.grow_step, 0);
        allocate_fn(old_capacity, old_capacity + self.grow_step)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_pool_default() {
        let pool = ResourcePool::new_default();
        assert_eq!(pool.grow_step, 0);
        assert_eq!(pool.hint_iterator, 0);
        assert!(pool.ticks.is_empty());
    }

    #[test]
    fn fallible_growth_resizes_ticks_before_allocating() {
        let mut pool = ResourcePool::new_default();
        pool.grow_step = 2;
        let result = pool.try_grow(&mut |_, _| Err::<(), _>("allocation failed"));
        assert_eq!(result, Err("allocation failed"));
        assert_eq!(pool.ticks.len(), 2);
    }
}

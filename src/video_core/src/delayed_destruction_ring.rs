// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/delayed_destruction_ring.h
//!
//! Container to push objects to be destroyed a few ticks in the future.

/// A ring buffer that defers destruction of objects by `TICKS_TO_DESTROY` ticks.
///
/// Each call to [`tick`] advances to the next slot, clearing any objects in that slot.
/// Objects pushed via [`push`] are placed in the current slot and will be dropped
/// when that slot is cleared after a full cycle.
#[derive(Clone)]
pub struct DelayedDestructionRing<T, const TICKS_TO_DESTROY: usize> {
    index: usize,
    elements: [Vec<T>; TICKS_TO_DESTROY],
}

impl<T, const TICKS_TO_DESTROY: usize> DelayedDestructionRing<T, TICKS_TO_DESTROY> {
    /// Creates a new ring with `TICKS_TO_DESTROY` slots.
    pub fn new() -> Self {
        Self {
            index: 0,
            elements: std::array::from_fn(|_| Vec::new()),
        }
    }

    /// Advances to the next tick, dropping all objects in the new slot.
    pub fn tick(&mut self) {
        self.index = (self.index + 1) % TICKS_TO_DESTROY;
        self.elements[self.index].clear();
    }

    /// Pushes an object into the current slot for deferred destruction.
    pub fn push(&mut self, object: T) {
        self.elements[self.index].push(object);
    }

    #[cfg(test)]
    pub(crate) fn retained_len(&self) -> usize {
        self.elements.iter().map(Vec::len).sum()
    }
}

impl<T, const TICKS_TO_DESTROY: usize> Default for DelayedDestructionRing<T, TICKS_TO_DESTROY> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use super::*;

    struct DropCounter(Rc<Cell<usize>>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn object_is_destroyed_after_one_complete_ring() {
        let drops = Rc::new(Cell::new(0));
        let mut ring = DelayedDestructionRing::<_, 3>::new();
        ring.push(DropCounter(Rc::clone(&drops)));

        ring.tick();
        ring.tick();
        assert_eq!(drops.get(), 0);
        ring.tick();
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn outer_storage_matches_the_const_generic_size() {
        let ring = DelayedDestructionRing::<u32, 5>::new();
        assert_eq!(ring.elements.len(), 5);
    }
}

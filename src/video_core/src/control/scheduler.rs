// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's `video_core/control/scheduler.h` and `scheduler.cpp`.
//!
//! The `Scheduler` receives command lists from host threads and dispatches
//! them to the correct GPU channel's DMA pusher under a global scheduling
//! lock.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

use super::channel_state::ChannelState;
use crate::dma_pusher::CommandList;

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

/// GPU channel scheduler.
///
/// Corresponds to `Tegra::Control::Scheduler` in upstream.
///
pub struct Scheduler {
    /// Combines upstream's `channels` map and `scheduling_guard`: the mutex
    /// protects the map for the same duration as Eden's scheduling guard.
    channels: Mutex<HashMap<i32, Arc<Mutex<ChannelState>>>>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            channels: Mutex::new(HashMap::new()),
        }
    }

    /// Push a command list to a channel for execution.
    ///
    /// Corresponds to `Scheduler::Push(GPU&, s32, CommandList&&)`.
    pub fn push(&self, gpu: &crate::gpu::Gpu, channel: i32, entries: CommandList) {
        let channel_state = {
            let channels = self.channels.lock();
            let channel_state = Arc::clone(
                channels
                    .get(&channel)
                    .expect("Scheduler::push: channel not found"),
            );

            let bind_id = channel_state.lock().bind_id;
            gpu.bind_channel(bind_id);
            channel_state
        };

        let mut cs = channel_state.lock();
        let dma_pusher = cs
            .dma_pusher
            .as_mut()
            .expect("Scheduler::push: dma_pusher not initialized");
        dma_pusher.push(entries);
        dma_pusher.dispatch_calls();
    }

    /// Register a channel with the scheduler.
    ///
    /// Corresponds to `Scheduler::DeclareChannel(shared_ptr<ChannelState>)`.
    pub fn declare_channel(&self, new_channel: Arc<Mutex<ChannelState>>) {
        let bind_id = new_channel.lock().bind_id;
        self.channels.lock().entry(bind_id).or_insert(new_channel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_declare_channel() {
        let sched = Scheduler::new();

        let cs = Arc::new(Mutex::new(ChannelState::new(5)));
        sched.declare_channel(cs);

        assert!(sched.channels.lock().contains_key(&5));
    }

    #[test]
    fn declaring_an_existing_channel_preserves_the_first_entry_like_emplace() {
        let sched = Scheduler::new();
        let first = Arc::new(Mutex::new(ChannelState::new(5)));
        let duplicate = Arc::new(Mutex::new(ChannelState::new(5)));

        sched.declare_channel(Arc::clone(&first));
        sched.declare_channel(duplicate);

        let channels = sched.channels.lock();
        assert!(Arc::ptr_eq(channels.get(&5).unwrap(), &first));
    }
}

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
    pending_channels: Mutex<Vec<Arc<Mutex<ChannelState>>>>,
}

// Non-owning capture of upstream BindChannel's [this, gpu_ptr, bind_id].
// ThreadManager must join the backend before either owner can move or drop.
#[derive(Clone, Copy)]
struct DeferredBind {
    scheduler: *const Scheduler,
    gpu: *const crate::gpu::Gpu,
    bind_id: i32,
}
// Only the GPU frontend submits these captures; the backend serializes their
// execution. The scheduling guard remains acquired on the backend, not while
// enqueueing (the ring may wait for that same backend to make space).
unsafe impl Send for DeferredBind {}
impl DeferredBind {
    unsafe fn run(self) {
        let scheduler = unsafe { &*self.scheduler };
        let gpu = unsafe { &*self.gpu };
        let _guard = scheduler.channels.lock();
        gpu.bind_channel(self.bind_id);
    }
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            channels: Mutex::new(HashMap::new()),
            pending_channels: Mutex::new(Vec::new()),
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

            channel_state
        };
        // Clone the DMA handle under the channel lock, then release that lock
        // before dispatch. A split backend must be able to bind this channel
        // while the frontend waits for previously submitted work to drain.
        let (bind_id, dma) = {
            let cs = channel_state.lock();
            (cs.bind_id, Arc::clone(cs.dma_pusher.as_ref().expect("channel DMA")))
        };
        self.bind_channel(gpu, bind_id);
        let pending = {
            let mut dma = dma.lock();
            dma.push(entries);
            dma.dispatch_calls();
            dma.has_pending_acquire()
        };
        if pending {
            let mut channels = self.pending_channels.lock();
            if !channels.iter().any(|state| Arc::ptr_eq(state, &channel_state)) {
                channels.push(channel_state);
            }
        }
        if self.has_pending_channels() {
            self.resume_pending(gpu);
        }
    }

    fn bind_channel(&self, gpu: &crate::gpu::Gpu, bind_id: i32) {
        if let Some(split) = crate::gpu_split::current() {
            let bind = DeferredBind { scheduler: self, gpu, bind_id };
            split.defer(move || unsafe { bind.run() });
            return;
        }
        let _guard = self.channels.lock();
        gpu.bind_channel(bind_id);
    }

    pub fn has_pending_channels(&self) -> bool {
        !self.pending_channels.lock().is_empty()
    }

    /// Running one channel may release another: retry until none advances.
    /// The GPU thread owns dispatch; this mutex gives Rust the same shared
    /// channel lifetime as upstream's vector of shared_ptrs.
    pub fn resume_pending(&self, gpu: &crate::gpu::Gpu) {
        let mut pending = self.pending_channels.lock();
        let mut progress = true;
        while progress {
            progress = false;
            for state in pending.iter() {
                let (bind_id, dma) = {
                    let cs = state.lock();
                    (cs.bind_id, Arc::clone(cs.dma_pusher.as_ref().expect("pending channel DMA")))
                };
                let mut dma = dma.lock();
                // DispatchCalls does no work after power-off; do not count it
                // as progress and keep the GPU thread from observing stop.
                if !dma.is_powered_on() { return; }
                if !dma.has_pending_acquire() || !dma.can_resume_acquire() { continue; }
                self.bind_channel(gpu, bind_id);
                dma.dispatch_calls();
                progress = true;
            }
        }
        pending.retain(|state| {
            let dma = Arc::clone(state.lock().dma_pusher.as_ref().unwrap());
            let waiting = dma.lock().has_pending_acquire();
            waiting
        });
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
    fn split_binding_enqueues_outside_the_scheduling_guard() {
        let gpu = Box::new(crate::gpu::Gpu::new(false, false));
        gpu.create_channel(5);
        let scheduler = Box::new(Scheduler::new());
        let split = crate::gpu_split::GpuSplitFrontend::new(false);
        let scope = split.install();
        split.start_back(|| {});
        // BindChannel must not acquire the guard on the producer. The backend
        // takes it only after the queued command reaches its execution point.
        let guard = scheduler.channels.lock();
        scheduler.bind_channel(&gpu, 5);
        drop(guard);
        split.drain();
        split.stop().unwrap();
        drop(scope);
        assert!(scheduler.channels.try_lock().is_some());
    }

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

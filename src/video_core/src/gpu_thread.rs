// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/gpu_thread.h and video_core/gpu_thread.cpp
//!
//! Threaded GPU command queue for asynchronous GPU processing.
//! Matches upstream structure: ThreadManager owns a dedicated OS thread
//! that pops commands from an SPSC queue and dispatches them.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use crate::control::scheduler::Scheduler;
use crate::dma_pusher::CommandList;
use crate::rasterizer_interface::RasterizerHandle;
use common::bounded_threadsafe_queue::BoundedSPSCQueue;
use common::settings;
use common::thread::{
    set_current_thread_name, set_current_thread_priority, set_current_thread_to_performance_cores,
    ThreadPriority,
};
use ruzu_core::core::SystemRef;
use ruzu_core::frontend::graphics_context::{GraphicsContextHandle, ScopedGraphicsContext};

/// Device address type.
pub type DAddr = u64;

// ---------------------------------------------------------------------------
// Command types — matches upstream gpu_thread.h
// ---------------------------------------------------------------------------

/// Command to signal that a command list is ready for processing.
pub struct SubmitListCommand {
    pub channel: i32,
    pub entries: CommandList,
}

/// Command to flush a region.
pub struct FlushRegionCommand {
    pub addr: DAddr,
    pub size: u64,
}

/// Command to invalidate a region.
pub struct InvalidateRegionCommand {
    pub addr: DAddr,
    pub size: u64,
}

/// Command to flush and invalidate a region.
pub struct FlushAndInvalidateRegionCommand {
    pub addr: DAddr,
    pub size: u64,
}

/// Command to make the GPU process pending requests.
pub struct GpuTickCommand;

/// All possible GPU thread commands.
/// Matches upstream `CommandData` variant.
pub enum CommandData {
    None,
    SubmitList(SubmitListCommand),
    FlushRegion(FlushRegionCommand),
    InvalidateRegion(InvalidateRegionCommand),
    FlushAndInvalidateRegion(FlushAndInvalidateRegionCommand),
    GpuTick(GpuTickCommand),
}

/// Container for a command with fence tracking.
/// Matches upstream `CommandDataContainer`.
pub struct CommandDataContainer {
    pub data: CommandData,
    pub fence: u64,
    pub block: bool,
}

impl Default for CommandDataContainer {
    fn default() -> Self {
        Self {
            data: CommandData::None,
            fence: 0,
            block: false,
        }
    }
}

// ---------------------------------------------------------------------------
// SynchState — matches upstream gpu_thread.h SynchState
// ---------------------------------------------------------------------------

/// Synchronization state for the GPU thread.
///
/// Upstream uses `Common::SPSCQueue<CommandDataContainer>` and serializes
/// producers with `write_lock`.
pub struct SynchState {
    pub write_lock: Mutex<()>,
    pub queue: BoundedSPSCQueue<CommandDataContainer>,
    pub last_fence: AtomicU64,
    pub signaled_fence: AtomicU64,
    /// Condvar to notify callers that a blocking command has completed.
    pub cv: Condvar,
}

impl SynchState {
    pub fn new() -> Self {
        Self {
            write_lock: Mutex::new(()),
            queue: BoundedSPSCQueue::with_default_capacity(),
            last_fence: AtomicU64::new(0),
            signaled_fence: AtomicU64::new(0),
            cv: Condvar::new(),
        }
    }

    /// Pop a command from the queue, blocking until one is available or stop is requested.
    /// Matches upstream `state.queue.PopWait(next, stop_token)`.
    pub fn pop_wait(&self, stop: &AtomicBool) -> Option<CommandDataContainer> {
        self.queue
            .pop_wait_with_stop(|| stop.load(Ordering::Relaxed))
    }

    /// Push a command to the queue and wake the consumer.
    /// Matches upstream `state.queue.EmplaceWait(...)`.
    pub fn emplace(&self, cmd: CommandDataContainer) {
        self.queue.emplace_wait(cmd);
    }

    pub fn notify_all(&self) {
        self.queue.notify_all();
    }
}

// ---------------------------------------------------------------------------
// ThreadManager — matches upstream gpu_thread.h/cpp ThreadManager
// ---------------------------------------------------------------------------

/// Manager for the GPU processing thread.
///
/// Matches upstream `VideoCommon::GPUThread::ThreadManager`.
pub struct ThreadManager {
    /// Upstream owner: `Core::System& system`.
    system: SystemRef,
    state: Arc<SynchState>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Matches upstream `VideoCore::RasterizerInterface* rasterizer`.
    rasterizer: Option<RasterizerHandle>,
    /// Raw pointer to owning GPU, installed by `start_thread`.
    ///
    /// Upstream reaches this through `system.GPU()` in `FlushRegion`.
    /// Rust stores the pointer already supplied to `StartThread` so the
    /// `ThreadManager` can call the same owner-local sync-operation methods.
    gpu: Option<usize>,
}

// Safety: ThreadManager is accessed under Gpu's Mutex lock.
// The rasterizer pointer is valid for the lifetime of the renderer.
unsafe impl Send for ThreadManager {}

impl ThreadManager {
    /// Creates a new thread manager.
    /// Matches upstream `ThreadManager::ThreadManager(Core::System&)`.
    pub fn new(system: SystemRef) -> Self {
        Self {
            system,
            state: Arc::new(SynchState::new()),
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
            rasterizer: None,
            gpu: None,
        }
    }

    pub fn set_system_ref(&mut self, system: SystemRef) {
        self.system = system;
    }

    /// Get the rasterizer pointer, or None if not set.
    fn rasterizer(&self) -> Option<RasterizerHandle> {
        self.rasterizer
    }

    /// Creates and starts the GPU thread.
    ///
    /// Matches upstream `ThreadManager::StartThread(renderer, context, scheduler)`.
    /// The thread runs `run_thread` which pops commands and dispatches them.
    ///
    /// # Safety
    /// `gpu_ptr`, `context_ptr`, `renderer_ptr`, and `scheduler_ptr` must remain
    /// valid for the lifetime of the thread.
    pub unsafe fn start_thread(
        &mut self,
        gpu_ptr: *const crate::gpu::Gpu,
        renderer_ptr: *mut dyn crate::renderer_base::RendererBase,
        context_ptr: *mut dyn ruzu_core::frontend::graphics_context::GraphicsContext,
        scheduler_ptr: *const Scheduler,
    ) {
        // Extract rasterizer from renderer, matching upstream:
        //   rasterizer = renderer.ReadRasterizer();
        let rasterizer_ptr = unsafe { &*renderer_ptr }.read_rasterizer();
        self.rasterizer = Some(RasterizerHandle::from_ref(unsafe { &*rasterizer_ptr }));
        self.gpu = Some(gpu_ptr as usize);

        let state = self.state.clone();
        let stop = self.stop.clone();
        let system = self.system;
        let gpu = gpu_ptr as usize;
        let sched = scheduler_ptr as usize;
        let rasterizer = self
            .rasterizer
            .expect("GPU thread rasterizer was just installed");
        assert!(
            !context_ptr.is_null(),
            "GPU thread requires a graphics context"
        );
        let context = GraphicsContextHandle::from_ref(unsafe { &*context_ptr });

        let handle = std::thread::Builder::new()
            .name("GPU".to_string())
            .spawn(move || {
                set_current_thread_name("GPU");
                set_current_thread_priority(ThreadPriority::Critical);
                set_current_thread_to_performance_cores();
                system.get().kernel().unwrap().register_host_thread();
                let gpu_ref = unsafe { &*(gpu as *const crate::gpu::Gpu) };
                let scheduler_ref = unsafe { &*(sched as *const Scheduler) };

                // Upstream: auto current_context = context.Acquire();
                let _current_context = unsafe { ScopedGraphicsContext::new(context) };

                run_thread(&state, &stop, gpu_ref, scheduler_ref, rasterizer);
            })
            .expect("Failed to spawn GPU thread");

        self.thread = Some(handle);
    }

    /// Push GPU command entries to be processed.
    /// Matches upstream `ThreadManager::SubmitList(s32, CommandList&&)`.
    pub fn submit_list(&self, channel: i32, entries: CommandList, is_async: bool) {
        self.push_command(
            CommandData::SubmitList(SubmitListCommand { channel, entries }),
            false,
            is_async,
        );
    }

    /// Notify rasterizer that a region should be flushed to Switch memory.
    /// Matches upstream `ThreadManager::FlushRegion(DAddr, u64)`.
    pub fn flush_region(&self, addr: DAddr, size: u64, is_async: bool) {
        if !is_async {
            self.push_command(
                CommandData::FlushRegion(FlushRegionCommand { addr, size }),
                false,
                is_async,
            );
        }
    }

    /// Notify rasterizer that a region should be invalidated.
    /// Matches upstream `ThreadManager::InvalidateRegion(DAddr, u64)`.
    ///
    /// Upstream calls directly on the rasterizer (NOT queued to the GPU thread).
    pub fn invalidate_region(&self, addr: DAddr, size: u64) {
        let rasterizer = self
            .rasterizer()
            .expect("InvalidateRegion requires a started GPU thread");
        // Safety: rasterizer pointer is valid for the lifetime of the renderer.
        unsafe { rasterizer.as_mut() }.on_cache_invalidation(addr, size);
    }

    /// Notify rasterizer that a region should be flushed and invalidated.
    /// Matches upstream `ThreadManager::FlushAndInvalidateRegion(DAddr, u64)`.
    ///
    /// Upstream flushes at High accuracy, then directly invalidates the rasterizer cache.
    pub fn flush_and_invalidate_region(&self, addr: DAddr, size: u64, is_async: bool) {
        if settings::is_gpu_level_high(&settings::values()) {
            if !is_async {
                self.push_command(
                    CommandData::FlushRegion(FlushRegionCommand { addr, size }),
                    false,
                    is_async,
                );
            } else {
                let gpu = self
                    .gpu
                    .expect("FlushAndInvalidateRegion requires a started GPU thread");
                let gpu = unsafe { &*(gpu as *const crate::gpu::Gpu) };
                let fence = gpu.request_flush(addr, size as usize);
                self.tick_gpu(is_async);
                gpu.wait_for_sync_operation(fence);
            }
        }
        let rasterizer = self
            .rasterizer()
            .expect("FlushAndInvalidateRegion requires a started GPU thread");
        // Safety: rasterizer pointer is valid for the lifetime of the renderer.
        unsafe { rasterizer.as_mut() }.on_cache_invalidation(addr, size);
    }

    /// Tick the GPU to process pending requests.
    /// Matches upstream `ThreadManager::TickGPU()`.
    pub fn tick_gpu(&self, is_async: bool) {
        self.push_command(CommandData::GpuTick(GpuTickCommand), false, is_async);
    }

    /// Push a command to be executed by the GPU thread.
    /// Matches upstream `ThreadManager::PushCommand(CommandData&&, bool)`.
    fn push_command(&self, command_data: CommandData, mut block: bool, is_async: bool) -> u64 {
        if !is_async {
            block = true;
        }

        let lock = self.state.write_lock.lock().unwrap();
        let fence = self
            .state
            .last_fence
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        self.state.emplace(CommandDataContainer {
            data: command_data,
            fence,
            block,
        });

        if block {
            let _guard = self
                .state
                .cv
                .wait_while(lock, |_| {
                    !self.stop.load(Ordering::Relaxed)
                        && fence > self.state.signaled_fence.load(Ordering::Relaxed)
                })
                .unwrap();
        }

        fence
    }

    /// Request the GPU thread to stop.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.state.notify_all();
        self.state.cv.notify_all();
    }

    /// Stop and join the GPU thread while its borrowed renderer and scheduler
    /// are still alive.
    ///
    /// Upstream gets the renderer ordering from C++ reverse member
    /// destruction: `GPU::Impl::gpu_thread` is destroyed before `renderer`.
    /// Rust drops fields in declaration order and frees its boxed scheduler,
    /// so `Gpu::drop` calls this explicitly before either borrowed owner.
    pub(crate) fn shutdown(&mut self) {
        self.request_stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ThreadManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ---------------------------------------------------------------------------
// RunThread — matches upstream gpu_thread.cpp RunThread()
// ---------------------------------------------------------------------------

/// The GPU thread entry point.
///
/// Matches upstream `static void RunThread(stop_token, system, renderer, context, scheduler, state)`.
/// Pops commands from the queue and dispatches them.
fn run_thread(
    state: &SynchState,
    stop: &AtomicBool,
    gpu: &crate::gpu::Gpu,
    scheduler: &Scheduler,
    rasterizer: RasterizerHandle,
) {
    while !stop.load(Ordering::Relaxed) {
        let Some(next) = state.pop_wait(stop) else {
            break; // Stop requested
        };
        if stop.load(Ordering::Relaxed) {
            break;
        }

        match next.data {
            CommandData::SubmitList(submit) => {
                scheduler.push(gpu, submit.channel, submit.entries);
            }
            CommandData::GpuTick(_) => {
                gpu.tick_work();
            }
            CommandData::FlushRegion(flush) => {
                // Upstream: rasterizer->FlushRegion(flush.addr, flush.size)
                unsafe { rasterizer.as_mut() }.flush_region(
                    flush.addr,
                    flush.size,
                    crate::cache_types::CacheType::ALL,
                );
            }
            CommandData::InvalidateRegion(inv) => {
                // Upstream: rasterizer->OnCacheInvalidation(inv.addr, inv.size)
                unsafe { rasterizer.as_mut() }.on_cache_invalidation(inv.addr, inv.size);
            }
            CommandData::FlushAndInvalidateRegion(_) => {
                // Upstream: ASSERT(false) — should not reach here
                unreachable!("FlushAndInvalidateRegion should not be queued");
            }
            CommandData::None => unreachable!("empty GPU thread command was queued"),
        }

        // Signal fence completion.
        state.signaled_fence.store(next.fence, Ordering::SeqCst);
        if next.block {
            let _lock = state.write_lock.lock().unwrap();
            state.cv.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_wakes_joins_and_is_idempotent() {
        let mut manager = ThreadManager::new(SystemRef::null());
        let state = Arc::clone(&manager.state);
        let stop = Arc::clone(&manager.stop);
        manager.thread = Some(std::thread::spawn(move || {
            let _ = state.pop_wait(&stop);
        }));

        manager.shutdown();
        assert!(manager.thread.is_none());
        manager.shutdown();
        assert!(manager.thread.is_none());
    }

    #[test]
    fn stop_request_releases_a_blocking_fence_waiter() {
        let manager = Arc::new(ThreadManager::new(SystemRef::null()));
        let worker_manager = Arc::clone(&manager);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            worker_manager.push_command(CommandData::GpuTick(GpuTickCommand), true, true);
            done_tx.send(()).unwrap();
        });

        while manager.state.last_fence.load(Ordering::Relaxed) == 0 {
            std::thread::yield_now();
        }
        manager.request_stop();

        done_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("stop request must release the blocking fence wait");
        worker.join().unwrap();
    }
}

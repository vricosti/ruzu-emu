// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/gpu_thread.h and video_core/gpu_thread.cpp
//!
//! Threaded GPU command queue for asynchronous GPU processing.
//! Matches upstream structure: ThreadManager owns a dedicated OS thread
//! that pops commands from an MPSC queue and dispatches them.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use crate::control::scheduler::Scheduler;
use crate::dma_pusher::CommandList;
use crate::rasterizer_interface::RasterizerHandle;
use common::bounded_threadsafe_queue::BoundedMPSCQueue;
use common::settings;
use common::thread::{set_current_thread_name, set_current_thread_priority, ThreadPriority};
use ruzu_core::core::SystemRef;
use ruzu_core::frontend::graphics_context::{GraphicsContextHandle, ScopedGraphicsContext};

/// Device address type.
pub type DAddr = u64;

#[derive(Default)]
struct GpuThreadProfile {
    push_submit: AtomicU64,
    push_tick: AtomicU64,
    push_flush: AtomicU64,
    push_invalidate: AtomicU64,
    pop_submit: AtomicU64,
    pop_tick: AtomicU64,
    pop_flush: AtomicU64,
    pop_invalidate: AtomicU64,
    done_submit: AtomicU64,
    submit_total_us: AtomicU64,
    submit_max_us: AtomicU64,
}

static GPU_THREAD_PROFILE: std::sync::OnceLock<GpuThreadProfile> = std::sync::OnceLock::new();
static GPU_THREAD_PROFILE_ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

fn trace_gpu_thread_submit(
    stage: u64,
    fence: u64,
    channel: i32,
    list_count: usize,
    prefetch_count: usize,
    elapsed_us: u64,
) {
    if !common::trace::is_enabled(common::trace::cat::GPU_THREAD) {
        return;
    }
    common::trace::emit_raw(
        common::trace::cat::GPU_THREAD,
        &[
            stage,
            fence,
            channel as i64 as u64,
            list_count as u64,
            prefetch_count as u64,
            elapsed_us,
        ],
    );
}

fn profile_enabled() -> bool {
    *GPU_THREAD_PROFILE_ENABLED
        .get_or_init(|| std::env::var_os("RUZU_PROFILE_GPU_THREAD").is_some())
}

fn profile() -> &'static GpuThreadProfile {
    GPU_THREAD_PROFILE.get_or_init(GpuThreadProfile::default)
}

#[inline]
fn with_profile(update: impl FnOnce(&GpuThreadProfile)) {
    if profile_enabled() {
        update(profile());
    }
}

fn record_submit_elapsed(elapsed: std::time::Duration) {
    let profile = profile();
    let elapsed_us = elapsed.as_micros().min(u128::from(u64::MAX)) as u64;
    profile.done_submit.fetch_add(1, Ordering::Relaxed);
    profile
        .submit_total_us
        .fetch_add(elapsed_us, Ordering::Relaxed);
    let mut current = profile.submit_max_us.load(Ordering::Relaxed);
    while elapsed_us > current {
        match profile.submit_max_us.compare_exchange_weak(
            current,
            elapsed_us,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(next) => current = next,
        }
    }
}

pub fn dump_gpu_thread_profile() {
    let Some(profile) = GPU_THREAD_PROFILE.get() else {
        return;
    };
    log::warn!("[GPU_THREAD_PROFILE] command counts:");
    log::warn!(
        "[GPU_THREAD_PROFILE]   push SubmitList={} GpuTick={} FlushRegion={} InvalidateRegion={}",
        profile.push_submit.load(Ordering::Relaxed),
        profile.push_tick.load(Ordering::Relaxed),
        profile.push_flush.load(Ordering::Relaxed),
        profile.push_invalidate.load(Ordering::Relaxed)
    );
    log::warn!(
        "[GPU_THREAD_PROFILE]   pop  SubmitList={} GpuTick={} FlushRegion={} InvalidateRegion={}",
        profile.pop_submit.load(Ordering::Relaxed),
        profile.pop_tick.load(Ordering::Relaxed),
        profile.pop_flush.load(Ordering::Relaxed),
        profile.pop_invalidate.load(Ordering::Relaxed)
    );
    let done_submit = profile.done_submit.load(Ordering::Relaxed);
    let total_submit_us = profile.submit_total_us.load(Ordering::Relaxed);
    let avg_submit_us = if done_submit == 0 {
        0
    } else {
        total_submit_us / done_submit
    };
    log::warn!(
        "[GPU_THREAD_PROFILE]   done SubmitList={} total_us={} avg_us={} max_us={}",
        done_submit,
        total_submit_us,
        avg_submit_us,
        profile.submit_max_us.load(Ordering::Relaxed)
    );
}

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
/// Upstream uses `Common::MPSCQueue<CommandDataContainer>`.
pub struct SynchState {
    pub write_lock: Mutex<()>,
    pub queue: BoundedMPSCQueue<CommandDataContainer>,
    pub last_fence: AtomicU64,
    pub signaled_fence: AtomicU64,
    /// Condvar to notify callers that a blocking command has completed.
    pub cv: Condvar,
}

impl SynchState {
    pub fn new() -> Self {
        Self {
            write_lock: Mutex::new(()),
            queue: BoundedMPSCQueue::with_default_capacity(),
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
    is_async: bool,
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
    /// Matches upstream `ThreadManager::ThreadManager(Core::System&, bool)`.
    pub fn new(system: SystemRef, is_async: bool) -> Self {
        Self {
            system,
            is_async,
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
        let rasterizer = self.rasterizer;
        let context = if context_ptr.is_null() {
            None
        } else {
            Some(GraphicsContextHandle::from_ref(unsafe { &*context_ptr }))
        };

        let handle = std::thread::Builder::new()
            .name("GPU".to_string())
            .spawn(move || {
                set_current_thread_name("GPU");
                set_current_thread_priority(ThreadPriority::Critical);
                system.get().kernel().unwrap().register_host_thread();
                let gpu_ref = unsafe { &*(gpu as *const crate::gpu::Gpu) };
                let scheduler_ref = unsafe { &*(sched as *const Scheduler) };

                // Upstream: auto current_context = context.Acquire();
                let _current_context =
                    context.map(|context| unsafe { ScopedGraphicsContext::new(context) });

                run_thread(&state, &stop, gpu_ref, scheduler_ref, rasterizer);
            })
            .expect("Failed to spawn GPU thread");

        self.thread = Some(handle);
    }

    /// Push GPU command entries to be processed.
    /// Matches upstream `ThreadManager::SubmitList(s32, CommandList&&)`.
    pub fn submit_list(&self, channel: i32, entries: CommandList) {
        self.push_command(
            CommandData::SubmitList(SubmitListCommand { channel, entries }),
            false,
        );
    }

    /// Notify rasterizer that a region should be flushed to Switch memory.
    /// Matches upstream `ThreadManager::FlushRegion(DAddr, u64)`.
    pub fn flush_region(&self, addr: DAddr, size: u64) {
        if !self.is_async {
            self.push_command(
                CommandData::FlushRegion(FlushRegionCommand { addr, size }),
                false,
            );
        }
    }

    /// Notify rasterizer that a region should be invalidated.
    /// Matches upstream `ThreadManager::InvalidateRegion(DAddr, u64)`.
    ///
    /// Upstream calls directly on the rasterizer (NOT queued to the GPU thread).
    pub fn invalidate_region(&self, addr: DAddr, size: u64) {
        if let Some(rasterizer) = self.rasterizer() {
            // Safety: rasterizer pointer is valid for the lifetime of the renderer.
            unsafe { rasterizer.as_mut() }.on_cache_invalidation(addr, size);
        }
    }

    /// Notify rasterizer that a region should be flushed and invalidated.
    /// Matches upstream `ThreadManager::FlushAndInvalidateRegion(DAddr, u64)`.
    ///
    /// Upstream flushes at High accuracy, then directly invalidates the rasterizer cache.
    pub fn flush_and_invalidate_region(&self, addr: DAddr, size: u64) {
        if settings::is_gpu_level_high(&settings::values()) {
            if !self.is_async {
                self.push_command(
                    CommandData::FlushRegion(FlushRegionCommand { addr, size }),
                    false,
                );
            } else if let Some(gpu) = self.gpu {
                let gpu = unsafe { &*(gpu as *const crate::gpu::Gpu) };
                let fence = gpu.request_flush(addr, size as usize);
                self.tick_gpu();
                gpu.wait_for_sync_operation(fence);
            } else {
                log::warn!("ThreadManager::flush_and_invalidate_region: GPU pointer not installed");
            }
        }
        if let Some(rasterizer) = self.rasterizer() {
            // Safety: rasterizer pointer is valid for the lifetime of the renderer.
            unsafe { rasterizer.as_mut() }.on_cache_invalidation(addr, size);
        }
    }

    /// Tick the GPU to process pending requests.
    /// Matches upstream `ThreadManager::TickGPU()`.
    pub fn tick_gpu(&self) {
        self.push_command(CommandData::GpuTick(GpuTickCommand), false);
    }

    /// Push a command to be executed by the GPU thread.
    /// Matches upstream `ThreadManager::PushCommand(CommandData&&, bool)`.
    fn push_command(&self, command_data: CommandData, mut block: bool) -> u64 {
        if profile_enabled() {
            match &command_data {
                CommandData::SubmitList(_) => {
                    profile().push_submit.fetch_add(1, Ordering::Relaxed);
                }
                CommandData::GpuTick(_) => {
                    profile().push_tick.fetch_add(1, Ordering::Relaxed);
                }
                CommandData::FlushRegion(_) => {
                    profile().push_flush.fetch_add(1, Ordering::Relaxed);
                }
                CommandData::InvalidateRegion(_) => {
                    profile().push_invalidate.fetch_add(1, Ordering::Relaxed);
                }
                CommandData::FlushAndInvalidateRegion(_) | CommandData::None => {}
            }
        }

        if !self.is_async {
            block = true;
        }

        let lock = self.state.write_lock.lock().unwrap();
        let fence = self.state.last_fence.fetch_add(1, Ordering::Relaxed) + 1;
        if let CommandData::SubmitList(submit) = &command_data {
            trace_gpu_thread_submit(
                1,
                fence,
                submit.channel,
                submit.entries.command_lists.len(),
                submit.entries.prefetch_command_list.len(),
                0,
            );
        }

        self.state.emplace(CommandDataContainer {
            data: command_data,
            fence,
            block,
        });

        if block {
            log::trace!(
                "push_command: waiting for fence {} (signaled={})",
                fence,
                self.state.signaled_fence.load(Ordering::Relaxed)
            );
            let _guard = self
                .state
                .cv
                .wait_while(lock, |_| {
                    fence > self.state.signaled_fence.load(Ordering::Relaxed)
                })
                .unwrap();
            log::trace!("push_command: fence {} done", fence);
        }

        fence
    }

    /// Request the GPU thread to stop.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.state.notify_all();
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
    rasterizer: Option<RasterizerHandle>,
) {
    while !stop.load(Ordering::Relaxed) {
        let Some(next) = state.pop_wait(stop) else {
            break; // Stop requested
        };

        match next.data {
            CommandData::SubmitList(submit) => {
                with_profile(|profile| {
                    profile.pop_submit.fetch_add(1, Ordering::Relaxed);
                });
                let list_count = submit.entries.command_lists.len();
                let prefetch_count = submit.entries.prefetch_command_list.len();
                trace_gpu_thread_submit(
                    2,
                    next.fence,
                    submit.channel,
                    list_count,
                    prefetch_count,
                    0,
                );
                let start = profile_enabled().then(std::time::Instant::now);
                scheduler.push(gpu, submit.channel, submit.entries);
                let elapsed = start.map(|start| start.elapsed());
                if let Some(elapsed) = elapsed {
                    record_submit_elapsed(elapsed);
                }
                trace_gpu_thread_submit(
                    3,
                    next.fence,
                    submit.channel,
                    list_count,
                    prefetch_count,
                    elapsed.map_or(0, |elapsed| elapsed.as_micros() as u64),
                );
            }
            CommandData::GpuTick(_) => {
                with_profile(|profile| {
                    profile.pop_tick.fetch_add(1, Ordering::Relaxed);
                });
                gpu.tick_work();
            }
            CommandData::FlushRegion(flush) => {
                with_profile(|profile| {
                    profile.pop_flush.fetch_add(1, Ordering::Relaxed);
                });
                // Upstream: rasterizer->FlushRegion(flush.addr, flush.size)
                if let Some(rasterizer) = rasterizer {
                    unsafe { rasterizer.as_mut() }.flush_region(
                        flush.addr,
                        flush.size,
                        crate::cache_types::CacheType::ALL,
                    );
                }
            }
            CommandData::InvalidateRegion(inv) => {
                with_profile(|profile| {
                    profile.pop_invalidate.fetch_add(1, Ordering::Relaxed);
                });
                // Upstream: rasterizer->OnCacheInvalidation(inv.addr, inv.size)
                if let Some(rasterizer) = rasterizer {
                    unsafe { rasterizer.as_mut() }.on_cache_invalidation(inv.addr, inv.size);
                }
            }
            CommandData::FlushAndInvalidateRegion(_) => {
                // Upstream: ASSERT(false) — should not reach here
                unreachable!("FlushAndInvalidateRegion should not be queued");
            }
            CommandData::None => {}
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
        let mut manager = ThreadManager::new(SystemRef::null(), true);
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
}

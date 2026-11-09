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

/// Upstream RunCommand: executes after the commands queued before it.
pub struct RunCommand {
    pub func: Box<dyn FnOnce() + Send>,
}

/// All possible GPU thread commands.
/// Matches upstream `CommandData` variant.
pub enum CommandData {
    None,
    SubmitList(SubmitListCommand),
    FlushRegion(FlushRegionCommand),
    InvalidateRegion(InvalidateRegionCommand),
    FlushAndInvalidateRegion(FlushAndInvalidateRegionCommand),
    GpuTick(GpuTickCommand),
    Run(RunCommand),
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
    thread: Option<JoinHandle<Option<crate::gpu_split::GpuSplit>>>,
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
    /// valid until NotifyShutdown has joined both the frontend and backend.
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
                let _wake_on_failure = StopOnUnwind { state: &state, stop: &stop };
                system.get().kernel().unwrap().register_host_thread();
                let gpu_ref = unsafe { &*(gpu as *const crate::gpu::Gpu) };
                let scheduler_ref = unsafe { &*(sched as *const Scheduler) };

                // Upstream: auto current_context = context.Acquire();
                let _current_context = unsafe { ScopedGraphicsContext::new(context) };

                let failure_state = state.clone();
                let failure_stop = stop.clone();
                let failure: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                    stop_gpu_thread(&failure_state, &failure_stop);
                });
                run_thread(&state, &stop, gpu_ref, scheduler_ref, rasterizer,
                    || crate::gpu_split::GpuSplitFrontend::maybe_start(system, &failure))
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

    pub fn run(&self, func: Box<dyn FnOnce() + Send>, is_async: bool) {
        self.push_command(CommandData::Run(RunCommand { func }), false, is_async);
    }

    /// Push a command to be executed by the GPU thread.
    /// Matches upstream `ThreadManager::PushCommand(CommandData&&, bool)`.
    fn push_command(&self, command_data: CommandData, mut block: bool, is_async: bool) -> u64 {
        if !is_async {
            block = true;
        }

        let lock = self.state.write_lock.lock().unwrap();
        if self.stop.load(Ordering::Acquire) {
            return self.state.last_fence.load(Ordering::Relaxed);
        }
        let fence = self
            .state
            .last_fence
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        if self.state.queue.emplace_wait_with_stop(CommandDataContainer {
            data: command_data, fence, block,
        }, || self.stop.load(Ordering::Acquire)).is_err() { return fence; }

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
        stop_gpu_thread(&self.state, &self.stop);
    }

    /// Stop and join the GPU thread while its borrowed renderer and scheduler
    /// are still alive.
    ///
    /// Upstream gets the renderer ordering from C++ reverse member
    /// destruction: `GPU::Impl::gpu_thread` is destroyed before `renderer`.
    /// Rust drops fields in declaration order and frees its boxed scheduler,
    /// so `Gpu::drop` calls this explicitly before either borrowed owner.
    pub(crate) fn shutdown(&mut self) {
        self.notify_shutdown();
    }

    /// Port of `ThreadManager::NotifyShutdown`.
    ///
    /// Request an immediate stop and join the GPU thread if it is still
    /// running, so that nothing it references is torn down underneath it
    /// (upstream: "STOP now, and DESTROY yourself"). Idempotent.
    pub fn notify_shutdown(&mut self) {
        if let Some(thread) = self.thread.take() {
            self.request_stop();
            match thread.join() {
                Ok(Some(mut back)) => {
                    // Upstream NotifyShutdown joins the front before Stop().
                    if back.stop().is_err() { log::error!("GPU backend thread panicked"); }
                }
                Ok(None) => {}
                Err(_) => log::error!("GPU frontend thread panicked"),
            }
        }
    }
}

impl Drop for ThreadManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// Shared stop path for shutdown and Rust panic recovery. Wake a full queue
// before taking write_lock: its producer may own that lock while waiting.
fn stop_gpu_thread(state: &SynchState, stop: &AtomicBool) {
    stop.store(true, Ordering::Release);
    state.notify_all();
    let _lock = state.write_lock.lock().unwrap_or_else(|poison| poison.into_inner());
    state.cv.notify_all();
}

struct StopOnUnwind<'a> { state: &'a SynchState, stop: &'a AtomicBool }
impl Drop for StopOnUnwind<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() { stop_gpu_thread(self.state, self.stop); }
    }
}

/// Upstream anonymous SignalCommand helper.
fn signal_command(state: &SynchState, fence: u64, block: bool) {
    state.signaled_fence.store(fence, Ordering::SeqCst);
    if block {
        let _lock = state.write_lock.lock().unwrap_or_else(|poison| poison.into_inner());
        state.cv.notify_all();
    }
}

// Typed non-owning captures for upstream GPU/state pointers. ThreadManager
// retains both owners until it has joined the front and then the back.
#[derive(Clone, Copy)]
struct BackCommandTarget { gpu: *const crate::gpu::Gpu, state: *const SynchState }
unsafe impl Send for BackCommandTarget {}
impl BackCommandTarget {
    unsafe fn tick(self) { unsafe { &*self.gpu }.tick_work(); }
    unsafe fn signal(self, fence: u64, block: bool) {
        signal_command(unsafe { &*self.state }, fence, block);
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
    mut maybe_start: impl FnMut() -> Option<crate::gpu_split::GpuSplitFrontend>,
) -> Option<crate::gpu_split::GpuSplit> {
    use crate::nx2_profile::{Scope, Zone};
    let mut split_owner = None;
    let mut split_scope = None;
    // Drop first on unwind, before joining the backend or dropping its scope.
    let _wake_on_failure = StopOnUnwind { state, stop };
    let mut last_progress = std::time::Instant::now();
    while !stop.load(Ordering::Relaxed) {
        crate::nx2_profile::report("GPU thread");
        if split_owner.is_none() {
            if let Some(front) = maybe_start() {
                split_scope = Some(front.install());
                split_owner = Some(front);
            }
        }
        let next = if scheduler.has_pending_channels() {
            {
                let _scope = Scope::new(Zone::ResumePending);
                scheduler.resume_pending(gpu);
            }
            match state.queue.try_pop() {
                Some(next) => Some(next),
                None => {
                    let _scope = Scope::new(Zone::PendingSleep);
                    if last_progress.elapsed() < std::time::Duration::from_micros(500) {
                        std::thread::yield_now();
                    } else {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    continue;
                }
            }
        } else {
            let _scope = Scope::new(Zone::Idle);
            state.pop_wait(stop)
        };
        let Some(next) = next else {
            break; // Stop requested
        };
        if stop.load(Ordering::Relaxed) {
            break;
        }
        last_progress = std::time::Instant::now();

        if let Some(split) = crate::gpu_split::current() {
            let target = BackCommandTarget { gpu, state };
            match next.data {
                CommandData::SubmitList(submit) => {
                    let _scope = Scope::new(Zone::Push);
                    scheduler.push(gpu, submit.channel, submit.entries);
                }
                CommandData::GpuTick(_) => split.defer(move || unsafe { target.tick() }),
                CommandData::Run(run) => split.defer_function(run.func),
                CommandData::FlushRegion(flush) => split.defer(move || unsafe {
                    rasterizer.with_mut(|r| r.flush_region(flush.addr, flush.size,
                        crate::cache_types::CacheType::ALL));
                }),
                CommandData::InvalidateRegion(inv) => split.defer(move || unsafe {
                    rasterizer.with_mut(|r| r.on_cache_invalidation(inv.addr, inv.size));
                }),
                _ => unreachable!("unsupported split GPU command"),
            }
            let fence = next.fence;
            let block = next.block;
            split.defer(move || unsafe { target.signal(fence, block) });
            split.publish();
            continue;
        }

        with_command_autorelease_pool(|| match next.data {
            CommandData::SubmitList(submit) => {
                let _scope = Scope::new(Zone::Push);
                scheduler.push(gpu, submit.channel, submit.entries);
            }
            CommandData::GpuTick(_) => {
                let _scope = Scope::new(Zone::Tick);
                gpu.tick_work();
            }
            CommandData::Run(run) => {
                let _scope = Scope::new(Zone::Run);
                (run.func)();
            }
            CommandData::FlushRegion(flush) => {
                let _scope = Scope::new(Zone::FlushInvalidate);
                // Upstream: rasterizer->FlushRegion(flush.addr, flush.size)
                unsafe { rasterizer.as_mut() }.flush_region(
                    flush.addr,
                    flush.size,
                    crate::cache_types::CacheType::ALL,
                );
            }
            CommandData::InvalidateRegion(inv) => {
                let _scope = Scope::new(Zone::FlushInvalidate);
                // Upstream: rasterizer->OnCacheInvalidation(inv.addr, inv.size)
                unsafe { rasterizer.as_mut() }.on_cache_invalidation(inv.addr, inv.size);
            }
            CommandData::FlushAndInvalidateRegion(_) => {
                // Upstream: ASSERT(false) — should not reach here
                unreachable!("FlushAndInvalidateRegion should not be queued");
            }
            CommandData::None => unreachable!("empty GPU thread command was queued"),
        });

        signal_command(state, next.fence, next.block);
    }
    drop(split_scope);
    split_owner.map(crate::gpu_split::GpuSplitFrontend::into_transport)
}

/// Native Metal calls can autorelease temporary objects internally. Unlike
/// Eden's driver-owned Vulkan entry points, this OS thread has no Cocoa event
/// loop to drain them. Bound their lifetime to one command, not the thread;
/// resources retained by the renderer or submitted command buffers survive.
fn with_command_autorelease_pool<R>(execute: impl FnOnce() -> R) -> R {
    #[cfg(target_os = "macos")]
    {
        objc2::rc::autoreleasepool(|_| execute())
    }
    #[cfg(not(target_os = "macos"))]
    {
        execute()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nx2_channels_resume_on_cross_channel_and_idle_cpu_release_and_stop_while_pending() {
        const CHILD: &str = "RUZU_GPU_PENDING_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", std::thread::current().name().unwrap(), "--test-threads=1"])
                .env(CHILD, "1").status().unwrap();
            assert!(status.success());
            return;
        }
        use crate::dma_pusher::{CommandHeader, DmaPusher};
        use crate::rasterizer_interface::RasterizerInterface;
        fn semaphore(address: u32, value: u32, operation: u32) -> Vec<CommandHeader> {
            [(1 << 29) | (4 << 16) | 4, 0, address, value, operation]
                .into_iter().map(|raw| CommandHeader { raw }).collect()
        }
        crate::nx2::G_NX2_AMPERE.store(true, Ordering::SeqCst);
        for split_enabled in [false, true] {
        let state = Arc::new(SynchState::new());
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker_state = state.clone();
        let worker_stop = stop.clone();
        let worker = std::thread::spawn(move || {
            let gpu = crate::gpu::Gpu::new(false, false);
            let scheduler = Scheduler::new();
            let mut backing = vec![0u8; 0x1000];
            let device = Arc::new(crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager::default());
            device.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
            device.smmu_map_with_cpu_backing(0x9000_0000, backing.as_mut_ptr(), 0x5000_0000, backing.len(), 3, true);
            let mut memory = crate::memory_manager::MemoryManager::new_with_geometry_and_device_memory(13, device, 32, 0x1_0000_0000, 16, 12);
            memory.map(0x20000, 0x9000_0000, 0x1000, 0, false);
            let memory = Arc::new(parking_lot::Mutex::new(memory));
            let mut rasterizer = Box::new(crate::renderer_null::null_rasterizer::RasterizerNull::new(Arc::new(
                crate::host1x::syncpoint_manager::SyncpointManager::new())));
            let first = gpu.create_channel(0);
            let second = gpu.create_channel(1);
            for channel in [&first, &second] {
                let mut cs = channel.lock();
                cs.memory_manager = Some(memory.clone());
                cs.dma_pusher = Some(Arc::new(parking_lot::Mutex::new(Box::new(DmaPusher::new(&gpu, SystemRef::null(), memory.clone(), std::ptr::null_mut())))));
                cs.dma_pusher.as_mut().unwrap().lock().bind_rasterizer(rasterizer.as_ref());
                rasterizer.initialize_channel(&mut cs);
                drop(cs);
                scheduler.declare_channel(channel.clone());
            }
            // Both channels deliberately share this address space; the null
            // backend needs no engine state for these semaphore-only streams.
            rasterizer.bind_channel(&mut first.lock());
            let split = split_enabled.then(|| {
                let split = crate::gpu_split::GpuSplitFrontend::new(false);
                let state = worker_state.clone();
                let stop = worker_stop.clone();
                split.start_back_with_failure(|| {}, move || stop_gpu_thread(&state, &stop));
                split
            });
            let split_scope = split.as_ref().map(|front| front.install());
            let mut waiting = semaphore(0x20800, 7, 1);
            waiting.extend(semaphore(0x20804, 99, 2 | (1 << 24)));
            scheduler.push(&gpu, 0, CommandList::from_prefetch(waiting.clone()));
            assert!(scheduler.has_pending_channels());
            assert_eq!(memory.lock().read::<u32>(0x20804), 0);
            scheduler.push(&gpu, 1, CommandList::from_prefetch(semaphore(0x20800, 7, 2 | (1 << 24))));
            assert!(!scheduler.has_pending_channels());
            if let Some(split) = &split { split.drain(); }
            assert_eq!(memory.lock().read::<u32>(0x20804), 99);
            memory.lock().write::<u32>(0x20800, 0);
            memory.lock().write::<u32>(0x20804, 0);
            scheduler.push(&gpu, 0, CommandList::from_prefetch(waiting));
            assert!(scheduler.has_pending_channels());
            ready_tx.send((memory.clone(), first.clone())).unwrap();
            run_thread(&worker_state, &worker_stop, &gpu, &scheduler, RasterizerHandle::from_ref(rasterizer.as_ref()), || None);
            drop(split_scope);
            if let Some(split) = split { split.stop().unwrap(); }
            done_tx.send(()).unwrap();
        });
        let (memory, channel) = ready_rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        // No submission wakes the thread: only a CPU semaphore write does.
        memory.lock().write::<u32>(0x20800, 7);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while memory.lock().read::<u32>(0x20804) != 99 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let resumed = memory.lock().read::<u32>(0x20804) == 99;
        state.emplace(CommandDataContainer {
            data: CommandData::SubmitList(SubmitListCommand { channel: 0, entries: CommandList::from_prefetch(semaphore(0x20808, 1, 1)) }),
            fence: 1, block: false,
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let dma = Arc::clone(channel.lock().dma_pusher.as_ref().unwrap());
        while !dma.lock().has_pending_acquire() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let pending = dma.lock().has_pending_acquire();
        stop.store(true, Ordering::Relaxed);
        state.notify_all();
        done_rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        worker.join().unwrap();
        assert!(resumed, "empty queue must poll CPU-released semaphores");
        assert!(pending, "stop must be exercised with an unmet acquisition");
        }
    }

    #[test]
    fn run_callbacks_are_fifo_and_sync_submission_waits_for_completion() {
        let manager = Arc::new(ThreadManager::new(SystemRef::null()));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first_calls = calls.clone();
        manager.run(
            Box::new(move || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                first_calls.lock().unwrap().push(1);
            }),
            true,
        );
        assert!(calls.lock().unwrap().is_empty());
        let worker_manager = manager.clone();
        let worker = std::thread::spawn(move || {
            let gpu = crate::gpu::Gpu::new(true, false);
            let scheduler = Scheduler::new();
            let rasterizer = crate::renderer_null::null_rasterizer::RasterizerNull::new(Arc::new(
                crate::host1x::syncpoint_manager::SyncpointManager::new(),
            ));
            run_thread(
                &worker_manager.state,
                &worker_manager.stop,
                &gpu,
                &scheduler,
                RasterizerHandle::from_ref(&rasterizer), || None,
            );
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let submit_manager = manager.clone();
        let second_calls = calls.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let submitter = std::thread::spawn(move || {
            submit_manager.run(
                Box::new(move || second_calls.lock().unwrap().push(2)),
                false,
            );
            done_tx.send(()).unwrap();
        });
        while manager.state.last_fence.load(Ordering::Relaxed) < 2 {
            std::thread::yield_now();
        }
        assert!(done_rx.try_recv().is_err());
        release_tx.send(()).unwrap();
        done_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(*calls.lock().unwrap(), vec![1, 2]);
        assert_eq!(manager.state.signaled_fence.load(Ordering::SeqCst), 2);
        manager.request_stop();
        submitter.join().unwrap();
        worker.join().unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn command_pool_drains_temporaries_but_preserves_retained_resources() {
        use objc2::rc::{Retained, Weak};
        use objc2::runtime::NSObject;

        let (temporary, retained) = with_command_autorelease_pool(|| {
            let temporary = NSObject::new();
            let weak = Weak::from_retained(&temporary);
            let _ = Retained::autorelease_ptr(temporary);
            assert!(weak.load().is_some());

            let resource = NSObject::new();
            let _ = Retained::autorelease_ptr(resource.clone());
            (weak, resource)
        });
        assert!(temporary.load().is_none());
        let weak = Weak::from_retained(&retained);
        assert!(weak.load().is_some());
        drop(retained);
        assert!(weak.load().is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn command_pool_drains_during_unwind() {
        use objc2::rc::{Retained, Weak};
        use objc2::runtime::NSObject;

        let mut temporary = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_command_autorelease_pool(|| {
                let object = NSObject::new();
                temporary = Some(Weak::from_retained(&object));
                let _ = Retained::autorelease_ptr(object);
                panic!("command failed");
            });
        }));
        assert!(result.is_err());
        assert!(temporary.unwrap().load().is_none());
    }

    // Owners deliberately follow manager: shutdown joins both GPU threads
    // before any non-owning GPU/scheduler/rasterizer capture can become invalid.
    struct SplitFixture {
        manager: ThreadManager,
        _gpu: Box<crate::gpu::Gpu>,
        _scheduler: Box<Scheduler>,
        _rasterizer: Box<crate::renderer_null::null_rasterizer::RasterizerNull>,
        front_done: Arc<AtomicBool>,
    }
    impl SplitFixture {
        fn new(register: impl FnOnce() + Send + 'static) -> Self {
            let mut manager = ThreadManager::new(SystemRef::null());
            let gpu = Box::new(crate::gpu::Gpu::new(true, false));
            let scheduler = Box::new(Scheduler::new());
            let rasterizer = Box::new(crate::renderer_null::null_rasterizer::RasterizerNull::new(
                Arc::new(crate::host1x::syncpoint_manager::SyncpointManager::new())));
            let gpu_ptr = &*gpu as *const crate::gpu::Gpu as usize;
            let scheduler_ptr = &*scheduler as *const Scheduler as usize;
            let rasterizer_handle = RasterizerHandle::from_ref(rasterizer.as_ref());
            let state = manager.state.clone();
            let stop = manager.stop.clone();
            let front_done = Arc::new(AtomicBool::new(false));
            let done = front_done.clone();
            manager.thread = Some(std::thread::spawn(move || {
                let mut register = Some(register);
                let back = run_thread(&state, &stop,
                    unsafe { &*(gpu_ptr as *const crate::gpu::Gpu) },
                    unsafe { &*(scheduler_ptr as *const Scheduler) }, rasterizer_handle, || {
                        let front = crate::gpu_split::GpuSplitFrontend::new(false);
                        let failure_state = state.clone();
                        let failure_stop = stop.clone();
                        front.start_back_with_failure(register.take().unwrap(), move || {
                            stop_gpu_thread(&failure_state, &failure_stop);
                        });
                        Some(front)
                    });
                assert!(crate::gpu_split::current().is_none());
                done.store(true, Ordering::Release);
                back
            }));
            Self { manager, _gpu: gpu, _scheduler: scheduler, _rasterizer: rasterizer, front_done }
        }
    }

    #[test]
    fn split_commands_signal_after_backend_execution_across_fresh_sessions() {
        for _ in 0..3 {
            let (registered, registration) = std::sync::mpsc::channel();
            let mut fixture = SplitFixture::new(move || {
                registered.send(std::thread::current().id()).unwrap();
            });
            let back_id = registration.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
            let calls = Arc::new(Mutex::new(Vec::new()));
            for value in [1, 2] {
                let calls = calls.clone();
                fixture.manager.run(Box::new(move || {
                    calls.lock().unwrap().push((value, std::thread::current().id()));
                }), value == 1);
            }
            assert_eq!(*calls.lock().unwrap(), [(1, back_id), (2, back_id)]);
            assert_eq!(fixture.manager.state.signaled_fence.load(Ordering::Acquire), 2);
            fixture.manager.notify_shutdown();
            assert!(fixture.front_done.load(Ordering::Acquire));
            fixture.manager.notify_shutdown();
        }
    }

    #[test]
    fn backend_failure_releases_sync_waiter_and_idle_frontend() {
        let mut fixture = SplitFixture::new(|| {});
        std::thread::scope(|scope| {
            let (done, result) = std::sync::mpsc::channel();
            let manager = &fixture.manager;
            scope.spawn(move || {
                manager.run(Box::new(|| panic!("injected backend callback failure")), false);
                done.send(()).unwrap();
            });
            result.recv_timeout(std::time::Duration::from_secs(3))
                .expect("backend failure must release the synchronous submitter");
        });
        fixture.manager.notify_shutdown();
        assert!(fixture.front_done.load(Ordering::Acquire));
        assert!(fixture.manager.stop.load(Ordering::Acquire));
        assert_eq!(fixture.manager.state.signaled_fence.load(Ordering::Acquire), 0,
            "a failed command must not be falsely signalled complete");
    }

    #[test]
    fn frontend_failure_stops_backend_before_owners_drop() {
        let mut fixture = SplitFixture::new(|| {});
        fixture.manager.state.emplace(CommandDataContainer::default());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !fixture.manager.stop.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(fixture.manager.stop.load(Ordering::Acquire));
        fixture.manager.notify_shutdown();
        assert!(fixture.manager.thread.is_none());
    }

    #[test]
    fn split_shutdown_joins_front_before_waiting_for_busy_backend() {
        let mut fixture = SplitFixture::new(|| {});
        let (entered, entry) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        fixture.manager.run(Box::new(move || {
            entered.send(()).unwrap();
            wait.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        }), true);
        entry.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        fixture.manager.request_stop();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !fixture.front_done.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        let front_returned = fixture.front_done.load(Ordering::Acquire);
        release.send(()).unwrap();
        fixture.manager.notify_shutdown();
        assert!(front_returned, "frontend return must not wait for the busy backend");
    }

    #[test]
    fn stop_releases_producer_waiting_on_a_full_command_queue() {
        let manager = ThreadManager::new(SystemRef::null());
        for _ in 0..common::bounded_threadsafe_queue::DEFAULT_CAPACITY {
            manager.run(Box::new(|| {}), true);
        }
        std::thread::scope(|scope| {
            let (done, result) = std::sync::mpsc::channel();
            let borrowed = &manager;
            scope.spawn(move || {
                borrowed.run(Box::new(|| panic!("cancelled command must never execute")), true);
                done.send(()).unwrap();
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while manager.state.last_fence.load(Ordering::Acquire)
                <= common::bounded_threadsafe_queue::DEFAULT_CAPACITY as u64
                && std::time::Instant::now() < deadline { std::thread::yield_now(); }
            manager.request_stop();
            result.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        });
    }

    #[test]
    fn shutdown_wakes_joins_and_is_idempotent() {
        let mut manager = ThreadManager::new(SystemRef::null());
        let state = Arc::clone(&manager.state);
        let stop = Arc::clone(&manager.stop);
        manager.thread = Some(std::thread::spawn(move || {
            let _ = state.pop_wait(&stop);
            None
        }));

        manager.shutdown();
        assert!(manager.thread.is_none());
        manager.shutdown();
        assert!(manager.thread.is_none());
    }

    #[test]
    fn notify_shutdown_stops_joins_and_is_idempotent() {
        let mut manager = ThreadManager::new(SystemRef::null());
        let state = Arc::clone(&manager.state);
        let stop = Arc::clone(&manager.stop);
        manager.thread = Some(std::thread::spawn(move || {
            let _ = state.pop_wait(&stop);
            None
        }));
        manager.notify_shutdown();
        assert!(manager.thread.is_none());
        assert!(manager.stop.load(Ordering::Relaxed));
        manager.notify_shutdown();
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

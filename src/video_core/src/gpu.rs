// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/gpu.h and video_core/gpu.cpp
//!
//! GPU controller: manages channels, engines, rendering, and host synchronization.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::dma_pusher::CommandList;
use crate::framebuffer_config::FramebufferConfig;
use crate::rasterizer_download_area::RasterizerDownloadArea;
use crate::rasterizer_interface::RasterizerHandle;
use crate::renderer_base::RendererBase;
use crate::shader_notify::{ShaderNotify, ShaderNotifyHandle};
use common::settings;
use ruzu_core::core::SystemRef;
use ruzu_core::gpu_core::{
    BlendMode as CoreBlendMode, FramebufferConfig as CoreFramebufferConfig, GpuChannelHandle,
    GpuCommandList, GpuCoreInterface, GpuMemoryManagerHandle,
    RasterizerDownloadArea as CoreRasterizerDownloadArea,
};
use ruzu_core::hle::service::nvdrv::nvdata::NvFence;

struct VideoGpuChannelHandle {
    gpu: *const Gpu,
    bind_id: i32,
    channel_state: Arc<parking_lot::Mutex<crate::control::channel_state::ChannelState>>,
}

struct VideoGpuMemoryManagerHandle {
    memory_manager: Arc<parking_lot::Mutex<crate::memory_manager::MemoryManager>>,
}

unsafe impl Send for VideoGpuChannelHandle {}
unsafe impl Sync for VideoGpuChannelHandle {}
unsafe impl Send for VideoGpuMemoryManagerHandle {}
unsafe impl Sync for VideoGpuMemoryManagerHandle {}

static NEXT_MEMORY_MANAGER_ID: AtomicUsize = AtomicUsize::new(1);

fn gpu_clock_multiplier(clock: common::settings_enums::GpuClock) -> u64 {
    use common::settings_enums::GpuClock;
    match clock {
        GpuClock::Boost => 256,
        GpuClock::Overclock => 512,
        GpuClock::Normal => 1,
    }
}

/// Device address type.
pub type DAddr = u64;

/// Render target format enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum RenderTargetFormat {
    None = 0x0,
    R32G32B32A32Float = 0xC0,
    R32G32B32A32Sint = 0xC1,
    R32G32B32A32Uint = 0xC2,
    R32G32B32X32Float = 0xC3,
    R32G32B32X32Sint = 0xC4,
    R32G32B32X32Uint = 0xC5,
    R16G16B16A16Unorm = 0xC6,
    R16G16B16A16Snorm = 0xC7,
    R16G16B16A16Sint = 0xC8,
    R16G16B16A16Uint = 0xC9,
    R16G16B16A16Float = 0xCA,
    R32G32Float = 0xCB,
    R32G32Sint = 0xCC,
    R32G32Uint = 0xCD,
    R16G16B16X16Float = 0xCE,
    A8R8G8B8Unorm = 0xCF,
    A8R8G8B8Srgb = 0xD0,
    A2B10G10R10Unorm = 0xD1,
    A2B10G10R10Uint = 0xD2,
    A8B8G8R8Unorm = 0xD5,
    A8B8G8R8Srgb = 0xD6,
    A8B8G8R8Snorm = 0xD7,
    A8B8G8R8Sint = 0xD8,
    A8B8G8R8Uint = 0xD9,
    R16G16Unorm = 0xDA,
    R16G16Snorm = 0xDB,
    R16G16Sint = 0xDC,
    R16G16Uint = 0xDD,
    R16G16Float = 0xDE,
    A2R10G10B10Unorm = 0xDF,
    B10G11R11Float = 0xE0,
    R32Sint = 0xE3,
    R32Uint = 0xE4,
    R32Float = 0xE5,
    X8R8G8B8Unorm = 0xE6,
    X8R8G8B8Srgb = 0xE7,
    R5G6B5Unorm = 0xE8,
    A1R5G5B5Unorm = 0xE9,
    R8G8Unorm = 0xEA,
    R8G8Snorm = 0xEB,
    R8G8Sint = 0xEC,
    R8G8Uint = 0xED,
    R16Unorm = 0xEE,
    R16Snorm = 0xEF,
    R16Sint = 0xF0,
    R16Uint = 0xF1,
    R16Float = 0xF2,
    R8Unorm = 0xF3,
    R8Snorm = 0xF4,
    R8Sint = 0xF5,
    R8Uint = 0xF6,
    X1R5G5B5Unorm = 0xF8,
    X8B8G8R8Unorm = 0xF9,
    X8B8G8R8Srgb = 0xFA,
}

/// Depth buffer format enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum DepthFormat {
    Z32Float = 0xA,
    Z16Unorm = 0x13,
    Z24UnormS8Uint = 0x14,
    X8Z24Unorm = 0x15,
    S8Z24Unorm = 0x16,
    S8Uint = 0x17,
    V8Z24Unorm = 0x18,
    Z32FloatX24S8Uint = 0x19,
}

/// The GPU controller, using pimpl pattern matching upstream.
pub struct Gpu {
    is_async: bool,
    use_nvdec: bool,
    shutting_down: AtomicBool,

    // Sync request infrastructure
    sync_requests: Mutex<VecDeque<Box<dyn FnOnce() + Send>>>,
    current_sync_fence: AtomicU64,
    last_sync_fence: Mutex<u64>,
    sync_request_cv: Condvar,
    pending_composite_fence: AtomicU64,
    request_swap_counters: Mutex<RequestSwapCounters>,

    // Channel management
    new_channel_id: Mutex<i32>,
    bound_channel: Mutex<i32>,

    /// The renderer backend (OpenGL, Vulkan, or Null).
    /// Upstream: `std::unique_ptr<VideoCore::RendererBase> renderer` in GPU::Impl.
    renderer: Mutex<Option<Box<dyn RendererBase>>>,

    /// Shader-build progress reported by renderer pipeline workers.
    /// Upstream: `std::unique_ptr<VideoCore::ShaderNotify> shader_notify`.
    /// Declared after `renderer` so renderer-owned workers are dropped first.
    shader_notify: ShaderNotify,

    /// Rasterizer extracted from renderer.
    /// Upstream: `VideoCore::RasterizerInterface* rasterizer` in GPU::Impl.
    rasterizer: Mutex<Option<RasterizerHandle>>,

    /// GPU channel scheduler.
    /// Upstream: `Tegra::Control::Scheduler scheduler` in GPU::Impl.
    scheduler: crate::control::scheduler::Scheduler,

    /// GPU command thread manager.
    /// Upstream: `VideoCommon::GPUThread::ThreadManager gpu_thread` in GPU::Impl.
    gpu_thread: Mutex<crate::gpu_thread::ThreadManager>,

    /// Registered GPU channels.
    /// Upstream: `std::unordered_map<s32, std::shared_ptr<ChannelState>> channels`.
    channels:
        Mutex<HashMap<i32, Arc<parking_lot::Mutex<crate::control::channel_state::ChannelState>>>>,

    /// Owner-local bridge for GPU VA command fetches that still need core guest memory access.
    /// This is a temporary Rust adaptation until `video_core::MemoryManager` owns the same
    /// backing memory integration as upstream.
    guest_memory_reader: Mutex<Option<crate::renderer_base::DeviceMemoryReader>>,
    guest_memory_writer: Mutex<Option<Arc<dyn Fn(u64, &[u8]) + Send + Sync>>>,
    /// Upstream owner: `Core::System& system`.
    system: Mutex<SystemRef>,
}

impl Drop for Gpu {
    fn drop(&mut self) {
        // C++ destroys `GPU::Impl` members in reverse declaration order, so
        // `gpu_thread` is stopped before `renderer`. Rust drops fields in
        // declaration order and its scheduler is boxed rather than stored
        // in-place; stop the thread while every raw pointer installed by
        // `start_thread` still refers to live storage.
        self.gpu_thread
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .shutdown();
    }
}

#[derive(Default)]
struct RequestSwapCounters {
    free_swap_counters: VecDeque<usize>,
    request_swap_counters: VecDeque<usize>,
}

impl Gpu {
    /// Creates a new GPU controller.
    pub fn new(is_async: bool, use_nvdec: bool) -> Self {
        Self {
            is_async,
            use_nvdec,
            shutting_down: AtomicBool::new(false),
            sync_requests: Mutex::new(VecDeque::new()),
            current_sync_fence: AtomicU64::new(0),
            last_sync_fence: Mutex::new(0),
            sync_request_cv: Condvar::new(),
            pending_composite_fence: AtomicU64::new(0),
            request_swap_counters: Mutex::new(RequestSwapCounters::default()),
            new_channel_id: Mutex::new(1),
            bound_channel: Mutex::new(-1),
            renderer: Mutex::new(None),
            shader_notify: ShaderNotify::new(),
            rasterizer: Mutex::new(None),
            scheduler: crate::control::scheduler::Scheduler::new(),
            gpu_thread: Mutex::new(crate::gpu_thread::ThreadManager::new(
                SystemRef::null(),
                is_async,
            )),
            channels: Mutex::new(HashMap::new()),
            guest_memory_reader: Mutex::new(None),
            guest_memory_writer: Mutex::new(None),
            system: Mutex::new(SystemRef::null()),
        }
    }

    pub fn set_system_ref(&self, system: SystemRef) {
        *self.system.lock().unwrap() = system;
        self.gpu_thread.lock().unwrap().set_system_ref(system);
    }

    pub fn system_ref(&self) -> SystemRef {
        *self.system.lock().unwrap()
    }

    /// Binds a renderer to the GPU.
    ///
    /// Upstream: `GPU::Impl::BindRenderer(unique_ptr<RendererBase> renderer_)`
    /// Also extracts the rasterizer and binds it to host1x memory manager.
    pub fn bind_renderer(&self, renderer: Box<dyn RendererBase>) {
        // Extract rasterizer from renderer.
        // Upstream: rasterizer = renderer->ReadRasterizer();
        let rasterizer_ptr = renderer.read_rasterizer();
        *self.rasterizer.lock().unwrap() =
            Some(RasterizerHandle::from_ref(unsafe { &*rasterizer_ptr }));
        // Upstream also does:
        // host1x.MemoryManager().BindInterface(rasterizer);
        // host1x.GMMU().BindRasterizer(rasterizer);
        if let Some(rasterizer) = self.rasterizer_handle() {
            if let Some(host1x) = self.system_ref().get().host1x_core() {
                if std::env::var_os("RUZU_DISABLE_HOST1X_INVALIDATE_BIND").is_none() {
                    host1x.bind_device_memory_invalidator(Box::new(move |addr, size| unsafe {
                        rasterizer.as_mut().invalidate_region(
                            addr,
                            size as u64,
                            crate::cache_types::CacheType::ALL,
                        );
                    }));
                }
                if std::env::var_os("RUZU_DISABLE_HOST1X_FLUSH_BIND").is_none() {
                    host1x.bind_device_memory_flusher(Box::new(move |addr, size| unsafe {
                        rasterizer.as_mut().flush_region(
                            addr,
                            size as u64,
                            crate::cache_types::CacheType::ALL,
                        );
                    }));
                }
                if let Some(host1x) = host1x
                    .as_any()
                    .downcast_ref::<crate::host1x::host1x::Host1x>()
                {
                    unsafe {
                        host1x.bind_gmmu_rasterizer(rasterizer.as_mut());
                    }
                }
            } else {
                log::warn!(
                    "Gpu::bind_renderer: host1x_core missing; Host1x device writes will not invalidate rasterizer caches"
                );
            }
        }
        log::info!(
            "Gpu::bind_renderer: renderer bound (vendor: {})",
            renderer.get_device_vendor()
        );
        *self.renderer.lock().unwrap() = Some(renderer);
        if let Some(ref mut renderer) = *self.renderer.lock().unwrap() {
            let gpu_ptr = self as *const Self as usize;
            renderer.set_gpu_ticks_getter(Arc::new(move || unsafe {
                let gpu = &*(gpu_ptr as *const Self);
                gpu.get_ticks()
            }));
            let gpu_ptr = self as *const Self as usize;
            renderer.set_gpu_tick_callback(Arc::new(move || unsafe {
                let gpu = &*(gpu_ptr as *const Self);
                gpu.tick_work();
            }));
            let gpu_ptr = self as *const Self as usize;
            renderer.set_invalidate_gpu_cache_callback(Arc::new(move || unsafe {
                let gpu = &*(gpu_ptr as *const Self);
                gpu.invalidate_gpu_cache();
            }));
            if let Some(writer) = self.guest_memory_writer.lock().unwrap().clone() {
                renderer.set_guest_memory_writer(writer);
            }
        }
    }

    /// Bind a GPU channel by its bind ID.
    ///
    /// Matches upstream `GPU::Impl::BindChannel(s32 channel_id)`.
    /// Sets the active channel for command processing.
    pub fn bind_channel(&self, channel_id: i32) {
        {
            let mut bound_channel = self.bound_channel.lock().unwrap();
            if *bound_channel == channel_id {
                return;
            }
            *bound_channel = channel_id;
        }

        let channel_state = {
            let channels = self.channels.lock().unwrap();
            channels.get(&channel_id).cloned()
        };
        let Some(channel_state) = channel_state else {
            panic!("Gpu::bind_channel missing channel {}", channel_id);
        };

        if let Some(rasterizer) = self.rasterizer_handle() {
            let rasterizer = unsafe { rasterizer.as_mut() };
            let mut channel_state = channel_state.lock();
            rasterizer.bind_channel(&mut channel_state);
        }
    }

    fn rasterizer_handle(&self) -> Option<RasterizerHandle> {
        *self.rasterizer.lock().unwrap()
    }

    pub fn set_guest_memory_reader(&self, reader: crate::renderer_base::DeviceMemoryReader) {
        // This is a guest/device-memory bridge for GPU command and compatibility paths.
        // The OpenGL present path owns its upstream-shaped MaxwellDeviceMemoryManager
        // reader from RendererOpenGL::new; replacing it here would make
        // Layer::LoadFBToScreenInfo diverge from device_memory.GetPointer(...).
        *self.guest_memory_reader.lock().unwrap() = Some(reader);
    }

    /// Resolve a GPU virtual address to host memory via the *bound channel's*
    /// `MemoryManager`, using `cpu_reader` for the underlying CPU/device-side
    /// reads. Returns `false` when no channel is bound or the bound channel
    /// has no memory manager. Used by the OpenGL shader cache to fault in
    /// Maxwell shader bytecode.
    pub fn read_gpu_memory(
        &self,
        gpu_va: u64,
        dst: &mut [u8],
        _cpu_reader: &dyn Fn(u64, &mut [u8]),
    ) -> bool {
        let bound = *self.bound_channel.lock().unwrap();
        if bound < 0 {
            return false;
        }
        let channel = match self.channels.lock().unwrap().get(&bound).cloned() {
            Some(c) => c,
            None => return false,
        };
        let channel = channel.lock();
        let Some(mm) = channel.memory_manager.as_ref() else {
            return false;
        };
        mm.lock().read_block(gpu_va, dst);
        true
    }

    pub fn read_guest_memory(&self, addr: u64, output: &mut [u8]) -> bool {
        let Some(reader) = self.guest_memory_reader.lock().unwrap().clone() else {
            return false;
        };
        reader(addr, output)
    }

    pub(crate) fn host1x_device_memory_manager(
        &self,
    ) -> Option<Arc<crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager>> {
        let system = self.system_ref();
        let host1x = system.get().host1x_core()?;
        host1x
            .as_any()
            .downcast_ref::<crate::host1x::host1x::Host1x>()
            .map(|host1x| Arc::clone(host1x.memory_manager()))
    }

    /// Set the guest memory writer callback.
    pub fn set_guest_memory_writer(&self, writer: Arc<dyn Fn(u64, &[u8]) + Send + Sync>) {
        *self.guest_memory_writer.lock().unwrap() = Some(writer);
        if let Some(ref mut renderer) = *self.renderer.lock().unwrap() {
            renderer.set_guest_memory_writer(
                self.guest_memory_writer
                    .lock()
                    .unwrap()
                    .as_ref()
                    .cloned()
                    .expect("guest_memory_writer just stored"),
            );
        }
    }

    /// Install a GPU VA → CPU VA translator on the renderer. The
    /// translator uses the GPU's currently bound channel's MemoryManager
    /// to resolve a GPU VA. Rasterizer code (e.g. RasterizerNull::query)
    /// uses this before calling `guest_memory_writer` so the eventual
    /// `Memory::write_block` receives a valid CPU VA.
    ///
    /// `gpu_ptr` is a raw `*const Gpu` provided by the caller (typically
    /// from `Box::as_ref()`). The caller must ensure the Gpu outlives the
    /// renderer — this matches the existing pattern in `ruzu_cmd` where a
    /// `*const Gpu` is captured for service callbacks (see `gpu_ptr` in
    /// `src/ruzu_cmd/src/main.rs`).
    ///
    /// # Safety
    ///
    /// `gpu_ptr` must point to a live `Gpu` that outlives all rasterizer
    /// invocations.
    pub unsafe fn install_gpu_to_cpu_translator(&self, gpu_ptr: *const Gpu) {
        let gpu_ptr_usize = gpu_ptr as usize;
        let translator: Arc<dyn Fn(u64) -> Option<u64> + Send + Sync> =
            Arc::new(move |gpu_va: u64| -> Option<u64> {
                let gpu = unsafe { &*(gpu_ptr_usize as *const Gpu) };
                let bound = *gpu.bound_channel.lock().unwrap();
                if bound < 0 {
                    return None;
                }
                let channel_arc = gpu.channels.lock().unwrap().get(&bound).cloned()?;
                let channel = channel_arc.lock();
                let mm_arc = channel.memory_manager.as_ref()?.clone();
                drop(channel);
                let result = mm_arc.lock().gpu_to_cpu_address(gpu_va);
                result
            });
        if let Some(ref mut renderer) = *self.renderer.lock().unwrap() {
            renderer.set_gpu_to_cpu_translator(translator);
        }
    }

    /// Write guest memory at the given CPU/device address.
    pub fn write_guest_memory(&self, addr: u64, data: &[u8]) {
        let Some(writer) = self.guest_memory_writer.lock().unwrap().clone() else {
            return;
        };
        writer(addr, data);
    }

    /// Create a new GPU channel and register it with the scheduler.
    ///
    /// Matches upstream `GPU::Impl::CreateChannel(s32 channel_id)`.
    pub fn create_channel(
        &self,
        channel_id: i32,
    ) -> Arc<parking_lot::Mutex<crate::control::channel_state::ChannelState>> {
        let channel_state = Arc::new(parking_lot::Mutex::new(
            crate::control::channel_state::ChannelState::new(channel_id),
        ));
        self.channels
            .lock()
            .unwrap()
            .insert(channel_id, channel_state.clone());

        // Register with scheduler.
        self.scheduler.declare_channel(channel_state.clone());

        channel_state
    }

    /// Allocate a new channel ID.
    ///
    /// Matches upstream `GPU::Impl::AllocateChannel()`.
    pub fn allocate_channel(&self) -> i32 {
        let mut id = self.new_channel_id.lock().unwrap();
        let channel_id = *id;
        *id += 1;
        channel_id
    }

    /// Returns a reference to the renderer, if bound.
    pub fn renderer(&self) -> std::sync::MutexGuard<'_, Option<Box<dyn RendererBase>>> {
        self.renderer.lock().unwrap()
    }

    /// Refresh settings shared by all renderer backends.
    ///
    /// Upstream owner/call chain:
    /// `Core::System::ApplySettings()` -> `GPU::Renderer()` ->
    /// `RendererBase::RefreshBaseSettings()`.
    pub fn refresh_renderer_settings(&self) {
        if let Some(renderer) = self.renderer.lock().unwrap().as_mut() {
            renderer.refresh_base_settings();
        }
    }

    /// Port of `GPU::ShaderNotify()`.
    pub fn shader_notify(&self) -> &ShaderNotify {
        &self.shader_notify
    }

    /// Rust representation of the non-owning pointer passed into renderer
    /// pipeline caches upstream.
    ///
    /// # Safety
    /// The handle must only be stored by a renderer that is immediately bound
    /// to this `Gpu`; `Gpu` drops that renderer before `shader_notify`.
    pub unsafe fn shader_notify_handle(&self) -> ShaderNotifyHandle {
        unsafe { ShaderNotifyHandle::new(&self.shader_notify) }
    }

    /// Flush all current written commands into the host GPU for execution.
    pub fn flush_commands(&self) {
        if let Some(rasterizer) = self.rasterizer_handle() {
            unsafe { rasterizer.as_mut() }.flush_commands();
        }
    }

    /// Synchronizes CPU writes with Host GPU memory.
    pub fn invalidate_gpu_cache(&self) {
        let Some(rasterizer) = self.rasterizer_handle() else {
            return;
        };
        let system = *self.system.lock().unwrap();
        if system.is_null() {
            log::warn!("Gpu::invalidate_gpu_cache: system ref not set");
            return;
        }
        let rasterizer = unsafe { rasterizer.as_mut() };
        system.get().gather_gpu_dirty_memory(&mut |addr, size| {
            rasterizer.on_cache_invalidation(addr, size as u64);
        });
    }

    /// Signal the ending of command list.
    pub fn on_command_list_end(&self) {
        if let Some(rasterizer) = self.rasterizer_handle() {
            unsafe { rasterizer.as_mut() }.release_fences(false);
        }
        settings::update_gpu_accuracy(&mut settings::values_mut());
    }

    /// Request a host GPU memory flush from the CPU.
    ///
    /// Port of upstream `GPU::RequestFlush(DAddr, std::size_t)`:
    ///
    /// ```cpp
    /// u64 GPU::RequestFlush(DAddr addr, std::size_t size) {
    ///     return impl->RequestSyncOperation(
    ///         [this, addr, size]() { impl->rasterizer->FlushRegion(addr, size); });
    /// }
    /// ```
    ///
    /// Enqueues a sync operation that calls the rasterizer's `flush_region`
    /// and returns the fence number. The caller is expected to either call
    /// `wait_for_sync_operation(fence)` to block on completion, or let the
    /// GPU thread drain it asynchronously via `tick_work`. This is the path
    /// a framebuffer from CPU memory (e.g. before QueueBuffer).
    pub fn request_flush(&self, addr: DAddr, size: usize) -> u64 {
        // Capture &Gpu as a raw `usize` so the closure is Send. The Gpu
        // outlives every sync request (sync_requests is owned by Gpu and
        // drained before drop), so the deref inside the callback is safe.
        let gpu_addr = self as *const Gpu as usize;
        self.request_sync_operation(Box::new(move || {
            let gpu = unsafe { &*(gpu_addr as *const Gpu) };
            if let Some(rasterizer) = gpu.rasterizer_handle() {
                unsafe { rasterizer.as_mut() }.flush_region(
                    addr,
                    size as u64,
                    crate::cache_types::CacheType::ALL,
                );
            }
        }))
    }

    /// Obtains current flush request fence id.
    /// Queue a synchronization operation to be executed by TickWork().
    /// Returns a fence number that can be passed to wait_for_sync_operation.
    ///
    /// Matches upstream `GPU::Impl::RequestSyncOperation(Func&& action)`.
    pub fn request_sync_operation(&self, action: Box<dyn FnOnce() + Send>) -> u64 {
        let mut requests = self.sync_requests.lock().unwrap();
        let mut last = self.last_sync_fence.lock().unwrap();
        *last += 1;
        let fence = *last;
        requests.push_back(action);
        fence
    }

    pub fn current_sync_request_fence(&self) -> u64 {
        self.current_sync_fence.load(Ordering::Relaxed)
    }

    /// Wait for a sync operation to complete.
    pub fn wait_for_sync_operation(&self, fence: u64) {
        let mut guard = self.sync_requests.lock().unwrap();
        while self.current_sync_fence.load(Ordering::Relaxed) < fence {
            guard = self.sync_request_cv.wait(guard).unwrap();
        }
    }

    /// Tick pending requests within the GPU.
    pub fn tick_work(&self) {
        let mut requests = self.sync_requests.lock().unwrap();
        while let Some(request) = requests.pop_front() {
            drop(requests);
            request();
            self.current_sync_fence.fetch_add(1, Ordering::Release);
            requests = self.sync_requests.lock().unwrap();
            self.sync_request_cv.notify_all();
        }
    }

    /// Returns the GPU ticks.
    pub fn get_ticks(&self) -> u64 {
        let system = *self.system.lock().unwrap();
        if system.is_null() {
            return 0;
        }

        let gpu_tick = system.get().core_timing().get_gpu_ticks();
        gpu_tick / gpu_clock_multiplier(*settings::values().gpu_clock.get_value())
    }

    /// Returns whether async GPU mode is enabled.
    pub fn is_async(&self) -> bool {
        self.is_async
    }

    /// Returns whether NVDEC is enabled.
    pub fn use_nvdec(&self) -> bool {
        self.use_nvdec
    }

    /// Start the GPU thread.
    ///
    /// Upstream: `GPU::Impl::Start()` calls `Settings::UpdateGPUAccuracy()`
    /// then `gpu_thread.StartThread(*renderer, renderer->Context(), *scheduler)`.
    pub fn start(&self) {
        settings::update_gpu_accuracy(&mut settings::values_mut());

        let mut renderer_guard = self.renderer.lock().unwrap();
        if let Some(ref mut renderer) = *renderer_guard {
            log::info!("Gpu::start: GPU started (renderer bound, starting GPU thread)");
            // Upstream: gpu_thread.StartThread(*renderer, renderer->Context(), *scheduler)
            let gpu_ptr = self as *const Gpu;
            let renderer_ptr = renderer.as_mut() as *mut dyn RendererBase;
            let context_ptr = renderer.context_ptr();
            let scheduler_ptr = &self.scheduler as *const crate::control::scheduler::Scheduler;
            drop(renderer_guard);
            // Safety: Gpu, renderer, and scheduler outlive the ThreadManager.
            unsafe {
                self.gpu_thread.lock().unwrap().start_thread(
                    gpu_ptr,
                    renderer_ptr,
                    context_ptr,
                    scheduler_ptr,
                );
            }
        } else {
            log::warn!("Gpu::start: no renderer bound");
        }
    }

    /// Notify shutdown.
    pub fn notify_shutdown(&self) {
        let _lock = self.sync_requests.lock().unwrap();
        self.shutting_down.store(true, Ordering::Relaxed);
        self.sync_request_cv.notify_all();
    }

    /// Push GPU command entries to be processed.
    /// Matches upstream `GPU::Impl::PushGPUEntries(s32, CommandList&&)`.
    pub fn push_gpu_entries(&self, channel: i32, entries: CommandList) {
        self.gpu_thread
            .lock()
            .unwrap()
            .submit_list(channel, entries);
    }

    /// Notify rasterizer about a CPU read.
    pub fn on_cpu_read(&self, addr: DAddr, size: u64) -> RasterizerDownloadArea {
        let Some(rasterizer) = self.rasterizer_handle() else {
            log::warn!("Gpu::on_cpu_read: no rasterizer bound, returning empty area");
            return RasterizerDownloadArea {
                start_address: addr,
                end_address: addr,
                preemtive: true,
            };
        };

        let flush_area = unsafe { rasterizer.as_mut() }.get_flush_area(addr, size);
        let mut raster_area = RasterizerDownloadArea {
            start_address: flush_area.start_address,
            end_address: flush_area.end_address,
            preemtive: flush_area.preemptive,
        };
        if raster_area.preemtive {
            return raster_area;
        }

        raster_area.preemtive = true;
        let start_address = raster_area.start_address;
        let end_address = raster_area.end_address;
        let gpu_addr = self as *const Gpu as usize;
        let fence = self.request_sync_operation(Box::new(move || {
            let gpu = unsafe { &*(gpu_addr as *const Gpu) };
            if let Some(rasterizer) = gpu.rasterizer_handle() {
                unsafe { rasterizer.as_mut() }.flush_region(
                    start_address,
                    end_address - start_address,
                    crate::cache_types::CacheType::ALL,
                );
            }
        }));
        self.gpu_thread.lock().unwrap().tick_gpu();
        self.wait_for_sync_operation(fence);
        raster_area
    }

    /// Flush a region.
    /// Matches upstream `GPU::Impl::FlushRegion(DAddr, u64)`.
    pub fn flush_region(&self, addr: DAddr, size: u64) {
        self.gpu_thread.lock().unwrap().flush_region(addr, size);
    }

    /// Invalidate a region.
    /// Matches upstream `GPU::Impl::InvalidateRegion(DAddr, u64)`.
    pub fn invalidate_region(&self, addr: DAddr, size: u64) {
        self.gpu_thread
            .lock()
            .unwrap()
            .invalidate_region(addr, size);
    }

    /// Notify rasterizer of a CPU write.
    /// Matches upstream `GPU::Impl::OnCPUWrite(DAddr, u64)`.
    pub fn on_cpu_write(&self, _addr: DAddr, _size: u64) -> bool {
        let Some(rasterizer) = self.rasterizer_handle() else {
            return false;
        };
        unsafe { rasterizer.as_mut() }.on_cpu_write(_addr, _size)
    }

    /// Flush and invalidate a region.
    /// Matches upstream `GPU::Impl::FlushAndInvalidateRegion(DAddr, u64)`.
    pub fn flush_and_invalidate_region(&self, addr: DAddr, size: u64) {
        self.gpu_thread
            .lock()
            .unwrap()
            .flush_and_invalidate_region(addr, size);
    }

    /// Request framebuffer compositing.
    ///
    /// Upstream: `GPU::Impl::RequestComposite(layers, fences)` enqueues a sync
    /// operation that waits for fences then calls `renderer->Composite(layers)`.
    /// Request a composite (frame presentation).
    ///
    /// Matches upstream `GPU::Impl::RequestComposite(layers, fences)`:
    /// queues fence registration as a sync operation and signals the GPU
    /// thread via TickGPU. The following HWC composition waits for registration
    /// through `WaitForComposite`. If fences are present, composition runs from
    /// the Host1x guest-syncpoint callback once every fence reaches its target.
    pub fn request_composite(&self, layers: Vec<FramebufferConfig>) {
        self.request_composite_with_fences(layers, Vec::new());
    }

    fn composite_layers(&self, layers: &[FramebufferConfig]) {
        let mut renderer_guard = self.renderer.lock().unwrap();
        if let Some(ref mut renderer) = *renderer_guard {
            renderer.composite(layers);
        }
    }

    fn request_composite_with_fences(&self, layers: Vec<FramebufferConfig>, fences: Vec<NvFence>) {
        // Capture a raw pointer to self for the callback via usize (Send-safe).
        // Safety: Gpu owns and drains the sync request queue before destruction.
        let gpu_addr = self as *const Gpu as usize;
        let pending_fence = self.request_sync_operation(Box::new(move || {
            let gpu = unsafe { &*(gpu_addr as *const Gpu) };
            let valid_fences: Vec<NvFence> =
                fences.into_iter().filter(|fence| fence.id >= 0).collect();
            if valid_fences.is_empty() {
                gpu.composite_layers(&layers);
                return;
            }

            let system = gpu.system_ref();
            let Some(host1x) = system.get().host1x_core() else {
                log::warn!(
                    "Gpu::request_composite missing host1x_core; composing without {} fences",
                    valid_fences.len()
                );
                gpu.composite_layers(&layers);
                return;
            };

            let current_request_counter = gpu.allocate_request_swap_counter(valid_fences.len());
            for fence in valid_fences {
                let layers = layers.clone();
                host1x.register_guest_action(
                    fence.id as u32,
                    fence.value,
                    Box::new(move || {
                        let gpu = unsafe { &*(gpu_addr as *const Gpu) };
                        if gpu.complete_request_swap_counter(current_request_counter) {
                            gpu.composite_layers(&layers);
                        }
                    }),
                );
            }
        }));
        self.pending_composite_fence
            .store(pending_fence, Ordering::Relaxed);
        self.gpu_thread.lock().unwrap().tick_gpu();
    }

    /// Wait for registration of the previous composite request.
    ///
    /// Matches upstream `GPU::Impl::WaitForComposite`: composition is queued
    /// asynchronously and the following HWC tick consumes its pending fence.
    pub fn wait_for_composite(&self) {
        let fence = self.pending_composite_fence.swap(0, Ordering::Relaxed);
        if fence == 0 || self.shutting_down.load(Ordering::Relaxed) {
            return;
        }
        self.wait_for_sync_operation(fence);
    }

    fn allocate_request_swap_counter(&self, num_fences: usize) -> usize {
        let mut counters = self.request_swap_counters.lock().unwrap();
        if let Some(index) = counters.free_swap_counters.pop_front() {
            counters.request_swap_counters[index] = num_fences;
            index
        } else {
            let index = counters.request_swap_counters.len();
            counters.request_swap_counters.push_back(num_fences);
            index
        }
    }

    fn complete_request_swap_counter(&self, index: usize) -> bool {
        let mut counters = self.request_swap_counters.lock().unwrap();
        let Some(counter) = counters.request_swap_counters.get_mut(index) else {
            return false;
        };
        if *counter == 0 {
            return false;
        }
        *counter -= 1;
        if *counter != 0 {
            return false;
        }
        counters.free_swap_counters.push_back(index);
        true
    }

    /// Get the applet capture buffer.
    ///
    /// Matches upstream: queues via sync request + TickGPU + wait.
    pub fn get_applet_capture_buffer(&self) -> Vec<u8> {
        use std::sync::{Arc, Mutex};
        let result = Arc::new(Mutex::new(Vec::new()));
        let result_clone = result.clone();
        let gpu_addr = self as *const Gpu as usize;
        let wait_fence = self.request_sync_operation(Box::new(move || {
            let gpu = unsafe { &*(gpu_addr as *const Gpu) };
            let mut renderer_guard = gpu.renderer.lock().unwrap();
            if let Some(ref mut renderer) = *renderer_guard {
                *result_clone.lock().unwrap() = renderer.get_applet_capture_buffer();
            }
        }));
        self.gpu_thread.lock().unwrap().tick_gpu();
        self.wait_for_sync_operation(wait_fence);
        Arc::try_unwrap(result).unwrap().into_inner().unwrap()
    }

    /// Renderer frame end notification.
    pub fn renderer_frame_end_notify(&self) {
        let system = self.system_ref();
        if system.is_null() {
            return;
        }
        if let Some(stats) = system.get().get_perf_stats() {
            stats.end_game_frame();
        }
    }
}

impl GpuChannelHandle for VideoGpuChannelHandle {
    fn bind_memory_manager(&self, memory_manager: Arc<dyn GpuMemoryManagerHandle>) {
        let memory_manager = memory_manager
            .as_any()
            .downcast_ref::<VideoGpuMemoryManagerHandle>()
            .expect("GPU memory manager handle must come from video_core::gpu::Gpu");
        let mut channel_state = self.channel_state.lock();
        channel_state.memory_manager = Some(Arc::clone(&memory_manager.memory_manager));
    }

    fn init_channel(&self, program_id: u64) {
        let mut channel_state = self.channel_state.lock();
        if channel_state.initialized {
            return;
        }

        let gpu = unsafe { &*self.gpu };
        channel_state.init(gpu, program_id);
        log::info!(
            "VideoGpuChannelHandle::init_channel: program_id={:#x} maxwell_3d={}",
            program_id,
            channel_state.maxwell_3d.is_some()
        );
        if let Some(rasterizer) = gpu.rasterizer_handle() {
            let rasterizer = unsafe { rasterizer.as_mut() };
            channel_state.bind_rasterizer(rasterizer);
            rasterizer.initialize_channel(&mut channel_state);
        }
    }

    fn bind_id(&self) -> i32 {
        self.bind_id
    }
}

impl GpuMemoryManagerHandle for VideoGpuMemoryManagerHandle {
    fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
        self
    }

    // The nvdrv (CPU-side) map/unmap entry points replay rasterizer
    // notifications AFTER releasing the memory-manager mutex. The GPU thread
    // takes the rasterizer lock during draws and then locks this memory
    // manager for address translation, so invoking the rasterizer while the
    // `Tegra::MemoryManager` has no mutex, so its inline rasterizer calls
    // never nest a cache lock under a memory-manager lock.
    fn map(&self, gpu_addr: u64, device_addr: u64, size: u64, kind: u32, is_big_pages: bool) {
        let (handle, ops) = {
            let mut mm = self.memory_manager.lock();
            mm.begin_deferring_rasterizer_ops();
            mm.map(gpu_addr, device_addr, size, kind, is_big_pages);
            (mm.rasterizer_handle(), mm.take_deferred_rasterizer_ops())
        };
        replay_deferred_rasterizer_ops(handle, ops);
    }

    fn map_sparse(&self, gpu_addr: u64, size: u64, is_big_pages: bool) {
        let (handle, ops) = {
            let mut mm = self.memory_manager.lock();
            mm.begin_deferring_rasterizer_ops();
            mm.map_sparse(gpu_addr, size, is_big_pages);
            (mm.rasterizer_handle(), mm.take_deferred_rasterizer_ops())
        };
        replay_deferred_rasterizer_ops(handle, ops);
    }

    fn unmap(&self, gpu_addr: u64, size: u64) {
        let (handle, ops) = {
            let mut mm = self.memory_manager.lock();
            mm.begin_deferring_rasterizer_ops();
            mm.unmap(gpu_addr, size);
            (mm.rasterizer_handle(), mm.take_deferred_rasterizer_ops())
        };
        replay_deferred_rasterizer_ops(handle, ops);
    }
}

/// Replay rasterizer notifications recorded under the memory-manager mutex.
/// Must be called with the mutex RELEASED — see the comment on
/// `VideoGpuMemoryManagerHandle::map`.
fn replay_deferred_rasterizer_ops(
    handle: Option<crate::rasterizer_interface::RasterizerHandle>,
    ops: Vec<crate::memory_manager::DeferredRasterizerOp>,
) {
    use crate::memory_manager::DeferredRasterizerOp;
    let Some(handle) = handle else { return };
    if ops.is_empty() {
        return;
    }
    // SAFETY: same contract as `GpuMemoryManager::with_rasterizer_mut` — the
    // handle points at the rasterizer owned by the renderer, which outlives
    // every memory manager bound to it.
    unsafe {
        handle.with_mut(|rasterizer| {
            for op in ops {
                match op {
                    DeferredRasterizerOp::ModifyGpuMemory { id, gpu_addr, size } => {
                        rasterizer.modify_gpu_memory(id, gpu_addr, size);
                    }
                    DeferredRasterizerOp::UnmapMemory { device_addr, size } => {
                        rasterizer.unmap_memory(device_addr, size);
                    }
                }
            }
        });
    }
}

impl GpuCoreInterface for Gpu {
    fn as_any(&self) -> &(dyn std::any::Any + Send) {
        self
    }

    fn get_device_vendor(&self) -> String {
        self.renderer
            .lock()
            .unwrap()
            .as_ref()
            .map_or_else(String::new, |renderer| renderer.get_device_vendor())
    }

    fn allocate_channel_handle(&self) -> Arc<dyn GpuChannelHandle> {
        let channel_id = self.allocate_channel();
        let channel_state = self.create_channel(channel_id);
        Arc::new(VideoGpuChannelHandle {
            gpu: self as *const Gpu,
            bind_id: channel_id,
            channel_state,
        })
    }

    fn allocate_memory_manager_handle(
        &self,
        address_space_bits: u64,
        split_address: u64,
        big_page_bits: u64,
        page_bits: u64,
    ) -> Arc<dyn GpuMemoryManagerHandle> {
        let id = NEXT_MEMORY_MANAGER_ID.fetch_add(1, Ordering::AcqRel);
        let device_memory = self
            .host1x_device_memory_manager()
            .expect("GPU memory-manager allocation requires a Host1x device-memory owner");
        let mm = crate::memory_manager::MemoryManager::new_with_geometry_and_device_memory(
            id,
            device_memory,
            address_space_bits,
            split_address,
            big_page_bits,
            page_bits,
        );
        Arc::new(VideoGpuMemoryManagerHandle {
            memory_manager: Arc::new(parking_lot::Mutex::new(mm)),
        })
    }

    fn init_address_space(&self, memory_manager: Arc<dyn GpuMemoryManagerHandle>) {
        let handle = memory_manager
            .as_any()
            .downcast_ref::<VideoGpuMemoryManagerHandle>()
            .expect("GPU memory manager handle must originate from video_core::Gpu");
        if self.rasterizer_handle().is_none() {
            return;
        }
        let Some(rasterizer) = self.rasterizer_handle() else {
            return;
        };
        let rasterizer = unsafe { rasterizer.as_mut() };
        handle.memory_manager.lock().bind_rasterizer(rasterizer);
    }

    fn push_gpu_entries(&self, channel_id: i32, entries: GpuCommandList) {
        let command_list = CommandList {
            command_lists: entries
                .command_lists
                .into_iter()
                .map(|header| crate::dma_pusher::CommandListHeader { raw: header.raw })
                .collect(),
            prefetch_command_list: entries
                .prefetch_command_list
                .into_iter()
                .map(|header| crate::dma_pusher::CommandHeader { raw: header.raw })
                .collect(),
        };
        Gpu::push_gpu_entries(self, channel_id, command_list);
    }

    fn request_composite(&self, layers: Vec<CoreFramebufferConfig>, fences: Vec<NvFence>) {
        let layers = layers
            .into_iter()
            .map(|layer| FramebufferConfig {
                address: layer.address,
                offset: layer.offset,
                width: layer.width,
                height: layer.height,
                stride: layer.stride,
                pixel_format: layer.pixel_format,
                transform_flags: layer.transform_flags,
                crop_rect: layer.crop_rect,
                blending: match layer.blending {
                    CoreBlendMode::Opaque => crate::framebuffer_config::BlendMode::Opaque,
                    CoreBlendMode::Premultiplied => {
                        crate::framebuffer_config::BlendMode::Premultiplied
                    }
                    CoreBlendMode::Coverage => crate::framebuffer_config::BlendMode::Coverage,
                },
            })
            .collect();
        self.request_composite_with_fences(layers, fences);
    }

    fn wait_for_composite(&self) {
        Gpu::wait_for_composite(self);
    }

    fn notify_shutdown(&self) {
        Gpu::notify_shutdown(self);
    }

    fn on_cpu_write(&self, addr: u64, size: u64) -> bool {
        Gpu::on_cpu_write(self, addr, size)
    }

    fn on_cpu_read(&self, addr: u64, size: u64) -> CoreRasterizerDownloadArea {
        let area = Gpu::on_cpu_read(self, addr, size);
        CoreRasterizerDownloadArea {
            start_address: area.start_address,
            end_address: area.end_address,
            preemptive: area.preemtive,
        }
    }

    fn flush_region(&self, addr: u64, size: u64) {
        Gpu::flush_region(self, addr, size);
    }

    fn invalidate_region(&self, addr: u64, size: u64) {
        Gpu::invalidate_region(self, addr, size);
    }

    fn get_applet_capture_buffer(&self) -> Vec<u8> {
        Gpu::get_applet_capture_buffer(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_manager::MemoryManager;
    use crate::rasterizer_interface::{
        RasterizerDownloadArea, RasterizerHandle, RasterizerInterface,
    };
    use std::sync::{Arc, Mutex as StdMutex, OnceLock};

    struct FakeRasterizer {
        accelerate_dma: crate::rasterizer_interface::TestAccelerateDMA,
        initialized_channels: Arc<StdMutex<Vec<i32>>>,
        bound_channels: Arc<StdMutex<Vec<i32>>>,
    }

    impl RasterizerInterface for FakeRasterizer {
        fn access_accelerate_dma(
            &mut self,
        ) -> &mut dyn crate::engines::maxwell_dma::AccelerateDMAInterface {
            &mut self.accelerate_dma
        }

        fn draw(
            &mut self,
            _draw_view: crate::engines::draw_manager::Maxwell3DDrawView<'_>,
            _instance_count: u32,
        ) {
        }
        fn draw_texture(
            &mut self,
            _draw_texture_view: crate::engines::draw_manager::Maxwell3DDrawTextureView<'_>,
        ) {
        }
        fn clear(
            &mut self,
            _clear_view: crate::engines::draw_manager::Maxwell3DClearView<'_>,
            _layer_count: u32,
        ) {
        }
        fn dispatch_compute(&mut self, _dispatch: &crate::engines::kepler_compute::DispatchCall) {}
        fn reset_counter(&mut self, _query_type: u32) {}
        fn query(
            &mut self,
            _gpu_addr: u64,
            _query_type: u32,
            _flags: crate::query_cache::types::QueryPropertiesFlags,
            _payload: u32,
            _subreport: u32,
        ) {
        }
        fn bind_graphics_uniform_buffer(
            &mut self,
            _stage: usize,
            _index: u32,
            _gpu_addr: u64,
            _size: u32,
        ) {
        }
        fn disable_graphics_uniform_buffer(&mut self, _stage: usize, _index: u32) {}
        fn signal_fence(&mut self, _func: Box<dyn FnOnce() + Send>) {}
        fn sync_operation(&mut self, _func: Box<dyn FnOnce() + Send>) {}
        fn signal_sync_point(&mut self, _value: u32) {}
        fn signal_reference(&mut self) {}
        fn release_fences(&mut self, _force: bool) {}
        fn flush_all(&mut self) {}
        fn flush_region(&mut self, _addr: u64, _size: u64, _which: crate::cache_types::CacheType) {}
        fn must_flush_region(
            &self,
            _addr: u64,
            _size: u64,
            _which: crate::cache_types::CacheType,
        ) -> bool {
            false
        }
        fn get_flush_area(&self, addr: u64, _size: u64) -> RasterizerDownloadArea {
            RasterizerDownloadArea {
                start_address: addr,
                end_address: addr,
                preemptive: true,
            }
        }
        fn invalidate_region(
            &mut self,
            _addr: u64,
            _size: u64,
            _which: crate::cache_types::CacheType,
        ) {
        }
        fn on_cache_invalidation(&mut self, _addr: u64, _size: u64) {}
        fn on_cpu_write(&mut self, _addr: u64, _size: u64) -> bool {
            false
        }
        fn invalidate_gpu_cache(&mut self) {}
        fn unmap_memory(&mut self, _addr: u64, _size: u64) {}
        fn modify_gpu_memory(&mut self, _as_id: usize, _addr: u64, _size: u64) {}
        fn flush_and_invalidate_region(
            &mut self,
            _addr: u64,
            _size: u64,
            _which: crate::cache_types::CacheType,
        ) {
        }
        fn wait_for_idle(&mut self) {}
        fn fragment_barrier(&mut self) {}
        fn tiled_cache_barrier(&mut self) {}
        fn flush_commands(&mut self) {}
        fn tick_frame(&mut self) {}
        fn accelerate_inline_to_memory(
            &mut self,
            _address: u64,
            _copy_size: usize,
            _memory: &[u8],
        ) {
        }
        fn initialize_channel(
            &mut self,
            channel: &mut crate::control::channel_state::ChannelState,
        ) {
            self.initialized_channels
                .lock()
                .unwrap()
                .push(channel.bind_id);
        }
        fn bind_channel(&mut self, channel: &mut crate::control::channel_state::ChannelState) {
            self.bound_channels.lock().unwrap().push(channel.bind_id);
        }
    }

    #[test]
    fn guest_memory_reader_preserves_success_status() {
        let gpu = Gpu::new(false, false);
        let calls = Arc::new(StdMutex::new(Vec::new()));
        let calls_for_reader = Arc::clone(&calls);

        gpu.set_guest_memory_reader(Arc::new(move |addr, output| {
            calls_for_reader.lock().unwrap().push((addr, output.len()));
            if addr == 0x1000 {
                output.copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
                true
            } else {
                false
            }
        }));

        let mut mapped = [0; 4];
        assert!(gpu.read_guest_memory(0x1000, &mut mapped));
        assert_eq!(mapped, [0x11, 0x22, 0x33, 0x44]);

        let mut unmapped = [0xaa; 4];
        assert!(!gpu.read_guest_memory(0x2000, &mut unmapped));
        assert_eq!(unmapped, [0xaa; 4]);
        assert_eq!(*calls.lock().unwrap(), vec![(0x1000, 4), (0x2000, 4)]);
    }

    #[test]
    fn init_channel_initializes_without_changing_the_bound_channel() {
        let gpu = Gpu::new(false, false);
        let channel_state = gpu.create_channel(7);
        channel_state.lock().memory_manager = Some(Arc::new(parking_lot::Mutex::new(
            MemoryManager::new_with_geometry(1, 32, 0x1_0000_0000, 16, 12),
        )));

        let initialized_channels = Arc::new(StdMutex::new(Vec::new()));
        let bound_channels = Arc::new(StdMutex::new(Vec::new()));
        let rasterizer = Box::new(FakeRasterizer {
            accelerate_dma: Default::default(),
            initialized_channels: initialized_channels.clone(),
            bound_channels: bound_channels.clone(),
        });
        let rasterizer_ptr: *mut FakeRasterizer = Box::into_raw(rasterizer);
        *gpu.rasterizer.lock().unwrap() =
            Some(RasterizerHandle::from_ref(unsafe { &*rasterizer_ptr }));

        let handle = VideoGpuChannelHandle {
            gpu: &gpu as *const Gpu,
            bind_id: 7,
            channel_state: channel_state.clone(),
        };
        handle.init_channel(0x1234);

        assert!(channel_state.lock().initialized);
        assert!(channel_state
            .lock()
            .memory_manager
            .as_ref()
            .unwrap()
            .lock()
            .has_bound_rasterizer());
        assert_eq!(*initialized_channels.lock().unwrap(), vec![7]);
        assert!(bound_channels.lock().unwrap().is_empty());

        unsafe {
            drop(Box::from_raw(rasterizer_ptr));
        }
    }

    #[test]
    fn bind_channel_notifies_rasterizer_once_per_new_channel() {
        let gpu = Gpu::new(false, false);
        gpu.create_channel(7);

        let bound_channels = Arc::new(StdMutex::new(Vec::new()));
        let rasterizer = Box::new(FakeRasterizer {
            accelerate_dma: Default::default(),
            initialized_channels: Arc::new(StdMutex::new(Vec::new())),
            bound_channels: bound_channels.clone(),
        });
        let rasterizer_ptr: *mut FakeRasterizer = Box::into_raw(rasterizer);
        *gpu.rasterizer.lock().unwrap() =
            Some(RasterizerHandle::from_ref(unsafe { &*rasterizer_ptr }));

        gpu.bind_channel(7);
        gpu.bind_channel(7);

        assert_eq!(*bound_channels.lock().unwrap(), vec![7]);

        unsafe {
            drop(Box::from_raw(rasterizer_ptr));
        }
    }

    #[test]
    fn request_swap_counter_reuses_freed_counter_after_all_fences_complete() {
        let gpu = Gpu::new(false, false);

        let first = gpu.allocate_request_swap_counter(2);
        assert!(!gpu.complete_request_swap_counter(first));
        assert!(gpu.complete_request_swap_counter(first));

        let second = gpu.allocate_request_swap_counter(1);
        assert_eq!(second, first);
        assert!(gpu.complete_request_swap_counter(second));

        let counters = gpu.request_swap_counters.lock().unwrap();
        assert_eq!(counters.request_swap_counters.len(), 1);
        assert_eq!(counters.free_swap_counters.front().copied(), Some(first));
    }

    #[test]
    fn bind_memory_manager_and_init_do_not_change_the_bound_channel() {
        let gpu = Gpu::new(false, false);
        let channel_state = gpu.create_channel(7);

        let initialized_channels = Arc::new(StdMutex::new(Vec::new()));
        let bound_channels = Arc::new(StdMutex::new(Vec::new()));
        let rasterizer = Box::new(FakeRasterizer {
            accelerate_dma: Default::default(),
            initialized_channels: initialized_channels.clone(),
            bound_channels: bound_channels.clone(),
        });
        let rasterizer_ptr: *mut FakeRasterizer = Box::into_raw(rasterizer);
        *gpu.rasterizer.lock().unwrap() =
            Some(RasterizerHandle::from_ref(unsafe { &*rasterizer_ptr }));

        let channel_handle = VideoGpuChannelHandle {
            gpu: &gpu as *const Gpu,
            bind_id: 7,
            channel_state: channel_state.clone(),
        };
        let memory_handle =
            Arc::new(VideoGpuMemoryManagerHandle {
                memory_manager: Arc::new(parking_lot::Mutex::new(
                    MemoryManager::new_with_geometry(1, 32, 0x1_0000_0000, 16, 12),
                )),
            });

        channel_handle.bind_memory_manager(memory_handle);

        assert!(channel_state.lock().memory_manager.is_some());
        assert!(bound_channels.lock().unwrap().is_empty());

        channel_handle.init_channel(0x1234);

        assert_eq!(*initialized_channels.lock().unwrap(), vec![7]);
        assert!(bound_channels.lock().unwrap().is_empty());

        unsafe {
            drop(Box::from_raw(rasterizer_ptr));
        }
    }

    #[test]
    fn get_ticks_uses_core_timing_and_gpu_clock_setting() {
        static SETTINGS_LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
        let _guard = SETTINGS_LOCK
            .get_or_init(|| StdMutex::new(()))
            .lock()
            .unwrap();

        let system = ruzu_core::core::System::new();
        system.core_timing().add_ticks(512);
        let base_gpu_ticks = system.core_timing().get_gpu_ticks();

        let gpu = Gpu::new(false, false);
        gpu.set_system_ref(ruzu_core::core::SystemRef::from_ref(&system));

        let previous_gpu_clock = {
            let values = settings::values();
            *values.gpu_clock.get_value()
        };

        settings::values_mut()
            .gpu_clock
            .set_value(common::settings_enums::GpuClock::Normal);
        assert_eq!(gpu.get_ticks(), base_gpu_ticks);

        settings::values_mut()
            .gpu_clock
            .set_value(common::settings_enums::GpuClock::Boost);
        assert_eq!(gpu.get_ticks(), base_gpu_ticks / 256);

        settings::values_mut()
            .gpu_clock
            .set_value(common::settings_enums::GpuClock::Overclock);
        assert_eq!(gpu.get_ticks(), base_gpu_ticks / 512);

        settings::values_mut()
            .gpu_clock
            .set_value(previous_gpu_clock);
    }

    #[test]
    fn renderer_frame_end_notify_updates_perf_stats_game_frames() {
        let mut system = ruzu_core::core::System::new();
        system.init_perf_stats(0x0100_0000_0000_0000);

        let gpu = Gpu::new(false, false);
        gpu.set_system_ref(ruzu_core::core::SystemRef::from_ref(&system));

        std::thread::sleep(std::time::Duration::from_millis(1));
        gpu.renderer_frame_end_notify();

        let results = system.get_and_reset_perf_stats();
        assert!(results.average_game_fps > 0.0);
    }
}

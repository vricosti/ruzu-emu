// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Vulkan command scheduler with command chunk batching.
//!
//! Ref: Eden `video_core/renderer_vulkan/vk_scheduler.{h,cpp}` — batches commands into chunks,
//! manages render pass state, and submits to the GPU queue.

use ash::vk;
use log::{debug, trace};
use std::collections::VecDeque;
use std::mem::{align_of, size_of, MaybeUninit};
use std::ptr::NonNull;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::command_pool::CommandPool;
use super::graphics_pipeline::GraphicsPipeline;
use super::master_semaphore::MasterSemaphore;
use super::query_cache::{QueryRuntimeState, SamplesQueryState, TfbCounterState};
use super::state_tracker::StateTracker;
use super::texture_cache::RenderTargetFramebuffer;
use crate::texture_cache::types::NUM_RT;
use crate::vulkan_common::vulkan_wrapper::PIPELINE_STAGE_GRAPHICS_COMPUTE;

pub(crate) type SubmitCallback = Arc<dyn Fn() + Send + Sync>;

const COMMAND_CHUNK_CAPACITY: usize = 0x8000;
const NO_COMMAND: usize = usize::MAX;

#[repr(C)]
#[derive(Clone, Copy)]
struct CommandHeader {
    next: usize,
    payload_offset: usize,
    execute: unsafe fn(*mut u8, vk::CommandBuffer, vk::CommandBuffer),
    drop_payload: unsafe fn(*mut u8),
}

#[repr(C, align(64))]
struct CommandStorage([MaybeUninit<u8>; COMMAND_CHUNK_CAPACITY]);

/// Batch of recorded Vulkan commands (Eden: `CommandChunk`, 32 KiB arena).
struct CommandChunk {
    storage: Box<CommandStorage>,
    first: usize,
    last: usize,
    command_offset: usize,
    submit: Option<SubmitRequest>,
}

impl CommandChunk {
    fn new() -> Self {
        // The payload is an arena of `MaybeUninit<u8>` and has no initialized
        // value invariant. Allocate it directly on the heap so creating or
        // moving a chunk never copies or zeroes 32 KiB.
        let storage = unsafe { Box::<CommandStorage>::new_uninit().assume_init() };
        Self {
            storage,
            first: NO_COMMAND,
            last: NO_COMMAND,
            command_offset: 0,
            submit: None,
        }
    }

    fn is_empty(&self) -> bool {
        self.first == NO_COMMAND && self.submit.is_none()
    }

    fn command_layout<T>(&self) -> Option<(usize, usize, usize)> {
        assert!(
            align_of::<T>() <= align_of::<CommandStorage>(),
            "Vulkan scheduler command alignment exceeds the command arena"
        );
        let header_offset = self
            .command_offset
            .next_multiple_of(align_of::<CommandHeader>());
        let payload_offset = header_offset
            .checked_add(size_of::<CommandHeader>())?
            .next_multiple_of(align_of::<T>());
        let end_offset = payload_offset.checked_add(size_of::<T>())?;
        (end_offset <= COMMAND_CHUNK_CAPACITY).then_some((
            header_offset,
            payload_offset,
            end_offset,
        ))
    }

    fn record<T>(&mut self, command: T) -> Result<(), T>
    where
        T: FnOnce(vk::CommandBuffer, vk::CommandBuffer) + Send + 'static,
    {
        let Some((header_offset, payload_offset, end_offset)) = self.command_layout::<T>() else {
            return Err(command);
        };
        let base = self.storage.0.as_mut_ptr().cast::<u8>();
        let header = CommandHeader {
            next: NO_COMMAND,
            payload_offset,
            execute: execute_command::<T>,
            drop_payload: drop_command::<T>,
        };
        unsafe {
            base.add(header_offset)
                .cast::<CommandHeader>()
                .write(header);
            base.add(payload_offset).cast::<T>().write(command);
            if self.last != NO_COMMAND {
                (*base.add(self.last).cast::<CommandHeader>()).next = header_offset;
            } else {
                self.first = header_offset;
            }
        }
        self.last = header_offset;
        self.command_offset = end_offset;
        Ok(())
    }

    fn pop_header(&mut self) -> Option<CommandHeader> {
        if self.first == NO_COMMAND {
            return None;
        }
        let header_offset = self.first;
        let header = unsafe {
            self.storage
                .0
                .as_ptr()
                .cast::<u8>()
                .add(header_offset)
                .cast::<CommandHeader>()
                .read()
        };
        self.first = header.next;
        if self.first == NO_COMMAND {
            self.last = NO_COMMAND;
        }
        Some(header)
    }

    fn execute_all(
        &mut self,
        cmdbuf: vk::CommandBuffer,
        upload_cmdbuf: vk::CommandBuffer,
    ) -> Option<SubmitRequest> {
        while let Some(header) = self.pop_header() {
            let payload = unsafe {
                self.storage
                    .0
                    .as_mut_ptr()
                    .cast::<u8>()
                    .add(header.payload_offset)
            };
            unsafe {
                (header.execute)(payload, cmdbuf, upload_cmdbuf);
            }
        }
        self.command_offset = 0;
        self.submit.take()
    }
}

impl Drop for CommandChunk {
    fn drop(&mut self) {
        while let Some(header) = self.pop_header() {
            let payload = unsafe {
                self.storage
                    .0
                    .as_mut_ptr()
                    .cast::<u8>()
                    .add(header.payload_offset)
            };
            unsafe {
                (header.drop_payload)(payload);
            }
        }
    }
}

unsafe fn execute_command<T>(
    payload: *mut u8,
    cmdbuf: vk::CommandBuffer,
    upload_cmdbuf: vk::CommandBuffer,
) where
    T: FnOnce(vk::CommandBuffer, vk::CommandBuffer),
{
    let command = unsafe { payload.cast::<T>().read() };
    command(cmdbuf, upload_cmdbuf);
}

unsafe fn drop_command<T>(payload: *mut u8) {
    unsafe {
        payload.cast::<T>().drop_in_place();
    }
}

struct SubmitRequest {
    signal_semaphore: vk::Semaphore,
    wait_semaphore: vk::Semaphore,
    tick: u64,
}

/// Port of upstream `Scheduler::State`.
#[derive(Default)]
struct SchedulerState {
    renderpass: vk::RenderPass,
    framebuffer: vk::Framebuffer,
    render_area: vk::Extent2D,
    graphics_pipeline: Option<NonNull<GraphicsPipeline>>,
    is_rescaling: bool,
    rescaling_defined: bool,
    needs_state_enable_refresh: bool,
    descriptor_buffer_chunk: u32,
    descriptor_buffer_bound: bool,
}

#[derive(Clone)]
struct DeferredClear {
    framebuffer: Option<RenderTargetFramebuffer>,
    color_clear_mask: u32,
    color_values: [vk::ClearValue; 8],
    depth_stencil: bool,
    depth_stencil_value: vk::ClearValue,
}

impl Default for DeferredClear {
    fn default() -> Self {
        Self {
            framebuffer: None,
            color_clear_mask: 0,
            color_values: [vk::ClearValue::default(); 8],
            depth_stencil: false,
            depth_stencil_value: vk::ClearValue::default(),
        }
    }
}

/// Command buffer scheduler with submission tracking.
///
/// Ref: Eden `Scheduler` — batches commands, tracks render pass state,
/// and submits to the GPU queue with tick-based synchronization.
pub struct Scheduler {
    device: ash::Device,
    transform_feedback_supported: bool,

    /// Port of upstream `Scheduler::master_semaphore`.
    master_semaphore: Arc<MasterSemaphore>,

    /// Current chunk being recorded to.
    current_chunk: CommandChunk,

    /// Upstream scheduler-local command-buffer state.
    state: SchedulerState,
    deferred_clear: DeferredClear,

    /// Port of upstream render-pass attachment tracking fields.
    num_renderpass_images: usize,
    renderpass_images: [vk::Image; NUM_RT + 1],
    renderpass_image_ranges: [vk::ImageSubresourceRange; NUM_RT + 1],

    /// Port of upstream `Scheduler::submit_mutex`.
    submit_mutex: Arc<Mutex<()>>,

    /// Port of upstream `Scheduler::on_submit`.
    on_submit: Arc<Mutex<Option<SubmitCallback>>>,

    /// Upstream `Scheduler` owns a `StateTracker&` and invalidates command
    /// buffer state after helper draws. Some Rust construction paths still
    /// build a scheduler before a rasterizer state tracker exists, so this is
    /// installed by the rasterizer once both owners are allocated.
    state_tracker: Option<NonNull<StateTracker>>,

    /// Safe Rust adaptation of upstream `Scheduler::query_cache`.
    ///
    /// The scheduler only needs these three independently locked pieces of the
    /// query cache. Keeping shared state handles preserves the upstream call
    /// ordering without constructing aliased `&mut QueryCache` references.
    samples_query_state: Option<Arc<parking_lot::Mutex<SamplesQueryState>>>,
    tfb_query_state: Option<Arc<parking_lot::Mutex<TfbCounterState>>>,
    query_runtime_state: Option<Arc<parking_lot::Mutex<QueryRuntimeState>>>,

    /// Port of upstream `Scheduler::WorkerThread`: owns command-buffer
    /// recording, command-pool rotation, and queue submission.
    worker: Option<Arc<SchedulerWorker>>,
    worker_thread: Option<std::thread::JoinHandle<()>>,

    frame_interval: Duration,
    start_time: Instant,
    last_target_fps: f64,
    max_frame_count: u64,
    frame_counter: u64,
}

struct SchedulerWorker {
    state: Mutex<SchedulerWorkerState>,
    job_cv: Condvar,
    drained_cv: Condvar,
    stop: std::sync::atomic::AtomicBool,
}

struct SchedulerWorkerState {
    chunks: VecDeque<CommandChunk>,
    chunk_reserve: Vec<CommandChunk>,
    in_flight: usize,
}

/// Stable synchronization subset of `Scheduler` used by Vulkan fences.
///
/// Upstream `InnerFence` stores a `Scheduler&`. The Rust rasterizer owns the
/// scheduler by value, so fences retain clones of only the scheduler-owned
/// synchronization objects instead of a pointer into a movable owner.
#[derive(Clone)]
pub(crate) struct SchedulerWaitHandle {
    master_semaphore: Arc<MasterSemaphore>,
}

impl SchedulerWaitHandle {
    pub(crate) fn wait(&self, tick: u64) {
        if tick == 0 {
            return;
        }
        self.master_semaphore.wait(tick);
    }
}

impl SchedulerWorkerState {
    fn is_drained(&self) -> bool {
        self.chunks.is_empty() && self.in_flight == 0
    }

    fn pop_front(&mut self) -> Option<CommandChunk> {
        let chunk = self.chunks.pop_front()?;
        self.in_flight += 1;
        Some(chunk)
    }
}

struct WorkerContext {
    device: ash::Device,
    device_fault: Option<vk::ExtDeviceFaultFn>,
    device_fault_reported: bool,
    master_semaphore: Arc<MasterSemaphore>,
    command_pool: CommandPool,
    current_cmdbuf: vk::CommandBuffer,
    upload_cmdbuf: vk::CommandBuffer,
    submit_mutex: Arc<Mutex<()>>,
    on_submit: Arc<Mutex<Option<SubmitCallback>>>,
}

impl SchedulerWorker {
    fn new() -> Self {
        Self {
            state: Mutex::new(SchedulerWorkerState {
                chunks: VecDeque::new(),
                chunk_reserve: Vec::new(),
                in_flight: 0,
            }),
            job_cv: Condvar::new(),
            drained_cv: Condvar::new(),
            stop: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn push(&self, chunk: CommandChunk) {
        self.state.lock().unwrap().chunks.push_back(chunk);
        self.job_cv.notify_one();
    }

    fn acquire_chunk(&self) -> CommandChunk {
        self.state
            .lock()
            .unwrap()
            .chunk_reserve
            .pop()
            .unwrap_or_else(CommandChunk::new)
    }

    fn wait_drained(&self) {
        let mut state = self.state.lock().unwrap();
        while !state.is_drained() {
            state = self.drained_cv.wait(state).unwrap();
        }
    }

    fn run(&self, mut context: WorkerContext) {
        common::thread::set_current_thread_priority(common::thread::ThreadPriority::Critical);
        loop {
            let chunk = {
                let mut state = self.state.lock().unwrap();
                loop {
                    if let Some(chunk) = state.pop_front() {
                        break Some(chunk);
                    }
                    if self.stop.load(Ordering::Acquire) {
                        break None;
                    }
                    state = self.job_cv.wait(state).unwrap();
                }
            };
            let Some(mut chunk) = chunk else {
                break;
            };

            let submit = chunk.execute_all(context.current_cmdbuf, context.upload_cmdbuf);
            if let Some(submit) = submit {
                if let Err(error) = context.submit_execution(&submit) {
                    log::error!(
                        "Vulkan worker failed to submit tick {}: {error:?}",
                        submit.tick
                    );
                    // Eden's `vk::Check` throws out of the worker entry point,
                    // which terminates the process. Do not continue recording
                    // against a failed queue submission.
                    std::process::abort();
                }
                if let Err(error) = context.allocate_worker_command_buffer() {
                    log::error!(
                        "Vulkan worker failed to rotate command buffers after tick {}: {error:?}",
                        submit.tick
                    );
                    std::process::abort();
                }
            }

            let mut state = self.state.lock().unwrap();
            state.in_flight -= 1;
            state.chunk_reserve.push(chunk);
            if state.is_drained() {
                self.drained_cv.notify_all();
            }
        }
    }
}

impl WorkerContext {
    fn report_device_fault(&mut self) {
        if self.device_fault_reported {
            return;
        }
        self.device_fault_reported = true;
        let Some(extension) = self.device_fault.as_ref() else {
            return;
        };
        let mut counts = vk::DeviceFaultCountsEXT::default();
        let first = unsafe {
            (extension.get_device_fault_info_ext)(
                self.device.handle(),
                &mut counts,
                std::ptr::null_mut(),
            )
        };
        if first != vk::Result::SUCCESS {
            log::error!("vkGetDeviceFaultInfoEXT count query failed: {first:?}");
            return;
        }
        let mut addresses =
            vec![vk::DeviceFaultAddressInfoEXT::default(); counts.address_info_count as usize];
        let mut vendors =
            vec![vk::DeviceFaultVendorInfoEXT::default(); counts.vendor_info_count as usize];
        let mut vendor_binary = vec![0u8; counts.vendor_binary_size as usize];
        let mut info = vk::DeviceFaultInfoEXT::default();
        info.p_address_infos = addresses.as_mut_ptr();
        info.p_vendor_infos = vendors.as_mut_ptr();
        info.p_vendor_binary_data = vendor_binary.as_mut_ptr().cast();
        let second = unsafe {
            (extension.get_device_fault_info_ext)(self.device.handle(), &mut counts, &mut info)
        };
        if second != vk::Result::SUCCESS {
            log::error!("vkGetDeviceFaultInfoEXT detail query failed: {second:?}");
            return;
        }
        let description = unsafe { std::ffi::CStr::from_ptr(info.description.as_ptr()) };
        log::error!(
            "Vulkan device fault: description={} addresses={} vendors={} vendor_binary_size={}",
            description.to_string_lossy(),
            counts.address_info_count,
            counts.vendor_info_count,
            counts.vendor_binary_size
        );
        for (index, address) in addresses.iter().enumerate() {
            log::error!(
                "Vulkan device fault address[{index}]: type={:?} reported=0x{:016X} precision=0x{:016X}",
                address.address_type,
                address.reported_address,
                address.address_precision
            );
        }
        for (index, vendor) in vendors.iter().enumerate() {
            let description = unsafe { std::ffi::CStr::from_ptr(vendor.description.as_ptr()) };
            log::error!(
                "Vulkan device fault vendor[{index}]: description={} code=0x{:016X} data=0x{:016X}",
                description.to_string_lossy(),
                vendor.vendor_fault_code,
                vendor.vendor_fault_data
            );
        }
    }

    fn allocate_worker_command_buffer(&mut self) -> Result<(), vk::Result> {
        self.current_cmdbuf = self.command_pool.commit();
        self.upload_cmdbuf = self.command_pool.commit();
        let begin_info = vk::CommandBufferBeginInfo::builder()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
            .build();
        unsafe {
            self.device
                .reset_command_buffer(self.current_cmdbuf, vk::CommandBufferResetFlags::empty())?;
            self.device
                .reset_command_buffer(self.upload_cmdbuf, vk::CommandBufferResetFlags::empty())?;
            self.device
                .begin_command_buffer(self.current_cmdbuf, &begin_info)?;
            self.device
                .begin_command_buffer(self.upload_cmdbuf, &begin_info)?;
        }
        Ok(())
    }

    fn submit_execution(&mut self, submit: &SubmitRequest) -> Result<(), vk::Result> {
        unsafe {
            let write_barrier = vk::MemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
                .build();
            self.device.cmd_pipeline_barrier(
                self.upload_cmdbuf,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[write_barrier],
                &[],
                &[],
            );
            self.device.end_command_buffer(self.upload_cmdbuf)?;
            self.device.end_command_buffer(self.current_cmdbuf)?;
        }

        let callback = self.on_submit.lock().unwrap().clone();
        if let Some(callback) = callback {
            callback();
        }

        let _submit_lock = self.submit_mutex.lock().unwrap();
        let result = self.master_semaphore.submit_queue(
            self.current_cmdbuf,
            self.upload_cmdbuf,
            submit.signal_semaphore,
            submit.wait_semaphore,
            submit.tick,
        );
        drop(_submit_lock);
        if result == vk::Result::ERROR_DEVICE_LOST {
            self.report_device_fault();
            crate::vulkan_common::vulkan_device::report_device_loss();
        }
        if result == vk::Result::SUCCESS {
            Ok(())
        } else {
            Err(result)
        }
    }
}

impl Scheduler {
    /// Create a new scheduler.
    pub fn new(
        device: ash::Device,
        queue: vk::Queue,
        graphics_family: u32,
        timeline_semaphore_supported: bool,
        synchronization2_core: bool,
        synchronization2_khr: Option<ash::extensions::khr::Synchronization2>,
        device_fault: Option<vk::ExtDeviceFaultFn>,
        transform_feedback_supported: bool,
    ) -> Result<Self, vk::Result> {
        if !timeline_semaphore_supported {
            log::warn!(
                "Scheduler: timeline semaphores unavailable; using the upstream fence fallback"
            );
        }

        let master_semaphore = Arc::new(MasterSemaphore::new(
            device.clone(),
            queue,
            timeline_semaphore_supported,
            synchronization2_core,
            synchronization2_khr,
        )?);
        let submit_mutex = Arc::new(Mutex::new(()));
        let on_submit = Arc::new(Mutex::new(None));
        let worker = Arc::new(SchedulerWorker::new());
        let mut worker_context = WorkerContext {
            device: device.clone(),
            device_fault,
            device_fault_reported: false,
            master_semaphore: Arc::clone(&master_semaphore),
            command_pool: CommandPool::new(
                Arc::clone(&master_semaphore),
                device.clone(),
                graphics_family,
            ),
            current_cmdbuf: vk::CommandBuffer::null(),
            upload_cmdbuf: vk::CommandBuffer::null(),
            submit_mutex: Arc::clone(&submit_mutex),
            on_submit: Arc::clone(&on_submit),
        };
        worker_context.allocate_worker_command_buffer()?;
        let thread_worker = Arc::clone(&worker);
        let worker_thread = std::thread::Builder::new()
            .name("VulkanWorker".into())
            .spawn(move || thread_worker.run(worker_context))
            .expect("Failed to spawn Vulkan scheduler worker");

        Ok(Self {
            device,
            transform_feedback_supported,
            master_semaphore,
            current_chunk: CommandChunk::new(),
            state: SchedulerState::default(),
            deferred_clear: DeferredClear::default(),
            num_renderpass_images: 0,
            renderpass_images: [vk::Image::null(); NUM_RT + 1],
            renderpass_image_ranges: [vk::ImageSubresourceRange::default(); NUM_RT + 1],
            submit_mutex,
            on_submit,
            state_tracker: None,
            samples_query_state: None,
            tfb_query_state: None,
            query_runtime_state: None,
            worker: Some(worker),
            worker_thread: Some(worker_thread),
            frame_interval: Duration::ZERO,
            start_time: Instant::now(),
            last_target_fps: 0.0,
            max_frame_count: 0,
            frame_counter: 0,
        })
    }

    pub fn submit_mutex(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.submit_mutex)
    }

    /// Port of upstream `Scheduler::RegisterOnSubmit`.
    pub(crate) fn register_on_submit(&mut self, callback: Option<SubmitCallback>) {
        *self.on_submit.lock().unwrap() = callback;
    }

    pub(crate) fn wait_handle(&self) -> SchedulerWaitHandle {
        SchedulerWaitHandle {
            master_semaphore: Arc::clone(&self.master_semaphore),
        }
    }

    pub fn set_state_tracker(&mut self, state_tracker: NonNull<StateTracker>) {
        self.state_tracker = Some(state_tracker);
    }

    pub(crate) fn set_samples_query_state(
        &mut self,
        state: Arc<parking_lot::Mutex<SamplesQueryState>>,
    ) {
        self.samples_query_state = Some(state);
    }

    pub(crate) fn set_query_runtime_state(
        &mut self,
        state: Arc<parking_lot::Mutex<QueryRuntimeState>>,
    ) {
        self.query_runtime_state = Some(state);
    }

    pub(crate) fn set_tfb_query_state(&mut self, state: Arc<parking_lot::Mutex<TfbCounterState>>) {
        self.tfb_query_state = Some(state);
    }

    /// Release the safe query-cache state handles before their Vulkan owners
    /// are destroyed by `RasterizerVulkan`.
    pub(crate) fn clear_query_cache_state(&mut self) {
        self.samples_query_state = None;
        self.tfb_query_state = None;
        self.query_runtime_state = None;
    }

    /// Record a command that only needs the render command buffer.
    pub fn record(&mut self, cmd: impl FnOnce(vk::CommandBuffer) + Send + 'static) {
        self.record_with_upload_buffer(move |render_cmd, _upload_cmd| cmd(render_cmd));
    }

    /// Record a command that needs both render and upload command buffers.
    pub fn record_with_upload_buffer(
        &mut self,
        cmd: impl FnOnce(vk::CommandBuffer, vk::CommandBuffer) + Send + 'static,
    ) {
        let command = match self.current_chunk.record(cmd) {
            Ok(()) => return,
            Err(command) => command,
        };
        self.dispatch_work();
        if self.current_chunk.record(command).is_err() {
            panic!("Vulkan scheduler command exceeds the 32 KiB command chunk");
        }
    }

    /// Begin a render pass if not already inside one with matching parameters.
    /// Port of `Scheduler::RequestRenderpass(const Framebuffer*)`.
    pub fn request_renderpass(&mut self, framebuffer: &RenderTargetFramebuffer) {
        if self
            .deferred_clear
            .framebuffer
            .as_ref()
            .is_some_and(|pending| pending == framebuffer)
        {
            self.realize_deferred_clear();
            return;
        }
        let render_area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: framebuffer.render_area(),
        };
        if self.state.renderpass == framebuffer.render_pass()
            && self.state.framebuffer == framebuffer.handle()
            && self.state.render_area.width == render_area.extent.width
            && self.state.render_area.height == render_area.extent.height
        {
            return;
        }
        // Ends any active pass and realizes a deferred clear.
        self.end_render_pass();
        self.begin_render_pass_impl(
            framebuffer.handle(),
            framebuffer.render_pass(),
            render_area,
            &[],
            framebuffer.num_images(),
            framebuffer.images(),
            framebuffer.image_ranges(),
        );
    }

    /// Port of `Scheduler::DeferColorClear`.
    pub fn defer_color_clear(
        &mut self,
        framebuffer: &RenderTargetFramebuffer,
        rt_slot: u32,
        value: vk::ClearValue,
    ) -> bool {
        if self.is_render_pass_active() {
            return false;
        }
        if self
            .deferred_clear
            .framebuffer
            .as_ref()
            .is_some_and(|pending| pending != framebuffer)
        {
            self.realize_deferred_clear();
            self.end_render_pass();
        }
        self.deferred_clear.framebuffer = Some(framebuffer.clone());
        self.deferred_clear.color_clear_mask |= 1 << rt_slot;
        self.deferred_clear.color_values[rt_slot as usize] = value;
        true
    }

    /// Port of `Scheduler::DeferDepthStencilClear`.
    pub fn defer_depth_stencil_clear(
        &mut self,
        framebuffer: &RenderTargetFramebuffer,
        value: vk::ClearValue,
    ) -> bool {
        if self.is_render_pass_active() {
            return false;
        }
        if self
            .deferred_clear
            .framebuffer
            .as_ref()
            .is_some_and(|pending| pending != framebuffer)
        {
            self.realize_deferred_clear();
            self.end_render_pass();
        }
        self.deferred_clear.framebuffer = Some(framebuffer.clone());
        self.deferred_clear.depth_stencil = true;
        self.deferred_clear.depth_stencil_value = value;
        true
    }

    /// Port of `Scheduler::RealizeDeferredClear`.
    fn realize_deferred_clear(&mut self) {
        let deferred = std::mem::take(&mut self.deferred_clear);
        let Some(framebuffer) = deferred.framebuffer else {
            return;
        };
        let mut clear_values = [vk::ClearValue::default(); NUM_RT + 1];
        let mut clear_value_count = 0;
        let base = framebuffer.render_pass_key_base();
        for slot in 0..8 {
            if base.color_formats[slot] != crate::surface::PixelFormat::Invalid {
                clear_values[clear_value_count] = deferred.color_values[slot];
                clear_value_count += 1;
            }
        }
        if base.depth_format != crate::surface::PixelFormat::Invalid {
            clear_values[clear_value_count] = deferred.depth_stencil_value;
            clear_value_count += 1;
        }
        let color_discard_mask = if framebuffer.discards_msaa_color() {
            deferred.color_clear_mask
        } else {
            0
        };
        let renderpass = framebuffer
            .render_pass_variant(
                deferred.color_clear_mask,
                deferred.depth_stencil,
                color_discard_mask,
            )
            .expect("failed to create deferred-clear render-pass variant");
        self.end_render_pass();
        self.begin_render_pass_impl(
            framebuffer.handle(),
            renderpass,
            vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: framebuffer.render_area(),
            },
            &clear_values[..clear_value_count],
            framebuffer.num_images(),
            framebuffer.images(),
            framebuffer.image_ranges(),
        );
    }

    pub fn request_renderpass_raw(
        &mut self,
        framebuffer: vk::Framebuffer,
        renderpass: vk::RenderPass,
        render_area: vk::Rect2D,
        clear_values: &[vk::ClearValue],
        images: &[vk::Image],
        image_ranges: &[vk::ImageSubresourceRange],
    ) {
        if self.state.renderpass == renderpass
            && self.state.framebuffer == framebuffer
            && self.state.render_area.width == render_area.extent.width
            && self.state.render_area.height == render_area.extent.height
        {
            return;
        }
        self.end_render_pass();
        self.begin_render_pass_impl(
            framebuffer,
            renderpass,
            render_area,
            clear_values,
            images.len(),
            images,
            image_ranges,
        );
    }

    /// Port of `Scheduler::BeginRenderPassImpl`.
    fn begin_render_pass_impl(
        &mut self,
        framebuffer: vk::Framebuffer,
        renderpass: vk::RenderPass,
        render_area: vk::Rect2D,
        clear_values: &[vk::ClearValue],
        num_images: usize,
        images: &[vk::Image],
        image_ranges: &[vk::ImageSubresourceRange],
    ) {
        trace!("Scheduler: beginning render pass");
        let device = self.device.clone();
        assert!(clear_values.len() <= NUM_RT + 1);
        assert!(num_images <= NUM_RT + 1);
        assert!(images.len() >= num_images);
        assert!(image_ranges.len() >= num_images);
        let mut values = [vk::ClearValue::default(); NUM_RT + 1];
        values[..clear_values.len()].copy_from_slice(clear_values);
        let clear_value_count = clear_values.len();

        // Match upstream ordering: publish the active render pass before
        // recording its begin command, then populate the attachment arrays.
        self.state.renderpass = renderpass;
        self.state.framebuffer = framebuffer;
        self.state.render_area = render_area.extent;
        self.record(move |cmdbuf| unsafe {
            let rp_begin = vk::RenderPassBeginInfo::builder()
                .render_pass(renderpass)
                .framebuffer(framebuffer)
                .render_area(render_area)
                .clear_values(&values[..clear_value_count])
                .build();
            device.cmd_begin_render_pass(cmdbuf, &rp_begin, vk::SubpassContents::INLINE);
        });

        let mut renderpass_images = [vk::Image::null(); NUM_RT + 1];
        let mut renderpass_image_ranges = [vk::ImageSubresourceRange::default(); NUM_RT + 1];
        renderpass_images[..num_images].copy_from_slice(&images[..num_images]);
        renderpass_image_ranges[..num_images].copy_from_slice(&image_ranges[..num_images]);
        self.num_renderpass_images = num_images;
        self.renderpass_images = renderpass_images;
        self.renderpass_image_ranges = renderpass_image_ranges;
    }

    /// End the current render pass if inside one.
    pub fn request_outside_render_pass_operation_context(&mut self) {
        self.end_render_pass();
    }

    fn end_render_pass(&mut self) {
        self.realize_deferred_clear();
        if self.state.renderpass == vk::RenderPass::null() {
            return;
        }

        trace!("Scheduler: ending render pass");
        // Preserve upstream `Scheduler::EndRenderPass` ordering without
        // re-entering the query-cache owner through an aliased `&mut`.
        if let Some(state) = self.tfb_query_state.as_ref().cloned() {
            state.lock().close_counter(self);
        }
        if let Some(state) = self.samples_query_state.as_ref().cloned() {
            state.lock().pause_counter(self);
        }
        if let Some(state) = self.query_runtime_state.as_ref().cloned() {
            let conditional_rendering = state.lock().pause_host_conditional_rendering();
            if let Some(conditional_rendering) = conditional_rendering {
                self.record(move |cmdbuf| unsafe {
                    (conditional_rendering.cmd_end_conditional_rendering_ext)(cmdbuf);
                });
            }
        }
        let num_images = self.num_renderpass_images;
        let images = self.renderpass_images;
        let image_ranges = self.renderpass_image_ranges;
        let transform_feedback_supported = self.transform_feedback_supported;
        let device = self.device.clone();
        self.record(move |cmdbuf| unsafe {
            device.cmd_end_render_pass(cmdbuf);
            let mut barriers = [vk::ImageMemoryBarrier::default(); NUM_RT + 1];
            for index in 0..num_images {
                let range = image_ranges[index];
                let is_color = range.aspect_mask.contains(vk::ImageAspectFlags::COLOR);
                let is_depth_stencil = range
                    .aspect_mask
                    .intersects(vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL);
                let src_access_mask = if is_color {
                    vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                } else if is_depth_stencil {
                    vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE
                } else {
                    vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                        | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE
                };
                barriers[index] = vk::ImageMemoryBarrier::builder()
                    .src_access_mask(src_access_mask)
                    .dst_access_mask(
                        vk::AccessFlags::SHADER_READ
                            | vk::AccessFlags::SHADER_WRITE
                            | vk::AccessFlags::COLOR_ATTACHMENT_READ
                            | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                            | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ
                            | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
                    )
                    .old_layout(vk::ImageLayout::GENERAL)
                    .new_layout(vk::ImageLayout::GENERAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(images[index])
                    .subresource_range(range)
                    .build();
            }
            device.cmd_pipeline_barrier(
                cmdbuf,
                vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS
                    | vk::PipelineStageFlags::LATE_FRAGMENT_TESTS
                    | vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                PIPELINE_STAGE_GRAPHICS_COMPUTE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &barriers[..num_images],
            );
            if transform_feedback_supported {
                let xfb_output_barrier = vk::MemoryBarrier::builder()
                    .src_access_mask(vk::AccessFlags::TRANSFORM_FEEDBACK_WRITE_EXT)
                    .dst_access_mask(
                        vk::AccessFlags::VERTEX_ATTRIBUTE_READ | vk::AccessFlags::TRANSFER_READ,
                    )
                    .build();
                device.cmd_pipeline_barrier(
                    cmdbuf,
                    vk::PipelineStageFlags::TRANSFORM_FEEDBACK_EXT,
                    vk::PipelineStageFlags::VERTEX_INPUT | vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[xfb_output_barrier],
                    &[],
                    &[],
                );
            }
        });
        self.state.renderpass = vk::RenderPass::null();
        self.num_renderpass_images = 0;
    }

    /// Whether we are currently inside a render pass.
    pub fn is_render_pass_active(&self) -> bool {
        self.state.renderpass != vk::RenderPass::null()
    }

    /// Port of upstream `Scheduler::UpdateGraphicsPipeline`.
    pub fn update_graphics_pipeline(&mut self, pipeline: Option<&GraphicsPipeline>) -> bool {
        let uses_extended_dynamic_state = pipeline
            .as_ref()
            .is_some_and(|pipeline| pipeline.uses_extended_dynamic_state());
        let pipeline = pipeline.map(NonNull::from);
        Self::update_graphics_pipeline_state(
            &mut self.state,
            self.state_tracker,
            pipeline,
            uses_extended_dynamic_state,
        )
    }

    fn update_graphics_pipeline_state(
        state: &mut SchedulerState,
        mut state_tracker: Option<NonNull<StateTracker>>,
        pipeline: Option<NonNull<GraphicsPipeline>>,
        uses_extended_dynamic_state: bool,
    ) -> bool {
        let pipeline_is_present = pipeline.is_some();
        if state.graphics_pipeline == pipeline {
            if pipeline_is_present
                && uses_extended_dynamic_state
                && state.needs_state_enable_refresh
            {
                if let Some(state_tracker) = state_tracker.as_mut() {
                    unsafe {
                        state_tracker.as_mut().invalidate_state_enable_flag();
                    }
                }
                state.needs_state_enable_refresh = false;
            }
            return false;
        }
        state.graphics_pipeline = pipeline;

        if !pipeline_is_present {
            return true;
        }

        if !uses_extended_dynamic_state {
            state.needs_state_enable_refresh = true;
        } else if state.needs_state_enable_refresh {
            if let Some(state_tracker) = state_tracker.as_mut() {
                unsafe {
                    state_tracker.as_mut().invalidate_state_enable_flag();
                }
            }
            state.needs_state_enable_refresh = false;
        }
        true
    }

    /// Port of upstream `Scheduler::UpdateRescaling`.
    pub fn update_rescaling(&mut self, is_rescaling: bool) -> bool {
        if self.state.rescaling_defined && self.state.is_rescaling == is_rescaling {
            return false;
        }
        self.state.rescaling_defined = true;
        self.state.is_rescaling = is_rescaling;
        true
    }

    /// Port of upstream `Scheduler::UpdateDescriptorBufferChunk`.
    pub fn update_descriptor_buffer_chunk(&mut self, descriptor_chunk: u32) -> bool {
        if self.state.descriptor_buffer_bound
            && descriptor_chunk == self.state.descriptor_buffer_chunk
        {
            return false;
        }
        self.state.descriptor_buffer_bound = true;
        self.state.descriptor_buffer_chunk = descriptor_chunk;
        true
    }

    /// Port of upstream `Scheduler::InvalidateState`.
    pub fn invalidate_state(&mut self) {
        self.state.graphics_pipeline = None;
        self.state.rescaling_defined = false;
        self.state.descriptor_buffer_bound = false;
        if let Some(mut state_tracker) = self.state_tracker {
            unsafe {
                state_tracker.as_mut().invalidate_command_buffer_state();
            }
        }
    }

    /// Port of upstream `Scheduler::DispatchWork`.
    pub fn dispatch_work(&mut self) {
        if self.current_chunk.is_empty() {
            return;
        }

        let worker = self.worker.as_ref().expect("scheduler worker must exist");
        let next_chunk = worker.acquire_chunk();
        let chunk = std::mem::replace(&mut self.current_chunk, next_chunk);
        worker.push(chunk);
    }

    /// Port of upstream `Scheduler::WaitWorker`.
    ///
    pub fn wait_worker(&mut self) {
        self.dispatch_work();
        if let Some(worker) = self.worker.as_ref() {
            worker.wait_drained();
        }
    }

    /// Flush — end render pass, dispatch remaining work, submit to GPU, return tick.
    pub fn flush(&mut self) -> u64 {
        self.flush_with_semaphores(vk::Semaphore::null(), vk::Semaphore::null())
    }

    /// Port of upstream `Scheduler::Flush(vk::Semaphore signal_semaphore)`.
    pub fn flush_with_signal(&mut self, signal_semaphore: vk::Semaphore) -> u64 {
        self.flush_with_semaphores(signal_semaphore, vk::Semaphore::null())
    }

    /// Full port of upstream `Scheduler::Flush(signal_semaphore, wait_semaphore)`.
    pub fn flush_with_semaphores(
        &mut self,
        signal_semaphore: vk::Semaphore,
        wait_semaphore: vk::Semaphore,
    ) -> u64 {
        let tick = self.submit_execution(signal_semaphore, wait_semaphore);
        self.allocate_new_context();
        debug!("Scheduler: flushed at tick {}", tick);
        tick
    }

    /// Port of upstream `Scheduler::SubmitExecution`.
    fn submit_execution(
        &mut self,
        signal_semaphore: vk::Semaphore,
        wait_semaphore: vk::Semaphore,
    ) -> u64 {
        self.end_pending_operations();
        self.invalidate_state();
        let tick = self.master_semaphore.next_tick();
        self.current_chunk.submit = Some(SubmitRequest {
            signal_semaphore,
            wait_semaphore,
            tick,
        });
        self.dispatch_work();
        tick
    }

    /// Port of upstream's currently empty `Scheduler::AllocateNewContext`.
    fn allocate_new_context(&mut self) {}

    /// Port of upstream `Scheduler::EndPendingOperations`.
    fn end_pending_operations(&mut self) {
        if let Some(state) = self.samples_query_state.as_ref().cloned() {
            state.lock().reset_counter(self);
        }
        self.request_outside_render_pass_operation_context();
    }

    /// Flush + wait for GPU completion.
    pub fn finish(&mut self) {
        self.finish_with_semaphores(vk::Semaphore::null(), vk::Semaphore::null());
    }

    /// Full port of upstream `Scheduler::Finish(signal_semaphore, wait_semaphore)`.
    pub fn finish_with_semaphores(
        &mut self,
        signal_semaphore: vk::Semaphore,
        wait_semaphore: vk::Semaphore,
    ) {
        let presubmit_tick = self.current_tick();
        self.submit_execution(signal_semaphore, wait_semaphore);
        self.wait(presubmit_tick);
        self.allocate_new_context();
    }

    /// Get the current tick value.
    pub fn current_tick(&self) -> u64 {
        self.master_semaphore.current_tick()
    }

    /// Last tick the GPU has fully completed.
    ///
    /// Port of upstream `MasterSemaphore::KnownGpuTick`. Delayed-destruction
    /// rings must retire against this value, not against the submission tick:
    /// with pipelined submissions the CPU-side tick runs ahead of the GPU.
    pub fn known_gpu_tick(&self) -> u64 {
        self.master_semaphore.known_gpu_tick()
    }

    /// Port of upstream `Scheduler::GetMasterSemaphore`.
    pub fn get_master_semaphore(&self) -> &Arc<MasterSemaphore> {
        &self.master_semaphore
    }

    /// Returns true when the GPU has completed `tick`.
    ///
    /// Port of upstream `Scheduler::IsFree` through its owned
    /// `MasterSemaphore`.
    pub fn is_free(&self, tick: u64) -> bool {
        self.master_semaphore.is_free(tick)
    }

    /// Tick that will be signalled by the next `Flush`.
    pub fn pending_tick(&self) -> u64 {
        self.current_tick()
    }

    /// Port of upstream `Scheduler::Wait`.
    pub fn wait(&mut self, tick: u64) {
        self.wait_with_frame_pacing(tick, 0.0);
    }

    /// Eden `Scheduler::Wait`, including its optional target-FPS pacing.
    pub fn wait_with_frame_pacing(&mut self, tick: u64, target_fps: f64) {
        if tick > 0 && tick >= self.current_tick() {
            // The tick has not been submitted yet; flush so it will signal.
            self.flush();
        }
        if tick > 0 {
            self.master_semaphore.wait(tick);
        }

        if *common::settings::values().use_speed_limit.get_value() && target_fps > 0.0 {
            let now = Instant::now();
            if self.last_target_fps != target_fps {
                self.frame_interval = Duration::from_secs_f64(1.0 / target_fps);
                self.max_frame_count = (0.1 * target_fps) as u64;
                self.last_target_fps = target_fps;
                self.frame_counter = 0;
                self.start_time = now;
            }
            self.frame_counter += 1;
            let target_time =
                self.start_time + self.frame_interval.mul_f64(self.frame_counter as f64);
            if target_time >= now {
                let sleep_time = target_time.duration_since(now);
                if sleep_time > Duration::from_millis(2) {
                    std::thread::sleep(sleep_time - Duration::from_millis(1));
                }
                while Instant::now() < target_time {
                    std::thread::yield_now();
                }
            } else if self.frame_counter > self.max_frame_count {
                self.frame_counter = 0;
                self.start_time = now;
            }
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.wait_drained();
            worker.stop.store(true, Ordering::Release);
            worker.job_cv.notify_all();
            if let Some(handle) = self.worker_thread.take() {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ash::vk::Handle;
    use std::sync::atomic::AtomicU64;

    #[test]
    fn test_command_chunk_new_is_empty() {
        let chunk = CommandChunk::new();
        assert!(chunk.is_empty());
    }

    #[test]
    fn scheduler_state_defaults_match_upstream() {
        let state = SchedulerState::default();
        assert_eq!(state.renderpass, vk::RenderPass::null());
        assert_eq!(state.framebuffer, vk::Framebuffer::null());
        assert_eq!(state.render_area, vk::Extent2D::default());
        assert!(state.graphics_pipeline.is_none());
        assert!(!state.rescaling_defined);
        assert!(!state.needs_state_enable_refresh);
        assert!(!state.descriptor_buffer_bound);
    }

    #[test]
    fn scheduler_worker_is_not_drained_while_chunk_is_in_flight() {
        let mut state = SchedulerWorkerState {
            chunks: VecDeque::new(),
            chunk_reserve: Vec::new(),
            in_flight: 1,
        };
        assert!(!state.is_drained());

        state.in_flight = 0;
        assert!(state.is_drained());
    }

    #[test]
    fn command_chunk_executes_commands_in_record_order() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut chunk = CommandChunk::new();
        for value in [3, 1, 4] {
            let order = Arc::clone(&order);
            assert!(chunk
                .record(move |_, _| {
                    order.lock().unwrap().push(value);
                })
                .is_ok());
        }

        let submit = chunk.execute_all(vk::CommandBuffer::null(), vk::CommandBuffer::null());

        assert!(submit.is_none());
        assert_eq!(*order.lock().unwrap(), [3, 1, 4]);
    }

    #[test]
    fn command_chunk_drops_unexecuted_commands_once() {
        struct DropProbe(Arc<AtomicU64>);

        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        let drops = Arc::new(AtomicU64::new(0));
        {
            let mut chunk = CommandChunk::new();
            let probe = DropProbe(Arc::clone(&drops));
            assert!(chunk
                .record(move |_, _| {
                    std::hint::black_box(&probe);
                })
                .is_ok());
            assert_eq!(drops.load(Ordering::Relaxed), 0);
        }
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn command_chunk_reuses_arena_after_execution() {
        let executions = Arc::new(AtomicU64::new(0));
        let mut chunk = CommandChunk::new();
        for expected in 1..=2 {
            let command_executions = Arc::clone(&executions);
            assert!(chunk
                .record(move |_, _| {
                    command_executions.fetch_add(1, Ordering::Relaxed);
                })
                .is_ok());
            chunk.execute_all(vk::CommandBuffer::null(), vk::CommandBuffer::null());
            assert_eq!(executions.load(Ordering::Relaxed), expected);
            assert_eq!(chunk.command_offset, 0);
        }
    }

    #[test]
    fn command_chunk_preserves_command_alignment() {
        #[repr(align(64))]
        struct AlignedCapture(Arc<AtomicU64>);

        let executions = Arc::new(AtomicU64::new(0));
        let capture = AlignedCapture(Arc::clone(&executions));
        let mut chunk = CommandChunk::new();
        assert!(chunk
            .record(move |_, _| {
                capture.0.fetch_add(1, Ordering::Relaxed);
            })
            .is_ok());

        chunk.execute_all(vk::CommandBuffer::null(), vk::CommandBuffer::null());
        assert_eq!(executions.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn worker_queue_pops_chunks_fifo_and_tracks_in_flight() {
        let mut first = CommandChunk::new();
        first.submit = Some(SubmitRequest {
            signal_semaphore: vk::Semaphore::null(),
            wait_semaphore: vk::Semaphore::null(),
            tick: 7,
        });
        let mut second = CommandChunk::new();
        second.submit = Some(SubmitRequest {
            signal_semaphore: vk::Semaphore::null(),
            wait_semaphore: vk::Semaphore::null(),
            tick: 8,
        });
        let mut state = SchedulerWorkerState {
            chunks: VecDeque::from([first, second]),
            chunk_reserve: Vec::new(),
            in_flight: 0,
        };

        let first = state.pop_front().unwrap();
        assert_eq!(first.submit.as_ref().unwrap().tick, 7);
        assert_eq!(state.in_flight, 1);
        let second = state.pop_front().unwrap();
        assert_eq!(second.submit.as_ref().unwrap().tick, 8);
        assert_eq!(state.in_flight, 2);
    }

    #[test]
    fn submit_request_preserves_both_external_semaphores() {
        let signal = vk::Semaphore::from_raw(0x51);
        let wait = vk::Semaphore::from_raw(0x72);
        let mut chunk = CommandChunk::new();
        chunk.submit = Some(SubmitRequest {
            signal_semaphore: signal,
            wait_semaphore: wait,
            tick: 9,
        });

        let submit = chunk
            .execute_all(vk::CommandBuffer::null(), vk::CommandBuffer::null())
            .unwrap();

        assert_eq!(submit.signal_semaphore, signal);
        assert_eq!(submit.wait_semaphore, wait);
        assert_eq!(submit.tick, 9);
    }

    #[test]
    fn graphics_pipeline_transitions_refresh_state_enable_like_upstream() {
        let mut static_storage = MaybeUninit::<GraphicsPipeline>::uninit();
        let mut dynamic_storage = MaybeUninit::<GraphicsPipeline>::uninit();
        let static_pipeline = NonNull::new(static_storage.as_mut_ptr()).unwrap();
        let dynamic_pipeline = NonNull::new(dynamic_storage.as_mut_ptr()).unwrap();
        let mut state = SchedulerState::default();
        let mut state_tracker = StateTracker::new();
        let tracker = Some(NonNull::from(&mut state_tracker));

        assert!(Scheduler::update_graphics_pipeline_state(
            &mut state,
            tracker,
            Some(static_pipeline),
            false,
        ));
        assert!(state.needs_state_enable_refresh);
        assert!(!state_tracker.touch_state_enable());

        assert!(!Scheduler::update_graphics_pipeline_state(
            &mut state,
            tracker,
            Some(static_pipeline),
            false,
        ));
        assert!(state.needs_state_enable_refresh);

        assert!(Scheduler::update_graphics_pipeline_state(
            &mut state,
            tracker,
            Some(dynamic_pipeline),
            true,
        ));
        assert!(!state.needs_state_enable_refresh);
        assert!(state_tracker.touch_state_enable());

        state.needs_state_enable_refresh = true;
        assert!(!Scheduler::update_graphics_pipeline_state(
            &mut state,
            tracker,
            Some(dynamic_pipeline),
            true,
        ));
        assert!(!state.needs_state_enable_refresh);
        assert!(state_tracker.touch_state_enable());

        state.needs_state_enable_refresh = true;
        assert!(Scheduler::update_graphics_pipeline_state(
            &mut state, tracker, None, false,
        ));
        assert!(state.needs_state_enable_refresh);
    }

    #[test]
    fn command_chunk_rejects_a_command_past_upstream_capacity() {
        let first = {
            let payload = [0u8; 0x5000];
            move |_, _| {
                std::hint::black_box(payload);
            }
        };
        let second = {
            let payload = [0u8; 0x5000];
            move |_, _| {
                std::hint::black_box(payload);
            }
        };
        let mut chunk = CommandChunk::new();

        assert!(chunk.record(first).is_ok());
        assert!(chunk.record(second).is_err());
    }
}

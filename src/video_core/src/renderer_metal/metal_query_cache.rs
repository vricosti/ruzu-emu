// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal query ownership.
//!
//! This is the Metal counterpart of Eden's
//! `renderer_vulkan/vk_query_cache.{h,cpp}`. Metal visibility counters are
//! written to one shared result buffer. Each draw uses a distinct aligned
//! slot, so reports can sum all draws since the matching counter reset without
//! touching storage that is still in flight.
//! Report sums are resolved incrementally on GPU. Fence callbacks retain one
//! immutable, shared-storage result instead of summing visibility banks on CPU.

use std::ptr::NonNull;
use std::sync::Arc;

use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLBlitCommandEncoder, MTLBuffer, MTLRenderCommandEncoder, MTLRenderPassDescriptor,
    MTLVisibilityResultMode,
};
use thiserror::Error;

use common::settings;

use crate::buffer_cache::buffer_cache_base::{
    BufferCacheAsyncBuffer, BufferCopy, ObtainBufferOperation, ObtainBufferSynchronize,
};
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::memory_manager::MemoryManager;
use crate::query_cache::query_base::{GuestQuery, QueryBase, QueryFlagBits};
use crate::query_cache::query_cache::{DeviceMemoryWriter, GpuAddressTranslator, RenderConditionState, SyncValuesStruct};
use crate::query_cache::query_cache_base::{QueryCacheBase, QueryLocation};
use crate::query_cache::query_stream::{SimpleStreamer, StreamerInterface, StreamerInterfaceBase};
use crate::query_cache::types::{ComparisonMode, QueryPropertiesFlags, QueryType};
use crate::renderer_base::GpuTicksGetter;

use super::metal_buffer::{MetalBuffer, MetalBufferError};
use super::metal_buffer_cache::MetalCommonBufferCache;
use super::metal_compute_pass::{
    ConditionalRenderingResolvePass, MetalComputePassError, VisibilityResolvePass,
};
use super::metal_device::MetalDevice;
use super::metal_scheduler::{MetalScheduler, MetalSchedulerError};
use super::metal_staging_buffer_pool::{MetalStagingBufferError, MetalStagingBufferPool};

const QUERY_SLOT_SIZE: usize = std::mem::size_of::<u64>();
// Metal limits visibility-result offsets to 256 KiB minus one 64-bit result.
const QUERY_SLOT_COUNT: usize = (256 * 1024) / QUERY_SLOT_SIZE;

#[derive(Debug, Error)]
pub enum MetalQueryCacheError {
    #[error("invalid query synchronization range")]
    SyncRange,
    #[error(transparent)]
    Staging(#[from] MetalStagingBufferError),
    #[error(transparent)]
    Compute(#[from] MetalComputePassError),
    #[error(transparent)]
    Buffer(#[from] MetalBufferError),
    #[error(transparent)]
    Scheduler(#[from] MetalSchedulerError),
}

/// Counterpart of vk_query_cache.cpp::HostSyncValues: value stays on GPU.
pub struct HostSyncValues {
    pub address: u64,
    pub size: u64,
    pub offset: u64,
}

/// Rust counterpart of the SyncValues template's two input record types.
pub trait QuerySyncValue {
    const GENERATES_BASE_BUFFER: bool;
    fn address(&self) -> u64;
    fn size(&self) -> u64;
    fn value_or_offset(&self) -> u64;
}

impl QuerySyncValue for SyncValuesStruct {
    const GENERATES_BASE_BUFFER: bool = true;
    fn address(&self) -> u64 {
        self.address
    }
    fn size(&self) -> u64 {
        self.size
    }
    fn value_or_offset(&self) -> u64 {
        self.value
    }
}

impl QuerySyncValue for HostSyncValues {
    const GENERATES_BASE_BUFFER: bool = false;
    fn address(&self) -> u64 {
        self.address
    }
    fn size(&self) -> u64 {
        self.size
    }
    fn value_or_offset(&self) -> u64 {
        self.offset
    }
}

/// Native owner corresponding to Vulkan::QueryCacheRuntime and its Impl.
/// Scheduler/staging owners must remain at their construction addresses, as for
/// Metal's BufferCacheRuntime. No CPU fence callback accesses this runtime.
pub struct QueryCacheRuntime {
    scheduler: NonNull<MetalScheduler>,
    staging_pool: NonNull<MetalStagingBufferPool>,
    little_cache: Vec<(u64, u64)>,
    redirect_cache: Vec<usize>,
    buffers_to_upload_to: Vec<(Arc<MetalBuffer>, u32)>,
    copies_setup: Vec<Vec<BufferCopy>>,
    conditional_resolve_pass: ConditionalRenderingResolvePass,
    hcr_resolve_buffer: Arc<MetalBuffer>,
    hcr_setup: Option<MetalConditionalRendering>,
    is_hcr_running: bool,
}

/// Native counterpart of VkConditionalRenderingBeginInfoEXT. Consumers record
/// indirect-argument masking, rather than opening a Vulkan conditional region.
#[derive(Clone)]
pub struct MetalConditionalRendering {
    pub buffer: Arc<MetalBuffer>,
    pub offset: usize,
    pub inverted: bool,
    /// Only the private resolve buffer is inaccessible to guest shader writes.
    pub(crate) private_resolve: bool,
}

impl QueryCacheRuntime {
    /// # Safety
    /// Both services must remain at these addresses and outlive this runtime.
    /// Calls must be serialized with all other users of these same services.
    pub unsafe fn new(
        device: &MetalDevice,
        scheduler: &mut MetalScheduler,
        staging_pool: &mut MetalStagingBufferPool,
    ) -> Result<Self, MetalQueryCacheError> {
        Ok(Self {
            scheduler: NonNull::from(scheduler),
            staging_pool: NonNull::from(staging_pool),
            little_cache: Vec::new(),
            redirect_cache: Vec::new(),
            buffers_to_upload_to: Vec::new(),
            copies_setup: Vec::new(),
            conditional_resolve_pass: ConditionalRenderingResolvePass::new(device)?,
            hcr_resolve_buffer: Arc::new(MetalBuffer::new_private(device, 4)?),
            hcr_setup: None,
            is_hcr_running: false,
        })
    }

    pub fn end_host_conditional_rendering(&mut self) {
        self.pause_host_conditional_rendering();
        self.hcr_setup = None;
    }

    pub fn pause_host_conditional_rendering(&mut self) {
        self.is_hcr_running = false;
    }

    pub fn resume_host_conditional_rendering(&mut self) {
        self.is_hcr_running = self.hcr_setup.is_some();
    }

    pub fn active_conditional_rendering(&self) -> Option<MetalConditionalRendering> {
        self.hcr_setup
            .as_ref()
            .filter(|_| self.is_hcr_running)
            .cloned()
    }

    /// Compare an authoritative buffer-cache value against zero or its partner
    /// at +16. The owner retains a dedicated predicate, not a recyclable staging
    /// slot. Ordered GPU consumers must capture it before the next resolve.
    pub fn host_conditional_rendering_compare_bc_impl(
        &mut self,
        buffer_cache: &mut MetalCommonBufferCache,
        address: u64,
        is_equal: bool,
        compare_to_zero: bool,
    ) -> Result<(), MetalQueryCacheError> {
        let size = if compare_to_zero { 8 } else { 24 };
        if address % 4 != 0 || address.checked_add(size as u64).is_none() {
            self.end_host_conditional_rendering();
            return Err(MetalQueryCacheError::SyncRange);
        }
        let (source, offset) = {
            let mutex = Arc::clone(&buffer_cache.mutex);
            let _guard = mutex.lock();
            let (id, offset) = buffer_cache.obtain_cpu_buffer(
                address,
                size,
                ObtainBufferSynchronize::FullSynchronize,
                ObtainBufferOperation::DoNothing,
            );
            (
                buffer_cache
                    .backend_buffer(id)
                    .expect("obtained conditional buffer")
                    .handle(),
                offset,
            )
        };
        let was_running = self.is_hcr_running;
        self.pause_host_conditional_rendering();
        if let Err(error) = self.conditional_resolve_pass.resolve(
            unsafe { self.scheduler.as_mut() },
            &self.hcr_resolve_buffer,
            0,
            &source,
            offset as usize,
            compare_to_zero,
        ) {
            self.end_host_conditional_rendering();
            return Err(error.into());
        }
        self.hcr_setup = Some(MetalConditionalRendering {
            buffer: Arc::clone(&self.hcr_resolve_buffer),
            offset: 0,
            inverted: !is_equal,
            private_resolve: true,
        });
        if was_running {
            self.resume_host_conditional_rendering();
        }
        Ok(())
    }

    /// Eden's zero-versus-query fast path consumes the low 32-bit word directly
    /// (the native indirect masker, like EXT conditional rendering, reads u32).
    pub fn host_conditional_rendering_compare_value_impl(
        &mut self,
        buffer_cache: &mut MetalCommonBufferCache,
        address: u64,
        is_equal: bool,
    ) -> Result<(), MetalQueryCacheError> {
        if address % 4 != 0 || address.checked_add(8).is_none() {
            self.end_host_conditional_rendering();
            return Err(MetalQueryCacheError::SyncRange);
        }
        let (buffer, offset) = {
            let mutex = Arc::clone(&buffer_cache.mutex);
            let _guard = mutex.lock();
            let (id, offset) = buffer_cache.obtain_cpu_buffer(
                address,
                8,
                ObtainBufferSynchronize::FullSynchronize,
                ObtainBufferOperation::DoNothing,
            );
            (
                buffer_cache
                    .backend_buffer(id)
                    .expect("obtained conditional buffer")
                    .handle(),
                offset,
            )
        };
        // Preserve running/paused state. Unlike Eden's handle+offset early-out,
        // always refresh inversion: changing equality at the same address must
        // not retain the previous condition.
        self.hcr_setup = Some(MetalConditionalRendering {
            buffer,
            offset: offset as usize,
            inverted: is_equal,
            private_resolve: false,
        });
        Ok(())
    }

    /// Port of QueryCacheRuntime::SyncValues. The caller supplies device, not
    /// GPU virtual, addresses. FullSynchronize preserves surrounding CPU bytes;
    /// DoNothing matches upstream: query writes have their own dirty tracking.
    pub fn sync_values<T: QuerySyncValue>(
        &mut self,
        buffer_cache: &mut MetalCommonBufferCache,
        values: &[T],
        base_src_buffer: Option<&ProtocolObject<dyn MTLBuffer>>,
    ) -> Result<(), MetalQueryCacheError> {
        if values.is_empty() {
            return Ok(());
        }
        self.little_cache.clear();
        self.redirect_cache.clear();
        let mut total_size = 0usize;
        for value in values {
            let size = value.size();
            if !matches!(size, 4 | 8)
                || value.address() % 4 != 0
                || value.address().checked_add(size).is_none()
            {
                return Err(MetalQueryCacheError::SyncRange);
            }
            if !T::GENERATES_BASE_BUFFER
                && base_src_buffer.is_none_or(|buffer| {
                    value.value_or_offset() % 4 != 0
                        || value
                            .value_or_offset()
                            .checked_add(size)
                            .is_none_or(|end| end > buffer.length() as u64)
                })
            {
                return Err(MetalQueryCacheError::SyncRange);
            }
            total_size = total_size
                .checked_add(size as usize)
                .ok_or(MetalQueryCacheError::SyncRange)?;
            let base = value.address() & !4095;
            // Include the second page if a 64-bit report crosses a page boundary.
            // Eden assumes its typed query destinations fit the first page.
            let end = value
                .address()
                .checked_add(size)
                .and_then(|end| end.checked_add(4095))
                .map(|end| end & !4095)
                .ok_or(MetalQueryCacheError::SyncRange)?;
            let mut found = None;
            for (index, range) in self.little_cache.iter_mut().enumerate() {
                if base <= range.1 && range.0 <= end {
                    range.0 = range.0.min(base);
                    range.1 = range.1.max(end);
                    found = Some(index);
                    break;
                }
            }
            let index = found.unwrap_or_else(|| {
                self.little_cache.push((base, end));
                self.little_cache.len() - 1
            });
            self.redirect_cache.push(index);
        }
        if self
            .little_cache
            .iter()
            .any(|&(start, end)| end - start > u32::MAX as u64)
        {
            return Err(MetalQueryCacheError::SyncRange);
        }
        let mutex = Arc::clone(&buffer_cache.mutex);
        let _guard = mutex.lock();
        buffer_cache.buffer_operations(|cache| {
            self.buffers_to_upload_to.clear();
            for &(start, end) in &self.little_cache {
                let (id, offset) = cache.obtain_cpu_buffer(
                    start,
                    (end - start) as u32,
                    ObtainBufferSynchronize::FullSynchronize,
                    ObtainBufferOperation::DoNothing,
                );
                self.buffers_to_upload_to.push((
                    cache.backend_buffer(id).expect("obtained buffer").handle(),
                    offset,
                ));
            }
        });

        // Borrow the scheduler only after BufferOperations: the buffer runtime
        // uses this same scheduler to record cache uploads/merges above.
        let scheduler = unsafe { self.scheduler.as_mut() };
        let mut staging = if T::GENERATES_BASE_BUFFER {
            Some(
                unsafe { self.staging_pool.as_mut() }
                    .request_upload_buffer(scheduler, total_size, false)?,
            )
        } else {
            None
        };
        self.copies_setup.clear();
        self.copies_setup
            .resize_with(self.little_cache.len(), Vec::new);
        let mut accumulated_size = 0usize;
        for (index, value) in values.iter().enumerate() {
            let which = self.redirect_cache[index];
            let src_offset = if let Some(staging) = staging.as_mut() {
                staging.mapped_span_mut()
                    [accumulated_size..accumulated_size + value.size() as usize]
                    .copy_from_slice(
                        &value.value_or_offset().to_ne_bytes()[..value.size() as usize],
                    );
                staging.offset() + accumulated_size as u64
            } else {
                value.value_or_offset()
            };
            self.copies_setup[which].push(BufferCopy {
                src_offset,
                dst_offset: self.buffers_to_upload_to[which].1 as u64 + value.address()
                    - self.little_cache[which].0,
                size: value.size(),
            });
            accumulated_size += value.size() as usize;
        }
        let source = staging
            .as_ref()
            .map(|value| value.buffer.handle())
            .or(base_src_buffer)
            .ok_or(MetalQueryCacheError::SyncRange)?;
        scheduler.request_outside_render_pass_operation_context();
        scheduler.with_blit_encoder(|encoder| {
            for ((destination, _), copies) in
                self.buffers_to_upload_to.iter().zip(&self.copies_setup)
            {
                for copy in copies {
                    destination.mark_content_range_modified(copy.dst_offset, u64::from(copy.size));
                    unsafe {
                        encoder.copyFromBuffer_sourceOffset_toBuffer_destinationOffset_size(
                            source,
                            copy.src_offset as usize,
                            destination.handle(),
                            copy.dst_offset as usize,
                            copy.size as usize,
                        );
                    }
                }
            }
        })?;
        Ok(())
    }
}

pub type MetalQueryOperation = Box<dyn FnOnce() + Send>;

/// Scoped device-address writer; never stored in the retained report graph.
struct QueryDeviceMemoryWriter<'a>(&'a MaxwellDeviceMemoryManager);

impl DeviceMemoryWriter for QueryDeviceMemoryWriter<'_> {
    fn write_u32(&mut self, address: u64, value: u32) {
        self.0.write_u32(address, value);
    }

    fn write_u64(&mut self, address: u64, value: u64) {
        self.0.write_u64(address, value);
    }
}

pub enum MetalQueryReport {
    Complete,
    SignalFence(MetalQueryOperation),
    /// Host-produced values must remain deferred even when the configured
    /// fence behavior executes the signal callback before GPU completion.
    SignalFenceAfterCompletion(MetalQueryOperation),
    SyncOperation(MetalQueryOperation),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PayloadReportAction {
    Immediate,
    SignalFence,
    SyncOperation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetalVisibilityQuery {
    offset: usize,
}

impl MetalVisibilityQuery {
    pub fn offset(self) -> usize {
        self.offset
    }
}

struct MetalVisibilityBank {
    buffer: MetalBuffer,
    offsets: Vec<usize>,
}

struct MetalVisibilityBufferHandle {
    buffer: objc2::rc::Retained<ProtocolObject<dyn MTLBuffer>>,
}

// SAFETY: Metal buffers are thread-safe Objective-C resources. The fence
// callback only reads a shared-storage buffer after its command buffer has
// completed; it neither records commands nor accesses a live encoder.
unsafe impl Send for MetalVisibilityBufferHandle {}

impl MetalVisibilityBufferHandle {
    /// The owning scheduler tick must have completed before reading shared data.
    unsafe fn value_after_completion(&self) -> u64 {
        unsafe { self.buffer.contents().as_ptr().cast::<u64>().read() }
    }
}

/// Native report storage corresponding to the backend's host query objects.
/// The final GPU result is retained until the report's fence callback completes.
struct MetalReportQuery {
    base: QueryBase,
    result: Option<MetalVisibilityBufferHandle>,
}

struct MetalReportStreamer {
    queries: SimpleStreamer<MetalReportQuery>,
    pending_sync: Vec<usize>,
}

impl MetalReportStreamer {
    fn new(query_type: QueryType) -> Self {
        Self {
            queries: SimpleStreamer::new(query_type as usize),
            pending_sync: Vec::new(),
        }
    }

    /// Native GuestStreamer::SyncWrites / SamplesStreamer::SyncWrites adapter.
    /// The caller holds the buffer-cache and report locks in that order.
    fn sync_writes(
        &mut self,
        runtime: &mut QueryCacheRuntime,
        buffer_cache: &mut MetalCommonBufferCache,
    ) -> Result<(), MetalQueryCacheError> {
        let mut guest_values = Vec::new();
        let mut guest_ids = Vec::new();
        for &id in &self.pending_sync {
            let query = self.queries.get_query_mut(id).expect("pending sync slot");
            if query.base.flags.intersects(
                QueryFlagBits::IS_REWRITTEN
                    | QueryFlagBits::IS_INVALIDATED
                    | QueryFlagBits::IS_HOST_SYNCED,
            ) {
                continue;
            }
            if let Some(result) = &query.result {
                // SamplesQueryBank::QUERY_SIZE is always 8, independently of
                // the guest report's timestamp/CPU write width.
                runtime.sync_values(
                    buffer_cache,
                    &[HostSyncValues {
                        address: query.base.guest_address,
                        size: 8,
                        offset: 0,
                    }],
                    Some(&result.buffer),
                )?;
                query.base.flags.insert(QueryFlagBits::IS_HOST_SYNCED);
            } else {
                guest_values.push(SyncValuesStruct {
                    address: query.base.guest_address,
                    value: query.base.value,
                    size: if query.base.flags.contains(QueryFlagBits::HAS_TIMESTAMP) {
                        8
                    } else {
                        4
                    },
                });
                guest_ids.push(id);
            }
        }
        runtime.sync_values(buffer_cache, &guest_values, None)?;
        for id in guest_ids {
            self.queries
                .get_query_mut(id)
                .expect("pending guest sync slot")
                .base
                .flags
                .insert(QueryFlagBits::IS_HOST_SYNCED);
        }
        self.pending_sync.clear();
        Ok(())
    }
}

impl StreamerInterface for MetalReportStreamer {
    fn get_query(&self, id: usize) -> Option<&QueryBase> {
        self.queries.get_query(id).map(|query| &query.base)
    }

    fn get_query_mut(&mut self, id: usize) -> Option<&mut QueryBase> {
        self.queries.get_query_mut(id).map(|query| &mut query.base)
    }

    fn write_counter(
        &mut self,
        address: u64,
        has_timestamp: bool,
        value: u32,
        _subreport: Option<u32>,
    ) -> usize {
        let id = self.queries.build_query(MetalReportQuery {
            base: GuestQuery::new(has_timestamp, address, value as u64).base,
            result: None,
        });
        self.pending_sync.push(id);
        id
    }

    fn free(&mut self, query_id: usize) {
        self.pending_sync.retain(|&id| id != query_id);
        self.queries
            .get_query_mut(query_id)
            .expect("live report slot")
            .result = None;
        self.queries.free(query_id);
    }

    fn base(&self) -> &StreamerInterfaceBase {
        &self.queries.base
    }
    fn base_mut(&mut self) -> &mut StreamerInterfaceBase {
        &mut self.queries.base
    }
}

/// Shared QueryCacheBase indexing with stable backend-owned streamer objects.
/// Access, including fence callbacks, is serialized by MetalQueryCache::reports.
struct MetalQueryReports {
    base: Box<QueryCacheBase>,
    payload: Box<MetalReportStreamer>,
    samples: Box<MetalReportStreamer>,
    pending_gpu_reports: bool,
}

// SAFETY: The base's bound raw pointers refer to itself and its boxed streamers
// owned here. No rasterizer/runtime/channel pointer is installed or exposed. All access
// is under the reports mutex, and callbacks retain that same owner. MTLBuffer
// handles are thread-safe; shared contents are read only after fence completion.
unsafe impl Send for MetalQueryReports {}

impl MetalQueryReports {
    fn new() -> Self {
        let mut reports = Self {
            base: Box::new(QueryCacheBase::new()),
            payload: Box::new(MetalReportStreamer::new(QueryType::Payload)),
            samples: Box::new(MetalReportStreamer::new(QueryType::ZPassPixelCount64)),
            pending_gpu_reports: false,
        };
        reports.base.impl_.owner = Some(reports.base.as_mut() as *mut QueryCacheBase);
        reports
            .base
            .impl_
            .register_streamer(QueryType::Payload as usize, reports.payload.as_mut());
        reports.base.impl_.register_streamer(
            QueryType::ZPassPixelCount64 as usize,
            reports.samples.as_mut(),
        );
        reports
    }

    fn insert(
        &mut self,
        address: u64,
        flags: QueryPropertiesFlags,
        value: u64,
        result: Option<MetalVisibilityBufferHandle>,
    ) -> QueryLocation {
        let streamer = if result.is_some() {
            &mut self.samples
        } else {
            &mut self.payload
        };
        let id = streamer.write_counter(
            address,
            flags.contains(QueryPropertiesFlags::HAS_TIMEOUT),
            value as u32,
            None,
        );
        let location = QueryLocation::new(streamer.get_id() as u32, id as u32);
        let query = streamer.queries.get_query_mut(id).expect("new report slot");
        if flags.contains(QueryPropertiesFlags::IS_A_FENCE) {
            query.base.flags.insert(QueryFlagBits::IS_FENCE);
        }
        if result.is_some() {
            self.pending_gpu_reports = true;
            query
                .base
                .flags
                .remove(QueryFlagBits::IS_FINAL_VALUE_SYNCED);
            query.base.flags.insert(QueryFlagBits::IS_HOST_MANAGED);
        }
        query.result = result;
        self.base.cache_query_location(address, location);
        location
    }

    /// Called with the reports mutex held and only after the owning GPU fence.
    fn complete(
        &mut self,
        location: QueryLocation,
        device_memory: &MaxwellDeviceMemoryManager,
        gpu_ticks_getter: Option<GpuTicksGetter>,
    ) {
        let streamer = if location.stream_id() == QueryType::Payload as usize {
            &mut self.payload
        } else {
            &mut self.samples
        };
        let query = streamer
            .queries
            .get_query_mut(location.query_id())
            .expect("pending report slot");
        if !query.base.flags.contains(QueryFlagBits::IS_INVALIDATED) {
            if let Some(result) = &query.result {
                query.base.value = unsafe { result.value_after_completion() };
                query
                    .base
                    .flags
                    .insert(QueryFlagBits::IS_FINAL_VALUE_SYNCED);
            }
            // CounterReport checks invalidation, but not IsRewritten: older
            // ordered writes still complete without evicting the newer entry.
            if query.base.flags.contains(QueryFlagBits::HAS_TIMESTAMP) {
                let ticks = gpu_ticks_getter.map_or(0, |getter| getter());
                device_memory.write_u64(query.base.guest_address.wrapping_add(8), ticks);
                device_memory.write_u64(query.base.guest_address, query.base.value);
            } else {
                device_memory.write_u32(query.base.guest_address, query.base.value as u32);
            }
        }
        self.base.impl_.pending_unregister.push(location);
    }
}

pub struct MetalQueryCache {
    device: MetalDevice,
    visibility_buffer: MetalBuffer,
    next_slot: usize,
    zpass_slots: Vec<usize>,
    completed_zpass_banks: Vec<MetalVisibilityBank>,
    visibility_resolve: VisibilityResolvePass,
    resolved_slots: usize,
    resolved_value: Option<Arc<MetalBuffer>>,
    reports: Arc<parking_lot::Mutex<MetalQueryReports>>,
}

impl MetalQueryCache {
    pub fn new(device: &MetalDevice) -> Result<Self, MetalQueryCacheError> {
        Ok(Self {
            device: device.clone(),
            visibility_buffer: MetalBuffer::new(device, QUERY_SLOT_COUNT * QUERY_SLOT_SIZE)?,
            next_slot: 0,
            zpass_slots: Vec::new(),
            completed_zpass_banks: Vec::new(),
            visibility_resolve: VisibilityResolvePass::new(device)?,
            resolved_slots: 0,
            resolved_value: None,
            reports: Arc::new(parking_lot::Mutex::new(MetalQueryReports::new())),
        })
    }

    pub fn reset_counter(&mut self, query_type: u32) {
        if is_zpass_query(query_type) {
            self.zpass_slots.clear();
            self.completed_zpass_banks.clear();
            self.resolved_slots = 0;
            self.resolved_value = None;
        }
    }

    pub fn prepare_draw(
        &mut self,
        _scheduler: &mut MetalScheduler,
        zpass_enabled: bool,
    ) -> Result<Option<MetalVisibilityQuery>, MetalQueryCacheError> {
        if !zpass_enabled {
            return Ok(None);
        }
        if self.next_slot >= QUERY_SLOT_COUNT {
            let next_buffer = MetalBuffer::new(&self.device, QUERY_SLOT_COUNT * QUERY_SLOT_SIZE)?;
            let completed_buffer = std::mem::replace(&mut self.visibility_buffer, next_buffer);
            self.completed_zpass_banks.push(MetalVisibilityBank {
                buffer: completed_buffer,
                offsets: std::mem::take(&mut self.zpass_slots),
            });
            self.next_slot = 0;
        }
        let offset = self.next_slot * QUERY_SLOT_SIZE;
        self.next_slot += 1;
        unsafe {
            self.visibility_buffer
                .contents_ptr()
                .add(offset)
                .cast::<u64>()
                .write(0);
        }
        self.zpass_slots.push(offset);
        Ok(Some(MetalVisibilityQuery { offset }))
    }

    pub fn attach_render_pass(&self, descriptor: &MTLRenderPassDescriptor) {
        descriptor.setVisibilityResultBuffer(Some(self.visibility_buffer.handle()));
    }

    pub fn visibility_result_buffer_identity(&self) -> usize {
        let buffer: *const ProtocolObject<dyn objc2_metal::MTLBuffer> =
            self.visibility_buffer.handle();
        buffer.cast::<()>() as usize
    }

    pub fn configure_draw(
        encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
        query: Option<MetalVisibilityQuery>,
    ) {
        let (mode, offset) = query.map_or((MTLVisibilityResultMode::Disabled, 0), |query| {
            (MTLVisibilityResultMode::Counting, query.offset())
        });
        encoder.setVisibilityResultMode_offset(mode, offset);
    }

    pub fn report(
        &mut self,
        scheduler: &mut MetalScheduler,
        memory_manager: Option<std::sync::Arc<parking_lot::Mutex<MemoryManager>>>,
        gpu_ticks_getter: Option<GpuTicksGetter>,
        gpu_addr: u64,
        query_type: u32,
        flags: QueryPropertiesFlags,
        payload: u32,
    ) -> Result<MetalQueryReport, MetalQueryCacheError> {
        // QueryCacheBase::CounterReport translates before queuing its callback.
        // Retain the device owner, not the channel's mutable GPU address space.
        let destination = memory_manager.as_ref().and_then(|manager| {
            let manager = manager.lock();
            manager
                .gpu_to_cpu_address(gpu_addr)
                .map(|address| (Arc::clone(manager.device_memory()), address))
        });
        let Some((device_memory, device_addr)) = destination else {
            return Ok(MetalQueryReport::Complete);
        };
        let action = if query_type == QueryType::Payload as u32 {
            let gpu_level_high = settings::is_gpu_level_high(&settings::values());
            payload_report_action(flags, gpu_level_high)
        } else if flags.contains(QueryPropertiesFlags::IS_A_FENCE) {
            PayloadReportAction::SignalFence
        } else {
            PayloadReportAction::SyncOperation
        };
        if action == PayloadReportAction::Immediate {
            let operation = query_write_operation(
                device_memory,
                gpu_ticks_getter,
                device_addr,
                payload as u64,
                flags.contains(QueryPropertiesFlags::HAS_TIMEOUT),
            );
            operation();
            return Ok(MetalQueryReport::Complete);
        }

        let result = if is_zpass_query(query_type) {
            let result = MetalVisibilityBufferHandle {
                buffer: self
                    .resolve_visibility_counter(scheduler)?
                    .retained_handle(),
            };
            // Even a reused result needs a real queue fence rather than a stub
            // when its earlier producer may still be executing on the GPU.
            scheduler.active_command_buffer()?;
            Some(result)
        } else {
            None
        };
        let reports = self.reports.clone();
        let location = reports.lock().insert(
            device_addr,
            flags,
            self.query_value(query_type, payload),
            result,
        );
        let operation: MetalQueryOperation = Box::new(move || {
            reports
                .lock()
                .complete(location, &device_memory, gpu_ticks_getter);
        });
        Ok(if action == PayloadReportAction::SignalFence {
            if is_zpass_query(query_type) {
                MetalQueryReport::SignalFenceAfterCompletion(operation)
            } else {
                MetalQueryReport::SignalFence(operation)
            }
        } else {
            MetalQueryReport::SyncOperation(operation)
        })
    }

    pub fn invalidate_region(&self, address: u64, size: usize) {
        self.reports.lock().base.invalidate_region(address, size);
    }

    /// Returns whether the caller must ReleaseFences, after this report lock
    /// has been dropped. Final values use the shared SemiFlushQueryDirty rules.
    pub fn flush_region(
        &self,
        address: u64,
        size: usize,
        device_memory: &MaxwellDeviceMemoryManager,
    ) -> bool {
        self.reports.lock().base.flush_region_with_memory(
            address,
            size,
            &mut QueryDeviceMemoryWriter(device_memory),
        )
    }

    /// Native counterpart of QueryCacheBase::AccelerateHostConditionalRendering.
    /// State/translation are borrowed from the current channel only for this
    /// call; the retained report graph never owns a channel/runtime pointer.
    /// Consumers must all honor the resulting setup before advertising this
    /// through RasterizerInterface::accelerate_conditional_rendering.
    pub fn accelerate_host_conditional_rendering(
        &self,
        runtime: &mut QueryCacheRuntime,
        buffer_cache: &mut MetalCommonBufferCache,
        memory: &dyn GpuAddressTranslator,
        state: RenderConditionState,
    ) -> Result<bool, MetalQueryCacheError> {
        if state.override_mode != 0
            || matches!(
                state.comparison_mode,
                ComparisonMode::True | ComparisonMode::False
            )
        {
            runtime.end_host_conditional_rendering();
            return Ok(false);
        }
        let compare_to_zero = state.comparison_mode == ComparisonMode::Conditional;
        let size = if compare_to_zero { 8 } else { 24 };
        // The range overload searches for a mapped page and drops the byte
        // offset; it is not a contiguous-range translation API. Resolve the
        // exact endpoints of this at-most-24-byte record instead.
        let address = memory.gpu_to_cpu_address(state.address);
        let last = state
            .address
            .checked_add(size - 1)
            .and_then(|last| memory.gpu_to_cpu_address(last));
        let Some(address) = address.filter(|address| address.checked_add(size - 1) == last) else {
            runtime.end_host_conditional_rendering();
            return Ok(false);
        };
        let buffer_mutex = Arc::clone(&buffer_cache.mutex);
        let _buffer_guard = buffer_mutex.lock();
        let reports = self.reports.lock();
        let first = reports.base.lookup_query_for_conditional_rendering(address);
        let second = if compare_to_zero {
            None
        } else {
            reports
                .base
                .lookup_query_for_conditional_rendering(address + 16)
        };
        // Metadata references cannot escape this guard. Failed SyncValues must
        // never make a pending host result appear ready in the buffer cache.
        for query in [first, second].into_iter().flatten() {
            if query
                .flags
                .intersects(QueryFlagBits::IS_INVALIDATED | QueryFlagBits::IS_REWRITTEN)
                || !query.flags.contains(QueryFlagBits::IS_HOST_SYNCED)
            {
                runtime.end_host_conditional_rendering();
                return Ok(false);
            }
        }
        if compare_to_zero && first.is_none()
            && !buffer_cache.is_region_gpu_modified(address, size as usize)
        {
            // Metal must otherwise mask indirect arguments in compute before
            // every draw. For a CPU-owned condition, retain Maxwell's ordinary
            // synchronized ReadBlock evaluation instead. GPU-written buffers
            // and cached reports still require the ordered GPU predicate.
            runtime.end_host_conditional_rendering();
            return Ok(false);
        }
        if !compare_to_zero {
            let mut qc_dirty = false;
            let mut in_bc = false;
            let mut any_cached = false;
            for (address, query) in [(address, first), (address + 16, second)] {
                if let Some(query) = query {
                    any_cached = true;
                    qc_dirty |= query.flags.contains(QueryFlagBits::IS_HOST_MANAGED)
                        && !query.flags.contains(QueryFlagBits::IS_GUEST_SYNCED);
                } else if buffer_cache.is_region_gpu_modified(address, 8) {
                    any_cached = true;
                    in_bc = true;
                }
            }
            if !any_cached || (!qc_dirty && !in_bc) {
                runtime.end_host_conditional_rendering();
                return Ok(false);
            }
        }
        // Native Metal can resolve the full pair without a CPU read or the
        // Vulkan low-accuracy/driver-specific unconditional-render shortcuts.
        runtime.host_conditional_rendering_compare_bc_impl(
            buffer_cache,
            address,
            state.comparison_mode != ComparisonMode::IfNotEqual,
            compare_to_zero,
        )?;
        Ok(true)
    }

    pub fn notify_wfi(
        &self,
        runtime: &mut QueryCacheRuntime,
        buffer_cache: &mut MetalCommonBufferCache,
    ) -> Result<(), MetalQueryCacheError> {
        let buffer_mutex = Arc::clone(&buffer_cache.mutex);
        let _buffer_guard = buffer_mutex.lock();
        let mut reports = self.reports.lock();
        // QueryCacheBase::ForEachStreamer visits Payload before Samples.
        // Native report reductions already provide PresyncWrites' GPU source;
        // SyncValues encodes ordered blits after those compute/render producers.
        reports.payload.sync_writes(runtime, buffer_cache)?;
        reports.samples.sync_writes(runtime, buffer_cache)?;
        Ok(())
    }

    /// CounterReport completion sentences slots; CommitAsyncFlushes schedules
    /// their retirement as an ordered fence operation, as in QueryCacheBase.
    pub fn commit_async_flushes(&self) -> MetalQueryOperation {
        let reports = self.reports.clone();
        {
            let mut state = reports.lock();
            let needs_completion = std::mem::take(&mut state.pending_gpu_reports);
            let mask = if needs_completion {
                1u64 << QueryType::ZPassPixelCount64 as u32
            } else {
                0
            };
            state.base.impl_.flushes_pending.push_back(mask);
        }
        Box::new(move || reports.lock().base.unregister_pending())
    }

    /// Keep query state alive for FenceManager's ShouldWaitAsyncFlushes and
    /// PopAsyncFlushes callbacks without capturing a movable rasterizer.
    pub fn async_flush_callbacks(&self) -> (impl FnMut() -> bool, impl FnMut() + Send + 'static) {
        let should_wait = self.reports.clone();
        let pop = self.reports.clone();
        (
            move || should_wait.lock().base.should_wait_async_flushes(),
            move || {
                pop.lock()
                    .base
                    .impl_
                    .flushes_pending
                    .pop_front()
                    .expect("query flush per queued fence");
            },
        )
    }

    fn query_value(&self, query_type: u32, payload: u32) -> u64 {
        if query_type == QueryType::Payload as u32 {
            return payload as u64;
        }
        if query_type == QueryType::StreamingPrimitivesNeededMinusSucceeded as u32 {
            return 0;
        }
        1
    }

    /// Native counterpart of SamplesStreamer accumulation before query sync.
    /// Resolve only slots added since the last report; earlier result buffers
    /// remain immutable for fence callbacks and conditional-render consumers.
    pub(super) fn resolve_visibility_counter(
        &mut self,
        scheduler: &mut MetalScheduler,
    ) -> Result<Arc<MetalBuffer>, MetalQueryCacheError> {
        let mut result = if let Some(value) = &self.resolved_value {
            value.clone()
        } else {
            let zero = Arc::new(MetalBuffer::new(&self.device, 8)?);
            zero.write(0, &[0; 8])?;
            zero
        };
        let mut skip = self.resolved_slots;
        let mut added = 0;
        let banks = self
            .completed_zpass_banks
            .iter()
            .map(|bank| (&bank.buffer, bank.offsets.as_slice()))
            .chain(std::iter::once((
                &self.visibility_buffer,
                self.zpass_slots.as_slice(),
            )));
        for (buffer, offsets) in banks {
            let consumed = skip.min(offsets.len());
            skip -= consumed;
            let offsets = &offsets[consumed..];
            if offsets.is_empty() {
                continue;
            }
            debug_assert!(offsets
                .windows(2)
                .all(|pair| pair[1] == pair[0] + QUERY_SLOT_SIZE));
            result = self.visibility_resolve.run(
                scheduler,
                buffer,
                offsets[0],
                offsets.len(),
                &result,
            )?;
            added += offsets.len();
        }
        self.resolved_slots += added;
        self.resolved_value = Some(result.clone());
        Ok(result)
    }
}

fn payload_report_action(flags: QueryPropertiesFlags, gpu_level_high: bool) -> PayloadReportAction {
    if flags.contains(QueryPropertiesFlags::IS_A_FENCE) {
        PayloadReportAction::SignalFence
    } else if gpu_level_high {
        PayloadReportAction::SyncOperation
    } else {
        PayloadReportAction::Immediate
    }
}

fn query_write_operation(
    device_memory: Arc<MaxwellDeviceMemoryManager>,
    gpu_ticks_getter: Option<GpuTicksGetter>,
    device_addr: u64,
    value: u64,
    has_timestamp: bool,
) -> MetalQueryOperation {
    Box::new(move || {
        if has_timestamp {
            let ticks = gpu_ticks_getter.map_or(0, |getter| getter());
            device_memory.write_u64(device_addr.wrapping_add(8), ticks);
            device_memory.write_u64(device_addr, value);
        } else {
            device_memory.write_u32(device_addr, value as u32);
        }
    })
}

fn is_zpass_query(query_type: u32) -> bool {
    query_type == QueryType::ZPassPixelCount as u32
        || query_type == QueryType::ZPassPixelCount64 as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    struct QuerySyncMemory(Arc<Vec<u8>>);
    impl crate::buffer_cache::buffer_cache_base::DeviceMemoryAccess for QuerySyncMemory {
        fn get_pointer(&self, address: u64) -> Option<*const u8> {
            self.0.get(address as usize).map(|byte| byte as *const u8)
        }
        fn read_block_unsafe(&self, address: u64, dst: &mut [u8]) {
            dst.copy_from_slice(&self.0[address as usize..address as usize + dst.len()]);
        }
        fn write_block_unsafe(&self, _: u64, _: &[u8]) {
            panic!("query GPU synchronization must not write guest memory");
        }
    }

    struct QuerySyncFixture {
        cache: MetalCommonBufferCache,
        runtime: QueryCacheRuntime,
        staging: Box<MetalStagingBufferPool>,
        scheduler: Box<MetalScheduler>,
        device: MetalDevice,
        _channel: Box<crate::control::channel_state::ChannelState>,
        _tracker: Box<MaxwellDeviceMemoryManager>,
    }

    impl QuerySyncFixture {
        fn new() -> Self {
            let device = MetalDevice::new().unwrap();
            let mut scheduler = Box::new(MetalScheduler::new(&device));
            let mut staging = Box::new(MetalStagingBufferPool::new(&device).unwrap());
            let buffer_runtime = super::super::metal_buffer_cache::BufferCacheRuntime::new(
                &device,
                &mut scheduler,
                &mut staging,
            );
            let tracker = Box::new(MaxwellDeviceMemoryManager::default());
            let mut cache = MetalCommonBufferCache::new(&tracker, buffer_runtime);
            cache.set_device_memory(Box::new(QuerySyncMemory(Arc::new(vec![0xa5; 0x40000]))));
            let channel = Box::new(crate::control::channel_state::ChannelState::new(1));
            cache.create_channel(&channel);
            cache.bind_to_channel(1);
            // The fixture owns stable boxes, dropped after the runtime/cache.
            let runtime =
                unsafe { QueryCacheRuntime::new(&device, &mut scheduler, &mut staging) }.unwrap();
            Self {
                cache,
                runtime,
                staging,
                scheduler,
                device,
                _channel: channel,
                _tracker: tracker,
            }
        }

        fn read_gpu(&mut self, address: u64, size: u32) -> Vec<u8> {
            let (id, offset) = self.cache.obtain_cpu_buffer(
                address,
                size,
                ObtainBufferSynchronize::NoSynchronize,
                ObtainBufferOperation::DoNothing,
            );
            let buffer = self.cache.backend_buffer(id).unwrap().handle();
            let readback = MetalBuffer::new(&self.device, size as usize).unwrap();
            buffer
                .encode_copy(
                    &mut self.scheduler,
                    &readback,
                    offset as usize,
                    0,
                    size as usize,
                )
                .unwrap();
            self.scheduler.finish_all().unwrap();
            let mut bytes = vec![0; size as usize];
            readback.read(0, &mut bytes).unwrap();
            bytes
        }
    }

    fn conditional_channel_memory() -> MemoryManager {
        let mut memory = MemoryManager::new_with_geometry_and_device_memory(
            1,
            Arc::new(MaxwellDeviceMemoryManager::default()),
            32,
            0x1_0000_0000,
            16,
            12,
        );
        memory.map(0x20000, 0x8000, 0x1000, 0, false);
        memory
    }

    #[test]
    fn conditional_lookup_translates_channel_and_requires_synced_reports() {
        let mut f = QuerySyncFixture::new();
        let cache = MetalQueryCache::new(&f.device).unwrap();
        let memory = conditional_channel_memory();
        let upload = MetalBuffer::new(&f.device, 8).unwrap();
        upload.write(0, &6u64.to_ne_bytes()).unwrap();
        let result = MetalBuffer::new(&f.device, 8).unwrap();
        upload
            .encode_copy(&mut f.scheduler, &result, 0, 0, 8)
            .unwrap();
        let flags = QueryPropertiesFlags::IS_A_FENCE | QueryPropertiesFlags::HAS_TIMEOUT;
        let location = cache.reports.lock().insert(
            0x8100,
            flags,
            0,
            Some(MetalVisibilityBufferHandle {
                buffer: result.retained_handle(),
            }),
        );
        cache.reports.lock().insert(0x8110, flags, 6, None);
        let mut state = RenderConditionState {
            override_mode: 0,
            comparison_mode: ComparisonMode::IfEqual,
            address: 0x20100,
        };
        assert!(!cache
            .accelerate_host_conditional_rendering(&mut f.runtime, &mut f.cache, &memory, state,)
            .unwrap());
        cache.notify_wfi(&mut f.runtime, &mut f.cache).unwrap();
        let tick = f.scheduler.current_tick();
        for (mode, expected) in [
            (ComparisonMode::IfEqual, true),
            (ComparisonMode::IfNotEqual, false),
        ] {
            state.comparison_mode = mode;
            assert!(
                cache
                    .accelerate_host_conditional_rendering(
                        &mut f.runtime,
                        &mut f.cache,
                        &memory,
                        state,
                    )
                    .unwrap()
            );
            f.runtime.resume_host_conditional_rendering();
            let predicate = f.runtime.active_conditional_rendering().unwrap();
            let readback = MetalBuffer::new(&f.device, 4).unwrap();
            predicate
                .buffer
                .encode_copy(&mut f.scheduler, &readback, predicate.offset, 0, 4)
                .unwrap();
            f.scheduler.finish_all().unwrap();
            let mut bytes = [0u8; 4];
            readback.read(0, &mut bytes).unwrap();
            assert_eq!(
                (u32::from_ne_bytes(bytes) != 0) ^ predicate.inverted,
                expected
            );
        }
        assert!(
            f.scheduler.current_tick() > tick,
            "only test readbacks submitted work"
        );
        cache
            .reports
            .lock()
            .base
            .impl_
            .obtain_query_mut(location)
            .unwrap()
            .flags
            .insert(QueryFlagBits::IS_GUEST_SYNCED);
        assert!(!cache
            .accelerate_host_conditional_rendering(&mut f.runtime, &mut f.cache, &memory, state,)
            .unwrap());
        assert!(f.runtime.active_conditional_rendering().is_none());
    }

    #[test]
    fn conditional_lookup_plus_four_invalidations_and_unmapped_ranges() {
        let mut f = QuerySyncFixture::new();
        let cache = MetalQueryCache::new(&f.device).unwrap();
        let mut memory = conditional_channel_memory();
        let flags = QueryPropertiesFlags::IS_A_FENCE;
        cache.reports.lock().insert(0x8144, flags, 13, None);
        let state = RenderConditionState {
            override_mode: 0,
            comparison_mode: ComparisonMode::Conditional,
            address: 0x20140,
        };
        // If the +4 lookup is missed, this would wrongly accept an unsynced
        // report through the unconditional buffer-cache resolve path.
        assert!(!cache
            .accelerate_host_conditional_rendering(&mut f.runtime, &mut f.cache, &memory, state,)
            .unwrap());
        cache.notify_wfi(&mut f.runtime, &mut f.cache).unwrap();
        assert!(cache
            .accelerate_host_conditional_rendering(&mut f.runtime, &mut f.cache, &memory, state,)
            .unwrap());
        f.runtime.resume_host_conditional_rendering();
        assert!(f.runtime.active_conditional_rendering().is_some());
        cache.invalidate_region(0x8144, 4);
        assert!(cache
            .reports
            .lock()
            .base
            .lookup_query_for_conditional_rendering(0x8140)
            .is_none());
        let mut bytes = vec![0xa5; 0x40000];
        bytes[0x8144..0x8148].fill(0);
        f.cache
            .set_device_memory(Box::new(QuerySyncMemory(Arc::new(bytes))));
        f.cache.write_memory(0x8144, 4);
        assert!(!cache
            .accelerate_host_conditional_rendering(&mut f.runtime, &mut f.cache, &memory, state,)
            .unwrap());
        assert!(f.runtime.active_conditional_rendering().is_none(),
            "CPU-owned condition must not retain old GPU value 13");
        for bad in [
            RenderConditionState {
                override_mode: 1,
                ..state
            },
            RenderConditionState {
                comparison_mode: ComparisonMode::True,
                ..state
            },
            RenderConditionState {
                comparison_mode: ComparisonMode::False,
                ..state
            },
            RenderConditionState {
                address: 0x21000,
                ..state
            },
            RenderConditionState {
                address: u64::MAX - 3,
                ..state
            },
            RenderConditionState {
                address: 0x20ff8,
                comparison_mode: ComparisonMode::IfEqual,
                ..state
            },
        ] {
            assert!(!cache
                .accelerate_host_conditional_rendering(&mut f.runtime, &mut f.cache, &memory, bad,)
                .unwrap());
            f.runtime.resume_host_conditional_rendering();
            assert!(f.runtime.active_conditional_rendering().is_none());
        }
        memory.unmap(0x20000, 0x1000);
        assert!(!cache
            .accelerate_host_conditional_rendering(&mut f.runtime, &mut f.cache, &memory, state,)
            .unwrap());
        f.scheduler.finish_all().unwrap();
    }

    #[test]
    fn conditional_lookup_shared_index_prioritizes_exact_and_stays_in_page() {
        let cache = MetalQueryCache::new(&MetalDevice::new().unwrap()).unwrap();
        let mut reports = cache.reports.lock();
        let flags = QueryPropertiesFlags::IS_A_FENCE;
        reports.insert(0x8804, flags, 14, None);
        assert_eq!(
            reports
                .base
                .lookup_query_for_conditional_rendering(0x8800)
                .unwrap()
                .value,
            14
        );
        reports.insert(0x8800, flags, 10, None);
        assert_eq!(
            reports
                .base
                .lookup_query_for_conditional_rendering(0x8800)
                .unwrap()
                .value,
            10
        );
        reports.insert(0x8800, flags, 20, None);
        assert_eq!(
            reports
                .base
                .lookup_query_for_conditional_rendering(0x8800)
                .unwrap()
                .value,
            20
        );
        reports.insert(0x9000, flags, 30, None);
        assert!(reports
            .base
            .lookup_query_for_conditional_rendering(0x8ffc)
            .is_none());
        assert_eq!(
            reports
                .base
                .lookup_query_for_conditional_rendering(0x9000)
                .unwrap()
                .value,
            30
        );
    }

    #[test]
    fn common_gpu_write_notifications_advance_native_generation_within_one_tick() {
        let mut f = QuerySyncFixture::new();
        let (id, _) = f.cache.obtain_cpu_buffer(
            0x8100, 24, ObtainBufferSynchronize::NoSynchronize,
            ObtainBufferOperation::DoNothing,
        );
        let buffer = f.cache.backend_buffer(id).unwrap().handle();
        let generation = buffer.content_generation();
        let tick = f.scheduler.current_tick();
        for expected in 1..=2 {
            let (written_id, _) = f.cache.obtain_cpu_buffer(
                0x8100, 24, ObtainBufferSynchronize::NoSynchronize,
                ObtainBufferOperation::MarkAsWritten,
            );
            assert_eq!(written_id, id);
            assert_eq!(buffer.content_generation(), generation + expected);
            assert_eq!(f.cache.backend_buffer(id).unwrap().write_tick(), tick);
            assert_eq!(f.scheduler.current_tick(), tick);
        }
    }

    #[test]
    fn conditional_lookup_accepts_gpu_modified_buffers_without_query_metadata() {
        let mut f = QuerySyncFixture::new();
        let cache = MetalQueryCache::new(&f.device).unwrap();
        let memory = conditional_channel_memory();
        let state = RenderConditionState {
            override_mode: 0,
            comparison_mode: ComparisonMode::IfEqual,
            address: 0x20100,
        };
        assert!(!cache
            .accelerate_host_conditional_rendering(&mut f.runtime, &mut f.cache, &memory, state,)
            .unwrap());
        assert!(!f.scheduler.has_active_work());
        f.runtime
            .sync_values(
                &mut f.cache,
                &[
                    SyncValuesStruct {
                        address: 0x8100,
                        value: 0x100000003,
                        size: 8,
                    },
                    SyncValuesStruct {
                        address: 0x8110,
                        value: 0x100000003,
                        size: 8,
                    },
                ],
                None,
            )
            .unwrap();
        f.cache.obtain_cpu_buffer(
            0x8100,
            24,
            ObtainBufferSynchronize::NoSynchronize,
            ObtainBufferOperation::MarkAsWritten,
        );
        let tick = f.scheduler.current_tick();
        assert!(cache
            .accelerate_host_conditional_rendering(&mut f.runtime, &mut f.cache, &memory, state,)
            .unwrap());
        assert_eq!(f.scheduler.current_tick(), tick);
        f.runtime.resume_host_conditional_rendering();
        let predicate = f.runtime.active_conditional_rendering().unwrap();
        let readback = MetalBuffer::new(&f.device, 4).unwrap();
        predicate
            .buffer
            .encode_copy(&mut f.scheduler, &readback, predicate.offset, 0, 4)
            .unwrap();
        f.scheduler.finish_all().unwrap();
        let mut bytes = [0u8; 4];
        readback.read(0, &mut bytes).unwrap();
        assert_eq!(u32::from_ne_bytes(bytes), 1);
        assert!(!predicate.inverted);
    }

    #[test]
    fn cpu_owned_conditional_uses_engine_evaluation_without_new_gpu_work() {
        let mut f = QuerySyncFixture::new();
        let cache = MetalQueryCache::new(&f.device).unwrap();
        let memory = conditional_channel_memory();
        // Exercise replacement of a previously active GPU conditional region.
        f.runtime.host_conditional_rendering_compare_value_impl(&mut f.cache, 0x8100, true).unwrap();
        f.runtime.resume_host_conditional_rendering();
        assert!(f.runtime.active_conditional_rendering().is_some());
        f.scheduler.finish_all().unwrap();
        let tick = f.scheduler.current_tick();
        let state = RenderConditionState {
            override_mode: 0,
            comparison_mode: ComparisonMode::Conditional,
            address: 0x20100,
        };
        assert!(!f.cache.is_region_gpu_modified(0x8100, 8));
        assert!(!cache.accelerate_host_conditional_rendering(&mut f.runtime, &mut f.cache, &memory, state).unwrap());
        f.runtime.resume_host_conditional_rendering();
        assert!(f.runtime.active_conditional_rendering().is_none());
        assert!(!f.scheduler.has_active_work());
        assert_eq!(f.scheduler.current_tick(), tick);
    }

    #[test]
    fn gpu_owned_conditional_without_report_keeps_gpu_two_word_comparison() {
        let mut f = QuerySyncFixture::new();
        let cache = MetalQueryCache::new(&f.device).unwrap();
        let memory = conditional_channel_memory();
        let state = RenderConditionState {
            override_mode: 0,
            comparison_mode: ComparisonMode::Conditional,
            address: 0x20100,
        };
        // Guest RAM contains 0xa5 in both words throughout. Only the GPU copy
        // is authoritative; a CPU fallback would incorrectly render all cases.
        for (value, expected) in [(0, 0), (1, 0), (0x1_0000_0000, 0), (0x1_0000_0001, 1)] {
            f.runtime.sync_values(&mut f.cache,
                &[SyncValuesStruct { address: 0x8100, value, size: 8 }], None).unwrap();
            f.cache.obtain_cpu_buffer(0x8100, 8, ObtainBufferSynchronize::NoSynchronize,
                ObtainBufferOperation::MarkAsWritten);
            let tick = f.scheduler.current_tick();
            assert!(cache.accelerate_host_conditional_rendering(&mut f.runtime, &mut f.cache, &memory, state).unwrap());
            f.runtime.resume_host_conditional_rendering();
            let predicate = f.runtime.active_conditional_rendering().unwrap();
            assert_eq!(f.scheduler.current_tick(), tick, "no CPU wait/readback in production");
            let readback = MetalBuffer::new(&f.device, 4).unwrap();
            predicate.buffer.encode_copy(&mut f.scheduler, &readback, predicate.offset, 0, 4).unwrap();
            f.scheduler.finish_all().unwrap();
            let mut bytes = [0; 4];
            readback.read(0, &mut bytes).unwrap();
            assert_eq!(u32::from_ne_bytes(bytes), expected);
        }
    }

    #[test]
    fn conditional_runtime_resolves_gpu_values_and_snapshots_ordered_draws() {
        use super::super::metal_compute_pass::{
            ConditionalArgumentLayout, ConditionalRenderingArgumentsPass,
        };
        let mut f = QuerySyncFixture::new();
        let arguments = ConditionalRenderingArgumentsPass::new(&f.device).unwrap();
        let draw = MetalBuffer::new(&f.device, 16).unwrap();
        draw.write(0, bytemuck::cast_slice(&[3u32, 7, 5, 9]))
            .unwrap();
        let tick = f.scheduler.current_tick();
        let mut snapshots = Vec::new();
        // Literals cover each half, ignored middle words, equality/inversion,
        // and multiple overwrites of one dedicated predicate before submission.
        for (first, second, compare_zero, equal, expected_instances) in [
            (0x0000_0001_0000_0002u64, 0u64, true, true, 7u32),
            (0x0000_0001_0000_0000, 0, true, true, 0),
            (0x0000_0000_0000_0002, 0, true, true, 0),
            (0x1234_5678_90ab_cdef, 0x1234_5678_90ab_cdef, false, true, 7),
            (0x1234_5678_90ab_cdef, 0x1234_5679_90ab_cdef, false, true, 0),
            (
                0x1234_5678_90ab_cdef,
                0x1234_5679_90ab_cdef,
                false,
                false,
                7,
            ),
        ] {
            f.runtime
                .sync_values(
                    &mut f.cache,
                    &[
                        SyncValuesStruct {
                            address: 0x8100,
                            value: first,
                            size: 8,
                        },
                        SyncValuesStruct {
                            address: 0x8108,
                            value: u64::MAX,
                            size: 8,
                        },
                        SyncValuesStruct {
                            address: 0x8110,
                            value: second,
                            size: 8,
                        },
                    ],
                    None,
                )
                .unwrap();
            f.runtime
                .host_conditional_rendering_compare_bc_impl(
                    &mut f.cache,
                    0x8100,
                    equal,
                    compare_zero,
                )
                .unwrap();
            f.runtime.resume_host_conditional_rendering();
            let predicate = f.runtime.active_conditional_rendering().unwrap();
            let masked = arguments
                .resolve(
                    &mut f.scheduler,
                    &mut f.staging,
                    &predicate.buffer,
                    predicate.offset,
                    predicate.inverted,
                    &draw,
                    0,
                    16,
                    1,
                    ConditionalArgumentLayout::Draw,
                )
                .unwrap();
            let readback = MetalBuffer::new(&f.device, 16).unwrap();
            masked
                .buffer
                .encode_copy(&mut f.scheduler, &readback, masked.offset, 0, 16)
                .unwrap();
            snapshots.push((readback, expected_instances));
        }
        assert_eq!(f.scheduler.current_tick(), tick);
        f.runtime.end_host_conditional_rendering();
        f.runtime.resume_host_conditional_rendering();
        assert!(f.runtime.active_conditional_rendering().is_none());
        f.scheduler.finish_all().unwrap();
        for (buffer, expected) in snapshots {
            let mut bytes = [0u8; 16];
            buffer.read(0, &mut bytes).unwrap();
            assert_eq!(
                bytemuck::cast_slice::<u8, u32>(&bytes),
                &[3, expected, 5, 9]
            );
        }
    }

    #[test]
    fn conditional_runtime_direct_query_uses_low_word_and_current_inversion() {
        use super::super::metal_compute_pass::{
            ConditionalArgumentLayout, ConditionalRenderingArgumentsPass,
        };
        let mut f = QuerySyncFixture::new();
        let arguments = ConditionalRenderingArgumentsPass::new(&f.device).unwrap();
        let draw = MetalBuffer::new(&f.device, 16).unwrap();
        draw.write(0, bytemuck::cast_slice(&[3u32, 7, 5, 9]))
            .unwrap();
        let tick = f.scheduler.current_tick();
        let mut snapshots = Vec::new();
        for (value, equal, expected_instances) in [
            (0x0000_0001_0000_0000u64, true, 7u32),
            (0x0000_0001_0000_0000, false, 0),
            (0x0000_0000_0000_0001, false, 7),
            (0x0000_0000_0000_0001, true, 0),
        ] {
            f.runtime
                .sync_values(
                    &mut f.cache,
                    &[SyncValuesStruct {
                        address: 0x8100,
                        value,
                        size: 8,
                    }],
                    None,
                )
                .unwrap();
            f.runtime
                .host_conditional_rendering_compare_value_impl(&mut f.cache, 0x8100, equal)
                .unwrap();
            f.runtime.resume_host_conditional_rendering();
            let predicate = f.runtime.active_conditional_rendering().unwrap();
            let masked = arguments
                .resolve(
                    &mut f.scheduler,
                    &mut f.staging,
                    &predicate.buffer,
                    predicate.offset,
                    predicate.inverted,
                    &draw,
                    0,
                    16,
                    1,
                    ConditionalArgumentLayout::Draw,
                )
                .unwrap();
            let readback = MetalBuffer::new(&f.device, 16).unwrap();
            masked
                .buffer
                .encode_copy(&mut f.scheduler, &readback, masked.offset, 0, 16)
                .unwrap();
            snapshots.push((readback, expected_instances));
        }
        assert_eq!(f.scheduler.current_tick(), tick);
        f.runtime.end_host_conditional_rendering();
        f.scheduler.finish_all().unwrap();
        for (buffer, expected) in snapshots {
            let mut bytes = [0u8; 16];
            buffer.read(0, &mut bytes).unwrap();
            assert_eq!(
                bytemuck::cast_slice::<u8, u32>(&bytes),
                &[3, expected, 5, 9]
            );
        }
    }

    #[test]
    fn conditional_runtime_lifecycle_refreshes_inversion_and_rejects_stale_state() {
        let mut f = QuerySyncFixture::new();
        f.runtime.resume_host_conditional_rendering();
        assert!(f.runtime.active_conditional_rendering().is_none());
        f.runtime
            .host_conditional_rendering_compare_value_impl(&mut f.cache, 0x8100, true)
            .unwrap();
        assert!(f.runtime.active_conditional_rendering().is_none());
        f.runtime.resume_host_conditional_rendering();
        let old = f.runtime.active_conditional_rendering().unwrap();
        assert!(old.inverted);
        f.runtime
            .host_conditional_rendering_compare_value_impl(&mut f.cache, 0x8100, false)
            .unwrap();
        let current = f.runtime.active_conditional_rendering().unwrap();
        assert_eq!(old.buffer.raw_handle(), current.buffer.raw_handle());
        assert_eq!(old.offset, current.offset);
        assert!(!current.inverted);
        assert!(
            old.inverted,
            "recorded setup is a snapshot, not mutable shared flags"
        );
        f.runtime.pause_host_conditional_rendering();
        assert!(f.runtime.active_conditional_rendering().is_none());
        f.runtime.resume_host_conditional_rendering();
        assert!(f.runtime.active_conditional_rendering().is_some());
        assert!(f
            .runtime
            .host_conditional_rendering_compare_bc_impl(&mut f.cache, u64::MAX - 3, true, false,)
            .is_err());
        f.runtime.resume_host_conditional_rendering();
        assert!(f.runtime.active_conditional_rendering().is_none());
        assert!(f
            .runtime
            .host_conditional_rendering_compare_value_impl(&mut f.cache, 0x8101, true,)
            .is_err());
        f.runtime.resume_host_conditional_rendering();
        assert!(f.runtime.active_conditional_rendering().is_none());
        f.scheduler.finish_all().unwrap();
    }

    #[test]
    fn pending_query_sync_filters_old_reports_and_preserves_gpu_counter_width() {
        let mut f = QuerySyncFixture::new();
        let cache = MetalQueryCache::new(&f.device).unwrap();
        let flags = QueryPropertiesFlags::IS_A_FENCE;
        let old = cache.reports.lock().insert(0x8100, flags, 11, None);
        let current = cache.reports.lock().insert(0x8100, flags, 22, None);
        let invalid = cache.reports.lock().insert(0x8120, flags, 33, None);
        cache.invalidate_region(0x8120, 4);
        let source = MetalBuffer::new(&f.device, 8).unwrap();
        let upload = MetalBuffer::new(&f.device, 8).unwrap();
        upload
            .write(0, &0x0102030455667788u64.to_ne_bytes())
            .unwrap();
        upload
            .encode_copy(&mut f.scheduler, &source, 0, 0, 8)
            .unwrap();
        let host = cache.reports.lock().insert(
            0x8140,
            flags,
            0,
            Some(MetalVisibilityBufferHandle {
                buffer: source.retained_handle(),
            }),
        );
        let timestamped = cache.reports.lock().insert(
            0x8160,
            flags | QueryPropertiesFlags::HAS_TIMEOUT,
            55,
            None,
        );
        let tick = f.scheduler.current_tick();
        cache.notify_wfi(&mut f.runtime, &mut f.cache).unwrap();
        assert_eq!(f.scheduler.current_tick(), tick);
        {
            let reports = cache.reports.lock();
            for location in [current, host, timestamped] {
                assert!(reports
                    .base
                    .impl_
                    .obtain_query(location)
                    .unwrap()
                    .flags
                    .contains(QueryFlagBits::IS_HOST_SYNCED));
            }
            for location in [old, invalid] {
                assert!(!reports
                    .base
                    .impl_
                    .obtain_query(location)
                    .unwrap()
                    .flags
                    .contains(QueryFlagBits::IS_HOST_SYNCED));
            }
            assert!(
                !reports
                    .base
                    .impl_
                    .obtain_query(host)
                    .unwrap()
                    .flags
                    .contains(QueryFlagBits::IS_FINAL_VALUE_SYNCED),
                "GPU synchronization must not pretend the CPU value is available"
            );
            assert!(reports.payload.pending_sync.is_empty());
            assert!(reports.samples.pending_sync.is_empty());
        }
        assert_eq!(f.read_gpu(0x8100, 8), [22, 0, 0, 0, 0xa5, 0xa5, 0xa5, 0xa5]);
        assert_eq!(f.read_gpu(0x8120, 8), [0xa5; 8]);
        assert_eq!(f.read_gpu(0x8140, 8), 0x0102030455667788u64.to_ne_bytes());
        assert_eq!(
            f.read_gpu(0x8160, 16),
            [55, 0, 0, 0, 0, 0, 0, 0, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5]
        );
        cache.notify_wfi(&mut f.runtime, &mut f.cache).unwrap();
        assert!(
            !f.scheduler.has_active_work(),
            "empty NotifyWFI must not record work"
        );
    }

    #[test]
    fn pending_query_sync_keeps_failed_reports_pending_and_drops_retired_ids() {
        let mut f = QuerySyncFixture::new();
        let cache = MetalQueryCache::new(&f.device).unwrap();
        let flags = QueryPropertiesFlags::IS_A_FENCE;
        let bad = cache.reports.lock().insert(0x8101, flags, 11, None);
        let good = cache.reports.lock().insert(0x8140, flags, 22, None);
        assert!(cache.notify_wfi(&mut f.runtime, &mut f.cache).is_err());
        {
            let reports = cache.reports.lock();
            assert_eq!(reports.payload.pending_sync.len(), 2);
            for location in [bad, good] {
                assert!(!reports
                    .base
                    .impl_
                    .obtain_query(location)
                    .unwrap()
                    .flags
                    .contains(QueryFlagBits::IS_HOST_SYNCED));
            }
        }
        cache.invalidate_region(0x8101, 4);
        cache.notify_wfi(&mut f.runtime, &mut f.cache).unwrap();
        assert_eq!(f.read_gpu(0x8140, 4), 22u32.to_ne_bytes());
        let retired = cache.reports.lock().insert(0x8180, flags, 33, None);
        {
            let mut reports = cache.reports.lock();
            reports.base.impl_.pending_unregister.push(retired);
            reports.base.unregister_pending();
            assert!(reports.payload.pending_sync.is_empty());
        }
        let replacement = cache.reports.lock().insert(0x81a0, flags, 44, None);
        assert_eq!(replacement, retired);
        cache.notify_wfi(&mut f.runtime, &mut f.cache).unwrap();
        assert_eq!(f.read_gpu(0x8180, 4), [0xa5; 4]);
        assert_eq!(f.read_gpu(0x81a0, 4), 44u32.to_ne_bytes());
    }

    #[test]
    fn query_sync_guest_values_preserve_neighbor_bytes_and_upstream_page_groups() {
        let mut f = QuerySyncFixture::new();
        f.staging
            .request_upload_buffer(&mut f.scheduler, 17, false)
            .unwrap();
        let values = [
            SyncValuesStruct {
                address: 0x8104,
                value: 0x1122334455667788,
                size: 8,
            },
            SyncValuesStruct {
                address: 0xa020,
                value: 0x99887766,
                size: 4,
            },
            SyncValuesStruct {
                address: 0x9008,
                value: 0x0102030405060708,
                size: 8,
            },
            SyncValuesStruct {
                address: 0x81f8,
                value: 0x8877665544332211,
                size: 8,
            },
        ];
        let tick = f.scheduler.current_tick();
        f.runtime.sync_values(&mut f.cache, &values, None).unwrap();
        assert_eq!(
            f.scheduler.current_tick(),
            tick,
            "SyncValues must not submit/wait"
        );
        assert_eq!(f.runtime.little_cache, [(0x8000, 0xa000), (0xa000, 0xb000)]);
        assert_eq!(f.runtime.redirect_cache, [0, 1, 0, 0]);
        let generations: Vec<_> = f.runtime.buffers_to_upload_to.iter()
            .map(|(buffer, _)| (Arc::clone(buffer), buffer.content_generation())).collect();
        f.runtime.sync_values(&mut f.cache, &values, None).unwrap();
        assert_eq!(f.scheduler.current_tick(), tick);
        for ((old, generation), (current, _)) in generations.iter()
            .zip(&f.runtime.buffers_to_upload_to) {
            assert!(Arc::ptr_eq(old, current));
            assert!(current.content_generation() > *generation,
                "query blits must invalidate converted indices even in one scheduler tick");
        }
        for value in values {
            let bytes = f.read_gpu(value.address - 4, 20);
            assert_eq!(&bytes[..4], &[0xa5; 4]);
            assert_eq!(
                &bytes[4..4 + value.size as usize],
                &value.value.to_ne_bytes()[..value.size as usize]
            );
            assert!(bytes[4 + value.size as usize..]
                .iter()
                .all(|&byte| byte == 0xa5));
        }
    }

    #[test]
    fn query_sync_host_values_copy_gpu_outputs_without_cpu_readback() {
        let mut f = QuerySyncFixture::new();
        let source = MetalBuffer::new_private(&f.device, 64).unwrap();
        let upload = MetalBuffer::new(&f.device, 64).unwrap();
        upload
            .write(16, &0x1122334455667788u64.to_ne_bytes())
            .unwrap();
        upload.write(40, &0x12345678u32.to_ne_bytes()).unwrap();
        upload
            .encode_copy(&mut f.scheduler, &source, 0, 0, 64)
            .unwrap();
        let values = [
            HostSyncValues {
                address: 0x8ffc,
                size: 8,
                offset: 16,
            },
            HostSyncValues {
                address: 0xa02c,
                size: 4,
                offset: 40,
            },
        ];
        let tick = f.scheduler.current_tick();
        f.runtime
            .sync_values(&mut f.cache, &values, Some(source.handle()))
            .unwrap();
        assert_eq!(f.scheduler.current_tick(), tick);
        drop(source);
        let bytes = f.read_gpu(0x8ff8, 16);
        assert_eq!(&bytes[..4], &[0xa5; 4]);
        assert_eq!(&bytes[4..12], &0x1122334455667788u64.to_ne_bytes());
        assert_eq!(&bytes[12..], &[0xa5; 4]);
        assert_eq!(f.read_gpu(0xa02c, 4), 0x12345678u32.to_ne_bytes());
    }

    #[test]
    fn query_sync_rejects_invalid_ranges_before_recording_work() {
        let mut f = QuerySyncFixture::new();
        f.runtime
            .sync_values::<SyncValuesStruct>(&mut f.cache, &[], None)
            .unwrap();
        for (address, size) in [(0x8000, 3), (0x8000, 16), (0x8001, 4), (u64::MAX - 3, 8)] {
            assert!(f
                .runtime
                .sync_values(
                    &mut f.cache,
                    &[SyncValuesStruct {
                        address,
                        size,
                        value: 1
                    }],
                    None
                )
                .is_err());
        }
        let source = MetalBuffer::new_private(&f.device, 16).unwrap();
        for offset in [1, 12, u64::MAX - 3] {
            assert!(f
                .runtime
                .sync_values(
                    &mut f.cache,
                    &[HostSyncValues {
                        address: 0x8000,
                        size: 8,
                        offset
                    }],
                    Some(source.handle())
                )
                .is_err());
        }
        assert!(f
            .runtime
            .sync_values(
                &mut f.cache,
                &[HostSyncValues {
                    address: 0x8000,
                    size: 4,
                    offset: 0
                }],
                None
            )
            .is_err());
        assert!(!f.scheduler.has_active_work());
    }

    #[test]
    fn low_accuracy_visibility_report_waits_for_its_gpu_fence() {
        use super::super::metal_fence_manager::{MetalFence, MetalFenceManager};
        use crate::fence_manager::FenceBase;
        use std::sync::atomic::{AtomicBool, Ordering};
        let _accuracy =
            crate::test_support::GpuAccuracyGuard::set(common::settings_enums::GpuAccuracy::Low);
        let device = MetalDevice::new().unwrap();
        let mut backing = vec![0xa5u8; 0x1000];
        let memory = Arc::new(MaxwellDeviceMemoryManager::default());
        memory.smmu_set_physical_base_for_test(backing.as_mut_ptr() as usize);
        memory.smmu_map_with_cpu_backing(
            0x8000,
            backing.as_mut_ptr(),
            0x4000,
            backing.len(),
            1,
            true,
        );
        let mut manager = MemoryManager::new_with_geometry_and_device_memory(
            1,
            memory.clone(),
            32,
            0x1_0000_0000,
            16,
            12,
        );
        manager.map(0x10000, 0x8000, 0x1000, 0, false);
        let mut cache = MetalQueryCache::new(&device).unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let slot = cache.prepare_draw(&mut scheduler, true).unwrap().unwrap();
        let upload = MetalBuffer::new(&device, 8).unwrap();
        upload.write(0, &73u64.to_ne_bytes()).unwrap();
        upload
            .encode_copy(
                &mut scheduler,
                &cache.visibility_buffer,
                0,
                slot.offset(),
                8,
            )
            .unwrap();
        let report = cache
            .report(
                &mut scheduler,
                Some(Arc::new(parking_lot::Mutex::new(manager))),
                None,
                0x10020,
                QueryType::ZPassPixelCount64 as u32,
                QueryPropertiesFlags::IS_A_FENCE,
                0,
            )
            .unwrap();
        let MetalQueryReport::SignalFenceAfterCompletion(operation) = report else {
            panic!("GPU report must be a completion operation at low accuracy too");
        };
        let reports = cache.reports.clone();
        let command_buffer = scheduler.active_command_buffer().unwrap();
        let mut fences = MetalFenceManager::new(false);
        let signaled = Arc::new(AtomicBool::new(false));
        let signal = signaled.clone();
        // This is the rasterizer's routing: data is a deferred operation,
        // while the optional early signal carries no GPU read.
        fences.sync_operation(operation);
        fences.sync_operation(cache.commit_async_flushes());
        let (should_wait, pop) = cache.async_flush_callbacks();
        let (should_wait_release, pop_release) = cache.async_flush_callbacks();
        fences.signal_fence(
            Box::new(move || signal.store(true, Ordering::Relaxed)),
            move |_| MetalFence::from_command_buffer(command_buffer.clone()),
            |_| {},
            should_wait,
            MetalFence::is_signaled,
            pop,
            || true,
            || {},
            || {
                scheduler.flush().unwrap();
            },
            || {},
        );
        assert!(
            signaled.load(Ordering::Relaxed),
            "exercise the early signal path"
        );
        assert_eq!(
            memory.read_u32(0x8020),
            0xa5a5a5a5,
            "query must not run at the early signal"
        );
        let (should_wait_ordering, pop_ordering) = cache.async_flush_callbacks();
        fences.signal_ordering(should_wait_ordering, |_| false, pop_ordering, || {});
        assert_eq!(
            memory.read_u32(0x8020),
            0xa5a5a5a5,
            "WFI must not release an incomplete query batch"
        );
        assert!(cache.flush_region(0x8020, 4, &memory));
        assert!(
            reports.try_lock().is_some(),
            "ReleaseFences must run outside the report lock"
        );
        assert_eq!(memory.read_u32(0x8020), 0xa5a5a5a5);
        drop(cache);
        fences.wait_pending_fences(
            false,
            |_| MetalFence::stubbed(),
            |_| {},
            should_wait_release,
            MetalFence::is_signaled,
            |fence| fence.wait_for_fence(),
            pop_release,
            || false,
            || {},
            || {},
            || {},
        );
        assert_eq!(memory.read_u32(0x8020), 73);
        assert_eq!(&backing[0x24..0x30], &[0xa5; 12]);
        assert!(reports.lock().base.cached_queries[&8].is_empty());
        assert_eq!(reports.lock().samples.queries.old_queries.len(), 1);
    }

    #[test]
    fn query_flush_masks_keep_empty_batches_in_fifo_order() {
        let cache = MetalQueryCache::new(&MetalDevice::new().unwrap()).unwrap();
        let mut operations = Vec::new();
        for requires_gpu in [false, true, false, true] {
            cache.reports.lock().pending_gpu_reports = requires_gpu;
            operations.push(cache.commit_async_flushes());
        }
        let (mut should_wait, mut pop) = cache.async_flush_callbacks();
        for expected in [false, true, false, true] {
            assert_eq!(should_wait(), expected);
            pop();
        }
        assert!(!should_wait());
        assert!(cache.reports.lock().base.impl_.flushes_pending.is_empty());
        for operation in operations {
            operation();
        }
    }

    #[test]
    fn scoped_flush_region_writes_final_values_without_timestamps_or_retirement() {
        let cache = MetalQueryCache::new(&MetalDevice::new().unwrap()).unwrap();
        let mut backing = vec![0xa5u8; 0x1000];
        let memory = MaxwellDeviceMemoryManager::default();
        memory.smmu_set_physical_base_for_test(backing.as_mut_ptr() as usize);
        memory.smmu_map_with_cpu_backing(
            0x8000,
            backing.as_mut_ptr(),
            0x4000,
            backing.len(),
            1,
            true,
        );
        let flags = QueryPropertiesFlags::IS_A_FENCE;
        let short = cache.reports.lock().insert(0x8020, flags, 0x12345678, None);
        let long =
            cache
                .reports
                .lock()
                .insert(0x8040, flags | QueryPropertiesFlags::HAS_TIMEOUT, 0, None);
        cache
            .reports
            .lock()
            .base
            .impl_
            .obtain_query_mut(long)
            .unwrap()
            .value = 0x1122334455667788;
        cache.reports.lock().insert(0x8060, flags, 99, None);
        cache.invalidate_region(0x8060, 4);
        let synced = cache.reports.lock().insert(0x8080, flags, 101, None);
        cache
            .reports
            .lock()
            .base
            .impl_
            .obtain_query_mut(synced)
            .unwrap()
            .flags
            .insert(QueryFlagBits::IS_GUEST_SYNCED);
        assert!(!cache.flush_region(0x8020, 0x64, &memory));
        assert_eq!(memory.read_u32(0x8020), 0x12345678);
        assert_eq!(memory.read_u32(0x8024), 0xa5a5a5a5);
        assert_eq!(memory.read_u64(0x8040), 0x1122334455667788);
        assert_eq!(
            &backing[0x48..0x50],
            &[0xa5; 8],
            "semi-flush must not generate a timestamp"
        );
        assert_eq!(&backing[0x60..0x64], &[0xa5; 4]);
        assert_eq!(&backing[0x80..0x84], &[0xa5; 4]);
        let reports = cache.reports.lock();
        assert!(!reports
            .base
            .impl_
            .obtain_query(short)
            .unwrap()
            .flags
            .contains(QueryFlagBits::IS_GUEST_SYNCED));
        assert!(reports.base.impl_.pending_unregister.is_empty());
        assert!(reports.payload.queries.old_queries.is_empty());
    }

    #[test]
    fn report_slots_retire_only_when_the_ordered_flush_operation_runs() {
        let cache = MetalQueryCache::new(&MetalDevice::new().unwrap()).unwrap();
        let reports = cache.reports.clone();
        let location = reports
            .lock()
            .insert(0x8020, QueryPropertiesFlags::IS_A_FENCE, 1, None);
        cache.invalidate_region(0x8020, 4);
        // A flush callback ahead of this report cannot recycle its slot.
        cache.commit_async_flushes()();
        assert!(reports.lock().payload.queries.old_queries.is_empty());
        reports
            .lock()
            .complete(location, &MaxwellDeviceMemoryManager::default(), None);
        assert!(reports.lock().payload.queries.old_queries.is_empty());
        let retirement = cache.commit_async_flushes();
        drop(cache);
        std::thread::spawn(retirement).join().unwrap();
        assert_eq!(
            reports
                .lock()
                .payload
                .queries
                .old_queries
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [location.query_id()]
        );
        assert!(reports.lock().base.impl_.pending_unregister.is_empty());
    }

    #[test]
    fn overwritten_reports_do_not_unregister_the_newer_report() {
        let mut backing = vec![0xa5u8; 0x1000];
        let memory = MaxwellDeviceMemoryManager::default();
        memory.smmu_set_physical_base_for_test(backing.as_mut_ptr() as usize);
        memory.smmu_map_with_cpu_backing(
            0x8000,
            backing.as_mut_ptr(),
            0x4000,
            backing.len(),
            1,
            true,
        );
        let mut reports = MetalQueryReports::new();
        let flags = QueryPropertiesFlags::IS_A_FENCE;
        let first = reports.insert(0x8020, flags, 5, None);
        let second = reports.insert(0x8020, flags, 7, None);
        assert!(reports
            .base
            .impl_
            .obtain_query(first)
            .unwrap()
            .flags
            .contains(QueryFlagBits::IS_REWRITTEN));
        assert!(!reports
            .base
            .impl_
            .obtain_query(second)
            .unwrap()
            .flags
            .contains(QueryFlagBits::IS_REWRITTEN));

        reports.complete(first, &memory, None);
        assert_eq!(memory.read_u32(0x8020), 5);
        assert_eq!(reports.base.cached_queries[&8][&0x20], second);
        assert!(
            reports.payload.queries.old_queries.is_empty(),
            "completion must not recycle before unregister"
        );
        reports.base.unregister_pending();
        let reused = reports.insert(0x8040, flags, 11, None);
        assert_eq!(reused, first, "completed slot should be reused");
        reports.complete(second, &memory, None);
        assert_eq!(memory.read_u32(0x8020), 7);
        reports.base.unregister_pending();
        assert!(!reports.base.cached_queries[&8].contains_key(&0x20));
        assert_eq!(reports.base.cached_queries[&8][&0x40], reused);
        reports.complete(reused, &memory, None);
        assert_eq!(memory.read_u32(0x8040), 11);
        reports.base.unregister_pending();
        assert!(reports.base.cached_queries[&8].is_empty());
        for value in 0..1000 {
            let query = reports.insert(0x8020, flags, value, None);
            reports.complete(query, &memory, None);
            reports.base.unregister_pending();
        }
        assert_eq!(
            reports.payload.queries.slot_queries.len(),
            2,
            "slots must not grow with completed reports"
        );
    }

    #[test]
    fn invalidation_suppresses_values_and_timestamps_but_not_other_reports() {
        let mut backing = vec![0xa5u8; 0x1000];
        let memory = MaxwellDeviceMemoryManager::default();
        memory.smmu_set_physical_base_for_test(backing.as_mut_ptr() as usize);
        memory.smmu_map_with_cpu_backing(
            0x8000,
            backing.as_mut_ptr(),
            0x4000,
            backing.len(),
            1,
            true,
        );
        let mut reports = MetalQueryReports::new();
        let flags = QueryPropertiesFlags::IS_A_FENCE | QueryPropertiesFlags::HAS_TIMEOUT;
        let invalid = reports.insert(0x8020, flags, 5, None);
        let valid = reports.insert(0x8040, flags, 7, None);
        // Match QueryCacheBase's four-byte query-location overlap test.
        reports.base.invalidate_region(0x8023, 1);
        assert!(reports
            .base
            .impl_
            .obtain_query(invalid)
            .unwrap()
            .flags
            .contains(QueryFlagBits::IS_INVALIDATED));
        reports.complete(
            invalid,
            &memory,
            Some(Arc::new(|| panic!("invalid report must not read ticks"))),
        );
        reports.complete(valid, &memory, Some(Arc::new(|| 19)));
        reports.base.unregister_pending();
        assert_eq!(&backing[0x20..0x30], &[0xa5; 16]);
        assert_eq!(memory.read_u64(0x8040), 7);
        assert_eq!(memory.read_u64(0x8048), 19);
        assert!(reports.base.cached_queries[&8].is_empty());
    }

    #[test]
    fn invalidated_gpu_reports_do_not_read_the_pending_gpu_value() {
        let device = MetalDevice::new().unwrap();
        let result = Arc::new(MetalBuffer::new(&device, 8).unwrap());
        result.write(0, &17u64.to_ne_bytes()).unwrap();
        let mut reports = MetalQueryReports::new();
        let location = reports.insert(
            0x8020,
            QueryPropertiesFlags::IS_A_FENCE,
            0,
            Some(MetalVisibilityBufferHandle {
                buffer: result.retained_handle(),
            }),
        );
        assert!(reports.base.is_query_dirty(location));
        assert!(!reports
            .base
            .impl_
            .obtain_query(location)
            .unwrap()
            .flags
            .contains(QueryFlagBits::IS_FINAL_VALUE_SYNCED));
        reports.base.invalidate_region(0x8020, 4);
        reports.complete(location, &MaxwellDeviceMemoryManager::default(), None);
        reports.base.unregister_pending();
        let retired = reports
            .samples
            .queries
            .get_query(location.query_id())
            .unwrap();
        assert!(
            retired.result.is_none(),
            "retirement must release the GPU buffer"
        );
        assert!(!retired
            .base
            .flags
            .contains(QueryFlagBits::IS_FINAL_VALUE_SYNCED));
    }

    #[test]
    fn deferred_reports_keep_device_destination_across_gpu_address_remaps() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let device = MetalDevice::new().unwrap();
        for zpass in [false, true] {
            for timestamp in [false, true] {
                let mut backing = vec![0xa5u8; 0x2000];
                let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
                device_memory.smmu_set_physical_base_for_test(backing.as_mut_ptr() as usize);
                device_memory.smmu_map_with_cpu_backing(
                    0x8000,
                    backing.as_mut_ptr(),
                    0x4000,
                    backing.len(),
                    1,
                    true,
                );
                let mut manager = MemoryManager::new_with_geometry_and_device_memory(
                    1,
                    device_memory.clone(),
                    32,
                    0x1_0000_0000,
                    16,
                    12,
                );
                manager.map(0x10000, 0x8000, 0x1000, 0, false);
                let manager = Arc::new(parking_lot::Mutex::new(manager));
                let mut cache = MetalQueryCache::new(&device).unwrap();
                let mut scheduler = MetalScheduler::new(&device);
                let value = if zpass {
                    0x1122334455667788u64
                } else {
                    0x55667788
                };
                if zpass {
                    let query = cache.prepare_draw(&mut scheduler, true).unwrap().unwrap();
                    let upload = MetalBuffer::new(&device, 8).unwrap();
                    upload.write(0, &value.to_ne_bytes()).unwrap();
                    upload
                        .encode_copy(
                            &mut scheduler,
                            &cache.visibility_buffer,
                            0,
                            query.offset(),
                            8,
                        )
                        .unwrap();
                }
                let ticks = Arc::new(AtomicU64::new(1));
                let getter = ticks.clone();
                let mut flags = QueryPropertiesFlags::IS_A_FENCE;
                if timestamp {
                    flags |= QueryPropertiesFlags::HAS_TIMEOUT;
                }
                let report = cache
                    .report(
                        &mut scheduler,
                        Some(manager.clone()),
                        Some(Arc::new(move || getter.load(Ordering::Relaxed))),
                        0x10020,
                        if zpass {
                            QueryType::ZPassPixelCount64
                        } else {
                            QueryType::Payload
                        } as u32,
                        flags,
                        value as u32,
                    )
                    .unwrap();
                manager.lock().unmap(0x10000, 0x1000);
                manager.lock().map(0x10000, 0x9000, 0x1000, 0, false);
                // A callback must not need the old GPU address-space owner.
                drop(manager);
                drop(cache);
                ticks.store(0x1020304050607080, Ordering::Relaxed);
                scheduler.finish_all().unwrap();
                let operation = match report {
                    MetalQueryReport::SignalFenceAfterCompletion(operation) if zpass => operation,
                    MetalQueryReport::SignalFence(operation) if !zpass => operation,
                    _ => panic!(
                        "GPU reports must request completion; payloads keep guest fence behavior"
                    ),
                };
                std::thread::spawn(operation).join().unwrap();
                if timestamp {
                    assert_eq!(&backing[0x20..0x28], &value.to_ne_bytes());
                    assert_eq!(&backing[0x28..0x30], &0x1020304050607080u64.to_ne_bytes());
                } else {
                    assert_eq!(&backing[0x20..0x24], &(value as u32).to_ne_bytes());
                    assert_eq!(&backing[0x24..0x30], &[0xa5; 12]);
                }
                assert!(
                    backing[0x1000..].iter().all(|&byte| byte == 0xa5),
                    "new GPU mapping must not receive the old report"
                );
            }
        }
    }

    #[test]
    fn unmapped_report_is_discarded_before_recording_a_gpu_resolve() {
        let device = MetalDevice::new().unwrap();
        let mut cache = MetalQueryCache::new(&device).unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let manager = Arc::new(parking_lot::Mutex::new(
            MemoryManager::new_with_geometry_and_device_memory(
                1,
                Arc::new(MaxwellDeviceMemoryManager::default()),
                32,
                0x1_0000_0000,
                16,
                12,
            ),
        ));
        for query_type in [QueryType::Payload, QueryType::ZPassPixelCount64] {
            let report = cache
                .report(
                    &mut scheduler,
                    Some(manager.clone()),
                    None,
                    0x10020,
                    query_type as u32,
                    QueryPropertiesFlags::IS_A_FENCE,
                    7,
                )
                .unwrap();
            assert!(matches!(report, MetalQueryReport::Complete));
            assert!(!scheduler.has_active_work());
        }
    }

    #[test]
    fn visibility_reports_accumulate_new_gpu_slots_without_mutating_older_reports() {
        let device = MetalDevice::new().unwrap();
        let mut cache = MetalQueryCache::new(&device).unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut reports = Vec::new();
        for (index, value) in [7u64, 11, u64::MAX].into_iter().enumerate() {
            if index == 1 {
                cache.next_slot = QUERY_SLOT_COUNT;
            }
            let query = cache.prepare_draw(&mut scheduler, true).unwrap().unwrap();
            let upload = MetalBuffer::new(&device, 8).unwrap();
            upload.write(0, &value.to_ne_bytes()).unwrap();
            upload
                .encode_copy(
                    &mut scheduler,
                    &cache.visibility_buffer,
                    0,
                    query.offset(),
                    8,
                )
                .unwrap();
            let report = cache.resolve_visibility_counter(&mut scheduler).unwrap();
            assert!(
                Arc::ptr_eq(
                    &report,
                    &cache.resolve_visibility_counter(&mut scheduler).unwrap()
                ),
                "no new slots must reuse the immutable result"
            );
            reports.push(report);
        }
        cache.reset_counter(QueryType::ZPassPixelCount64 as u32);
        reports.push(cache.resolve_visibility_counter(&mut scheduler).unwrap());
        let query = cache.prepare_draw(&mut scheduler, true).unwrap().unwrap();
        let upload = MetalBuffer::new(&device, 8).unwrap();
        upload.write(0, &9u64.to_ne_bytes()).unwrap();
        upload
            .encode_copy(
                &mut scheduler,
                &cache.visibility_buffer,
                0,
                query.offset(),
                8,
            )
            .unwrap();
        reports.push(cache.resolve_visibility_counter(&mut scheduler).unwrap());
        drop(cache);
        scheduler.finish_all().unwrap();
        for (report, expected) in reports.into_iter().zip([7, 18, 17, 0, 9]) {
            let mut bytes = [0; 8];
            report.read(0, &mut bytes).unwrap();
            assert_eq!(u64::from_ne_bytes(bytes), expected);
        }
    }

    #[test]
    fn payload_and_unsupported_query_values_match_upstream_fallbacks() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let cache = MetalQueryCache::new(&device).unwrap();

        assert_eq!(cache.query_value(QueryType::Payload as u32, 0x1234), 0x1234);
        assert_eq!(
            cache.query_value(
                QueryType::StreamingPrimitivesNeededMinusSucceeded as u32,
                0xffff_ffff,
            ),
            0
        );
        assert_eq!(cache.query_value(QueryType::AlphaBetaClocks as u32, 0), 1);
    }

    #[test]
    fn payload_reports_follow_upstream_fence_and_accuracy_ordering() {
        assert_eq!(
            payload_report_action(QueryPropertiesFlags::empty(), false),
            PayloadReportAction::Immediate
        );
        assert_eq!(
            payload_report_action(QueryPropertiesFlags::HAS_TIMEOUT, true),
            PayloadReportAction::SyncOperation
        );
        assert_eq!(
            payload_report_action(QueryPropertiesFlags::IS_A_FENCE, false),
            PayloadReportAction::SignalFence
        );
        assert_eq!(
            payload_report_action(QueryPropertiesFlags::IS_A_FENCE, true),
            PayloadReportAction::SignalFence
        );
    }

    #[test]
    fn visibility_slots_are_distinct_and_reset_drops_the_accumulation_set() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut cache = MetalQueryCache::new(&device).unwrap();
        let mut scheduler = MetalScheduler::new(&device);

        let first = cache.prepare_draw(&mut scheduler, true).unwrap().unwrap();
        let second = cache.prepare_draw(&mut scheduler, true).unwrap().unwrap();
        assert_eq!(first.offset(), 0);
        assert_eq!(second.offset(), QUERY_SLOT_SIZE);
        assert_eq!(cache.zpass_slots.len(), 2);

        cache.reset_counter(QueryType::ZPassPixelCount64 as u32);
        assert!(cache.zpass_slots.is_empty());
    }

    #[test]
    fn visibility_report_snapshot_survives_counter_reset() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut cache = MetalQueryCache::new(&device).unwrap();
        let mut scheduler = MetalScheduler::new(&device);

        let first = cache.prepare_draw(&mut scheduler, true).unwrap().unwrap();
        let second = cache.prepare_draw(&mut scheduler, true).unwrap().unwrap();
        unsafe {
            cache
                .visibility_buffer
                .contents_ptr()
                .add(first.offset())
                .cast::<u64>()
                .write(7);
            cache
                .visibility_buffer
                .contents_ptr()
                .add(second.offset())
                .cast::<u64>()
                .write(11);
        }
        let snapshot = cache.resolve_visibility_counter(&mut scheduler).unwrap();

        cache.reset_counter(QueryType::ZPassPixelCount64 as u32);

        scheduler.finish_all().unwrap();
        let mut bytes = [0; 8];
        snapshot.read(0, &mut bytes).unwrap();
        assert_eq!(u64::from_ne_bytes(bytes), 18);
    }

    #[test]
    fn visibility_slots_wrap_before_metals_maximum_offset() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut cache = MetalQueryCache::new(&device).unwrap();
        let mut scheduler = MetalScheduler::new(&device);

        let before_wrap = cache.prepare_draw(&mut scheduler, true).unwrap().unwrap();
        unsafe {
            cache
                .visibility_buffer
                .contents_ptr()
                .add(before_wrap.offset())
                .cast::<u64>()
                .write(7);
        }
        cache.next_slot = QUERY_SLOT_COUNT;

        let after_wrap = cache.prepare_draw(&mut scheduler, true).unwrap().unwrap();
        unsafe {
            cache
                .visibility_buffer
                .contents_ptr()
                .add(after_wrap.offset())
                .cast::<u64>()
                .write(11);
        }

        assert_eq!(after_wrap.offset(), 0);
        assert_eq!(cache.next_slot, 1);
        assert!(after_wrap.offset() <= 256 * 1024 - QUERY_SLOT_SIZE);
        assert_eq!(cache.completed_zpass_banks.len(), 1);
        let snapshot = cache.resolve_visibility_counter(&mut scheduler).unwrap();
        scheduler.finish_all().unwrap();
        let mut bytes = [0; 8];
        snapshot.read(0, &mut bytes).unwrap();
        assert_eq!(u64::from_ne_bytes(bytes), 18);
    }
}

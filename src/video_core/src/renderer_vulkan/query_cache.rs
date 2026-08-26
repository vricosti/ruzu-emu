// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `vk_query_cache.h` / `vk_query_cache.cpp`.
//!
//! Vulkan-specific query cache runtime that handles GPU query synchronization,
//! conditional rendering, and streamer interfaces.
//!
//! Upstream uses a PIMPL pattern (`QueryCacheRuntimeImpl`) to hide the complex
//! internal state including query pool banks, streamers, and host conditional
//! rendering state.

use std::collections::{BTreeMap, VecDeque};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use ash::vk;
use ash::vk::Handle;

use crate::buffer_cache::buffer_cache_base::{ObtainBufferOperation, ObtainBufferSynchronize};
use crate::control::channel_state::ChannelState;
use crate::control::channel_state_cache::ChannelCacheAccessor;
use crate::engines::maxwell_3d::PrimitiveTopology;
use crate::query_cache::bank_base::{BankBase, BankLike, BankPool};
use crate::query_cache::query_cache::{
    DeviceMemoryWriter, GpuAddressTranslator, GuestStreamer, QueryCacheRuntimeHandle, StubStreamer,
    SyncValuesRuntime, SyncValuesStruct,
};
use crate::query_cache::query_cache_base::{LookupData, QueryCacheBase, QueryLocation};
use crate::query_cache::query_stream::StreamerInterface;
use crate::query_cache::types::{QueryPropertiesFlags, QueryType};
use crate::vulkan_common::vulkan_device::Device;
use crate::vulkan_common::vulkan_memory_allocator::{MappedBuffer, MemoryAllocator, MemoryUsage};

use super::buffer_cache::VulkanCommonBufferCache;
use super::compute_pass::{ConditionalRenderingResolvePass, QueriesPrefixScanPass};
use super::descriptor_pool::DescriptorPool;
use super::master_semaphore::MasterSemaphore;
use super::scheduler::Scheduler;
use super::staging_buffer_pool::StagingBufferPool;
use super::update_descriptor::ComputePassDescriptorQueue;

// ---------------------------------------------------------------------------
// Constants (from vk_query_cache.cpp)
// ---------------------------------------------------------------------------

/// Size of each query bank (number of query slots per pool).
/// Port of `SamplesQueryBank::BANK_SIZE`.
pub const SAMPLES_QUERY_BANK_SIZE: usize = 256;

/// Size of each query result in bytes.
/// Port of `SamplesQueryBank::QUERY_SIZE`.
pub const SAMPLES_QUERY_SIZE: usize = 8;

const MIN_SCAN_BUFFER_LOG2: usize = 11;

#[derive(Clone, Copy)]
struct ScanBufferPair {
    resolve: vk::Buffer,
    intermediary: vk::Buffer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HostSyncValues {
    address: u64,
    size: u64,
    offset: u64,
}

struct SamplesPendingSync {
    report: SamplesReport,
    address: u64,
}

fn scan_buffer_log2(required: usize) -> usize {
    let required = required.max(1);
    let log2 = (usize::BITS - (required - 1).leading_zeros()) as usize;
    log2.max(MIN_SCAN_BUFFER_LOG2)
}

fn accumulation_limits(
    first_checkpoint: usize,
    last_checkpoint: usize,
    slots_used: usize,
) -> (usize, usize) {
    (
        first_checkpoint.min(slots_used),
        last_checkpoint.max(slots_used),
    )
}

/// Port of `SamplesQueryBank` in `vk_query_cache.cpp`.
///
/// Upstream derives from `VideoCommon::BankBase` and keeps a `Scheduler&` so
/// `IsDead` can consult `Scheduler::IsFree`. The port holds the scheduler's
/// `MasterSemaphore` instead, which is the part of the scheduler that answers
/// that question and is the only piece reachable from a shared handle.
///
/// The port stores banks as `Arc` because a query's spans are resolved on the
/// fence thread, after the streamer's borrow has ended (see `SamplesQuerySpan`);
/// shared ownership is what keeps the bank alive until then. `base` is behind a
/// mutex as a consequence: an `Arc` grants shared access only.
///
/// Address stability comes for free with that and is not the reason for it.
/// Upstream holds a `SamplesQueryBank*` into its `std::deque`, which is a
/// segmented container whose element references survive `push_back`; the
/// `VecDeque` that `BankPool` is built on is a single reallocating ring buffer,
/// so `BankPool<Box<_>>` would be the equivalent had lifetime not been the
/// binding constraint.
struct SamplesQueryBank {
    base: parking_lot::Mutex<BankBase>,
    device: ash::Device,
    master_semaphore: Arc<MasterSemaphore>,
    query_pool: vk::QueryPool,
    index: usize,
    host_results: parking_lot::Mutex<Vec<u64>>,
    /// Guards host access to the query pool, which Vulkan requires to be
    /// externally synchronized, against the host-side whole-pool reset.
    host_access: parking_lot::Mutex<()>,
    last_used_tick: AtomicU64,
    /// Set by `BankLike::reset` when the pool recycles this bank. Rust cannot
    /// pass `&mut Scheduler` through `BankPool::reserve_bank`, so the GPU-side
    /// pool reset upstream performs inside `Reset()` is recorded by the
    /// streamer immediately after, before any slot of the bank is handed out.
    pending_pool_reset: AtomicBool,
}

impl SamplesQueryBank {
    fn new(
        device: ash::Device,
        master_semaphore: Arc<MasterSemaphore>,
        scheduler: &mut Scheduler,
        index: usize,
        host_query_reset_supported: bool,
    ) -> Result<Arc<Self>, vk::Result> {
        let create_info = vk::QueryPoolCreateInfo::builder()
            .query_type(vk::QueryType::OCCLUSION)
            .query_count(SAMPLES_QUERY_BANK_SIZE as u32)
            .build();
        let query_pool = unsafe { device.create_query_pool(&create_info, None)? };
        let bank = Arc::new(Self {
            base: parking_lot::Mutex::new(BankBase::new(SAMPLES_QUERY_BANK_SIZE)),
            device,
            master_semaphore,
            query_pool,
            index,
            host_results: parking_lot::Mutex::new(vec![0u64; SAMPLES_QUERY_BANK_SIZE]),
            host_access: parking_lot::Mutex::new(()),
            last_used_tick: AtomicU64::new(0),
            pending_pool_reset: AtomicBool::new(false),
        });
        bank.record_pool_reset(scheduler, host_query_reset_supported);
        Ok(bank)
    }

    /// GPU-side half of upstream `SamplesQueryBank::Reset()`.
    fn record_pool_reset(&self, scheduler: &mut Scheduler, host_query_reset_supported: bool) {
        if host_query_reset_supported {
            let _host_access = self.host_access.lock();
            unsafe {
                self.device
                    .reset_query_pool(self.query_pool, 0, SAMPLES_QUERY_BANK_SIZE as u32);
            }
        } else {
            scheduler.request_outside_render_pass_operation_context();
            let device = self.device.clone();
            let query_pool = self.query_pool;
            scheduler.record(move |cmdbuf| unsafe {
                device.cmd_reset_query_pool(cmdbuf, query_pool, 0, SAMPLES_QUERY_BANK_SIZE as u32);
            });
        }
    }

    /// Record the reset the pool deferred when it recycled this bank.
    fn flush_pending_pool_reset(
        &self,
        scheduler: &mut Scheduler,
        host_query_reset_supported: bool,
    ) {
        if self.pending_pool_reset.swap(false, Ordering::Relaxed) {
            self.record_pool_reset(scheduler, host_query_reset_supported);
        }
    }

    /// Port of `BankBase::Reserve` as used by `ReserveBankSlot`.
    fn reserve(&self) -> Option<usize> {
        let (built, slot) = self.base.lock().reserve();
        built.then_some(slot)
    }

    fn is_closed(&self) -> bool {
        self.base.lock().is_closed()
    }

    /// Port of `BankBase::Close`, called by `PushUnsyncedQueries`.
    fn close(&self) {
        self.base.lock().close();
    }

    /// Port of `SamplesQueryBank::AddReference`, which also stamps the tick.
    fn add_reference(&self, how_many: usize, scheduler: &Scheduler) {
        self.base.lock().add_reference(how_many);
        self.last_used_tick
            .store(scheduler.current_tick(), Ordering::Relaxed);
    }

    fn close_reference(&self, how_many: usize) {
        self.base.lock().close_reference(how_many);
    }

    /// Port of `SamplesQueryBank::Sync`: one ranged readback per bank.
    fn sync(&self, start: usize, size: usize) -> Result<(), vk::Result> {
        let mut host_results = self.host_results.lock();
        let _host_access = self.host_access.lock();
        unsafe {
            self.device.get_query_pool_results(
                self.query_pool,
                start as u32,
                size as u32,
                &mut host_results[start..start + size],
                vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WAIT,
            )
        }
    }

    /// Port of `SamplesQueryBank::GetResults`, summed over one slot range.
    fn sum_results(&self, start: usize, size: usize) -> u64 {
        let host_results = self.host_results.lock();
        host_results[start..start + size]
            .iter()
            .fold(0u64, |total, value| total.wrapping_add(*value))
    }
}

impl BankLike for Arc<SamplesQueryBank> {
    /// Port of `SamplesQueryBank::IsDead`.
    fn is_dead(&self) -> bool {
        self.base.lock().is_dead()
            && self
                .master_semaphore
                .is_free(self.last_used_tick.load(Ordering::Relaxed))
    }

    /// CPU-side half of upstream `SamplesQueryBank::Reset()`; the pool reset is
    /// deferred to `flush_pending_pool_reset`.
    fn reset(&mut self) {
        self.base.lock().reset();
        self.host_results.lock().fill(0);
        self.pending_pool_reset.store(true, Ordering::Relaxed);
    }
}

impl Drop for SamplesQueryBank {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_query_pool(self.query_pool, None);
        }
    }
}

/// One `(bank, start, amount)` triple of upstream's `ApplyBankOp` walk.
///
/// Upstream re-walks `next_bank` from the pool every time it touches a query.
/// The port materializes the walk when the report is taken, because reports are
/// resolved on the fence thread, where the streamer and its pool are not
/// reachable. Holding the bank keeps its reference count raised, which is what
/// upstream's `AddReference`/`CloseReference` pairing does.
///
/// Materializing the walk also leaves upstream's `SamplesQueryBank::next_bank`
/// chaining without a reader, so the port does not carry that field.
struct SamplesQuerySpan {
    bank: Arc<SamplesQueryBank>,
    start: usize,
    amount: usize,
}

impl Drop for SamplesQuerySpan {
    /// Port of `SamplesStreamer::Free`, which closes the query's references.
    fn drop(&mut self) {
        self.bank.close_reference(self.amount);
    }
}

/// Counter state of the samples streamer.
///
/// Upstream keeps `current_query`, `current_bank_slot` and `has_started` as
/// plain members of `SamplesStreamer`.
pub(crate) struct SamplesQueryState {
    /// Slot currently recording, as `(bank, slot)`.
    current: Option<(Arc<SamplesQueryBank>, usize)>,
    /// Slots closed since the last report, in acquisition order.
    history: Vec<(Arc<SamplesQueryBank>, usize)>,
}

impl SamplesQueryState {
    /// Port of `SamplesStreamer::PauseCounter`.
    pub(crate) fn pause_counter(&mut self, scheduler: &mut Scheduler) {
        let Some((bank, slot)) = self.current.take() else {
            return;
        };
        let device = bank.device.clone();
        let query_pool = bank.query_pool;
        scheduler.record(move |cmdbuf| unsafe {
            device.cmd_end_query(cmdbuf, query_pool, slot as u32);
        });
        self.history.push((bank, slot));
    }

    /// Port of `SamplesStreamer::ResetCounter`, which abandons the current
    /// query after pausing it.
    pub(crate) fn reset_counter(&mut self, scheduler: &mut Scheduler) {
        self.pause_counter(scheduler);
        self.abandon_history();
    }

    /// Port of `SamplesStreamer::AbandonCurrentQuery` -> `Free`: closes the
    /// references `ReserveHostQuery` took for slots that are never reported.
    fn abandon_history(&mut self) {
        for (bank, _) in self.history.drain(..) {
            bank.close_reference(1);
        }
    }
}

struct SamplesStreamer {
    device: ash::Device,
    memory_allocator: NonNull<MemoryAllocator>,
    /// Port of `SamplesStreamer::bank_pool` / `current_bank`. Upstream also
    /// keeps `current_bank_id`, which it needs to chain banks through
    /// `next_bank`; the port resolves spans eagerly and has no reader for it.
    bank_pool: BankPool<Arc<SamplesQueryBank>>,
    current_bank: Option<Arc<SamplesQueryBank>>,
    host_query_reset_supported: bool,
    state: Arc<parking_lot::Mutex<SamplesQueryState>>,
    prefix_scan_pass: QueriesPrefixScanPass,
    scan_buffers: Vec<Option<ScanBufferPair>>,
    accumulation_buffer: vk::Buffer,
    driver_id: vk::DriverId,
    amend_value: Arc<AtomicU64>,
    accumulation_value: Arc<AtomicU64>,
    num_slots_used: usize,
    first_accumulation_checkpoint: usize,
    last_accumulation_checkpoint: usize,
    accumulation_since_last_sync: bool,
    pending_sync: Vec<SamplesPendingSync>,
    sync_values_stash: Vec<(Vec<HostSyncValues>, vk::Buffer)>,
    // Port of SamplesStreamer::pending_flush_queries/pending_flush_sets.
    // Host query results are resolved only after the corresponding scheduler
    // fence has completed, before the fence callbacks consume them.
    pending_flush_queries: Vec<Arc<SamplesReportState>>,
    pending_flush_sets: parking_lot::Mutex<VecDeque<Vec<Arc<SamplesReportState>>>>,
}

impl SamplesStreamer {
    fn new(
        vulkan_device: &Device,
        device: ash::Device,
        scheduler: &mut Scheduler,
        memory_allocator: &MemoryAllocator,
        descriptor_pool: &DescriptorPool,
        compute_pass_descriptor_queue: &mut ComputePassDescriptorQueue,
        driver_id: vk::DriverId,
        host_query_reset_supported: bool,
    ) -> Result<Self, vk::Result> {
        let prefix_scan_pass = QueriesPrefixScanPass::new(
            vulkan_device,
            scheduler,
            descriptor_pool,
            compute_pass_descriptor_queue,
        )?;
        let accumulation_buffer = memory_allocator
            .create_buffer(
                &vk::BufferCreateInfo::builder()
                    .size(SAMPLES_QUERY_SIZE as u64)
                    .usage(
                        vk::BufferUsageFlags::TRANSFER_DST
                            | vk::BufferUsageFlags::TRANSFER_SRC
                            | vk::BufferUsageFlags::STORAGE_BUFFER,
                    )
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .build(),
                MemoryUsage::DeviceLocal,
            )
            .map_err(|error| error.result)?;
        scheduler.request_outside_render_pass_operation_context();
        let device_for_fill = device.clone();
        scheduler.record(move |cmdbuf| unsafe {
            device_for_fill.cmd_fill_buffer(
                cmdbuf,
                accumulation_buffer,
                0,
                SAMPLES_QUERY_SIZE as u64,
                0,
            );
        });
        let mut scan_buffers = Vec::with_capacity(usize::BITS as usize + 1);
        scan_buffers.resize_with(usize::BITS as usize + 1, || None);
        Ok(Self {
            device,
            memory_allocator: NonNull::from(memory_allocator),
            bank_pool: BankPool::new(),
            current_bank: None,
            host_query_reset_supported,
            state: Arc::new(parking_lot::Mutex::new(SamplesQueryState {
                current: None,
                history: Vec::new(),
            })),
            prefix_scan_pass,
            scan_buffers,
            accumulation_buffer,
            driver_id,
            amend_value: Arc::new(AtomicU64::new(0)),
            accumulation_value: Arc::new(AtomicU64::new(0)),
            num_slots_used: 0,
            first_accumulation_checkpoint: 0,
            last_accumulation_checkpoint: 0,
            accumulation_since_last_sync: false,
            pending_sync: Vec::new(),
            sync_values_stash: Vec::new(),
            pending_flush_queries: Vec::new(),
            pending_flush_sets: parking_lot::Mutex::new(VecDeque::new()),
        })
    }

    fn create_scan_buffers(
        memory_allocator: &MemoryAllocator,
        capacity: usize,
    ) -> Result<(vk::Buffer, vk::Buffer), vk::Result> {
        let size = (capacity * SAMPLES_QUERY_SIZE) as u64;
        let usage = vk::BufferUsageFlags::TRANSFER_DST
            | vk::BufferUsageFlags::TRANSFER_SRC
            | vk::BufferUsageFlags::STORAGE_BUFFER;
        let resolve = memory_allocator
            .create_buffer(
                &vk::BufferCreateInfo::builder()
                    .size(size)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .build(),
                MemoryUsage::DeviceLocal,
            )
            .map_err(|error| error.result)?;
        let intermediary = memory_allocator
            .create_buffer(
                &vk::BufferCreateInfo::builder()
                    .size(size)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .build(),
                MemoryUsage::DeviceLocal,
            )
            .map_err(|error| error.result)?;
        Ok((resolve, intermediary))
    }

    fn obtain_scan_buffers(&mut self, required: usize) -> Result<ScanBufferPair, vk::Result> {
        let log2 = scan_buffer_log2(required);
        if let Some(buffers) = self.scan_buffers[log2] {
            return Ok(buffers);
        }
        let capacity = 1usize << log2;
        let memory_allocator = unsafe { self.memory_allocator.as_ref() };
        let (resolve, intermediary) = Self::create_scan_buffers(memory_allocator, capacity)?;
        let buffers = ScanBufferPair {
            resolve,
            intermediary,
        };
        self.scan_buffers[log2] = Some(buffers);
        Ok(buffers)
    }

    fn shared_state(&self) -> Arc<parking_lot::Mutex<SamplesQueryState>> {
        Arc::clone(&self.state)
    }

    /// Port of `SamplesStreamer::ReserveBank`.
    fn reserve_bank(&mut self, scheduler: &mut Scheduler) -> Result<(), vk::Result> {
        let device = self.device.clone();
        let master_semaphore = Arc::clone(scheduler.get_master_semaphore());
        let host_query_reset_supported = self.host_query_reset_supported;
        let new_bank_id = self.bank_pool.reserve_bank(|queue, index| {
            queue.push_back(SamplesQueryBank::new(
                device,
                master_semaphore,
                scheduler,
                index,
                host_query_reset_supported,
            )?);
            Ok(())
        })?;
        let new_bank = Arc::clone(self.bank_pool.get_bank(new_bank_id));
        new_bank.flush_pending_pool_reset(scheduler, self.host_query_reset_supported);
        self.current_bank = Some(new_bank);
        Ok(())
    }

    /// Port of `SamplesStreamer::ReserveBankSlot`.
    fn reserve_bank_slot(&mut self, scheduler: &mut Scheduler) -> Result<usize, vk::Result> {
        if self
            .current_bank
            .as_ref()
            .is_none_or(|bank| bank.is_closed())
        {
            self.reserve_bank(scheduler)?;
        }
        let bank = self
            .current_bank
            .as_ref()
            .expect("reserve_bank leaves a current bank");
        Ok(bank
            .reserve()
            .expect("a bank that is not closed has a free slot"))
    }

    /// Port of `SamplesStreamer::ReserveHostQuery`, which takes the slot's
    /// reference on the owning bank.
    fn reserve_host_query(
        &mut self,
        scheduler: &mut Scheduler,
    ) -> Result<(Arc<SamplesQueryBank>, usize), vk::Result> {
        let slot = self.reserve_bank_slot(scheduler)?;
        let bank = Arc::clone(
            self.current_bank
                .as_ref()
                .expect("reserve_bank_slot leaves a current bank"),
        );
        bank.add_reference(1, scheduler);
        self.num_slots_used += 1;
        Ok((bank, slot))
    }

    /// Port of `SamplesStreamer::StartCounter`.
    fn start_counter(&mut self, scheduler: &mut Scheduler) {
        if self.state.lock().current.is_some() {
            return;
        }
        let (bank, slot) = match self.reserve_host_query(scheduler) {
            Ok(query) => query,
            Err(error) => {
                log::error!("Failed to allocate a Vulkan occlusion query bank: {error:?}");
                return;
            }
        };
        let device = bank.device.clone();
        let query_pool = bank.query_pool;
        scheduler.record(move |cmdbuf| unsafe {
            let use_precise = common::settings::is_gpu_level_high(&common::settings::values());
            device.cmd_begin_query(
                cmdbuf,
                query_pool,
                slot as u32,
                if use_precise {
                    vk::QueryControlFlags::PRECISE
                } else {
                    vk::QueryControlFlags::empty()
                },
            );
        });
        self.state.lock().current = Some((bank, slot));
    }

    fn reset_counter(
        &mut self,
        scheduler: &mut Scheduler,
        sync_operation: impl FnOnce(Box<dyn FnOnce() + Send>),
    ) {
        self.state.lock().reset_counter(scheduler);
        let amend_value = Arc::clone(&self.amend_value);
        let accumulation_value = Arc::clone(&self.accumulation_value);
        sync_operation(Box::new(move || {
            amend_value.store(0, Ordering::Relaxed);
            accumulation_value.store(0, Ordering::Relaxed);
        }));
        self.accumulation_since_last_sync = false;
        self.first_accumulation_checkpoint =
            self.first_accumulation_checkpoint.min(self.num_slots_used);
        self.last_accumulation_checkpoint =
            self.last_accumulation_checkpoint.max(self.num_slots_used);
    }

    /// Port of `SamplesStreamer::CloseCounter`, which upstream defines as a
    /// plain `PauseCounter()`.
    fn close_counter(&mut self, scheduler: &mut Scheduler) {
        self.state.lock().pause_counter(scheduler);
    }

    fn take_report(&mut self, scheduler: &mut Scheduler, address: u64) -> Option<SamplesReport> {
        let mut state = self.state.lock();
        state.pause_counter(scheduler);
        if state.history.is_empty() {
            return None;
        }
        // Upstream `WriteCounter` copies the whole current query and takes one
        // additional bank reference per copied slot. Keep `history` as the
        // current cumulative query until `ResetCounter` or `PresyncWrites`,
        // and give this report its own matching references.
        let history = clone_cumulative_history(&state.history, |bank| {
            bank.add_reference(1, scheduler);
        });
        let report = SamplesReport::new(
            coalesce_query_spans(history),
            Arc::clone(&self.amend_value),
            Arc::clone(&self.accumulation_value),
        );
        self.pending_sync.push(SamplesPendingSync {
            report: report.clone(),
            address,
        });
        Some(report)
    }

    fn has_pending_sync(&self) -> bool {
        !self.pending_sync.is_empty()
    }

    fn presync_writes(
        &mut self,
        scheduler: &mut Scheduler,
        mut sync_operation: impl FnMut(Box<dyn FnOnce() + Send>),
    ) {
        if self.pending_sync.is_empty() {
            return;
        }
        self.state.lock().pause_counter(scheduler);
        if matches!(
            self.driver_id,
            vk::DriverId::QUALCOMM_PROPRIETARY
                | vk::DriverId::ARM_PROPRIETARY
                | vk::DriverId::MESA_TURNIP
        ) {
            self.pending_sync.clear();
            self.sync_values_stash.clear();
            return;
        }

        self.sync_values_stash.clear();
        let Some(scan_buffers) = self.obtain_scan_buffers(self.num_slots_used).ok() else {
            return;
        };
        let resolve_buffer = scan_buffers.resolve;
        let intermediary_buffer = scan_buffers.intermediary;
        let ranges = ordered_pending_ranges(&self.pending_sync);
        let mut offsets = BTreeMap::new();
        let mut base_offset = 0usize;
        for (bank, start, amount) in &ranges {
            offsets.insert(bank.index, (*start, base_offset));
            base_offset += amount * SAMPLES_QUERY_SIZE;
        }
        let device = self.device.clone();
        scheduler.request_outside_render_pass_operation_context();
        scheduler.record(move |cmdbuf| unsafe {
            let mut base_offset = 0usize;
            for (bank, start, amount) in ranges {
                let offset = base_offset as u64;
                device.cmd_copy_query_pool_results(
                    cmdbuf,
                    bank.query_pool,
                    start as u32,
                    amount as u32,
                    resolve_buffer,
                    offset,
                    SAMPLES_QUERY_SIZE as u64,
                    vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WAIT,
                );
                let barrier = vk::BufferMemoryBarrier::builder()
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .buffer(resolve_buffer)
                    .offset(offset)
                    .size((amount * SAMPLES_QUERY_SIZE) as u64)
                    .build();
                device.cmd_pipeline_barrier(
                    cmdbuf,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[barrier],
                    &[],
                );
                base_offset += amount * SAMPLES_QUERY_SIZE;
            }
        });

        let mut direct_values = Vec::new();
        let mut accumulated_values = Vec::new();
        let mut has_multi_queries = false;
        for pending in &self.pending_sync {
            let slots = pending.report.slot_count();
            if slots == 0 {
                continue;
            }
            let Some((bank_id, slot)) = pending.report.last_slot() else {
                continue;
            };
            let Some(&(range_start, range_offset)) = offsets.get(&bank_id) else {
                continue;
            };
            let value = HostSyncValues {
                address: pending.address,
                size: SAMPLES_QUERY_SIZE as u64,
                offset: (range_offset + (slot - range_start) * SAMPLES_QUERY_SIZE) as u64,
            };
            if self.accumulation_since_last_sync || slots > 1 {
                has_multi_queries = true;
                accumulated_values.push(value);
            } else {
                direct_values.push(value);
            }
        }
        if !direct_values.is_empty() {
            self.sync_values_stash.push((direct_values, resolve_buffer));
        }
        if has_multi_queries {
            self.sync_values_stash
                .push((accumulated_values, intermediary_buffer));
            let (min_accumulation_limit, max_accumulation_limit) = accumulation_limits(
                self.first_accumulation_checkpoint,
                self.last_accumulation_checkpoint,
                self.num_slots_used,
            );
            self.prefix_scan_pass.run(
                self.accumulation_buffer,
                intermediary_buffer,
                resolve_buffer,
                self.num_slots_used,
                min_accumulation_limit,
                max_accumulation_limit,
            );
        } else {
            let accumulation_buffer = self.accumulation_buffer;
            let device = self.device.clone();
            scheduler.request_outside_render_pass_operation_context();
            scheduler.record(move |cmdbuf| unsafe {
                device.cmd_fill_buffer(
                    cmdbuf,
                    accumulation_buffer,
                    0,
                    SAMPLES_QUERY_SIZE as u64,
                    0,
                );
            });
        }

        let replicated_history = {
            let mut state = self.state.lock();
            let history = clone_cumulative_history(&state.history, |bank| {
                bank.add_reference(1, scheduler);
            });
            state.abandon_history();
            history
        };
        if !replicated_history.is_empty() {
            let report = SamplesReport::new(
                coalesce_query_spans(replicated_history),
                Arc::clone(&self.amend_value),
                Arc::clone(&self.accumulation_value),
            );
            self.queue_host_report(&report);
            sync_operation(Box::new(move || {
                let _ = report.resolve();
            }));
        }
        let amend_value = Arc::clone(&self.amend_value);
        let accumulation_value = Arc::clone(&self.accumulation_value);
        sync_operation(Box::new(move || {
            amend_value.store(
                accumulation_value.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
        }));

        self.num_slots_used = 0;
        self.first_accumulation_checkpoint = usize::MAX;
        self.last_accumulation_checkpoint = 0;
        self.accumulation_since_last_sync = has_multi_queries;
        self.pending_sync.clear();
    }

    fn sync_writes(&mut self, runtime: &mut QueryCacheRuntime) {
        for (values, source_buffer) in self.sync_values_stash.drain(..) {
            runtime.sync_host_values(&values, source_buffer);
        }
    }

    fn queue_host_report(&mut self, report: &SamplesReport) {
        self.pending_flush_queries.push(Arc::clone(&report.state));
    }

    fn has_unsynced_queries(&self) -> bool {
        !self.pending_flush_queries.is_empty()
    }

    /// Port of `SamplesStreamer::PushUnsyncedQueries`.
    fn push_unsynced_queries(&mut self, scheduler: &mut Scheduler) {
        self.state.lock().pause_counter(scheduler);
        if let Some(current_bank) = self.current_bank.as_ref() {
            current_bank.close();
        }
        self.pending_flush_sets
            .lock()
            .push_back(std::mem::take(&mut self.pending_flush_queries));
    }

    fn should_wait_async_flushes(&self) -> bool {
        self.pending_flush_sets
            .lock()
            .front()
            .is_some_and(|reports| !reports.is_empty())
    }

    /// Port of `SamplesStreamer::PopUnsyncedQueries`: sync each bank range
    /// once, then sum the results of every query in the set.
    fn pop_unsynced_queries(&mut self) {
        let Some(reports) = self.pending_flush_sets.lock().pop_front() else {
            return;
        };
        if sync_report_ranges(&reports) {
            for report in reports {
                report.resolve_from_synced_results();
            }
        }
    }
}

/// Snapshot the whole current query while retaining its original history.
/// Each copy takes the independent bank reference that upstream `WriteCounter`
/// adds to the newly built host query.
fn clone_cumulative_history<T>(
    history: &[(Arc<T>, usize)],
    mut add_reference: impl FnMut(&Arc<T>),
) -> Vec<(Arc<T>, usize)> {
    history
        .iter()
        .map(|(bank, slot)| {
            add_reference(bank);
            (Arc::clone(bank), *slot)
        })
        .collect()
}

/// Coalesce per-slot acquisitions into upstream's `(bank, start, amount)`
/// ranges, so a query that filled contiguous slots of one bank is synced with a
/// single `vkGetQueryPoolResults` like upstream's `ApplyBanksWideOp`.
///
/// Reference ownership moves from the acquisition list into the spans: each
/// entry contributed one `AddReference`, and the span closes `amount` of them.
fn coalesce_query_spans(history: Vec<(Arc<SamplesQueryBank>, usize)>) -> Vec<SamplesQuerySpan> {
    let mut spans: Vec<SamplesQuerySpan> = Vec::new();
    for (bank, slot) in history {
        if let Some(last) = spans.last_mut() {
            if Arc::ptr_eq(&last.bank, &bank) && last.start + last.amount == slot {
                last.amount += 1;
                continue;
            }
        }
        spans.push(SamplesQuerySpan {
            bank,
            start: slot,
            amount: 1,
        });
    }
    spans
}

/// Extend the min/max range associated with one bank.
///
/// This is upstream `ApplyBanksWideOp`'s indexing operation. `value` carries
/// the object needed by the caller while `key` identifies it; keeping the
/// merge mechanical makes the range behavior independently testable.
fn include_wide_range<K: Eq, V>(
    ranges: &mut Vec<(K, V, usize, usize)>,
    key: K,
    value: V,
    start: usize,
    amount: usize,
) {
    if let Some((_, _, range_start, range_end)) =
        ranges.iter_mut().find(|(current, _, _, _)| current == &key)
    {
        *range_start = (*range_start).min(start);
        *range_end = (*range_end).max(start + amount);
    } else {
        ranges.push((key, value, start, start + amount));
    }
}

/// Port of `SamplesStreamer::ApplyBanksWideOp<false>` as used by
/// `PopUnsyncedQueries`: merge all report spans by bank before issuing host
/// readbacks.
fn sync_report_ranges(reports: &[Arc<SamplesReportState>]) -> bool {
    let mut ranges: Vec<(usize, Arc<SamplesQueryBank>, usize, usize)> = Vec::new();
    for report in reports {
        for span in &report.spans {
            include_wide_range(
                &mut ranges,
                Arc::as_ptr(&span.bank) as usize,
                Arc::clone(&span.bank),
                span.start,
                span.amount,
            );
        }
    }
    for (_, bank, start, end) in ranges {
        if let Err(error) = bank.sync(start, end - start) {
            if error == vk::Result::ERROR_DEVICE_LOST {
                crate::vulkan_common::vulkan_device::report_device_loss();
            }
            log::error!("vkGetQueryPoolResults failed for occlusion query: {error:?}");
            return false;
        }
    }
    true
}

/// Ordered counterpart of upstream `ApplyBanksWideOp<true>` used by
/// `PresyncWrites`: bank id order determines the resolve-buffer layout.
fn ordered_pending_ranges(
    pending: &[SamplesPendingSync],
) -> Vec<(Arc<SamplesQueryBank>, usize, usize)> {
    let mut ranges: BTreeMap<usize, (Arc<SamplesQueryBank>, usize, usize)> = BTreeMap::new();
    for pending_query in pending {
        for span in &pending_query.report.state.spans {
            let entry = ranges
                .entry(span.bank.index)
                .or_insert_with(|| (Arc::clone(&span.bank), usize::MAX, usize::MIN));
            entry.1 = entry.1.min(span.start);
            entry.2 = entry.2.max(span.start + span.amount);
        }
    }
    ranges
        .into_values()
        .map(|(bank, start, end)| (bank, start, end - start))
        .collect()
}

struct SamplesReportState {
    spans: Vec<SamplesQuerySpan>,
    resolved: parking_lot::Mutex<Option<u64>>,
    amend_value: Arc<AtomicU64>,
    accumulation_value: Arc<AtomicU64>,
}

impl SamplesReportState {
    /// Finish upstream `PopUnsyncedQueries` after `ApplyBanksWideOp` populated
    /// the bank result arrays.
    fn resolve_from_synced_results(&self) {
        let total = self.spans.iter().fold(0u64, |total, span| {
            total.wrapping_add(span.bank.sum_results(span.start, span.amount))
        });
        *self.resolved.lock() = Some(total);
    }

    fn slot_count(&self) -> usize {
        self.spans.iter().map(|span| span.amount).sum()
    }

    fn last_slot(&self) -> Option<(usize, usize)> {
        self.spans
            .last()
            .map(|span| (span.bank.index, span.start + span.amount - 1))
    }
}

#[derive(Clone)]
struct SamplesReport {
    state: Arc<SamplesReportState>,
}

impl SamplesReport {
    fn new(
        spans: Vec<SamplesQuerySpan>,
        amend_value: Arc<AtomicU64>,
        accumulation_value: Arc<AtomicU64>,
    ) -> Self {
        Self {
            state: Arc::new(SamplesReportState {
                spans,
                resolved: parking_lot::Mutex::new(None),
                amend_value,
                accumulation_value,
            }),
        }
    }

    fn slot_count(&self) -> usize {
        self.state.slot_count()
    }

    fn resolve(self) -> Option<u64> {
        let value = self
            .state
            .resolved
            .lock()
            .map(|value| value.wrapping_add(self.state.amend_value.load(Ordering::Relaxed)))?;
        self.state
            .accumulation_value
            .store(value, Ordering::Relaxed);
        Some(value)
    }

    fn last_slot(&self) -> Option<(usize, usize)> {
        self.state.last_slot()
    }
}

struct PrimitivesReport {
    counter: PrimitivesCounter,
    stride: u64,
    topology: PrimitiveTopology,
    patch_vertices: u32,
}

enum PrimitivesCounter {
    None,
    TransformFeedback(TfbReport),
    GuestMemory {
        device_memory: Arc<crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager>,
        address: u64,
    },
}

impl PrimitivesReport {
    fn resolve(self) -> u64 {
        let byte_count = match self.counter {
            PrimitivesCounter::None => return 0,
            PrimitivesCounter::TransformFeedback(counter) => counter.resolve(),
            PrimitivesCounter::GuestMemory {
                device_memory,
                address,
            } => u64::from(device_memory.read_u32(address)),
        };
        let stride = self.stride.max(1);
        if self.stride == 0 {
            log::warn!("Transform-feedback query has stride 0; using 1 to avoid division by zero");
        }
        primitives_from_vertices(byte_count / stride, self.topology, self.patch_vertices)
    }
}

fn primitives_from_vertices(
    num_vertices: u64,
    topology: PrimitiveTopology,
    patch_vertices: u32,
) -> u64 {
    match topology {
        PrimitiveTopology::Points => num_vertices,
        PrimitiveTopology::Lines => num_vertices / 2,
        PrimitiveTopology::LineLoop => u64::from(num_vertices > 1) * num_vertices,
        PrimitiveTopology::LineStrip => num_vertices.saturating_sub(1),
        PrimitiveTopology::LinesAdjacency => num_vertices / 4,
        PrimitiveTopology::LineStripAdjacency => num_vertices.saturating_sub(3),
        PrimitiveTopology::Triangles => num_vertices / 3,
        PrimitiveTopology::TrianglesAdjacency => num_vertices / 6,
        PrimitiveTopology::TriangleFan | PrimitiveTopology::TriangleStrip => {
            num_vertices.saturating_sub(2)
        }
        PrimitiveTopology::TriangleStripAdjacency => {
            if num_vertices > 4 {
                (num_vertices - 4) / 2
            } else {
                0
            }
        }
        PrimitiveTopology::Quads => num_vertices / 4,
        PrimitiveTopology::QuadStrip => {
            if num_vertices > 2 {
                (num_vertices - 2) / 2
            } else {
                0
            }
        }
        PrimitiveTopology::Polygon => u64::from(num_vertices >= 3),
        PrimitiveTopology::Patches => num_vertices / u64::from(patch_vertices.max(1)),
        _ => num_vertices,
    }
}

enum HostQueryReport {
    Samples(SamplesReport),
    TransformFeedback(TfbReport),
    Primitives(PrimitivesReport),
    #[cfg(test)]
    Test(u64),
}

fn is_host_query_report_synchronized(
    is_fence: bool,
    gpu_level_high: bool,
    fence_behavior: common::settings::GpuFenceBehavior,
) -> bool {
    if !is_fence {
        return true;
    }
    match fence_behavior {
        common::settings::GpuFenceBehavior::Default => gpu_level_high,
        common::settings::GpuFenceBehavior::Immediate => false,
        common::settings::GpuFenceBehavior::Balanced
        | common::settings::GpuFenceBehavior::Accurate
        | common::settings::GpuFenceBehavior::Strict => true,
    }
}

fn unsupported_query_payload(query_type: u32) -> u32 {
    if query_type == QueryType::StreamingPrimitivesNeededMinusSucceeded as u32 {
        0
    } else {
        1
    }
}

fn effective_query_type_and_payload(query_type: u32, payload: u32) -> (u32, u32) {
    let has_samples_streamer = query_type == QueryType::ZPassPixelCount64 as u32;
    let has_tfb_streamer = query_type == QueryType::StreamingByteCount as u32;
    let has_primitives_streamer = query_type == QueryType::StreamingPrimitivesNeeded as u32
        || query_type == QueryType::VtgPrimitivesOut as u32
        || query_type == QueryType::StreamingPrimitivesSucceeded as u32;
    let has_stub_streamer = query_type == QueryType::StreamingPrimitivesNeededMinusSucceeded as u32;
    let has_payload_streamer = query_type == QueryType::Payload as u32;
    if has_samples_streamer
        || has_payload_streamer
        || has_tfb_streamer
        || has_primitives_streamer
        || has_stub_streamer
    {
        (
            query_type,
            if has_samples_streamer
                || has_tfb_streamer
                || has_primitives_streamer
                || has_stub_streamer
            {
                0
            } else {
                payload
            },
        )
    } else {
        (
            QueryType::Payload as u32,
            unsupported_query_payload(query_type),
        )
    }
}

impl HostQueryReport {
    fn resolve(self) -> Option<u64> {
        match self {
            Self::Samples(report) => report.resolve(),
            Self::TransformFeedback(report) => Some(report.resolve()),
            Self::Primitives(report) => Some(report.resolve()),
            #[cfg(test)]
            Self::Test(value) => Some(value),
        }
    }
}

// ---------------------------------------------------------------------------
// Transform-feedback counter streamer
// ---------------------------------------------------------------------------

const TFB_QUERY_BANK_SIZE: usize = 1024;
const TFB_QUERY_SIZE: vk::DeviceSize = 4;
const NUM_TFB_STREAMS: usize = 4;
const NUM_TRANSFORM_FEEDBACK_BUFFERS: usize = 4;
const INVALID_TFB_SLOT: usize = NUM_TFB_STREAMS;

struct TfbQueryBankSlots {
    free: Vec<u32>,
    in_use: usize,
    last_used_tick: u64,
}

/// Port of upstream `TFBQueryBank`.
///
/// Eden copies transform-feedback counter values into a device-local bank and
/// later stages that bank for host access. The Rust owner keeps a matching
/// device-local bank plus a persistently mapped readback mirror so the fence
/// callback can resolve one slot without retaining a mutable staging-pool
/// borrow across the asynchronous operation.
struct TfbQueryBank {
    device: ash::Device,
    buffer: vk::Buffer,
    readback: MappedBuffer,
    slots: parking_lot::Mutex<TfbQueryBankSlots>,
    host_access: parking_lot::Mutex<()>,
}

impl TfbQueryBank {
    fn new(
        device: ash::Device,
        memory_allocator: &MemoryAllocator,
    ) -> Result<Arc<Self>, vk::Result> {
        let size = TFB_QUERY_SIZE * TFB_QUERY_BANK_SIZE as u64;
        let device_info = vk::BufferCreateInfo::builder()
            .size(size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .build();
        let buffer = memory_allocator
            .create_buffer(&device_info, MemoryUsage::DeviceLocal)
            .map_err(|error| error.result)?;
        let readback_info = vk::BufferCreateInfo::builder()
            .size(size)
            .usage(vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .build();
        let readback = memory_allocator
            .create_mapped_buffer(&readback_info, MemoryUsage::Download)
            .map_err(|error| error.result)?;
        Ok(Arc::new(Self {
            device,
            buffer,
            readback,
            slots: parking_lot::Mutex::new(TfbQueryBankSlots {
                free: (0..TFB_QUERY_BANK_SIZE as u32).rev().collect(),
                in_use: 0,
                last_used_tick: 0,
            }),
            host_access: parking_lot::Mutex::new(()),
        }))
    }

    fn reserve(&self, scheduler: &Scheduler) -> Option<u32> {
        let mut slots = self.slots.lock();
        if slots.free.is_empty() && slots.in_use == 0 && scheduler.is_free(slots.last_used_tick) {
            slots.free = (0..TFB_QUERY_BANK_SIZE as u32).rev().collect();
        }
        let slot = slots.free.pop()?;
        slots.in_use += 1;
        slots.last_used_tick = scheduler.pending_tick();
        Some(slot)
    }

    fn release(&self) {
        let mut slots = self.slots.lock();
        slots.in_use = slots
            .in_use
            .checked_sub(1)
            .expect("transform-feedback query bank reference count underflow");
    }

    fn resolve(&self, slot: u32) -> u32 {
        let _host_access = self.host_access.lock();
        self.readback.invalidate();
        let offset = slot as usize * TFB_QUERY_SIZE as usize;
        u32::from_le_bytes(
            self.readback.mapped_slice()[offset..offset + TFB_QUERY_SIZE as usize]
                .try_into()
                .expect("transform-feedback result slot has four bytes"),
        )
    }
}

struct TfbQueryLease {
    bank: Arc<TfbQueryBank>,
    slot: u32,
}

impl Drop for TfbQueryLease {
    fn drop(&mut self) {
        self.bank.release();
    }
}

#[derive(Clone)]
struct TfbReport(Arc<TfbQueryLease>);

impl TfbReport {
    fn resolve(self) -> u64 {
        self.0.bank.resolve(self.0.slot) as u64
    }
}

#[derive(Clone, Copy)]
struct TfbCounterConfig {
    buffers_count: usize,
    streams_mask: u64,
    stream_to_slot: [usize; NUM_TFB_STREAMS],
    strides: [usize; NUM_TFB_STREAMS],
}

impl Default for TfbCounterConfig {
    fn default() -> Self {
        Self {
            buffers_count: 0,
            streams_mask: 0,
            stream_to_slot: [INVALID_TFB_SLOT; NUM_TFB_STREAMS],
            strides: [1; NUM_TFB_STREAMS],
        }
    }
}

fn make_tfb_counter_config(
    feedback_state: crate::transform_feedback::TransformFeedbackState,
    buffers: [crate::engines::maxwell_3d::TransformFeedbackBufferInfo;
        NUM_TRANSFORM_FEEDBACK_BUFFERS],
) -> TfbCounterConfig {
    let mut config = TfbCounterConfig::default();
    for index in 0..NUM_TRANSFORM_FEEDBACK_BUFFERS {
        if buffers[index].enable == 0 {
            continue;
        }
        config.buffers_count = config.buffers_count.max(index + 1);
        let stream = feedback_state.layouts[index].stream as usize;
        if stream >= NUM_TFB_STREAMS {
            log::warn!("Transform-feedback stream {stream} is out of range");
            continue;
        }
        if config.streams_mask & (1 << stream) != 0 {
            continue;
        }
        config.strides[stream] = feedback_state.layouts[index].stride as usize;
        config.stream_to_slot[stream] = index;
        config.streams_mask |= 1 << stream;
    }
    config
}

pub(crate) struct TfbCounterState {
    device: ash::Device,
    transform_feedback: Option<vk::ExtTransformFeedbackFn>,
    counters_buffer: vk::Buffer,
    counter_buffers: [vk::Buffer; NUM_TFB_STREAMS],
    offsets: [vk::DeviceSize; NUM_TFB_STREAMS],
    maxwell3d: Option<usize>,
    config: TfbCounterConfig,
    last_queries: [Option<(u64, usize)>; NUM_TFB_STREAMS],
    has_started: bool,
    has_flushed_end_pending: bool,
}

impl TfbCounterState {
    fn bind_3d_engine(&mut self, maxwell3d: Option<usize>) {
        self.maxwell3d = maxwell3d.filter(|address| *address != 0);
    }

    fn update_buffers(&mut self) {
        self.last_queries.fill(None);
        let Some(maxwell3d) = self.maxwell3d else {
            self.config = TfbCounterConfig::default();
            return;
        };
        // `ChannelSetupCaches` stores the address of the channel-owned boxed
        // Maxwell3D. The box remains stable until the channel is erased, when
        // this pointer is cleared by `bind_3d_engine(None)`.
        let maxwell3d = unsafe { &*(maxwell3d as *const crate::engines::maxwell_3d::Maxwell3D) };
        self.config = make_tfb_counter_config(
            maxwell3d.transform_feedback_state(),
            std::array::from_fn(|index| maxwell3d.transform_feedback_buffer_info(index as u32)),
        );
    }

    fn transform_feedback_enabled(&self) -> bool {
        let Some(maxwell3d) = self.maxwell3d else {
            return false;
        };
        let maxwell3d = unsafe { &*(maxwell3d as *const crate::engines::maxwell_3d::Maxwell3D) };
        maxwell3d.transform_feedback_enabled()
    }

    fn tessellation_enabled(&self) -> bool {
        let Some(maxwell3d) = self.maxwell3d else {
            return false;
        };
        let maxwell3d = unsafe { &*(maxwell3d as *const crate::engines::maxwell_3d::Maxwell3D) };
        maxwell3d.shader_config_enabled(crate::engines::maxwell_3d::ShaderStageType::TessInit)
            || maxwell3d
                .shader_config_enabled(crate::engines::maxwell_3d::ShaderStageType::Tessellation)
    }

    fn primitives_state(&mut self, stream: usize) -> (PrimitiveTopology, u32, u64) {
        let Some(maxwell3d) = self.maxwell3d else {
            return (PrimitiveTopology::Points, 1, 1);
        };
        let maxwell3d = unsafe { &*(maxwell3d as *const crate::engines::maxwell_3d::Maxwell3D) };
        let mut topology = maxwell3d.draw_manager_topology();
        let patch_vertices = maxwell3d.patch_vertices().max(1);
        if topology == PrimitiveTopology::Patches {
            topology = match maxwell3d.tessellation_output_primitives() {
                0 => PrimitiveTopology::Points,
                1 => PrimitiveTopology::LineStrip,
                2 | 3 => PrimitiveTopology::TriangleStrip,
                _ => unreachable!("tessellation output primitive is a two-bit field"),
            };
        }
        let stride = self.config.strides.get(stream).copied().unwrap_or(1) as u64;
        (topology, patch_vertices, stride)
    }

    fn start_counter(&mut self, scheduler: &mut Scheduler) {
        if self.transform_feedback.is_none() {
            return;
        }
        self.flush_begin_tfb(scheduler);
        self.has_started = true;
    }

    fn flush_begin_tfb(&mut self, scheduler: &mut Scheduler) {
        if self.transform_feedback.is_none() || self.has_flushed_end_pending {
            return;
        }
        self.has_flushed_end_pending = true;
        self.update_buffers();
        let transform_feedback = self.transform_feedback.clone().unwrap();
        let buffers_count = self.config.buffers_count as u32;
        if !self.has_started || buffers_count == 0 {
            scheduler.record(move |cmdbuf| unsafe {
                (transform_feedback.cmd_begin_transform_feedback_ext)(
                    cmdbuf,
                    0,
                    0,
                    std::ptr::null(),
                    std::ptr::null(),
                );
            });
        } else {
            let buffers = self.counter_buffers;
            let offsets = self.offsets;
            let device = self.device.clone();
            scheduler.record(move |cmdbuf| unsafe {
                let barrier = vk::MemoryBarrier::builder()
                    .src_access_mask(vk::AccessFlags::TRANSFORM_FEEDBACK_COUNTER_WRITE_EXT)
                    .dst_access_mask(vk::AccessFlags::TRANSFORM_FEEDBACK_COUNTER_READ_EXT)
                    .build();
                device.cmd_pipeline_barrier(
                    cmdbuf,
                    vk::PipelineStageFlags::TRANSFORM_FEEDBACK_EXT,
                    vk::PipelineStageFlags::TRANSFORM_FEEDBACK_EXT,
                    vk::DependencyFlags::empty(),
                    &[barrier],
                    &[],
                    &[],
                );
                (transform_feedback.cmd_begin_transform_feedback_ext)(
                    cmdbuf,
                    0,
                    buffers_count,
                    buffers.as_ptr(),
                    offsets.as_ptr(),
                );
            });
        }
    }

    fn flush_end_tfb(&mut self, scheduler: &mut Scheduler) {
        let Some(transform_feedback) = self.transform_feedback.clone() else {
            return;
        };
        if !self.has_flushed_end_pending {
            return;
        }
        self.has_flushed_end_pending = false;
        self.update_buffers();
        let buffers_count = self.config.buffers_count as u32;
        let buffers = self.counter_buffers;
        let offsets = self.offsets;
        scheduler.record(move |cmdbuf| unsafe {
            if buffers_count == 0 {
                (transform_feedback.cmd_end_transform_feedback_ext)(
                    cmdbuf,
                    0,
                    0,
                    std::ptr::null(),
                    std::ptr::null(),
                );
            } else {
                (transform_feedback.cmd_end_transform_feedback_ext)(
                    cmdbuf,
                    0,
                    buffers_count,
                    buffers.as_ptr(),
                    offsets.as_ptr(),
                );
            }
        });
    }

    pub(crate) fn close_counter(&mut self, scheduler: &mut Scheduler) {
        if self.has_flushed_end_pending && scheduler.is_render_pass_active() {
            self.flush_end_tfb(scheduler);
        }
        if !self.transform_feedback_enabled() {
            self.config.streams_mask = 0;
            self.has_started = false;
        }
    }
}

struct TfbCounterStreamer {
    device: ash::Device,
    // SAFETY: RendererVulkan owns the allocator longer than RasterizerVulkan,
    // which owns this streamer. This mirrors Eden's allocator reference.
    memory_allocator: NonNull<MemoryAllocator>,
    state: Arc<parking_lot::Mutex<TfbCounterState>>,
    banks: Vec<Arc<TfbQueryBank>>,
    pending_sync: Vec<(TfbReport, u64)>,
    pending_flush_queries: Vec<TfbReport>,
    pending_flush_sets: parking_lot::Mutex<VecDeque<Vec<TfbReport>>>,
}

impl TfbCounterStreamer {
    fn new(
        instance: &ash::Instance,
        device: ash::Device,
        memory_allocator: &MemoryAllocator,
        transform_feedback_supported: bool,
    ) -> Result<Self, vk::Result> {
        let transform_feedback = transform_feedback_supported.then(|| {
            vk::ExtTransformFeedbackFn::load(|name| unsafe {
                std::mem::transmute(instance.get_device_proc_addr(device.handle(), name.as_ptr()))
            })
        });
        let usage = vk::BufferUsageFlags::TRANSFER_SRC
            | vk::BufferUsageFlags::TRANSFER_DST
            | if transform_feedback_supported {
                vk::BufferUsageFlags::TRANSFORM_FEEDBACK_COUNTER_BUFFER_EXT
            } else {
                vk::BufferUsageFlags::empty()
            };
        let create_info = vk::BufferCreateInfo::builder()
            .size(TFB_QUERY_SIZE * NUM_TFB_STREAMS as u64)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .build();
        let counters_buffer = memory_allocator
            .create_buffer(&create_info, MemoryUsage::DeviceLocal)
            .map_err(|error| error.result)?;
        Ok(Self {
            device: device.clone(),
            memory_allocator: NonNull::from(memory_allocator),
            state: Arc::new(parking_lot::Mutex::new(TfbCounterState {
                device,
                transform_feedback,
                counters_buffer,
                counter_buffers: [counters_buffer; NUM_TFB_STREAMS],
                offsets: std::array::from_fn(|index| index as u64 * TFB_QUERY_SIZE),
                maxwell3d: None,
                config: TfbCounterConfig::default(),
                last_queries: [None; NUM_TFB_STREAMS],
                has_started: false,
                has_flushed_end_pending: false,
            })),
            banks: Vec::new(),
            pending_sync: Vec::new(),
            pending_flush_queries: Vec::new(),
            pending_flush_sets: parking_lot::Mutex::new(VecDeque::new()),
        })
    }

    fn shared_state(&self) -> Arc<parking_lot::Mutex<TfbCounterState>> {
        Arc::clone(&self.state)
    }

    fn counter_enable(&mut self, scheduler: &mut Scheduler, enable: bool) {
        let mut state = self.state.lock();
        if enable {
            state.start_counter(scheduler);
        } else {
            state.close_counter(scheduler);
        }
    }

    fn close_counter(&mut self, scheduler: &mut Scheduler) {
        self.state.lock().close_counter(scheduler);
    }

    fn reserve(&mut self, scheduler: &Scheduler) -> Result<TfbReport, vk::Result> {
        for bank in &self.banks {
            if let Some(slot) = bank.reserve(scheduler) {
                return Ok(TfbReport(Arc::new(TfbQueryLease {
                    bank: Arc::clone(bank),
                    slot,
                })));
            }
        }
        // SAFETY: documented on `memory_allocator`; the renderer destroys the
        // rasterizer and its query cache before the allocator.
        let memory_allocator = unsafe { self.memory_allocator.as_ref() };
        let bank = TfbQueryBank::new(self.device.clone(), memory_allocator)?;
        let slot = bank
            .reserve(scheduler)
            .expect("a new transform-feedback query bank must have a free slot");
        self.banks.push(Arc::clone(&bank));
        Ok(TfbReport(Arc::new(TfbQueryLease { bank, slot })))
    }

    fn take_report(
        &mut self,
        scheduler: &mut Scheduler,
        address: u64,
        subreport: u32,
        sync_to_guest: bool,
    ) -> Option<TfbReport> {
        let state = self.state.lock();
        if state.transform_feedback.is_none() {
            return None;
        }
        let config = state.config;
        drop(state);
        let stream = subreport as usize;
        if stream >= NUM_TFB_STREAMS
            || config.streams_mask & (1 << stream) == 0
            || config.stream_to_slot[stream] >= NUM_TFB_STREAMS
        {
            return None;
        }
        self.state.lock().last_queries[stream] = Some((address, config.strides[stream]));
        scheduler.request_outside_render_pass_operation_context();
        self.close_counter(scheduler);
        let report = match self.reserve(scheduler) {
            Ok(report) => report,
            Err(error) => {
                log::error!("Failed to allocate transform-feedback query bank: {error:?}");
                return None;
            }
        };
        let counter_slot = config.stream_to_slot[stream];
        let (device, source, source_offset) = {
            let state = self.state.lock();
            (
                report.0.bank.device.clone(),
                state.counters_buffer,
                state.offsets[counter_slot],
            )
        };
        let bank = report.0.bank.buffer;
        let destination_offset = report.0.slot as u64 * TFB_QUERY_SIZE;
        scheduler.record(move |cmdbuf| unsafe {
            let counter_barrier = vk::MemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::TRANSFORM_FEEDBACK_COUNTER_WRITE_EXT)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .build();
            device.cmd_pipeline_barrier(
                cmdbuf,
                vk::PipelineStageFlags::TRANSFORM_FEEDBACK_EXT,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[counter_barrier],
                &[],
                &[],
            );
            device.cmd_copy_buffer(
                cmdbuf,
                source,
                bank,
                &[vk::BufferCopy {
                    src_offset: source_offset,
                    dst_offset: destination_offset,
                    size: TFB_QUERY_SIZE,
                }],
            );
        });
        if sync_to_guest {
            self.pending_sync.push((report.clone(), address));
        }
        self.pending_flush_queries.push(report.clone());
        Some(report)
    }

    fn has_pending_sync(&self) -> bool {
        !self.pending_sync.is_empty()
    }

    fn sync_writes(&mut self, scheduler: &mut Scheduler, runtime: &mut QueryCacheRuntime) {
        self.close_counter(scheduler);
        let mut values_by_bank: BTreeMap<u64, (vk::Buffer, Vec<HostSyncValues>)> = BTreeMap::new();
        for (report, address) in self.pending_sync.drain(..) {
            let bank = &report.0.bank;
            values_by_bank
                .entry(bank.buffer.as_raw())
                .or_insert_with(|| (bank.buffer, Vec::new()))
                .1
                .push(HostSyncValues {
                    address,
                    size: TFB_QUERY_SIZE,
                    offset: report.0.slot as u64 * TFB_QUERY_SIZE,
                });
        }
        for (_, (source_buffer, values)) in values_by_bank {
            runtime.sync_host_values(&values, source_buffer);
        }
    }

    fn has_unsynced_queries(&self) -> bool {
        !self.pending_flush_queries.is_empty()
    }

    fn push_unsynced_queries(&mut self, scheduler: &mut Scheduler) {
        self.close_counter(scheduler);
        if self.pending_flush_queries.is_empty() {
            return;
        }
        for report in &self.pending_flush_queries {
            let bank = &report.0.bank;
            let source = bank.buffer;
            let destination = bank.readback.buffer();
            let offset = report.0.slot as u64 * TFB_QUERY_SIZE;
            let device = bank.device.clone();
            scheduler.request_outside_render_pass_operation_context();
            scheduler.record(move |cmdbuf| unsafe {
                device.cmd_copy_buffer(
                    cmdbuf,
                    source,
                    destination,
                    &[vk::BufferCopy {
                        src_offset: offset,
                        dst_offset: offset,
                        size: TFB_QUERY_SIZE,
                    }],
                );
            });
        }
        let device = self.device.clone();
        scheduler.record(move |cmdbuf| unsafe {
            let barrier = vk::MemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
                .build();
            device.cmd_pipeline_barrier(
                cmdbuf,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::HOST,
                vk::DependencyFlags::empty(),
                &[barrier],
                &[],
                &[],
            );
        });
        self.pending_flush_sets
            .lock()
            .push_back(std::mem::take(&mut self.pending_flush_queries));
    }

    fn should_wait_async_flushes(&self) -> bool {
        self.pending_flush_sets
            .lock()
            .front()
            .is_some_and(|reports| !reports.is_empty())
    }

    fn pop_unsynced_queries(&mut self) {
        let _ = self.pending_flush_sets.lock().pop_front();
    }
}

/// Port of Eden's `PrimitivesSucceededStreamer`. It depends on the TFB byte
/// counter and converts the resulting vertex count according to Maxwell's
/// output topology when the report is resolved.
struct PrimitivesSucceededStreamer {
    device_memory: Arc<crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager>,
}

impl PrimitivesSucceededStreamer {
    fn new(
        device_memory: Arc<crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager>,
    ) -> Self {
        Self { device_memory }
    }

    fn take_report(
        &mut self,
        tfb_streamer: &mut TfbCounterStreamer,
        scheduler: &mut Scheduler,
        address: u64,
        subreport: u32,
    ) -> PrimitivesReport {
        let stream = subreport as usize;
        let (topology, patch_vertices, stride, last_query) = {
            let mut state = tfb_streamer.state.lock();
            let (topology, patch_vertices, stride) = state.primitives_state(stream);
            let last_query = state.last_queries.get(stream).copied().flatten();
            (topology, patch_vertices, stride, last_query)
        };
        let counter = if let Some((address, _)) = last_query {
            PrimitivesCounter::GuestMemory {
                device_memory: Arc::clone(&self.device_memory),
                address,
            }
        } else {
            tfb_streamer
                .take_report(scheduler, address, subreport, false)
                .map(PrimitivesCounter::TransformFeedback)
                .unwrap_or(PrimitivesCounter::None)
        };
        PrimitivesReport {
            counter,
            stride,
            topology,
            patch_vertices,
        }
    }
}

fn write_query_result(
    memory_manager: &parking_lot::Mutex<crate::memory_manager::MemoryManager>,
    gpu_addr: u64,
    has_timestamp: bool,
    value: u64,
    gpu_ticks: u64,
) {
    let mm = memory_manager.lock();
    if has_timestamp {
        mm.write_block_unsafe(gpu_addr + 8, &gpu_ticks.to_le_bytes());
        mm.write_block_unsafe(gpu_addr, &value.to_le_bytes());
    } else {
        mm.write_block_unsafe(gpu_addr, &(value as u32).to_le_bytes());
    }
}

// ---------------------------------------------------------------------------
// QueryCacheRuntime
// ---------------------------------------------------------------------------

/// Port of `QueryCacheRuntime` class.
///
/// Manages host conditional rendering, query barriers, value synchronization,
/// and streamer interfaces for all query types.
///
/// Upstream wraps the complex internal state behind a PIMPL
/// (`QueryCacheRuntimeImpl`) that contains:
/// - A vector of `SamplesQueryBank` objects (Vulkan query pools)
/// - Streamer objects for different query types
/// - Host conditional rendering state and buffers
/// - References to device, scheduler, staging pool, etc.
pub(crate) struct QueryRuntimeState {
    host_conditional_rendering_active: bool,
    host_conditional_rendering_paused: bool,
    conditional_rendering: Option<vk::ExtConditionalRenderingFn>,
    hcr_buffer: vk::Buffer,
    hcr_offset: u64,
    hcr_flags: vk::ConditionalRenderingFlagsEXT,
}

impl Default for QueryRuntimeState {
    fn default() -> Self {
        Self {
            host_conditional_rendering_active: false,
            host_conditional_rendering_paused: false,
            conditional_rendering: None,
            hcr_buffer: vk::Buffer::null(),
            hcr_offset: 0,
            hcr_flags: vk::ConditionalRenderingFlagsEXT::empty(),
        }
    }
}

impl QueryRuntimeState {
    fn clear_host_conditional_rendering(&mut self) {
        self.host_conditional_rendering_active = false;
        self.host_conditional_rendering_paused = false;
        self.hcr_buffer = vk::Buffer::null();
        self.hcr_offset = 0;
        self.hcr_flags = vk::ConditionalRenderingFlagsEXT::empty();
    }

    pub(crate) fn pause_host_conditional_rendering(
        &mut self,
    ) -> Option<vk::ExtConditionalRenderingFn> {
        if !self.host_conditional_rendering_active || self.host_conditional_rendering_paused {
            return None;
        }
        self.host_conditional_rendering_paused = true;
        self.conditional_rendering.clone()
    }

    fn resume_host_conditional_rendering(
        &mut self,
    ) -> Option<(
        vk::ExtConditionalRenderingFn,
        vk::Buffer,
        u64,
        vk::ConditionalRenderingFlagsEXT,
    )> {
        if !self.host_conditional_rendering_active || !self.host_conditional_rendering_paused {
            return None;
        }
        self.host_conditional_rendering_paused = false;
        Some((
            self.conditional_rendering.clone()?,
            self.hcr_buffer,
            self.hcr_offset,
            self.hcr_flags,
        ))
    }

    fn set_host_conditional_rendering(
        &mut self,
        buffer: vk::Buffer,
        offset: u64,
        flags: vk::ConditionalRenderingFlagsEXT,
    ) {
        self.hcr_buffer = buffer;
        self.hcr_offset = offset;
        self.hcr_flags = flags;
        self.host_conditional_rendering_active = true;
        self.host_conditional_rendering_paused = true;
    }
}

struct QueryRuntimeBackend {
    device: ash::Device,
    scheduler: NonNull<Scheduler>,
    staging_pool: NonNull<StagingBufferPool>,
    buffer_cache: NonNull<VulkanCommonBufferCache>,
    device_memory: Arc<crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager>,
    conditional_resolve_pass: Option<ConditionalRenderingResolvePass>,
    hcr_resolve_buffer: vk::Buffer,
    driver_id: vk::DriverId,
}

/// Stable non-owning link used by the runtime-owned guest streamers.
///
/// C++ stores a `QueryCacheRuntime&` in `GuestStreamer`. A Rust struct cannot
/// safely contain a reference to itself, so the backend is boxed and this
/// mechanical adapter points at that stable allocation. Ownership remains in
/// `QueryCacheRuntimeImpl`, matching upstream.
#[derive(Clone, Copy)]
struct QueryRuntimeSyncHandle(NonNull<QueryRuntimeBackend>);

impl SyncValuesRuntime for QueryRuntimeSyncHandle {
    fn sync_values(&mut self, values: Vec<SyncValuesStruct>) {
        // SAFETY: QueryCacheRuntime owns the boxed backend longer than its
        // streamers, and the box keeps the pointee stable when the runtime moves.
        unsafe { sync_guest_values(self.0.as_mut(), &values) };
    }
}

fn build_sync_value_regions(values: &[SyncValuesStruct]) -> (Vec<usize>, Vec<(u64, u64)>, usize) {
    build_sync_value_regions_from(values.iter().map(|value| (value.address, value.size)))
}

fn build_sync_value_regions_from(
    values: impl IntoIterator<Item = (u64, u64)>,
) -> (Vec<usize>, Vec<(u64, u64)>, usize) {
    const DEVICE_PAGE_SIZE: u64 = 0x1000;

    let mut redirect_cache = Vec::new();
    let mut little_cache: Vec<(u64, u64)> = Vec::new();
    let mut total_size = 0usize;
    for (address, size) in values {
        total_size += size as usize;
        let base = address & !(DEVICE_PAGE_SIZE - 1);
        let base_end = base + DEVICE_PAGE_SIZE;
        let mut found = false;
        for (index, location) in little_cache.iter_mut().enumerate() {
            let mut set_found = || {
                redirect_cache.push(index);
                found = true;
            };
            if base < location.1 && location.0 < base_end {
                set_found();
                break;
            }
            if location.0 == base_end {
                location.0 = base;
                set_found();
                break;
            }
            if location.1 == base {
                location.1 = base_end;
                set_found();
                break;
            }
        }
        if !found {
            redirect_cache.push(little_cache.len());
            little_cache.push((base, base_end));
        }
    }
    (redirect_cache, little_cache, total_size)
}

/// Port of `QueryCacheRuntime::SyncValues<SyncValuesStruct>`.
fn sync_guest_values(backend: &mut QueryRuntimeBackend, values: &[SyncValuesStruct]) {
    if values.is_empty() {
        return;
    }
    let (redirect_cache, little_cache, total_size) = build_sync_value_regions(values);

    let buffer_cache = unsafe { backend.buffer_cache.as_mut() };
    let mutex = Arc::clone(&buffer_cache.mutex);
    let _lock = mutex.lock();
    let mut destination_buffers = Vec::with_capacity(little_cache.len());
    for &(begin, end) in &little_cache {
        let Ok(size) = u32::try_from(end - begin) else {
            return;
        };
        let (buffer_id, offset) = buffer_cache.obtain_cpu_buffer(
            begin,
            size,
            ObtainBufferSynchronize::FullSynchronize,
            ObtainBufferOperation::DoNothing,
        );
        let raw_buffer = buffer_cache.resolve_backend_buffer_raw(buffer_id);
        if raw_buffer == 0 {
            return;
        }
        destination_buffers.push((vk::Buffer::from_raw(raw_buffer), u64::from(offset)));
    }
    drop(_lock);

    let Some(staging) =
        (unsafe { backend.staging_pool.as_mut() }).request_upload_buffer(total_size as u64)
    else {
        log::error!("QueryCacheRuntime::SyncValues failed to allocate an upload buffer");
        return;
    };
    let mut copies = vec![Vec::<vk::BufferCopy>::new(); little_cache.len()];
    let mut accumulated_size = 0usize;
    for (index, value) in values.iter().enumerate() {
        let size = value.size as usize;
        debug_assert!(size <= std::mem::size_of::<u64>());
        if size > std::mem::size_of::<u64>() {
            return;
        }
        let bytes = value.value.to_le_bytes();
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                staging.mapped.add(accumulated_size),
                size,
            );
        }
        let destination = redirect_cache[index];
        copies[destination].push(vk::BufferCopy {
            src_offset: staging.offset + accumulated_size as u64,
            dst_offset: destination_buffers[destination].1 + value.address
                - little_cache[destination].0,
            size: value.size,
        });
        accumulated_size += size;
    }

    let src_buffer = staging.buffer;
    let device = backend.device.clone();
    let scheduler = unsafe { backend.scheduler.as_mut() };
    scheduler.request_outside_render_pass_operation_context();
    scheduler.record(move |cmdbuf| unsafe {
        for (index, &(dst_buffer, _)) in destination_buffers.iter().enumerate() {
            device.cmd_copy_buffer(cmdbuf, src_buffer, dst_buffer, &copies[index]);
        }
    });
}

/// Port of `QueryCacheRuntime::SyncValues<HostSyncValues>` where the source
/// values already live in a Vulkan buffer produced by the query resolve.
fn sync_host_values(
    backend: &mut QueryRuntimeBackend,
    values: &[HostSyncValues],
    source_buffer: vk::Buffer,
) {
    if values.is_empty() {
        return;
    }
    let (redirect_cache, little_cache, _) =
        build_sync_value_regions_from(values.iter().map(|value| (value.address, value.size)));

    let buffer_cache = unsafe { backend.buffer_cache.as_mut() };
    let mutex = Arc::clone(&buffer_cache.mutex);
    let _lock = mutex.lock();
    let mut destination_buffers = Vec::with_capacity(little_cache.len());
    for &(begin, end) in &little_cache {
        let Ok(size) = u32::try_from(end - begin) else {
            return;
        };
        let (buffer_id, offset) = buffer_cache.obtain_cpu_buffer(
            begin,
            size,
            ObtainBufferSynchronize::FullSynchronize,
            ObtainBufferOperation::DoNothing,
        );
        let raw_buffer = buffer_cache.resolve_backend_buffer_raw(buffer_id);
        if raw_buffer == 0 {
            return;
        }
        destination_buffers.push((vk::Buffer::from_raw(raw_buffer), u64::from(offset)));
    }
    drop(_lock);

    let mut copies = vec![Vec::<vk::BufferCopy>::new(); little_cache.len()];
    for (index, value) in values.iter().enumerate() {
        let destination = redirect_cache[index];
        copies[destination].push(vk::BufferCopy {
            src_offset: value.offset,
            dst_offset: destination_buffers[destination].1 + value.address
                - little_cache[destination].0,
            size: value.size,
        });
    }
    let device = backend.device.clone();
    let scheduler = unsafe { backend.scheduler.as_mut() };
    scheduler.request_outside_render_pass_operation_context();
    scheduler.record(move |cmdbuf| unsafe {
        for (index, &(destination, _)) in destination_buffers.iter().enumerate() {
            device.cmd_copy_buffer(cmdbuf, source_buffer, destination, &copies[index]);
        }
    });
}

pub struct QueryCacheRuntime {
    guest_streamer: Option<Box<GuestStreamer<QueryRuntimeSyncHandle>>>,
    samples_streamer: Option<SamplesStreamer>,
    tfb_streamer: Option<TfbCounterStreamer>,
    primitives_succeeded_streamer: Option<PrimitivesSucceededStreamer>,
    primitives_needed_minus_succeeded_streamer: Option<Box<StubStreamer<QueryRuntimeSyncHandle>>>,
    state: Arc<parking_lot::Mutex<QueryRuntimeState>>,
    backend: Option<Box<QueryRuntimeBackend>>,
    /// Whether a 3D engine has been bound through the query-cache owner path.
    ///
    /// The concrete live engine address is held by `TfbCounterState`, which is
    /// shared with the scheduler; this flag preserves the runtime lifecycle
    /// contract exposed through `QueryCacheRuntimeHandle`.
    bound_3d_engine: bool,
}

impl QueryCacheRuntime {
    /// Port of `QueryCacheRuntime::QueryCacheRuntime`.
    ///
    /// In the full implementation, this creates the PIMPL with:
    /// - Device, scheduler, staging pool references
    /// - Buffer cache for query result storage
    /// - Compute pass descriptor queue for prefix scan
    /// - Descriptor pool for compute pass allocation
    /// - SamplesStreamer, TFBCounterStreamer, PrimitivesSucceededStreamer
    /// - ConditionalRenderingResolvePass
    /// - QueriesPrefixScanPass
    pub fn new() -> Self {
        QueryCacheRuntime {
            guest_streamer: None,
            samples_streamer: None,
            tfb_streamer: None,
            primitives_succeeded_streamer: None,
            primitives_needed_minus_succeeded_streamer: None,
            state: Arc::new(parking_lot::Mutex::new(QueryRuntimeState::default())),
            backend: None,
            bound_3d_engine: false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn new_vulkan(
        vulkan_device: &Device,
        instance: &ash::Instance,
        device: ash::Device,
        scheduler: &mut Scheduler,
        staging_pool: &mut StagingBufferPool,
        memory_allocator: &MemoryAllocator,
        buffer_cache: &mut VulkanCommonBufferCache,
        descriptor_pool: &DescriptorPool,
        compute_pass_descriptor_queue: &mut ComputePassDescriptorQueue,
        device_memory: Arc<crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager>,
        driver_id: vk::DriverId,
        transform_feedback_supported: bool,
        host_query_reset_supported: bool,
    ) -> Result<Self, vk::Result> {
        let conditional_rendering_supported = vulkan_device.is_ext_conditional_rendering();
        let conditional_rendering = conditional_rendering_supported.then(|| {
            vk::ExtConditionalRenderingFn::load(|name| unsafe {
                std::mem::transmute(instance.get_device_proc_addr(device.handle(), name.as_ptr()))
            })
        });
        let conditional_resolve_pass = if conditional_rendering_supported {
            Some(ConditionalRenderingResolvePass::new(
                vulkan_device,
                scheduler,
                descriptor_pool,
                compute_pass_descriptor_queue,
            )?)
        } else {
            None
        };
        let hcr_buffer_usage = vk::BufferUsageFlags::TRANSFER_DST
            | vk::BufferUsageFlags::STORAGE_BUFFER
            | if conditional_rendering_supported {
                vk::BufferUsageFlags::CONDITIONAL_RENDERING_EXT
            } else {
                vk::BufferUsageFlags::empty()
            };
        let hcr_resolve_buffer = memory_allocator
            .create_buffer(
                &vk::BufferCreateInfo::builder()
                    .size(std::mem::size_of::<u32>() as u64)
                    .usage(hcr_buffer_usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .build(),
                MemoryUsage::DeviceLocal,
            )
            .map_err(|error| error.result)?;
        let mut backend = Box::new(QueryRuntimeBackend {
            device: device.clone(),
            scheduler: NonNull::from(&mut *scheduler),
            staging_pool: NonNull::from(staging_pool),
            buffer_cache: NonNull::from(buffer_cache),
            device_memory: Arc::clone(&device_memory),
            conditional_resolve_pass,
            hcr_resolve_buffer,
            driver_id,
        });
        let sync_handle = QueryRuntimeSyncHandle(NonNull::from(backend.as_mut()));
        let samples_streamer = SamplesStreamer::new(
            vulkan_device,
            device.clone(),
            scheduler,
            memory_allocator,
            descriptor_pool,
            compute_pass_descriptor_queue,
            driver_id,
            host_query_reset_supported,
        )?;
        let tfb_streamer = TfbCounterStreamer::new(
            instance,
            device,
            memory_allocator,
            transform_feedback_supported,
        )?;
        Ok(Self {
            guest_streamer: Some(Box::new(GuestStreamer::new(
                QueryType::Payload as usize,
                sync_handle,
            ))),
            samples_streamer: Some(samples_streamer),
            tfb_streamer: Some(tfb_streamer),
            primitives_succeeded_streamer: Some(PrimitivesSucceededStreamer::new(device_memory)),
            primitives_needed_minus_succeeded_streamer: Some(Box::new(StubStreamer::new(
                QueryType::StreamingPrimitivesNeededMinusSucceeded as usize,
                sync_handle,
                0,
            ))),
            state: Arc::new(parking_lot::Mutex::new(QueryRuntimeState {
                conditional_rendering,
                ..QueryRuntimeState::default()
            })),
            backend: Some(backend),
            bound_3d_engine: false,
        })
    }

    pub(crate) fn shared_state(&self) -> Arc<parking_lot::Mutex<QueryRuntimeState>> {
        Arc::clone(&self.state)
    }

    fn sync_host_values(&mut self, values: &[HostSyncValues], source_buffer: vk::Buffer) {
        if let Some(backend) = self.backend.as_deref_mut() {
            sync_host_values(backend, values, source_buffer);
        }
    }

    fn sync_samples_writes(&mut self) {
        let Some(mut streamer) = self.samples_streamer.take() else {
            return;
        };
        streamer.sync_writes(self);
        self.samples_streamer = Some(streamer);
    }

    fn sync_tfb_writes(&mut self, scheduler: &mut Scheduler) {
        let Some(mut streamer) = self.tfb_streamer.take() else {
            return;
        };
        streamer.sync_writes(scheduler, self);
        self.tfb_streamer = Some(streamer);
    }

    /// Port of `QueryCacheRuntime::Barriers`.
    ///
    /// Inserts memory barriers before or after query operations.
    /// `is_prebarrier` determines direction: pre-barrier synchronizes
    /// previous writes, post-barrier makes results available for reads.
    pub fn barriers(&mut self, is_prebarrier: bool) {
        let Some(backend) = self.backend.as_mut() else {
            return;
        };
        let scheduler = unsafe { backend.scheduler.as_mut() };
        scheduler.request_outside_render_pass_operation_context();
        let device = backend.device.clone();
        scheduler.record(move |cmdbuf| unsafe {
            let (src_stage, dst_stage, barrier) = if is_prebarrier {
                (
                    vk::PipelineStageFlags::ALL_GRAPHICS
                        | vk::PipelineStageFlags::COMPUTE_SHADER
                        | vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::MemoryBarrier::builder()
                        .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                        .dst_access_mask(
                            vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::TRANSFER_WRITE,
                        )
                        .build(),
                )
            } else {
                (
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::ALL_GRAPHICS
                        | vk::PipelineStageFlags::COMPUTE_SHADER
                        | vk::PipelineStageFlags::TRANSFER
                        | vk::PipelineStageFlags::HOST,
                    vk::MemoryBarrier::builder()
                        .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                        .dst_access_mask(
                            vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE,
                        )
                        .build(),
                )
            };
            device.cmd_pipeline_barrier(
                cmdbuf,
                src_stage,
                dst_stage,
                vk::DependencyFlags::empty(),
                &[barrier],
                &[],
                &[],
            );
        });
    }

    /// Port of `QueryCacheRuntime::EndHostConditionalRendering`.
    ///
    /// Ends the current host conditional rendering scope by calling
    /// `vkCmdEndConditionalRenderingEXT`.
    pub fn end_host_conditional_rendering(&mut self) {
        self.pause_host_conditional_rendering();
        self.state.lock().clear_host_conditional_rendering();
    }

    /// Port of `QueryCacheRuntime::PauseHostConditionalRendering`.
    ///
    /// Temporarily pauses conditional rendering so that unconditional
    /// operations can be recorded.
    pub fn pause_host_conditional_rendering(&mut self) {
        let conditional_rendering = self.state.lock().pause_host_conditional_rendering();
        let (Some(conditional_rendering), Some(backend)) =
            (conditional_rendering, self.backend.as_mut())
        else {
            return;
        };
        unsafe { backend.scheduler.as_mut() }.record(move |cmdbuf| unsafe {
            (conditional_rendering.cmd_end_conditional_rendering_ext)(cmdbuf);
        });
    }

    /// Port of `QueryCacheRuntime::ResumeHostConditionalRendering`.
    ///
    /// Resumes previously paused conditional rendering.
    pub fn resume_host_conditional_rendering(&mut self) {
        let setup = self.state.lock().resume_host_conditional_rendering();
        let (Some((conditional_rendering, buffer, offset, flags)), Some(backend)) =
            (setup, self.backend.as_mut())
        else {
            return;
        };
        unsafe { backend.scheduler.as_mut() }.record(move |cmdbuf| unsafe {
            let begin_info = vk::ConditionalRenderingBeginInfoEXT::builder()
                .buffer(buffer)
                .offset(offset)
                .flags(flags)
                .build();
            (conditional_rendering.cmd_begin_conditional_rendering_ext)(cmdbuf, &begin_info);
        });
    }

    fn host_conditional_rendering_compare_value_impl(
        &mut self,
        object: LookupData,
        is_equal: bool,
    ) {
        let Some(backend) = self.backend.as_mut() else {
            return;
        };
        let buffer_cache = unsafe { backend.buffer_cache.as_mut() };
        let mutex = Arc::clone(&buffer_cache.mutex);
        let _lock = mutex.lock();
        let (buffer_id, offset) = buffer_cache.obtain_cpu_buffer(
            object.address,
            8,
            ObtainBufferSynchronize::FullSynchronize,
            ObtainBufferOperation::DoNothing,
        );
        let buffer = vk::Buffer::from_raw(buffer_cache.resolve_backend_buffer_raw(buffer_id));
        drop(_lock);
        let same_setup = {
            let state = self.state.lock();
            state.host_conditional_rendering_active
                && state.hcr_buffer == buffer
                && state.hcr_offset == u64::from(offset)
        };
        if same_setup {
            return;
        }
        let was_running = {
            let state = self.state.lock();
            state.host_conditional_rendering_active && !state.host_conditional_rendering_paused
        };
        if was_running {
            self.pause_host_conditional_rendering();
        }
        self.state.lock().set_host_conditional_rendering(
            buffer,
            u64::from(offset),
            if is_equal {
                vk::ConditionalRenderingFlagsEXT::INVERTED
            } else {
                vk::ConditionalRenderingFlagsEXT::empty()
            },
        );
        if was_running {
            self.resume_host_conditional_rendering();
        }
    }

    fn host_conditional_rendering_compare_bc_impl(
        &mut self,
        address: u64,
        is_equal: bool,
        compare_to_zero: bool,
    ) {
        let was_running = {
            let state = self.state.lock();
            state.host_conditional_rendering_active && !state.host_conditional_rendering_paused
        };
        if was_running {
            self.pause_host_conditional_rendering();
        }
        let Some(backend) = self.backend.as_mut() else {
            return;
        };
        let resolve_size = if compare_to_zero { 8 } else { 24 };
        let buffer_cache = unsafe { backend.buffer_cache.as_mut() };
        let mutex = Arc::clone(&buffer_cache.mutex);
        let _lock = mutex.lock();
        let (buffer_id, offset) = buffer_cache.obtain_cpu_buffer(
            address,
            resolve_size,
            ObtainBufferSynchronize::FullSynchronize,
            ObtainBufferOperation::DoNothing,
        );
        let src_buffer = vk::Buffer::from_raw(buffer_cache.resolve_backend_buffer_raw(buffer_id));
        drop(_lock);
        let Some(resolve_pass) = backend.conditional_resolve_pass.as_mut() else {
            return;
        };
        resolve_pass.resolve(
            backend.hcr_resolve_buffer,
            src_buffer,
            offset,
            compare_to_zero,
        );
        self.state.lock().set_host_conditional_rendering(
            backend.hcr_resolve_buffer,
            0,
            if is_equal {
                vk::ConditionalRenderingFlagsEXT::empty()
            } else {
                vk::ConditionalRenderingFlagsEXT::INVERTED
            },
        );
        if was_running {
            self.resume_host_conditional_rendering();
        }
    }

    /// Port of `QueryCacheRuntime::HostConditionalRenderingCompareValue`.
    ///
    /// Begins conditional rendering by comparing a single query result
    /// against zero. Returns true if host conditional rendering was activated.
    pub fn host_conditional_rendering_compare_value(
        &mut self,
        object_1: LookupData,
        _qc_dirty: bool,
    ) -> bool {
        if self
            .backend
            .as_ref()
            .is_none_or(|backend| backend.conditional_resolve_pass.is_none())
        {
            return false;
        }
        self.host_conditional_rendering_compare_bc_impl(object_1.address, true, true);
        true
    }

    /// Port of `QueryCacheRuntime::HostConditionalRenderingCompareValues`.
    ///
    /// Begins conditional rendering by comparing two query results.
    /// Returns true if host conditional rendering was activated.
    pub fn host_conditional_rendering_compare_values(
        &mut self,
        object_1: LookupData,
        object_2: LookupData,
        qc_dirty: bool,
        equal_check: bool,
    ) -> bool {
        let Some(backend) = self.backend.as_mut() else {
            return false;
        };
        if backend.conditional_resolve_pass.is_none() {
            return false;
        }
        let objects = [object_1, object_2];
        let mut in_query_cache = [false; 2];
        let mut in_buffer_cache = [false; 2];
        for index in 0..2 {
            in_query_cache[index] = objects[index].found_query.is_some();
            if !in_query_cache[index] {
                let buffer_cache = unsafe { backend.buffer_cache.as_ref() };
                let _lock = buffer_cache.mutex.lock();
                in_buffer_cache[index] =
                    buffer_cache.is_region_gpu_modified(objects[index].address, 8);
            }
        }
        let accelerated = [
            in_query_cache[0] || in_buffer_cache[0],
            in_query_cache[1] || in_buffer_cache[1],
        ];
        if !accelerated[0] && !accelerated[1] {
            self.end_host_conditional_rendering();
            return false;
        }
        if !qc_dirty && !in_buffer_cache[0] && !in_buffer_cache[1] {
            self.end_host_conditional_rendering();
            return false;
        }
        let is_gpu_high = common::settings::is_gpu_level_high(&common::settings::values());
        let driver_id = backend.driver_id;
        if (!is_gpu_high && driver_id == vk::DriverId::INTEL_PROPRIETARY_WINDOWS)
            || matches!(
                driver_id,
                vk::DriverId::QUALCOMM_PROPRIETARY
                    | vk::DriverId::ARM_PROPRIETARY
                    | vk::DriverId::MESA_TURNIP
            )
        {
            self.end_host_conditional_rendering();
            return true;
        }
        let mut is_null = [false; 2];
        for index in 0..2 {
            if accelerated[index] {
                continue;
            }
            let pointer = backend.device_memory.get_pointer(objects[index].address);
            is_null[index] = pointer.is_null()
                || unsafe { std::ptr::read_unaligned(pointer.cast::<u64>()) } == 0;
        }
        for index in 0..2 {
            if is_null[index] {
                self.host_conditional_rendering_compare_value_impl(
                    objects[(index + 1) % 2],
                    equal_check,
                );
                return true;
            }
        }
        if !is_gpu_high {
            self.end_host_conditional_rendering();
            return true;
        }
        if !in_buffer_cache[0] && !in_buffer_cache[1] {
            self.end_host_conditional_rendering();
            return true;
        }
        self.host_conditional_rendering_compare_bc_impl(object_1.address, equal_check, false);
        true
    }

    /// Port of `QueryCacheRuntime::Bind3DEngine`.
    ///
    /// Associates this runtime with a Maxwell3D engine instance for
    /// accessing register state during query operations.
    pub fn bind_3d_engine(&mut self) {
        // Stores reference to maxwell3d for ViewRegs access
        self.bound_3d_engine = true;
    }

    /// Returns whether host conditional rendering is currently active.
    pub fn is_host_conditional_rendering_active(&self) -> bool {
        self.state.lock().host_conditional_rendering_active
    }

    pub fn is_3d_engine_bound(&self) -> bool {
        self.bound_3d_engine
    }
}

impl QueryCacheRuntimeHandle for QueryCacheRuntime {
    fn barriers(&mut self, is_prebarrier: bool) {
        QueryCacheRuntime::barriers(self, is_prebarrier);
    }

    fn bind_3d_engine(&mut self) {
        QueryCacheRuntime::bind_3d_engine(self);
    }

    fn end_host_conditional_rendering(&mut self) {
        QueryCacheRuntime::end_host_conditional_rendering(self);
    }

    fn pause_host_conditional_rendering(&mut self) {
        QueryCacheRuntime::pause_host_conditional_rendering(self);
    }

    fn resume_host_conditional_rendering(&mut self) {
        QueryCacheRuntime::resume_host_conditional_rendering(self);
    }

    fn host_conditional_rendering_compare_value(
        &mut self,
        object_1: LookupData,
        qc_dirty: bool,
    ) -> bool {
        QueryCacheRuntime::host_conditional_rendering_compare_value(self, object_1, qc_dirty)
    }

    fn host_conditional_rendering_compare_values(
        &mut self,
        object_1: LookupData,
        object_2: LookupData,
        qc_dirty: bool,
        equal_check: bool,
    ) -> bool {
        QueryCacheRuntime::host_conditional_rendering_compare_values(
            self,
            object_1,
            object_2,
            qc_dirty,
            equal_check,
        )
    }
}

// ---------------------------------------------------------------------------
// QueryCache type alias
// ---------------------------------------------------------------------------

/// Port of `QueryCache` type alias.
///
/// In upstream: `using QueryCache = VideoCommon::QueryCacheBase<QueryCacheParams>;`
/// The generic QueryCacheBase provides the main cache logic, parameterized
/// by the Vulkan-specific runtime type.
pub struct QueryCache {
    pub base: QueryCacheBase,
    pub runtime: Box<QueryCacheRuntime>,
    /// Channel-bound GPU device memory manager. Used to translate the
    /// query's GPU virtual address to the underlying CPU/guest address
    /// when writing the query result back. Mirrors the wiring in
    /// `gl_query_cache::QueryCache`.
    channel_memory_manager: Option<Arc<parking_lot::Mutex<crate::memory_manager::MemoryManager>>>,
    gpu_memory_adapter: Option<Box<QueryGpuMemoryAdapter>>,
    device_memory_adapter: Option<Box<QueryDeviceMemoryAdapter>>,
    /// Source of the GPU tick counter for queries with timestamps.
    /// Mirrors `gl_query_cache::QueryCache::gpu_ticks_getter`.
    gpu_ticks_getter: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
}

struct QueryGpuMemoryAdapter(Arc<parking_lot::Mutex<crate::memory_manager::MemoryManager>>);

impl GpuAddressTranslator for QueryGpuMemoryAdapter {
    fn gpu_to_cpu_address(&self, gpu_addr: u64) -> Option<u64> {
        self.0.lock().gpu_to_cpu_address(gpu_addr)
    }
}

struct QueryDeviceMemoryAdapter(
    Arc<crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager>,
);

impl DeviceMemoryWriter for QueryDeviceMemoryAdapter {
    fn write_u32(&mut self, addr: u64, value: u32) {
        self.0.write_u32(addr, value);
    }

    fn write_u64(&mut self, addr: u64, value: u64) {
        self.0.write_u64(addr, value);
    }
}

impl QueryCache {
    pub fn new(
        vulkan_device: &Device,
        instance: &ash::Instance,
        device: ash::Device,
        scheduler: &mut Scheduler,
        staging_pool: &mut StagingBufferPool,
        memory_allocator: &MemoryAllocator,
        common_buffer_cache: &mut VulkanCommonBufferCache,
        descriptor_pool: &DescriptorPool,
        compute_pass_descriptor_queue: &mut ComputePassDescriptorQueue,
        device_memory: Arc<crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager>,
        driver_id: vk::DriverId,
        transform_feedback_supported: bool,
        host_query_reset_supported: bool,
    ) -> Result<Self, vk::Result> {
        let device_memory_adapter = Box::new(QueryDeviceMemoryAdapter(Arc::clone(&device_memory)));
        let runtime = Box::new(QueryCacheRuntime::new_vulkan(
            vulkan_device,
            instance,
            device,
            scheduler,
            staging_pool,
            memory_allocator,
            common_buffer_cache,
            descriptor_pool,
            compute_pass_descriptor_queue,
            device_memory,
            driver_id,
            transform_feedback_supported,
            host_query_reset_supported,
        )?);

        let mut cache = QueryCache {
            base: QueryCacheBase::new(),
            runtime,
            channel_memory_manager: None,
            gpu_memory_adapter: None,
            device_memory_adapter: Some(device_memory_adapter),
            gpu_ticks_getter: None,
        };
        cache.base.bind_runtime(cache.runtime.as_mut());
        cache.base.bind_device_memory(
            cache
                .device_memory_adapter
                .as_deref_mut()
                .expect("live Vulkan query cache always owns device memory"),
        );
        if let Some(streamer) = cache.runtime.guest_streamer.as_deref_mut() {
            cache
                .base
                .impl_
                .register_streamer(QueryType::Payload as usize, streamer);
        }
        if let Some(streamer) = cache
            .runtime
            .primitives_needed_minus_succeeded_streamer
            .as_deref_mut()
        {
            cache.base.impl_.register_streamer(
                QueryType::StreamingPrimitivesNeededMinusSucceeded as usize,
                streamer,
            );
        }
        Ok(cache)
    }

    #[cfg(test)]
    fn new_for_test() -> Self {
        Self {
            base: QueryCacheBase::new(),
            runtime: Box::new(QueryCacheRuntime::new()),
            channel_memory_manager: None,
            gpu_memory_adapter: None,
            device_memory_adapter: None,
            gpu_ticks_getter: None,
        }
    }

    pub fn create_channel(&mut self, channel: &ChannelState) {
        self.base.create_channel(channel);
    }

    pub fn bind_to_channel(&mut self, channel_id: i32) {
        self.base.bind_to_channel(channel_id);
        self.runtime.bind_3d_engine();
        if let Some(tfb_streamer) = self.runtime.tfb_streamer.as_mut() {
            tfb_streamer
                .state
                .lock()
                .bind_3d_engine(self.base.channel_caches.maxwell3d);
        }
        self.channel_memory_manager = self
            .base
            .channel_caches
            .current_channel_state()
            .and_then(ChannelCacheAccessor::gpu_memory_arc);
        self.gpu_memory_adapter = self
            .channel_memory_manager
            .as_ref()
            .map(|memory| Box::new(QueryGpuMemoryAdapter(Arc::clone(memory))));
        if let Some(adapter) = self.gpu_memory_adapter.as_deref_mut() {
            self.base.bind_gpu_memory(adapter);
        }
        if let Some(maxwell3d) = self
            .base
            .channel_caches
            .maxwell3d
            .filter(|value| *value != 0)
        {
            let source = unsafe { &mut *(maxwell3d as *mut crate::engines::maxwell_3d::Maxwell3D) };
            self.base.bind_render_condition_source(source);
        }
    }

    pub fn erase_channel(&mut self, channel_id: i32) {
        self.base.erase_channel(channel_id);
        if let Some(tfb_streamer) = self.runtime.tfb_streamer.as_mut() {
            tfb_streamer.state.lock().bind_3d_engine(None);
        }
        self.runtime.end_host_conditional_rendering();
        self.channel_memory_manager = None;
        self.gpu_memory_adapter = None;
    }

    pub fn accelerate_host_conditional_rendering(&mut self) -> bool {
        if self.channel_memory_manager.is_none() {
            self.runtime.end_host_conditional_rendering();
            return false;
        }
        self.base.accelerate_host_conditional_rendering()
    }

    /// Port of `QueryCache::NotifySegment`.
    ///
    /// Notifies the cache of a new command segment for query tracking.
    pub fn notify_segment(&mut self, is_draw: bool) {
        if is_draw {
            self.runtime.resume_host_conditional_rendering();
        } else {
            self.runtime.pause_host_conditional_rendering();
        }
    }

    /// Port of `QueryCache::CounterEnable`.
    ///
    /// Enables or disables a query counter type.
    pub fn counter_enable(&mut self, scheduler: &mut Scheduler, query_type: u32, enable: bool) {
        if query_type == crate::query_cache::types::QueryType::ZPassPixelCount64 as u32 {
            if let Some(samples_streamer) = self.runtime.samples_streamer.as_mut() {
                if enable {
                    samples_streamer.start_counter(scheduler);
                } else {
                    samples_streamer.state.lock().pause_counter(scheduler);
                }
            }
        } else if query_type == crate::query_cache::types::QueryType::StreamingByteCount as u32 {
            if let Some(tfb_streamer) = self.runtime.tfb_streamer.as_mut() {
                tfb_streamer.counter_enable(scheduler, enable);
            }
        }
    }

    pub fn transform_feedback_status(&self) -> Option<(bool, bool, bool)> {
        self.runtime.tfb_streamer.as_ref().map(|tfb_streamer| {
            let state = tfb_streamer.state.lock();
            (
                state.transform_feedback_enabled(),
                state.transform_feedback.is_some(),
                state.tessellation_enabled(),
            )
        })
    }

    pub fn transform_feedback_dispatch(&self) -> Option<vk::ExtTransformFeedbackFn> {
        self.runtime
            .tfb_streamer
            .as_ref()
            .and_then(|streamer| streamer.state.lock().transform_feedback.clone())
    }

    /// Wire the GPU tick getter used for timestamped queries.
    pub fn set_gpu_ticks_getter(&mut self, getter: Arc<dyn Fn() -> u64 + Send + Sync>) {
        self.gpu_ticks_getter = Some(getter);
    }

    pub(crate) fn samples_query_state(&self) -> Option<Arc<parking_lot::Mutex<SamplesQueryState>>> {
        self.runtime
            .samples_streamer
            .as_ref()
            .map(SamplesStreamer::shared_state)
    }

    pub(crate) fn tfb_query_state(&self) -> Option<Arc<parking_lot::Mutex<TfbCounterState>>> {
        self.runtime
            .tfb_streamer
            .as_ref()
            .map(TfbCounterStreamer::shared_state)
    }

    pub(crate) fn query_runtime_state(&self) -> Arc<parking_lot::Mutex<QueryRuntimeState>> {
        self.runtime.shared_state()
    }

    /// Port of `QueryCacheBase::CounterClose`, which calls `CloseCounter` on
    /// the streamer owning `query_type`.
    pub fn counter_close(&mut self, scheduler: &mut Scheduler, query_type: u32) {
        if query_type == crate::query_cache::types::QueryType::ZPassPixelCount64 as u32 {
            if let Some(samples_streamer) = self.runtime.samples_streamer.as_mut() {
                samples_streamer.close_counter(scheduler);
            }
        } else if query_type == crate::query_cache::types::QueryType::StreamingByteCount as u32 {
            if let Some(tfb_streamer) = self.runtime.tfb_streamer.as_mut() {
                tfb_streamer.close_counter(scheduler);
            }
        }
    }

    /// Port of `QueryCacheBase::CounterReset`.
    pub fn counter_reset(
        &mut self,
        scheduler: &mut Scheduler,
        query_type: u32,
        sync_operation: impl FnOnce(Box<dyn FnOnce() + Send>),
    ) {
        if query_type == crate::query_cache::types::QueryType::ZPassPixelCount64 as u32 {
            if let Some(samples_streamer) = self.runtime.samples_streamer.as_mut() {
                samples_streamer.reset_counter(scheduler, sync_operation);
            }
        } else if query_type == crate::query_cache::types::QueryType::StreamingByteCount as u32
            || query_type == crate::query_cache::types::QueryType::StreamingPrimitivesNeeded as u32
            || query_type == crate::query_cache::types::QueryType::VtgPrimitivesOut as u32
            || query_type
                == crate::query_cache::types::QueryType::StreamingPrimitivesSucceeded as u32
        {
            if let Some(tfb_streamer) = self.runtime.tfb_streamer.as_mut() {
                tfb_streamer.close_counter(scheduler);
            }
        }
    }
    pub fn invalidate_region(&mut self, addr: u64, size: usize) {
        self.base.invalidate_region(addr, size);
    }
    pub fn flush_region(&mut self, addr: u64, size: usize) {
        self.base.flush_region(addr, size);
    }
    pub fn notify_wfi(
        &mut self,
        scheduler: &mut Scheduler,
        mut sync_operation: impl FnMut(Box<dyn FnOnce() + Send>),
    ) {
        let samples_pending = self
            .runtime
            .samples_streamer
            .as_ref()
            .is_some_and(SamplesStreamer::has_pending_sync);
        let tfb_pending = self
            .runtime
            .tfb_streamer
            .as_ref()
            .is_some_and(TfbCounterStreamer::has_pending_sync);
        if !self.base.has_pending_sync() && !samples_pending && !tfb_pending {
            return;
        }
        self.base.presync_writes();
        if let Some(samples_streamer) = self.runtime.samples_streamer.as_mut() {
            samples_streamer.presync_writes(scheduler, &mut sync_operation);
        }
        self.runtime.barriers(true);
        self.base.sync_writes();
        self.runtime.sync_samples_writes();
        self.runtime.sync_tfb_writes(scheduler);
        self.runtime.barriers(false);
    }
    pub fn commit_async_flushes(
        &mut self,
        scheduler: &mut Scheduler,
        sync_operation: impl FnMut(Box<dyn FnOnce() + Send>),
    ) {
        self.notify_wfi(scheduler, sync_operation);
        self.base.commit_async_flushes();
        if let Some(samples_streamer) = self.runtime.samples_streamer.as_mut() {
            if samples_streamer.has_unsynced_queries() {
                samples_streamer.push_unsynced_queries(scheduler);
            }
        }
        if let Some(tfb_streamer) = self.runtime.tfb_streamer.as_mut() {
            if tfb_streamer.has_unsynced_queries() {
                tfb_streamer.push_unsynced_queries(scheduler);
            }
        }
    }
    pub fn has_uncommitted_flushes(&self) -> bool {
        self.base.has_uncommitted_flushes()
            || self
                .runtime
                .samples_streamer
                .as_ref()
                .is_some_and(SamplesStreamer::has_unsynced_queries)
            || self
                .runtime
                .tfb_streamer
                .as_ref()
                .is_some_and(TfbCounterStreamer::has_unsynced_queries)
    }
    pub fn should_wait_async_flushes(&self) -> bool {
        self.base.should_wait_async_flushes()
            || self
                .runtime
                .samples_streamer
                .as_ref()
                .is_some_and(SamplesStreamer::should_wait_async_flushes)
            || self
                .runtime
                .tfb_streamer
                .as_ref()
                .is_some_and(TfbCounterStreamer::should_wait_async_flushes)
    }
    pub fn pop_async_flushes(&mut self) {
        self.base.pop_async_flushes();
        if let Some(samples_streamer) = self.runtime.samples_streamer.as_mut() {
            samples_streamer.pop_unsynced_queries();
        }
        if let Some(tfb_streamer) = self.runtime.tfb_streamer.as_mut() {
            tfb_streamer.pop_unsynced_queries();
        }
    }

    /// Port of upstream `RasterizerVulkan::Query` →
    /// `QueryCacheBase<Vulkan>::CounterReport`. Captures a write-back
    /// closure and enqueues it via the rasterizer-provided `signal_fence`
    /// (for fence queries) or `sync_operation` (for synchronous queries).
    /// The closure runs on the GPU fence release thread once the host GPU
    /// finishes the corresponding work, and writes the query result to
    /// the guest memory address that the game polls.
    ///
    /// Mirrors `gl_query_cache::QueryCache::query`.
    pub fn query(
        &mut self,
        scheduler: &mut Scheduler,
        gpu_addr: u64,
        query_type: u32,
        flags: QueryPropertiesFlags,
        payload: u32,
        subreport: u32,
        signal_fence: impl FnOnce(Box<dyn FnOnce() + Send>),
        sync_operation: impl FnOnce(Box<dyn FnOnce() + Send>),
    ) {
        let Some(memory_manager) = self.channel_memory_manager.as_ref().cloned() else {
            return;
        };
        let Some(device_addr) = memory_manager.lock().gpu_to_cpu_address(gpu_addr) else {
            return;
        };

        let has_samples_streamer = query_type == QueryType::ZPassPixelCount64 as u32;
        let has_tfb_streamer = query_type == QueryType::StreamingByteCount as u32;
        let has_primitives_streamer = query_type == QueryType::StreamingPrimitivesNeeded as u32
            || query_type == QueryType::VtgPrimitivesOut as u32
            || query_type == QueryType::StreamingPrimitivesSucceeded as u32;
        let (effective_query_type, effective_payload) =
            effective_query_type_and_payload(query_type, payload);
        let has_timestamp = flags.contains(QueryPropertiesFlags::HAS_TIMEOUT);
        let is_fence = flags.contains(QueryPropertiesFlags::IS_A_FENCE);
        let (gpu_level_high, fence_behavior) = {
            let values = common::settings::values();
            (
                common::settings::is_gpu_level_high(&values),
                *values.gpu_fence_behavior.get_value(),
            )
        };
        let host_report_is_synchronized =
            is_host_query_report_synchronized(is_fence, gpu_level_high, fence_behavior);
        let payload_query_id = if effective_query_type == QueryType::Payload as u32 {
            self.runtime.guest_streamer.as_deref_mut().map(|streamer| {
                let query_id = streamer.write_counter(
                    device_addr,
                    has_timestamp,
                    effective_payload,
                    Some(subreport),
                );
                if is_fence {
                    if let Some(query) = streamer.get_query_mut(query_id) {
                        query
                            .flags
                            .insert(crate::query_cache::query_base::QueryFlagBits::IS_FENCE);
                    }
                }
                query_id
            })
        } else {
            None
        };
        let stub_query_id =
            if effective_query_type == QueryType::StreamingPrimitivesNeededMinusSucceeded as u32 {
                self.runtime
                    .primitives_needed_minus_succeeded_streamer
                    .as_deref_mut()
                    .map(|streamer| {
                        let query_id = streamer.write_counter(
                            device_addr,
                            has_timestamp,
                            effective_payload,
                            Some(subreport),
                        );
                        if is_fence {
                            if let Some(query) = streamer.get_query_mut(query_id) {
                                query.flags.insert(
                                    crate::query_cache::query_base::QueryFlagBits::IS_FENCE,
                                );
                            }
                        }
                        query_id
                    })
            } else {
                None
            };
        if !is_fence && effective_query_type == QueryType::Payload as u32 && !gpu_level_high {
            let gpu_ticks = if has_timestamp {
                self.gpu_ticks_getter
                    .as_ref()
                    .map(|getter| getter())
                    .unwrap_or(0)
            } else {
                0
            };
            write_query_result(
                &memory_manager,
                gpu_addr,
                has_timestamp,
                effective_payload as u64,
                gpu_ticks,
            );
            if let (Some(streamer), Some(query_id)) =
                (self.runtime.guest_streamer.as_deref_mut(), payload_query_id)
            {
                streamer.free(query_id);
            }
            return;
        }
        if let Some(query_id) = payload_query_id {
            self.base.cache_query_location(
                device_addr,
                QueryLocation::new(QueryType::Payload as u32, query_id as u32),
            );
        }
        if let Some(query_id) = stub_query_id {
            self.base.cache_query_location(
                device_addr,
                QueryLocation::new(
                    QueryType::StreamingPrimitivesNeededMinusSucceeded as u32,
                    query_id as u32,
                ),
            );
        }

        let host_report = if has_samples_streamer {
            let report = self
                .runtime
                .samples_streamer
                .as_mut()
                .and_then(|samples_streamer| samples_streamer.take_report(scheduler, device_addr));
            if let (Some(samples_streamer), Some(report)) =
                (self.runtime.samples_streamer.as_mut(), report.as_ref())
            {
                samples_streamer.queue_host_report(report);
            }
            report.map(HostQueryReport::Samples)
        } else if has_tfb_streamer {
            self.runtime
                .tfb_streamer
                .as_mut()
                .and_then(|streamer| streamer.take_report(scheduler, device_addr, subreport, true))
                .map(HostQueryReport::TransformFeedback)
        } else if has_primitives_streamer {
            self.runtime
                .primitives_succeeded_streamer
                .as_mut()
                .zip(self.runtime.tfb_streamer.as_mut())
                .map(|(primitives_streamer, tfb_streamer)| {
                    HostQueryReport::Primitives(primitives_streamer.take_report(
                        tfb_streamer,
                        scheduler,
                        device_addr,
                        subreport,
                    ))
                })
        } else {
            None
        };
        self.enqueue_query_writeback(
            gpu_addr,
            flags,
            effective_payload,
            host_report,
            host_report_is_synchronized,
            signal_fence,
            sync_operation,
        );
    }

    fn enqueue_query_writeback(
        &self,
        gpu_addr: u64,
        flags: QueryPropertiesFlags,
        payload: u32,
        host_report: Option<HostQueryReport>,
        host_report_is_synchronized: bool,
        signal_fence: impl FnOnce(Box<dyn FnOnce() + Send>),
        sync_operation: impl FnOnce(Box<dyn FnOnce() + Send>),
    ) {
        let Some(memory_manager) = self.channel_memory_manager.as_ref().cloned() else {
            return;
        };
        let has_timeout = flags.contains(QueryPropertiesFlags::HAS_TIMEOUT);
        let is_fence = flags.contains(QueryPropertiesFlags::IS_A_FENCE);
        let gpu_ticks_getter = self.gpu_ticks_getter.as_ref().cloned();
        let operation = Box::new(move || {
            if host_report.is_some() && !host_report_is_synchronized {
                log::error!(
                    "Query report value not synchronized. Consider increasing GPU accuracy."
                );
                return;
            }
            let value = if let Some(report) = host_report {
                let Some(value) = report.resolve() else {
                    log::error!(
                        "Query report value not synchronized. Consider increasing GPU accuracy."
                    );
                    return;
                };
                value
            } else {
                payload as u64
            };
            let gpu_ticks = if has_timeout {
                gpu_ticks_getter
                    .as_ref()
                    .map(|getter| getter())
                    .unwrap_or(0)
            } else {
                0
            };
            write_query_result(&memory_manager, gpu_addr, has_timeout, value, gpu_ticks);
        });
        if is_fence {
            signal_fence(operation);
        } else {
            sync_operation(operation);
        }
    }

    #[cfg(test)]
    pub fn bound_memory_manager_for_test(
        &self,
    ) -> Option<&Arc<parking_lot::Mutex<crate::memory_manager::MemoryManager>>> {
        self.channel_memory_manager.as_ref()
    }

    #[cfg(test)]
    pub fn has_bound_memory_manager_for_test(&self) -> bool {
        self.channel_memory_manager.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
    use crate::memory_manager::MemoryManager;
    use crate::query_cache::query_cache::QueryCacheRuntimeHandle;
    use crate::query_cache::query_cache_base::LookupData;
    use parking_lot::Mutex as ParkingMutex;
    use std::sync::Arc;

    fn make_query_memory_manager(
        gpu_addr: u64,
        d_addr: u64,
        size: usize,
    ) -> (Arc<ParkingMutex<MemoryManager>>, Vec<u8>) {
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let mut backing = vec![0u8; size];
        device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            d_addr,
            backing.as_mut_ptr(),
            0x4000_0000,
            size,
            5,
            true,
        );

        let mut mm = MemoryManager::new_with_geometry_and_device_memory(
            0,
            Arc::clone(&device_memory),
            40,
            1u64 << 34,
            16,
            12,
        );
        mm.map(gpu_addr, d_addr, size as u64, 0, false);
        (Arc::new(ParkingMutex::new(mm)), backing)
    }

    #[test]
    fn samples_report_becomes_available_only_after_query_sync() {
        let report = SamplesReport::new(
            Vec::new(),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        );
        assert_eq!(report.clone().resolve(), None);

        report.state.resolve_from_synced_results();

        assert_eq!(report.resolve(), Some(0));
    }

    #[test]
    fn samples_report_applies_amendment_and_updates_accumulation() {
        let amend_value = Arc::new(AtomicU64::new(9));
        let accumulation_value = Arc::new(AtomicU64::new(0));
        let report = SamplesReport::new(
            Vec::new(),
            Arc::clone(&amend_value),
            Arc::clone(&accumulation_value),
        );
        report.state.resolve_from_synced_results();

        assert_eq!(report.resolve(), Some(9));
        assert_eq!(accumulation_value.load(Ordering::Relaxed), 9);
    }

    #[test]
    fn samples_reset_checkpoints_bound_the_next_prefix_scan() {
        let mut first_checkpoint = usize::MAX;
        let mut last_checkpoint = 0;
        for slots_used in [2usize, 5] {
            first_checkpoint = first_checkpoint.min(slots_used);
            last_checkpoint = last_checkpoint.max(slots_used);
        }

        assert_eq!(
            accumulation_limits(first_checkpoint, last_checkpoint, 7),
            (2, 7)
        );
    }

    #[test]
    fn samples_reports_snapshot_the_full_history_within_one_sync_epoch() {
        let bank = Arc::new(());
        let mut history = vec![(Arc::clone(&bank), 3), (Arc::clone(&bank), 4)];
        let mut references_added = 0;

        let first = clone_cumulative_history(&history, |_| references_added += 1);
        history.push((Arc::clone(&bank), 5));
        let second = clone_cumulative_history(&history, |_| references_added += 1);

        assert_eq!(
            first.iter().map(|(_, slot)| *slot).collect::<Vec<_>>(),
            [3, 4]
        );
        assert_eq!(
            second.iter().map(|(_, slot)| *slot).collect::<Vec<_>>(),
            [3, 4, 5]
        );
        assert_eq!(history.len(), 3);
        assert_eq!(references_added, first.len() + second.len());
    }

    #[test]
    fn samples_wide_ranges_merge_all_reports_per_bank() {
        let mut ranges = Vec::new();
        include_wide_range(&mut ranges, 7u8, (), 4, 2);
        include_wide_range(&mut ranges, 9u8, (), 12, 1);
        include_wide_range(&mut ranges, 7u8, (), 2, 3);
        include_wide_range(&mut ranges, 7u8, (), 8, 2);

        assert_eq!(ranges, vec![(7, (), 2, 10), (9, (), 12, 13)]);
    }

    #[test]
    fn sync_values_group_same_and_adjacent_pages_like_upstream() {
        let values = [
            SyncValuesStruct {
                address: 0x2ff8,
                value: 1,
                size: 8,
            },
            SyncValuesStruct {
                address: 0x3000,
                value: 2,
                size: 4,
            },
            SyncValuesStruct {
                address: 0x9004,
                value: 3,
                size: 4,
            },
        ];

        let (redirects, regions, total_size) = build_sync_value_regions(&values);

        assert_eq!(redirects, vec![0, 0, 1]);
        assert_eq!(regions, vec![(0x2000, 0x4000), (0x9000, 0xa000)]);
        assert_eq!(total_size, 16);

        let host_values = values
            .iter()
            .enumerate()
            .map(|(index, value)| HostSyncValues {
                address: value.address,
                size: value.size,
                offset: (index * 8) as u64,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            build_sync_value_regions_from(
                host_values.iter().map(|value| (value.address, value.size))
            ),
            (redirects, regions, total_size)
        );
    }

    #[test]
    fn query_cache_runtime_state() {
        let mut rt = QueryCacheRuntime::new();
        assert!(!rt.is_host_conditional_rendering_active());
        rt.end_host_conditional_rendering();
        assert!(!rt.is_host_conditional_rendering_active());
    }

    #[test]
    fn inactive_host_conditional_rendering_is_not_resumed() {
        let mut state = QueryRuntimeState {
            host_conditional_rendering_active: false,
            host_conditional_rendering_paused: true,
            ..QueryRuntimeState::default()
        };

        state.resume_host_conditional_rendering();

        assert!(state.host_conditional_rendering_paused);
    }

    #[test]
    fn primitives_succeeded_matches_maxwell_topology_rules() {
        assert_eq!(
            primitives_from_vertices(9, PrimitiveTopology::Triangles, 1),
            3
        );
        assert_eq!(
            primitives_from_vertices(1, PrimitiveTopology::TriangleStrip, 1),
            0
        );
        assert_eq!(
            primitives_from_vertices(10, PrimitiveTopology::TriangleStripAdjacency, 1),
            3
        );
        assert_eq!(
            primitives_from_vertices(3, PrimitiveTopology::Polygon, 1),
            1
        );
        assert_eq!(
            primitives_from_vertices(7, PrimitiveTopology::Patches, 3),
            2
        );
        assert_eq!(
            primitives_from_vertices(7, PrimitiveTopology::Patches, 0),
            7
        );
    }

    #[test]
    fn fence_host_reports_require_delayed_gpu_accuracy() {
        use common::settings::GpuFenceBehavior;

        assert!(!is_host_query_report_synchronized(
            true,
            false,
            GpuFenceBehavior::Default
        ));
        assert!(is_host_query_report_synchronized(
            true,
            true,
            GpuFenceBehavior::Default
        ));
        assert!(!is_host_query_report_synchronized(
            true,
            true,
            GpuFenceBehavior::Immediate
        ));
        for behavior in [
            GpuFenceBehavior::Balanced,
            GpuFenceBehavior::Accurate,
            GpuFenceBehavior::Strict,
        ] {
            assert!(is_host_query_report_synchronized(true, false, behavior));
        }
        assert!(is_host_query_report_synchronized(
            false,
            false,
            GpuFenceBehavior::Immediate
        ));
    }

    #[test]
    fn primitives_needed_minus_succeeded_uses_upstream_stub_value() {
        assert_eq!(
            unsupported_query_payload(QueryType::StreamingPrimitivesNeededMinusSucceeded as u32),
            0
        );
        assert_eq!(
            effective_query_type_and_payload(
                QueryType::StreamingPrimitivesNeededMinusSucceeded as u32,
                0xDEAD_BEEF,
            ),
            (QueryType::StreamingPrimitivesNeededMinusSucceeded as u32, 0,)
        );
    }

    #[test]
    fn empty_zpass_report_uses_zero_instead_of_guest_payload() {
        assert_eq!(
            effective_query_type_and_payload(QueryType::ZPassPixelCount64 as u32, 0xDEAD_BEEF),
            (QueryType::ZPassPixelCount64 as u32, 0)
        );
        assert_eq!(
            effective_query_type_and_payload(QueryType::Payload as u32, 0xDEAD_BEEF),
            (QueryType::Payload as u32, 0xDEAD_BEEF)
        );
    }

    #[test]
    fn scan_buffers_use_upstream_minimum_size_class() {
        assert_eq!(scan_buffer_log2(1), MIN_SCAN_BUFFER_LOG2);
        assert_eq!(scan_buffer_log2(2048), MIN_SCAN_BUFFER_LOG2);
        assert_eq!(scan_buffer_log2(2049), MIN_SCAN_BUFFER_LOG2 + 1);
    }

    #[test]
    fn constants() {
        assert_eq!(SAMPLES_QUERY_BANK_SIZE, 256);
        assert_eq!(SAMPLES_QUERY_SIZE, 8);
        assert_eq!(TFB_QUERY_BANK_SIZE, 1024);
        assert_eq!(TFB_QUERY_SIZE, 4);
    }

    #[test]
    fn tfb_counter_config_matches_enabled_buffer_stream_mapping() {
        let mut feedback_state = crate::transform_feedback::TransformFeedbackState::default();
        feedback_state.layouts[0].stream = 2;
        feedback_state.layouts[0].stride = 24;
        feedback_state.layouts[3].stream = 1;
        feedback_state.layouts[3].stride = 40;
        let mut buffers = [crate::engines::maxwell_3d::TransformFeedbackBufferInfo::default(); 4];
        buffers[0].enable = 1;
        buffers[3].enable = 1;

        let config = make_tfb_counter_config(feedback_state, buffers);

        assert_eq!(config.buffers_count, 4);
        assert_eq!(config.streams_mask, (1 << 1) | (1 << 2));
        assert_eq!(
            config.stream_to_slot,
            [INVALID_TFB_SLOT, 3, 0, INVALID_TFB_SLOT]
        );
        assert_eq!(config.strides, [1, 40, 24, 1]);
    }

    #[test]
    fn runtime_trait_bridge_routes_conditional_rendering_hooks() {
        let mut rt = QueryCacheRuntime::new();
        let handle: &mut dyn QueryCacheRuntimeHandle = &mut rt;

        assert!(!handle.host_conditional_rendering_compare_value(
            LookupData {
                address: 0x1000,
                found_query: None,
            },
            false,
        ));
        assert!(!handle.host_conditional_rendering_compare_values(
            LookupData {
                address: 0x1000,
                found_query: None,
            },
            LookupData {
                address: 0x2000,
                found_query: None,
            },
            false,
            true,
        ));
        handle.end_host_conditional_rendering();
        assert!(!rt.is_host_conditional_rendering_active());
    }

    #[test]
    fn bind_to_channel_wires_memory_manager_from_channel_cache_owner() {
        let mut cache = QueryCache::new_for_test();
        let mm = Arc::new(ParkingMutex::new(MemoryManager::new(33)));
        let mut channel = ChannelState::new(8);
        channel.program_id = 0x3344;
        channel.memory_manager = Some(Arc::clone(&mm));

        cache.create_channel(&channel);
        cache.bind_to_channel(channel.bind_id);

        let bound = cache
            .bound_memory_manager_for_test()
            .expect("bound channel memory manager");
        assert!(Arc::ptr_eq(bound, &mm));
        assert_eq!(cache.base.channel_caches.program_id, 0x3344);
        assert!(cache.runtime.is_3d_engine_bound());

        cache.erase_channel(channel.bind_id);
        assert!(!cache.has_bound_memory_manager_for_test());
    }

    #[test]
    fn query_sync_operation_writes_payload_and_timestamp_through_bound_memory_manager() {
        let mut cache = QueryCache::new_for_test();
        let (mm, backing) = make_query_memory_manager(0x5038_50000, 0x5510_6000, 0x1000);

        let mut channel = ChannelState::new(9);
        channel.memory_manager = Some(Arc::clone(&mm));
        cache.create_channel(&channel);
        cache.bind_to_channel(channel.bind_id);
        cache.set_gpu_ticks_getter(Arc::new(|| 0x1122_3344_5566_7788));

        cache.enqueue_query_writeback(
            0x5038_50000,
            QueryPropertiesFlags::HAS_TIMEOUT,
            0xAABB_CCDD,
            None,
            true,
            |_func| panic!("fence path should not be used"),
            |func| func(),
        );

        assert_eq!(&backing[0..8], &(0xAABB_CCDDu64).to_le_bytes());
        assert_eq!(&backing[8..16], &0x1122_3344_5566_7788u64.to_le_bytes());
    }

    #[test]
    fn query_fence_operation_defers_writeback_to_signal_fence_callback() {
        let mut cache = QueryCache::new_for_test();
        let (mm, backing) = make_query_memory_manager(0x5038_50000, 0x5510_6000, 0x1000);

        let mut channel = ChannelState::new(10);
        channel.memory_manager = Some(Arc::clone(&mm));
        cache.create_channel(&channel);
        cache.bind_to_channel(channel.bind_id);

        let deferred = Arc::new(ParkingMutex::new(None::<Box<dyn FnOnce() + Send>>));
        let deferred_clone = Arc::clone(&deferred);

        cache.enqueue_query_writeback(
            0x5038_50000,
            QueryPropertiesFlags::IS_A_FENCE,
            0x1234_5678,
            None,
            true,
            move |func| {
                *deferred_clone.lock() = Some(func);
            },
            |_func| panic!("sync path should not be used"),
        );

        assert_eq!(&backing[0..4], &[0; 4]);

        let func = deferred.lock().take().expect("fence callback queued");
        func();

        assert_eq!(&backing[0..4], &0x1234_5678u32.to_le_bytes());
    }

    #[test]
    fn unsynchronized_fence_host_report_is_not_resolved_or_written() {
        let mut cache = QueryCache::new_for_test();
        let (mm, backing) = make_query_memory_manager(0x5038_50000, 0x5510_6000, 0x1000);
        let mut channel = ChannelState::new(11);
        channel.memory_manager = Some(mm);
        cache.create_channel(&channel);
        cache.bind_to_channel(channel.bind_id);

        cache.enqueue_query_writeback(
            0x5038_50000,
            QueryPropertiesFlags::IS_A_FENCE,
            0,
            Some(HostQueryReport::Test(0xDEAD_BEEF)),
            false,
            |func| func(),
            |_func| panic!("sync path should not be used"),
        );

        assert_eq!(&backing[0..4], &[0; 4]);
    }
}

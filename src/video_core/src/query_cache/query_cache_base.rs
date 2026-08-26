// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's `src/video_core/query_cache/query_cache_base.h`.
//!
//! Defines `QueryCacheBase`, the core query-cache manager that tracks cached
//! queries by page address, handles invalidation, flushing, and region-dirty
//! checks.  In C++ this is a CRTP template class; in Rust it is a concrete
//! struct parameterized by a `QueryCacheTraits` trait.

use std::collections::HashMap;
use std::sync::Mutex;

use common::hash::BuildUnorderedDenseHasher;
use common::settings;

use super::query_base::{QueryBase, QueryFlagBits, VAddr};
use super::query_cache::{
    DeviceMemoryWriter, GpuAddressTranslator, GpuTickSource, QueryCacheBaseImpl,
    QueryCacheRuntimeHandle, RenderConditionStateSource,
};
use super::query_stream::StreamerInterface;
use super::types::{ComparisonMode, QueryPropertiesFlags, QueryType};
use crate::control::channel_state::ChannelState;
use crate::control::channel_state_cache::{ChannelInfo, ChannelSetupCaches};
use crate::rasterizer_interface::RasterizerInterface;

// Re-export for convenience — upstream uses `Core::DEVICE_PAGEBITS`.
// The constant lives in `core::device_memory_manager` in the Rust port.
// We duplicate the value here to avoid a crate dependency cycle.
const DEVICE_PAGEBITS: u64 = 12;
const DEVICE_PAGEMASK: u64 = (1 << DEVICE_PAGEBITS) - 1;

/// GPU virtual address type alias.
pub type GPUVAddr = u64;

/// Lookup result used by conditional rendering acceleration.
///
/// Maps to C++ `LookupData`.
#[derive(Debug, Clone, Copy)]
pub struct LookupData {
    pub address: VAddr,
    pub found_query: Option<*const QueryBase>,
}

/// Packed location identifying a query within a specific streamer.
///
/// Maps to C++ `QueryCacheBase::QueryLocation` union.
///
/// Layout (matching C++ `BitField`):
///   - bits \[0..27): `query_id`
///   - bits \[27..32): `stream_id`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryLocation {
    pub raw: u32,
}

impl QueryLocation {
    /// Number of bits used for `query_id`.
    const QUERY_ID_BITS: u32 = 27;
    const QUERY_ID_MASK: u32 = (1 << Self::QUERY_ID_BITS) - 1;

    /// Create a new `QueryLocation` from stream and query IDs.
    pub fn new(stream_id: u32, query_id: u32) -> Self {
        debug_assert!(stream_id < 32, "stream_id must fit in 5 bits");
        debug_assert!(
            query_id <= Self::QUERY_ID_MASK,
            "query_id must fit in 27 bits"
        );
        Self {
            raw: (stream_id << Self::QUERY_ID_BITS) | (query_id & Self::QUERY_ID_MASK),
        }
    }

    /// Extract the stream ID (bits 27..32).
    pub fn stream_id(&self) -> usize {
        (self.raw >> Self::QUERY_ID_BITS) as usize
    }

    /// Extract the query ID (bits 0..27).
    pub fn query_id(&self) -> usize {
        (self.raw & Self::QUERY_ID_MASK) as usize
    }

    /// Unpack into `(stream_id, query_id)`.
    ///
    /// Maps to C++ `QueryLocation::unpack`.
    pub fn unpack(&self) -> (usize, usize) {
        (self.stream_id(), self.query_id())
    }
}

/// Trait that must be implemented by the runtime-specific query cache.
///
/// Maps to the `Traits` template parameter of C++ `QueryCacheBase<Traits>`.
pub trait QueryCacheTraits {
    /// The runtime type (e.g., Vulkan or OpenGL query cache runtime).
    type Runtime;

    /// Get a streamer interface by query type.
    fn get_streamer(
        runtime: &Self::Runtime,
        query_type: QueryType,
    ) -> Option<&dyn StreamerInterface>;
}

/// Page-indexed cache of query locations.
///
/// Outer key: page number (`address >> DEVICE_PAGEBITS`).
/// Inner key: page offset (`address & DEVICE_PAGEMASK`), as `u32`.
/// Value: the `QueryLocation` that wrote to that address.
pub type QueryPageCache = HashMap<u32, QueryLocation, BuildUnorderedDenseHasher>;
pub type ContentCache = HashMap<u64, QueryPageCache, BuildUnorderedDenseHasher>;

/// Core query cache state shared across all backend implementations.
///
/// Maps to C++ `QueryCacheBase<Traits>` (non-template-dependent state).
///
/// Methods that require access to the runtime-specific `impl` struct
/// (streamers, rasterizer, device memory) log a warning and return safe
/// defaults until those types are ported.
pub struct QueryCacheBase {
    /// Shared channel-cache owner.
    ///
    /// Upstream `QueryCacheBase<Traits>` inherits
    /// `ChannelSetupCaches<ChannelInfo>`. Rust carries the same owner state
    /// explicitly here.
    pub channel_caches: ChannelSetupCaches<ChannelInfo>,
    /// Page-indexed cache of active query locations.
    pub cached_queries: ContentCache,
    /// Mutex protecting `cached_queries`.
    pub cache_mutex: Mutex<()>,
    /// Inner shared owner state.
    ///
    /// Maps to upstream `QueryCacheBaseImpl`, which owns streamer mask/state,
    /// async flush queues, and pending unregister tracking.
    pub impl_: QueryCacheBaseImpl,
}

impl QueryCacheBase {
    /// Create a new empty query cache.
    pub fn new() -> Self {
        let mut this = Self {
            channel_caches: ChannelSetupCaches::new(),
            cached_queries: ContentCache::default(),
            cache_mutex: Mutex::new(()),
            impl_: QueryCacheBaseImpl::new(),
        };
        this.refresh_owner_binding();
        this
    }

    /// Create a query cache with the shared upstream owner graph already bound.
    ///
    /// This is the closest Rust owner to upstream `QueryCacheBase(gpu, rasterizer,
    /// device_memory, runtime)` until the full inherited `ChannelSetupCaches`
    /// graph is reattached.
    pub fn new_bound(
        rasterizer: &mut dyn RasterizerInterface,
        device_memory: &mut dyn DeviceMemoryWriter,
        runtime: &mut dyn QueryCacheRuntimeHandle,
        gpu_memory: &mut dyn GpuAddressTranslator,
        gpu: &mut dyn GpuTickSource,
        render_condition_source: &mut dyn RenderConditionStateSource,
    ) -> Self {
        let mut this = Self::new();
        this.bind_rasterizer(rasterizer);
        this.bind_device_memory(device_memory);
        this.bind_runtime(runtime);
        this.bind_gpu_memory(gpu_memory);
        this.bind_gpu(gpu);
        this.bind_render_condition_source(render_condition_source);
        this
    }

    fn refresh_owner_binding(&mut self) {
        let self_ptr = self as *mut Self;
        self.impl_.owner = Some(self_ptr);
    }

    pub fn bind_rasterizer(&mut self, rasterizer: &mut dyn RasterizerInterface) {
        self.impl_.bind_rasterizer(rasterizer);
    }

    pub fn bind_device_memory(&mut self, device_memory: &mut dyn DeviceMemoryWriter) {
        self.impl_.bind_device_memory(device_memory);
    }

    pub fn bind_runtime(&mut self, runtime: &mut dyn QueryCacheRuntimeHandle) {
        self.impl_.bind_runtime(runtime);
    }

    pub fn bind_gpu_memory(&mut self, gpu_memory: &mut dyn GpuAddressTranslator) {
        self.impl_.bind_gpu_memory(gpu_memory);
    }

    pub fn bind_gpu(&mut self, gpu: &mut dyn GpuTickSource) {
        self.impl_.bind_gpu(gpu);
    }

    pub fn bind_render_condition_source(
        &mut self,
        render_condition_source: &mut dyn RenderConditionStateSource,
    ) {
        self.impl_
            .bind_render_condition_source(render_condition_source);
    }

    pub fn create_channel(&mut self, channel: &ChannelState) {
        self.channel_caches.create_channel(channel);
    }

    pub fn bind_to_channel(&mut self, id: i32) {
        self.channel_caches.bind_to_channel(id);
        if let Some(runtime) = self.impl_.runtime_mut() {
            runtime.bind_3d_engine();
        }
    }

    pub fn erase_channel(&mut self, id: i32) {
        self.channel_caches.erase_channel(id);
    }

    // ── Cache-only operations (fully implementable without backend) ────

    /// Invalidate all cached queries overlapping `[addr, addr+size)`.
    ///
    /// Maps to C++ `QueryCacheBase::InvalidateRegion`:
    /// ```cpp
    /// IterateCache<true>(addr, size, [this](QueryLocation loc) {
    ///     InvalidateQuery(loc);
    /// });
    /// ```
    ///
    /// `InvalidateQuery` sets the `IsInvalidated` flag before matching entries
    /// are removed from `cached_queries` by `IterateCache<true>`.
    pub fn invalidate_region(&mut self, addr: VAddr, size: usize) {
        let impl_ = &mut self.impl_;
        Self::iterate_cache::<true, _>(
            &self.cache_mutex,
            &mut self.cached_queries,
            addr,
            size,
            |location| {
                Self::invalidate_query_impl(impl_, location);
                false
            },
        );
    }

    /// Flush all cached queries overlapping `[addr, addr+size)`.
    ///
    /// Maps to C++ `QueryCacheBase::FlushRegion`:
    /// ```cpp
    /// bool result = false;
    /// IterateCache<false>(addr, size, [&](QueryLocation loc) {
    ///     result |= SemiFlushQueryDirty(loc);
    ///     return result; // early-exit on first dirty
    /// });
    /// if (result) RequestGuestHostSync();
    /// ```
    pub fn flush_region(&mut self, addr: VAddr, size: usize) {
        let mut result = false;
        let impl_ = &mut self.impl_;
        Self::iterate_cache::<false, _>(
            &self.cache_mutex,
            &mut self.cached_queries,
            addr,
            size,
            |location| {
                result |= Self::semi_flush_query_dirty_impl(impl_, location);
                result
            },
        );
        if result {
            self.request_guest_host_sync();
        }
    }

    /// Cache the location created by a backend dispatch path.
    ///
    /// This is the indexing block at the end of upstream `CounterReport`,
    /// exposed mechanically while Vulkan still supplies the fence callbacks
    /// outside the shared owner.
    pub fn cache_query_location(&mut self, cpu_addr: VAddr, location: QueryLocation) {
        let page = cpu_addr >> DEVICE_PAGEBITS;
        let offset = (cpu_addr & DEVICE_PAGEMASK) as u32;
        let _lock = self.cache_mutex.lock().unwrap();
        let old_location = self
            .cached_queries
            .entry(page)
            .or_default()
            .insert(offset, location);
        if let Some(old_location) = old_location {
            if let Some(old_query) = self.impl_.obtain_query_mut(old_location) {
                old_query.flags |= QueryFlagBits::IS_REWRITTEN;
            }
        }
    }

    /// Build a bitmask from a slice of query types.
    ///
    /// Maps to C++ `QueryCacheBase::BuildMask`.
    pub fn build_mask(types: &[QueryType]) -> u64 {
        let mut mask: u64 = 0;
        for &query_type in types {
            mask |= 1u64 << (query_type as u64);
        }
        mask
    }

    /// Return true when a CPU region has been modified from the GPU.
    ///
    /// Maps to C++ `QueryCacheBase::IsRegionGpuModified`:
    /// ```cpp
    /// bool result = false;
    /// IterateCache<false>(addr, size, [&](QueryLocation loc) {
    ///     result |= IsQueryDirty(loc);
    ///     return result;
    /// });
    /// return result;
    /// ```
    ///
    pub fn is_region_gpu_modified(&mut self, addr: VAddr, size: usize) -> bool {
        let mut result = false;
        let impl_ = &self.impl_;
        Self::iterate_cache::<false, _>(
            &self.cache_mutex,
            &mut self.cached_queries,
            addr,
            size,
            |location| {
                result |= Self::is_query_dirty_impl(impl_, location);
                result
            },
        );
        result
    }

    // ── Streamer-dependent operations (stub with log::warn) ────────────

    /// Enable or disable a counter.
    ///
    /// Maps to C++ `QueryCacheBase::CounterEnable`:
    /// calls `StartCounter` or `PauseCounter` on the matching streamer.
    /// Requires the streamer array from `QueryCacheBaseImpl`.
    pub fn counter_enable(&mut self, _counter_type: QueryType, _is_enabled: bool) {
        let index = _counter_type as usize;
        let Some(streamer) = self.impl_.get_streamer_for_query_type_mut(index) else {
            log::warn!(
                "QueryCacheBase::counter_enable: missing streamer for query type {:?}",
                _counter_type
            );
            return;
        };
        if _is_enabled {
            streamer.start_counter();
        } else {
            streamer.pause_counter();
        }
    }

    /// Reset a counter.
    ///
    /// Maps to C++ `QueryCacheBase::CounterReset`:
    /// calls `ResetCounter` on the matching streamer.
    pub fn counter_reset(&mut self, _counter_type: QueryType) {
        let index = _counter_type as usize;
        let Some(streamer) = self.impl_.get_streamer_for_query_type_mut(index) else {
            log::warn!(
                "QueryCacheBase::counter_reset: missing streamer for query type {:?}",
                _counter_type
            );
            return;
        };
        streamer.reset_counter();
    }

    /// Close a counter.
    ///
    /// Maps to C++ `QueryCacheBase::CounterClose`:
    /// calls `CloseCounter` on the matching streamer.
    pub fn counter_close(&mut self, _counter_type: QueryType) {
        let index = _counter_type as usize;
        let Some(streamer) = self.impl_.get_streamer_for_query_type_mut(index) else {
            log::warn!(
                "QueryCacheBase::counter_close: missing streamer for query type {:?}",
                _counter_type
            );
            return;
        };
        streamer.close_counter();
    }

    /// Report a counter value to the given GPU virtual address.
    ///
    /// Maps to C++ `QueryCacheBase::CounterReport`.
    ///
    pub fn counter_report(
        &mut self,
        addr: GPUVAddr,
        mut counter_type: QueryType,
        flags: QueryPropertiesFlags,
        mut payload: u32,
        subreport: u32,
    ) {
        self.refresh_owner_binding();
        let has_timestamp = flags.contains(QueryPropertiesFlags::HAS_TIMEOUT);
        let is_fence = flags.contains(QueryPropertiesFlags::IS_A_FENCE);

        let mut streamer_id = counter_type as usize;
        if self
            .impl_
            .get_streamer_for_query_type(streamer_id)
            .is_none()
        {
            counter_type = QueryType::Payload;
            payload = 1;
            streamer_id = counter_type as usize;
        }

        let Some(cpu_addr) = self
            .impl_
            .gpu_memory()
            .and_then(|gpu_memory| gpu_memory.gpu_to_cpu_address(addr))
        else {
            return;
        };

        let Some(streamer_ptr) = self.impl_.streamers[streamer_id] else {
            return;
        };
        let streamer = unsafe { &mut *streamer_ptr };
        let new_query_id =
            streamer.write_counter(cpu_addr, has_timestamp, payload, Some(subreport));
        if let Some(query) = streamer.get_query_mut(new_query_id) {
            if is_fence {
                query.flags |= QueryFlagBits::IS_FENCE;
            }
        }

        let query_location = QueryLocation::new(streamer_id as u32, new_query_id as u32);
        let values = settings::values();
        let gpu_level_high = settings::is_gpu_level_high(&values);
        let is_synced = (if settings::is_gpu_fence_behavior_default(&values) {
            !gpu_level_high
        } else {
            !settings::is_gpu_fence_behavior_balanced(&values)
                && !settings::is_gpu_fence_behavior_accurate(&values)
                && !settings::is_gpu_fence_behavior_strict(&values)
        }) && is_fence;
        drop(values);

        if !is_fence && !gpu_level_high && counter_type == QueryType::Payload {
            let timestamp = if has_timestamp {
                self.impl_.gpu_mut().map(|gpu| gpu.get_ticks()).unwrap_or(0)
            } else {
                0
            };
            if let Some(device_memory) = self.impl_.device_memory_mut() {
                if has_timestamp {
                    device_memory.write_u64(cpu_addr.wrapping_add(8), timestamp);
                    device_memory.write_u64(cpu_addr, payload as u64);
                } else {
                    device_memory.write_u32(cpu_addr, payload);
                }
            }
            streamer.free(new_query_id);
            return;
        }

        let owner = self as *mut Self as usize;
        let operation = Box::new(move || {
            let owner = owner as *mut QueryCacheBase;
            let owner = unsafe { &mut *owner };

            let Some(query_snapshot) = owner
                .impl_
                .obtain_query(query_location)
                .map(|query| (query.flags, query.value))
            else {
                return;
            };
            let (query_flags, query_value) = query_snapshot;
            if query_flags.intersects(QueryFlagBits::IS_INVALIDATED) {
                if !is_synced {
                    owner.impl_.pending_unregister.push(query_location);
                }
                return;
            }
            if !query_flags.intersects(QueryFlagBits::IS_FINAL_VALUE_SYNCED) {
                log::error!(
                    "Query report value not synchronized. Consider increasing GPU accuracy."
                );
                if !is_synced {
                    owner.impl_.pending_unregister.push(query_location);
                }
                return;
            }

            let amend_value = owner
                .impl_
                .get_streamer_for_query_type(streamer_id)
                .map(|streamer| streamer.get_amend_value())
                .unwrap_or(0);
            let final_value = query_value.wrapping_add(amend_value);
            if let Some(query) = owner.impl_.obtain_query_mut(query_location) {
                query.value = final_value;
            }
            if let Some(streamer) = owner.impl_.get_streamer_for_query_type_mut(streamer_id) {
                streamer.set_accumulation_value(final_value);
            }
            let timestamp = if query_flags.intersects(QueryFlagBits::HAS_TIMESTAMP) {
                owner
                    .impl_
                    .gpu_mut()
                    .map(|gpu| gpu.get_ticks())
                    .unwrap_or(0)
            } else {
                0
            };
            if let Some(device_memory) = owner.impl_.device_memory_mut() {
                if query_flags.intersects(QueryFlagBits::HAS_TIMESTAMP) {
                    device_memory.write_u64(cpu_addr.wrapping_add(8), timestamp);
                    device_memory.write_u64(cpu_addr, final_value);
                } else {
                    device_memory.write_u32(cpu_addr, final_value as u32);
                }
            }
            if !is_synced {
                owner.impl_.pending_unregister.push(query_location);
            }
        });

        if is_fence {
            if let Some(rasterizer) = self.impl_.rasterizer_mut() {
                rasterizer.signal_fence(operation);
            }
        } else if let Some(rasterizer) = self.impl_.rasterizer_mut() {
            rasterizer.sync_operation(operation);
        }

        if is_synced {
            if let Some(streamer) = self.impl_.get_streamer_for_query_type_mut(streamer_id) {
                streamer.free(new_query_id);
            }
            return;
        }

        let cont_addr = cpu_addr >> DEVICE_PAGEBITS;
        let base = (cpu_addr & DEVICE_PAGEMASK) as u32;
        let _lock = self.cache_mutex.lock().unwrap();
        let sub_container = self.cached_queries.entry(cont_addr).or_default();
        if let Some(old_location) = sub_container.get(&base).copied() {
            if let Some(old_query) = self.impl_.obtain_query_mut(old_location) {
                old_query.flags |= QueryFlagBits::IS_REWRITTEN;
            }
        }
        sub_container.insert(base, query_location);
    }

    /// Notify that a Wait-For-Idle has been issued.
    ///
    /// Maps to C++ `QueryCacheBase::NotifyWFI`:
    /// syncs all pending streamer writes via PresyncWrites / SyncWrites and
    /// issues a runtime Barrier pair.
    pub fn notify_wfi(&mut self) {
        let mut should_sync = false;
        self.impl_.for_each_streamer(|streamer| {
            should_sync |= streamer.has_pending_sync();
            false
        });
        if !should_sync {
            return;
        }
        self.impl_.for_each_streamer_mut(|streamer| {
            streamer.presync_writes();
            false
        });
        if let Some(runtime) = self.impl_.runtime_mut() {
            runtime.barriers(true);
        }
        self.impl_.for_each_streamer_mut(|streamer| {
            streamer.sync_writes();
            false
        });
        if let Some(runtime) = self.impl_.runtime_mut() {
            runtime.barriers(false);
        }
    }

    /// Attempt to use host-side conditional rendering.
    ///
    /// Maps to C++ `QueryCacheBase::AccelerateHostConditionalRendering`.
    ///
    /// Reads Maxwell3D render-enable registers and performs GPU-side
    /// conditional rendering acceleration.  Requires Maxwell3D engine and
    /// runtime.  Returns false (fall back to CPU path).
    pub fn accelerate_host_conditional_rendering(&mut self) -> bool {
        const USE_RENDER_ENABLE: u32 = 0;

        let Some(render_condition_state) = self
            .impl_
            .render_condition_source()
            .map(|source| source.render_condition_state())
        else {
            return false;
        };

        let mut qc_dirty = false;
        let gen_lookup = |owner: &QueryCacheBase, address: GPUVAddr, qc_dirty: &mut bool| {
            let Some(cpu_addr) = owner
                .impl_
                .gpu_memory()
                .and_then(|gpu_memory| gpu_memory.gpu_to_cpu_address(address))
            else {
                return LookupData {
                    address: 0,
                    found_query: None,
                };
            };

            let _lock = owner.cache_mutex.lock().unwrap();
            let Some(sub_container) = owner.cached_queries.get(&(cpu_addr >> DEVICE_PAGEBITS))
            else {
                return LookupData {
                    address: cpu_addr,
                    found_query: None,
                };
            };

            let mut location = sub_container
                .get(&((cpu_addr & DEVICE_PAGEMASK) as u32))
                .copied();
            if location.is_none() {
                location = sub_container
                    .get(&(((cpu_addr & DEVICE_PAGEMASK) + 4) as u32))
                    .copied();
            }
            let Some(location) = location else {
                return LookupData {
                    address: cpu_addr,
                    found_query: None,
                };
            };
            let found_query = owner.impl_.obtain_query(location);
            if let Some(query) = found_query {
                *qc_dirty |= query.flags.intersects(QueryFlagBits::IS_HOST_MANAGED)
                    && !query.flags.intersects(QueryFlagBits::IS_GUEST_SYNCED);
            }
            LookupData {
                address: cpu_addr,
                found_query: found_query.map(|query| query as *const QueryBase),
            }
        };

        if render_condition_state.override_mode != USE_RENDER_ENABLE {
            if let Some(runtime) = self.impl_.runtime_mut() {
                runtime.end_host_conditional_rendering();
            }
            return false;
        }

        match render_condition_state.comparison_mode {
            ComparisonMode::True | ComparisonMode::False => {
                let Some(runtime) = self.impl_.runtime_mut() else {
                    return false;
                };
                runtime.end_host_conditional_rendering();
                false
            }
            ComparisonMode::Conditional => {
                let object_1 = gen_lookup(self, render_condition_state.address, &mut qc_dirty);
                let Some(runtime) = self.impl_.runtime_mut() else {
                    return false;
                };
                runtime.host_conditional_rendering_compare_value(object_1, qc_dirty)
            }
            ComparisonMode::IfEqual => {
                let object_1 = gen_lookup(self, render_condition_state.address, &mut qc_dirty);
                let object_2 = gen_lookup(
                    self,
                    render_condition_state.address.wrapping_add(16),
                    &mut qc_dirty,
                );
                let Some(runtime) = self.impl_.runtime_mut() else {
                    return false;
                };
                runtime
                    .host_conditional_rendering_compare_values(object_1, object_2, qc_dirty, true)
            }
            ComparisonMode::IfNotEqual => {
                let object_1 = gen_lookup(self, render_condition_state.address, &mut qc_dirty);
                let object_2 = gen_lookup(
                    self,
                    render_condition_state.address.wrapping_add(16),
                    &mut qc_dirty,
                );
                let Some(runtime) = self.impl_.runtime_mut() else {
                    return false;
                };
                runtime
                    .host_conditional_rendering_compare_values(object_1, object_2, qc_dirty, false)
            }
        }
    }

    /// Commit pending async flushes.
    ///
    /// Maps to C++ `QueryCacheBase::CommitAsyncFlushes`:
    /// calls `NotifyWFI`, then for each streamer with unsynced queries pushes
    /// them and appends a mask to `flushes_pending`.
    ///
    /// The full implementation requires the streamer array.  We push a zero
    /// mask (no-op flush batch) to keep `flushes_pending` in sync with any
    /// caller that checks `ShouldWaitAsyncFlushes`.
    pub fn commit_async_flushes(&mut self) {
        self.refresh_owner_binding();
        self.notify_wfi();
        let mask = {
            let _guard = self.impl_.flush_guard.lock().unwrap();
            let mut mask = 0u64;
            self.impl_.for_each_streamer(|streamer| {
                if streamer.has_unsynced_queries() {
                    mask |= 1u64 << streamer.get_id();
                }
                false
            });
            self.impl_.flushes_pending.push_back(mask);
            mask
        };
        let owner = self.impl_.owner.map(|owner| owner as usize);
        if let Some(rasterizer) = self.impl_.rasterizer_mut() {
            rasterizer.sync_operation(Box::new(move || {
                let Some(owner) = owner else {
                    return;
                };
                let owner = owner as *mut QueryCacheBase;
                unsafe { (*owner).unregister_pending() };
            }));
        }
        if mask == 0 {
            return;
        }
        let mut pending_mask = mask;
        let mut ran_mask = !mask;
        while pending_mask != 0 {
            let mut progressed = false;
            self.impl_
                .for_each_streamer_in_mut(pending_mask, |streamer| {
                    let dep_mask = streamer.get_dependent_mask();
                    if (dep_mask & !ran_mask) != 0 {
                        return false;
                    }
                    let index = streamer.get_id();
                    ran_mask |= 1u64 << index;
                    pending_mask &= !(1u64 << index);
                    streamer.push_unsynced_queries();
                    progressed = true;
                    false
                });
            if !progressed {
                break;
            }
        }
    }

    /// Check if there are uncommitted flushes.
    ///
    /// Maps to C++ `QueryCacheBase::HasUncommittedFlushes`:
    /// returns true if any streamer has unsynced queries.
    /// Without the streamer array, conservatively returns false.
    pub fn has_uncommitted_flushes(&self) -> bool {
        let mut result = false;
        self.impl_.for_each_streamer(|streamer| {
            result |= streamer.has_unsynced_queries();
            result
        });
        result
    }

    /// Check if we should wait for async flushes.
    ///
    /// Maps to C++ `QueryCacheBase::ShouldWaitAsyncFlushes`:
    /// ```cpp
    /// return !flushes_pending.empty() && flushes_pending.front() != 0;
    /// ```
    pub fn should_wait_async_flushes(&self) -> bool {
        let _guard = self.impl_.flush_guard.lock().unwrap();
        !self.impl_.flushes_pending.is_empty() && self.impl_.flushes_pending.front() != Some(&0u64)
    }

    /// Pop completed async flushes.
    ///
    /// Maps to C++ `QueryCacheBase::PopAsyncFlushes`:
    /// pops the front mask and calls `PopUnsyncedQueries` on each streamer
    /// in the mask (in dependency order).  Without streamers, just pops the
    /// front entry.
    pub fn pop_async_flushes(&mut self) {
        let mask = {
            let _guard = self.impl_.flush_guard.lock().unwrap();
            self.impl_.flushes_pending.pop_front()
        };
        let Some(mut mask) = mask else {
            return;
        };
        if mask == 0 {
            return;
        }
        let mut ran_mask = !mask;
        while mask != 0 {
            let mut progressed = false;
            self.impl_.for_each_streamer_in_mut(mask, |streamer| {
                let dep_mask = streamer.get_dependence_mask();
                if (dep_mask & !ran_mask) != 0 {
                    return false;
                }
                let index = streamer.get_id();
                ran_mask |= 1u64 << index;
                mask &= !(1u64 << index);
                streamer.pop_unsynced_queries();
                progressed = true;
                false
            });
            if !progressed {
                break;
            }
        }
    }

    /// Notify a GPU segment transition (pause/resume).
    ///
    /// Maps to C++ `QueryCacheBase::NotifySegment`:
    /// pauses or resumes host conditional rendering.
    pub fn notify_segment(&mut self, resume: bool) {
        if resume {
            if let Some(runtime) = self.impl_.runtime_mut() {
                runtime.resume_host_conditional_rendering();
            }
            return;
        }
        if let Some(runtime) = self.impl_.runtime_mut() {
            runtime.pause_host_conditional_rendering();
        }
    }

    /// Iterate over cached queries in the given address range.
    ///
    /// If `remove_from_cache` is true, matching entries are removed after
    /// iteration.
    ///
    /// Maps to C++ `QueryCacheBase::IterateCache<remove_from_cache>`.
    ///
    /// The callback `func` receives each matching `QueryLocation` and should
    /// return `true` to stop iteration early (matches the bool-returning
    /// variant in upstream).
    fn iterate_cache<const REMOVE_FROM_CACHE: bool, F>(
        cache_mutex: &Mutex<()>,
        cached_queries: &mut ContentCache,
        addr: VAddr,
        size: usize,
        mut func: F,
    ) where
        F: FnMut(QueryLocation) -> bool,
    {
        let addr_begin = addr;
        let addr_end = addr_begin.wrapping_add(size as u64);
        let page_end = addr_end >> DEVICE_PAGEBITS;

        let _lock = cache_mutex.lock().unwrap();
        let mut page = addr_begin >> DEVICE_PAGEBITS;
        while page <= page_end {
            let page_start = page << DEVICE_PAGEBITS;

            let in_range = |query_offset: u32| -> bool {
                let cache_begin = page_start.wrapping_add(query_offset as u64);
                let cache_end = cache_begin.wrapping_add(std::mem::size_of::<u32>() as u64);
                cache_begin < addr_end && addr_begin < cache_end
            };

            if let Some(contents) = cached_queries.get_mut(&page) {
                let mut early_exit = false;
                for (&offset, &location) in contents.iter() {
                    if !in_range(offset) {
                        continue;
                    }
                    if func(location) {
                        early_exit = true;
                        break;
                    }
                }

                if early_exit {
                    return;
                }
                if REMOVE_FROM_CACHE {
                    contents.retain(|&offset, _| !in_range(offset));
                }
            }

            page += 1;
        }
    }

    // ── Per-query operations ────────────────────────────────────────────

    /// Invalidate a single query by location.
    ///
    /// Maps to C++ `QueryCacheBase::InvalidateQuery`:
    /// sets `IsInvalidated` flag via `ObtainQuery(location)`.
    pub fn invalidate_query(&mut self, _location: QueryLocation) {
        Self::invalidate_query_impl(&mut self.impl_, _location);
    }

    fn invalidate_query_impl(impl_: &mut QueryCacheBaseImpl, location: QueryLocation) {
        let Some(query_base) = impl_.obtain_query_mut(location) else {
            return;
        };
        query_base.flags |= QueryFlagBits::IS_INVALIDATED;
    }

    /// Check if a query is dirty (host-managed but not guest-synced).
    ///
    /// Maps to C++ `QueryCacheBase::IsQueryDirty`:
    /// ```cpp
    /// return IsHostManaged && !IsGuestSynced;
    /// ```
    pub fn is_query_dirty(&self, location: QueryLocation) -> bool {
        Self::is_query_dirty_impl(&self.impl_, location)
    }

    fn is_query_dirty_impl(impl_: &QueryCacheBaseImpl, location: QueryLocation) -> bool {
        let Some(query_base) = impl_.obtain_query(location) else {
            return false;
        };
        query_base.flags.intersects(QueryFlagBits::IS_HOST_MANAGED)
            && !query_base.flags.intersects(QueryFlagBits::IS_GUEST_SYNCED)
    }

    /// Semi-flush a dirty query (write final value to guest memory if available).
    ///
    /// Maps to C++ `QueryCacheBase::SemiFlushQueryDirty`:
    /// if `IsFinalValueSynced && !IsGuestSynced` writes the value to guest
    /// memory via device_memory.GetPointer and returns false; otherwise
    /// returns whether the query is still host-managed-dirty.
    pub fn semi_flush_query_dirty(&mut self, location: QueryLocation) -> bool {
        Self::semi_flush_query_dirty_impl(&mut self.impl_, location)
    }

    fn semi_flush_query_dirty_impl(
        impl_: &mut QueryCacheBaseImpl,
        location: QueryLocation,
    ) -> bool {
        let Some((
            guest_address,
            value,
            has_timestamp,
            is_final_value_synced,
            is_guest_synced,
            is_host_managed,
        )) = impl_.obtain_query(location).map(|query_base| {
            (
                query_base.guest_address,
                query_base.value,
                query_base.flags.intersects(QueryFlagBits::HAS_TIMESTAMP),
                query_base
                    .flags
                    .intersects(QueryFlagBits::IS_FINAL_VALUE_SYNCED),
                query_base.flags.intersects(QueryFlagBits::IS_GUEST_SYNCED),
                query_base.flags.intersects(QueryFlagBits::IS_HOST_MANAGED),
            )
        })
        else {
            return false;
        };
        if is_final_value_synced && !is_guest_synced {
            if let Some(device_memory) = impl_.device_memory_mut() {
                if has_timestamp {
                    device_memory.write_u64(guest_address, value);
                } else {
                    device_memory.write_u32(guest_address, value as u32);
                }
            } else {
                log::warn!(
                    "QueryCacheBase::semi_flush_query_dirty: device-memory owner not yet bound"
                );
            }
            return false;
        }
        is_host_managed && !is_guest_synced
    }

    /// Request a guest-host synchronization.
    ///
    /// Maps to C++ `QueryCacheBase::RequestGuestHostSync`:
    /// calls `rasterizer.ReleaseFences()`.
    /// Requires the rasterizer interface.
    pub fn request_guest_host_sync(&self) {
        let Some(rasterizer) = self.impl_.rasterizer else {
            log::warn!("QueryCacheBase::request_guest_host_sync: rasterizer not yet bound");
            return;
        };
        unsafe { (&mut *rasterizer).release_fences(true) };
    }

    /// Unregister queries that have been processed.
    ///
    /// Maps to C++ `QueryCacheBase::UnregisterPending`:
    /// iterates `pending_unregister`, removes entries from `cached_queries`,
    /// and frees the corresponding query slots via the streamer.
    /// Requires the streamer array and `pending_unregister` vec (both live in
    /// `QueryCacheBaseImpl`).
    pub fn unregister_pending(&mut self) {
        if self.impl_.pending_unregister.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.impl_.pending_unregister);
        let _lock = self.cache_mutex.lock().unwrap();
        for location in pending {
            let Some(query_base) = self.impl_.obtain_query(location) else {
                continue;
            };
            let cont_addr = query_base.guest_address >> DEVICE_PAGEBITS;
            let base = (query_base.guest_address & DEVICE_PAGEMASK) as u32;
            if let Some(contents) = self.cached_queries.get_mut(&cont_addr) {
                if contents.get(&base).copied() == Some(location) {
                    contents.remove(&base);
                }
            }
            self.impl_.free_query_location(location);
        }
    }
}

impl Default for QueryCacheBase {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::super::query_stream::{StreamerInterface, StreamerInterfaceBase};
    use super::*;
    use crate::rasterizer_interface::RasterizerDownloadArea;

    struct CountingStreamer {
        base: StreamerInterfaceBase,
        query: QueryBase,
        last_write: Option<(VAddr, bool, u32, Option<u32>)>,
        start_calls: u32,
        pause_calls: u32,
        reset_calls: u32,
        close_calls: u32,
        presync_calls: u32,
        sync_calls: u32,
        pending_sync: bool,
        unsynced_queries: bool,
        push_calls: u32,
        pop_calls: u32,
        free_calls: u32,
    }

    impl CountingStreamer {
        fn new(id: usize, query: QueryBase) -> Self {
            Self {
                base: StreamerInterfaceBase::new(id),
                query,
                last_write: None,
                start_calls: 0,
                pause_calls: 0,
                reset_calls: 0,
                close_calls: 0,
                presync_calls: 0,
                sync_calls: 0,
                pending_sync: false,
                unsynced_queries: false,
                push_calls: 0,
                pop_calls: 0,
                free_calls: 0,
            }
        }
    }

    impl StreamerInterface for CountingStreamer {
        fn get_query(&self, id: usize) -> Option<&QueryBase> {
            (id == 0).then_some(&self.query)
        }

        fn get_query_mut(&mut self, id: usize) -> Option<&mut QueryBase> {
            (id == 0).then_some(&mut self.query)
        }

        fn write_counter(
            &mut self,
            address: VAddr,
            has_timestamp: bool,
            value: u32,
            subreport: Option<u32>,
        ) -> usize {
            self.last_write = Some((address, has_timestamp, value, subreport));
            self.query.guest_address = address;
            self.query.value = value as u64;
            self.query.flags = QueryFlagBits::IS_FINAL_VALUE_SYNCED;
            if has_timestamp {
                self.query.flags |= QueryFlagBits::HAS_TIMESTAMP;
            }
            0
        }

        fn free(&mut self, _query_id: usize) {
            self.free_calls += 1;
        }

        fn base(&self) -> &StreamerInterfaceBase {
            &self.base
        }

        fn base_mut(&mut self) -> &mut StreamerInterfaceBase {
            &mut self.base
        }

        fn start_counter(&mut self) {
            self.start_calls += 1;
        }

        fn pause_counter(&mut self) {
            self.pause_calls += 1;
        }

        fn reset_counter(&mut self) {
            self.reset_calls += 1;
        }

        fn close_counter(&mut self) {
            self.close_calls += 1;
        }

        fn has_pending_sync(&self) -> bool {
            self.pending_sync
        }

        fn presync_writes(&mut self) {
            self.presync_calls += 1;
        }

        fn sync_writes(&mut self) {
            self.sync_calls += 1;
        }

        fn has_unsynced_queries(&self) -> bool {
            self.unsynced_queries
        }

        fn push_unsynced_queries(&mut self) {
            self.push_calls += 1;
            self.unsynced_queries = false;
        }

        fn pop_unsynced_queries(&mut self) {
            self.pop_calls += 1;
        }
    }

    #[derive(Default)]
    struct CountingRuntime {
        bind_calls: u32,
        barrier_calls: Vec<bool>,
        end_calls: u32,
        pause_calls: u32,
        resume_calls: u32,
        compare_value_calls: Vec<(u64, bool, bool)>,
        compare_values_calls: Vec<(u64, u64, bool, bool, bool)>,
    }

    impl QueryCacheRuntimeHandle for CountingRuntime {
        fn bind_3d_engine(&mut self) {
            self.bind_calls += 1;
        }

        fn barriers(&mut self, is_prebarrier: bool) {
            self.barrier_calls.push(is_prebarrier);
        }

        fn end_host_conditional_rendering(&mut self) {
            self.end_calls += 1;
        }

        fn pause_host_conditional_rendering(&mut self) {
            self.pause_calls += 1;
        }

        fn resume_host_conditional_rendering(&mut self) {
            self.resume_calls += 1;
        }

        fn host_conditional_rendering_compare_value(
            &mut self,
            object_1: LookupData,
            qc_dirty: bool,
        ) -> bool {
            self.compare_value_calls.push((
                object_1.address,
                object_1.found_query.is_some(),
                qc_dirty,
            ));
            true
        }

        fn host_conditional_rendering_compare_values(
            &mut self,
            object_1: LookupData,
            object_2: LookupData,
            qc_dirty: bool,
            equal_check: bool,
        ) -> bool {
            self.compare_values_calls.push((
                object_1.address,
                object_2.address,
                object_1.found_query.is_some() && object_2.found_query.is_some(),
                qc_dirty,
                equal_check,
            ));
            true
        }
    }

    #[derive(Default)]
    struct CountingDeviceMemory {
        writes32: Vec<(u64, u32)>,
        writes64: Vec<(u64, u64)>,
    }

    impl DeviceMemoryWriter for CountingDeviceMemory {
        fn write_u32(&mut self, addr: u64, value: u32) {
            self.writes32.push((addr, value));
        }

        fn write_u64(&mut self, addr: u64, value: u64) {
            self.writes64.push((addr, value));
        }
    }

    #[derive(Default)]
    struct CountingGpuMemory {
        translations: HashMap<u64, u64>,
    }

    impl GpuAddressTranslator for CountingGpuMemory {
        fn gpu_to_cpu_address(&self, gpu_addr: u64) -> Option<u64> {
            self.translations.get(&gpu_addr).copied()
        }
    }

    #[derive(Default)]
    struct CountingGpu {
        ticks: u64,
    }

    impl GpuTickSource for CountingGpu {
        fn get_ticks(&self) -> u64 {
            self.ticks
        }
    }

    struct CountingRenderConditionSource {
        state: super::super::query_cache::RenderConditionState,
    }

    impl RenderConditionStateSource for CountingRenderConditionSource {
        fn render_condition_state(&self) -> super::super::query_cache::RenderConditionState {
            self.state
        }
    }

    #[derive(Default)]
    struct CountingRasterizer {
        accelerate_dma: crate::rasterizer_interface::TestAccelerateDMA,
        sync_operations: u32,
        release_fences_calls: Vec<bool>,
    }

    impl RasterizerInterface for CountingRasterizer {
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
            _flags: QueryPropertiesFlags,
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
        fn signal_fence(&mut self, func: Box<dyn FnOnce() + Send>) {
            func();
        }
        fn sync_operation(&mut self, func: Box<dyn FnOnce() + Send>) {
            self.sync_operations += 1;
            func();
        }
        fn signal_sync_point(&mut self, _value: u32) {}
        fn signal_reference(&mut self) {}
        fn release_fences(&mut self, force: bool) {
            self.release_fences_calls.push(force);
        }
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
        fn get_flush_area(&self, _addr: u64, _size: u64) -> RasterizerDownloadArea {
            RasterizerDownloadArea {
                start_address: 0,
                end_address: 0,
                preemptive: false,
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
    }

    #[test]
    fn async_flush_wait_state_uses_impl_owned_queue() {
        let mut cache = QueryCacheBase::new();
        assert!(!cache.should_wait_async_flushes());

        {
            let _guard = cache.impl_.flush_guard.lock().unwrap();
            cache.impl_.flushes_pending.push_back(0);
            cache.impl_.flushes_pending.push_back(0x20);
        }

        assert!(!cache.should_wait_async_flushes());
        cache.pop_async_flushes();
        assert!(cache.should_wait_async_flushes());
        cache.pop_async_flushes();
        assert!(!cache.should_wait_async_flushes());
    }

    #[test]
    fn commit_async_flushes_pushes_zero_batch_into_impl_queue() {
        let mut cache = QueryCacheBase::new();
        cache.commit_async_flushes();

        let _guard = cache.impl_.flush_guard.lock().unwrap();
        assert_eq!(cache.impl_.flushes_pending.len(), 1);
        assert_eq!(cache.impl_.flushes_pending.front(), Some(&0));
    }

    #[test]
    fn unregister_pending_clears_impl_owned_queue() {
        let mut cache = QueryCacheBase::new();
        cache
            .impl_
            .pending_unregister
            .push(QueryLocation::new(1, 2));
        cache
            .impl_
            .pending_unregister
            .push(QueryLocation::new(3, 4));

        cache.unregister_pending();

        assert!(cache.impl_.pending_unregister.is_empty());
    }

    #[test]
    fn counter_and_query_helpers_use_impl_owned_streamer_registry() {
        let mut cache = QueryCacheBase::new();
        let mut streamer = CountingStreamer::new(
            QueryType::Payload as usize,
            QueryBase::with_params(
                0x5000,
                QueryFlagBits::IS_HOST_MANAGED | QueryFlagBits::IS_FINAL_VALUE_SYNCED,
                7,
            ),
        );
        cache
            .impl_
            .register_streamer(QueryType::Payload as usize, &mut streamer);

        cache.counter_enable(QueryType::Payload, true);
        cache.counter_enable(QueryType::Payload, false);
        cache.counter_reset(QueryType::Payload);
        cache.counter_close(QueryType::Payload);

        let location = QueryLocation::new(QueryType::Payload as u32, 0);
        assert!(cache.is_query_dirty(location));
        cache.invalidate_query(location);
        assert!(streamer
            .query
            .flags
            .intersects(QueryFlagBits::IS_INVALIDATED));
        assert!(!cache.semi_flush_query_dirty(location));

        assert_eq!(streamer.start_calls, 1);
        assert_eq!(streamer.pause_calls, 1);
        assert_eq!(streamer.reset_calls, 1);
        assert_eq!(streamer.close_calls, 1);
    }

    #[test]
    fn backend_query_index_rewrites_old_location_and_flushes_new_value() {
        let mut cache = QueryCacheBase::new();
        let mut old_streamer = CountingStreamer::new(
            QueryType::Payload as usize,
            QueryBase::with_params(0x453c000, QueryFlagBits::IS_FINAL_VALUE_SYNCED, 0x1111),
        );
        let mut new_streamer = CountingStreamer::new(
            QueryType::StreamingPrimitivesNeededMinusSucceeded as usize,
            QueryBase::with_params(0x453c000, QueryFlagBits::IS_FINAL_VALUE_SYNCED, 0x2222),
        );
        let mut device_memory = CountingDeviceMemory::default();
        cache
            .impl_
            .register_streamer(QueryType::Payload as usize, &mut old_streamer);
        cache.impl_.register_streamer(
            QueryType::StreamingPrimitivesNeededMinusSucceeded as usize,
            &mut new_streamer,
        );
        cache.bind_device_memory(&mut device_memory);

        let old_location = QueryLocation::new(QueryType::Payload as u32, 0);
        let new_location =
            QueryLocation::new(QueryType::StreamingPrimitivesNeededMinusSucceeded as u32, 0);
        cache.cache_query_location(0x453c000, old_location);
        cache.cache_query_location(0x453c000, new_location);

        assert!(old_streamer
            .query
            .flags
            .contains(QueryFlagBits::IS_REWRITTEN));
        assert_eq!(
            cache.cached_queries[&(0x453c000 >> DEVICE_PAGEBITS)][&0],
            new_location
        );

        cache.flush_region(0x453c000, 4);
        assert_eq!(device_memory.writes32, vec![(0x453c000, 0x2222)]);
    }

    #[test]
    fn flush_region_stops_at_first_dirty_query_like_upstream() {
        let mut cache = QueryCacheBase::new();
        let mut dirty_streamer = CountingStreamer::new(
            QueryType::Payload as usize,
            QueryBase::with_params(0x1000, QueryFlagBits::IS_HOST_MANAGED, 0),
        );
        let mut later_streamer = CountingStreamer::new(
            QueryType::ZPassPixelCount64 as usize,
            QueryBase::with_params(0x2000, QueryFlagBits::IS_FINAL_VALUE_SYNCED, 0x2222),
        );
        let mut device_memory = CountingDeviceMemory::default();
        let mut rasterizer = CountingRasterizer::default();
        cache
            .impl_
            .register_streamer(QueryType::Payload as usize, &mut dirty_streamer);
        cache
            .impl_
            .register_streamer(QueryType::ZPassPixelCount64 as usize, &mut later_streamer);
        cache.bind_device_memory(&mut device_memory);
        cache.bind_rasterizer(&mut rasterizer);

        cache.cache_query_location(0x1000, QueryLocation::new(QueryType::Payload as u32, 0));
        cache.cache_query_location(
            0x2000,
            QueryLocation::new(QueryType::ZPassPixelCount64 as u32, 0),
        );
        cache.flush_region(0x1000, 0x1004);

        assert_eq!(rasterizer.release_fences_calls, vec![true]);
        assert!(device_memory.writes32.is_empty());
        assert!(device_memory.writes64.is_empty());
    }

    #[test]
    fn notify_and_async_flush_paths_use_registered_streamers() {
        let mut cache = QueryCacheBase::new();
        let mut streamer = CountingStreamer::new(0, QueryBase::new());
        streamer.pending_sync = true;
        streamer.unsynced_queries = true;
        let mut runtime = CountingRuntime::default();
        let mut rasterizer = CountingRasterizer::default();
        cache.impl_.register_streamer(0, &mut streamer);
        cache.bind_runtime(&mut runtime);
        cache.bind_rasterizer(&mut rasterizer);

        cache.notify_wfi();
        assert_eq!(streamer.presync_calls, 1);
        assert_eq!(streamer.sync_calls, 1);
        assert_eq!(runtime.barrier_calls, vec![true, false]);

        assert!(cache.has_uncommitted_flushes());
        cache.commit_async_flushes();
        {
            let _guard = cache.impl_.flush_guard.lock().unwrap();
            assert_eq!(cache.impl_.flushes_pending.front(), Some(&1));
        }
        assert_eq!(streamer.push_calls, 1);
        assert_eq!(rasterizer.sync_operations, 1);

        cache.pop_async_flushes();
        assert_eq!(streamer.pop_calls, 1);
    }

    #[test]
    fn segment_writeback_and_unregister_paths_use_bound_owners() {
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex as ParkingMutex;
        use std::sync::Arc;

        let _gpu_accuracy =
            crate::test_support::GpuAccuracyGuard::set(common::settings_enums::GpuAccuracy::High);
        let mut cache = QueryCacheBase::new();
        let mut flags = QueryFlagBits::IS_FINAL_VALUE_SYNCED;
        flags |= QueryFlagBits::HAS_TIMESTAMP;
        let mut payload_streamer = CountingStreamer::new(
            QueryType::Payload as usize,
            QueryBase::with_params(0x4400, flags, 0x1234_5678_9ABC_DEF0),
        );
        let mut zpass_streamer =
            CountingStreamer::new(QueryType::ZPassPixelCount64 as usize, QueryBase::new());
        let mut bytecount_streamer =
            CountingStreamer::new(QueryType::StreamingByteCount as usize, QueryBase::new());
        let mut runtime = CountingRuntime::default();
        let mut rasterizer = CountingRasterizer::default();
        let mut device_memory = CountingDeviceMemory::default();
        cache
            .impl_
            .register_streamer(QueryType::Payload as usize, &mut payload_streamer);
        cache
            .impl_
            .register_streamer(QueryType::ZPassPixelCount64 as usize, &mut zpass_streamer);
        cache.impl_.register_streamer(
            QueryType::StreamingByteCount as usize,
            &mut bytecount_streamer,
        );
        cache.bind_runtime(&mut runtime);
        cache.bind_rasterizer(&mut rasterizer);
        cache.bind_device_memory(&mut device_memory);

        let mut channel = ChannelState::new(9);
        channel.memory_manager = Some(Arc::new(ParkingMutex::new(MemoryManager::new(19))));
        cache.create_channel(&channel);
        cache.bind_to_channel(9);
        cache.notify_segment(false);
        cache.notify_segment(true);
        assert_eq!(runtime.bind_calls, 1);
        assert_eq!(runtime.pause_calls, 1);
        assert_eq!(runtime.resume_calls, 1);
        // Upstream `QueryCacheBase::NotifySegment` only pauses/resumes host
        // conditional rendering. Counter enable/disable belongs to the Vulkan
        // scheduler caller and must not close either streamer here.
        assert_eq!(zpass_streamer.close_calls, 0);
        assert_eq!(bytecount_streamer.close_calls, 0);

        let location = QueryLocation::new(QueryType::Payload as u32, 0);
        assert!(!cache.semi_flush_query_dirty(location));
        assert_eq!(
            device_memory.writes64,
            vec![(0x4400, 0x1234_5678_9ABC_DEF0)]
        );

        let cont_addr = 0x4400 >> DEVICE_PAGEBITS;
        let base = (0x4400 & DEVICE_PAGEMASK) as u32;
        cache
            .cached_queries
            .entry(cont_addr)
            .or_default()
            .insert(base, location);
        cache.impl_.pending_unregister.push(location);
        cache.commit_async_flushes();
        assert!(cache.impl_.pending_unregister.is_empty());
        assert!(cache
            .cached_queries
            .get(&cont_addr)
            .is_none_or(|contents| !contents.contains_key(&base)));
        assert_eq!(payload_streamer.free_calls, 1);

        cache.request_guest_host_sync();
        assert_eq!(rasterizer.release_fences_calls, vec![true]);
    }

    #[test]
    fn counter_report_payload_fast_path_matches_upstream_low_accuracy_behavior() {
        let _gpu_accuracy =
            crate::test_support::GpuAccuracyGuard::set(common::settings_enums::GpuAccuracy::Low);

        let mut cache = QueryCacheBase::new();
        let mut payload_streamer =
            CountingStreamer::new(QueryType::Payload as usize, QueryBase::new());
        let mut device_memory = CountingDeviceMemory::default();
        let mut gpu_memory = CountingGpuMemory::default();
        let mut gpu = CountingGpu {
            ticks: 0x1122_3344_5566_7788,
        };
        gpu_memory.translations.insert(0x8000, 0x4400);

        cache
            .impl_
            .register_streamer(QueryType::Payload as usize, &mut payload_streamer);
        cache.bind_device_memory(&mut device_memory);
        cache.bind_gpu_memory(&mut gpu_memory);
        cache.bind_gpu(&mut gpu);

        cache.counter_report(
            0x8000,
            QueryType::Payload,
            QueryPropertiesFlags::HAS_TIMEOUT,
            7,
            9,
        );

        assert_eq!(
            payload_streamer.last_write,
            Some((0x4400, true, 7, Some(9)))
        );
        assert_eq!(
            device_memory.writes64,
            vec![(0x4408, 0x1122_3344_5566_7788), (0x4400, 7)]
        );
        assert_eq!(payload_streamer.free_calls, 1);
        assert!(cache.cached_queries.is_empty());
    }

    #[test]
    fn counter_report_marks_rewritten_and_unregisters_on_sync_operation() {
        let _gpu_accuracy =
            crate::test_support::GpuAccuracyGuard::set(common::settings_enums::GpuAccuracy::High);

        let mut cache = QueryCacheBase::new();
        let mut payload_streamer = CountingStreamer::new(
            QueryType::Payload as usize,
            QueryBase::with_params(0, QueryFlagBits::empty(), 0),
        );
        let mut device_memory = CountingDeviceMemory::default();
        let mut gpu_memory = CountingGpuMemory::default();
        let mut gpu = CountingGpu {
            ticks: 0xAABB_CCDD_EEFF_0011,
        };
        let mut rasterizer = CountingRasterizer::default();
        gpu_memory.translations.insert(0x9000, 0x5500);

        cache
            .impl_
            .register_streamer(QueryType::Payload as usize, &mut payload_streamer);
        cache.bind_device_memory(&mut device_memory);
        cache.bind_gpu_memory(&mut gpu_memory);
        cache.bind_gpu(&mut gpu);
        cache.bind_rasterizer(&mut rasterizer);

        let first_location = QueryLocation::new(QueryType::Payload as u32, 0);
        cache
            .cached_queries
            .entry(0x5500 >> DEVICE_PAGEBITS)
            .or_default()
            .insert((0x5500 & DEVICE_PAGEMASK) as u32, first_location);

        cache.counter_report(
            0x9000,
            QueryType::Payload,
            QueryPropertiesFlags::empty(),
            3,
            0,
        );

        assert_eq!(rasterizer.sync_operations, 1);
        assert_eq!(device_memory.writes32, vec![(0x5500, 3)]);
        assert_eq!(payload_streamer.query.value, 3);
        assert!(payload_streamer
            .query
            .flags
            .intersects(QueryFlagBits::IS_REWRITTEN));
        assert_eq!(cache.impl_.pending_unregister, vec![first_location]);

        cache.unregister_pending();
        assert_eq!(payload_streamer.free_calls, 1);
        assert!(cache
            .cached_queries
            .get(&(0x5500 >> DEVICE_PAGEBITS))
            .is_none_or(|contents| contents.is_empty()));
    }

    #[test]
    fn counter_report_accumulation_uses_upstream_unsigned_wrapping() {
        let _gpu_accuracy =
            crate::test_support::GpuAccuracyGuard::set(common::settings_enums::GpuAccuracy::High);

        let mut cache = QueryCacheBase::new();
        let mut payload_streamer =
            CountingStreamer::new(QueryType::Payload as usize, QueryBase::new());
        payload_streamer.base.amend_value = u64::MAX;
        let mut device_memory = CountingDeviceMemory::default();
        let mut gpu_memory = CountingGpuMemory::default();
        let mut gpu = CountingGpu { ticks: 0 };
        let mut rasterizer = CountingRasterizer::default();
        gpu_memory.translations.insert(0xA000, 0x6600);

        cache
            .impl_
            .register_streamer(QueryType::Payload as usize, &mut payload_streamer);
        cache.bind_device_memory(&mut device_memory);
        cache.bind_gpu_memory(&mut gpu_memory);
        cache.bind_gpu(&mut gpu);
        cache.bind_rasterizer(&mut rasterizer);

        cache.counter_report(
            0xA000,
            QueryType::Payload,
            QueryPropertiesFlags::empty(),
            2,
            0,
        );

        assert_eq!(payload_streamer.query.value, 1);
        assert_eq!(device_memory.writes32, vec![(0x6600, 1)]);
    }

    #[test]
    fn accelerate_host_conditional_rendering_uses_runtime_compare_owner() {
        let mut cache = QueryCacheBase::new();
        let mut streamer = CountingStreamer::new(
            QueryType::Payload as usize,
            QueryBase::with_params(0x4400, QueryFlagBits::IS_HOST_MANAGED, 0x55),
        );
        let mut runtime = CountingRuntime::default();
        let mut gpu_memory = CountingGpuMemory::default();
        let mut render_condition_source = CountingRenderConditionSource {
            state: super::super::query_cache::RenderConditionState {
                override_mode: 0,
                comparison_mode: ComparisonMode::Conditional,
                address: 0x8000,
            },
        };
        gpu_memory.translations.insert(0x8000, 0x4400);

        cache
            .impl_
            .register_streamer(QueryType::Payload as usize, &mut streamer);
        cache.bind_runtime(&mut runtime);
        cache.bind_gpu_memory(&mut gpu_memory);
        cache.bind_render_condition_source(&mut render_condition_source);
        cache
            .cached_queries
            .entry(0x4400 >> DEVICE_PAGEBITS)
            .or_default()
            .insert(
                (0x4400 & DEVICE_PAGEMASK) as u32,
                QueryLocation::new(QueryType::Payload as u32, 0),
            );

        assert!(cache.accelerate_host_conditional_rendering());
        assert_eq!(runtime.end_calls, 0);
        assert_eq!(runtime.compare_value_calls, vec![(0x4400, true, true)]);
    }

    #[test]
    fn accelerate_host_conditional_rendering_ends_runtime_when_override_is_forced() {
        let mut cache = QueryCacheBase::new();
        let mut runtime = CountingRuntime::default();
        let mut render_condition_source = CountingRenderConditionSource {
            state: super::super::query_cache::RenderConditionState {
                override_mode: 1,
                comparison_mode: ComparisonMode::Conditional,
                address: 0x8000,
            },
        };
        cache.bind_runtime(&mut runtime);
        cache.bind_render_condition_source(&mut render_condition_source);

        assert!(!cache.accelerate_host_conditional_rendering());
        assert_eq!(runtime.end_calls, 1);
        assert!(runtime.compare_value_calls.is_empty());
    }

    #[test]
    fn new_bound_prebinds_shared_owner_graph() {
        let mut runtime = CountingRuntime::default();
        let mut rasterizer = CountingRasterizer::default();
        let mut device_memory = CountingDeviceMemory::default();
        let mut gpu_memory = CountingGpuMemory::default();
        let mut gpu = CountingGpu { ticks: 7 };
        let mut render_condition_source = CountingRenderConditionSource {
            state: super::super::query_cache::RenderConditionState {
                override_mode: 0,
                comparison_mode: ComparisonMode::False,
                address: 0,
            },
        };

        let cache = QueryCacheBase::new_bound(
            &mut rasterizer,
            &mut device_memory,
            &mut runtime,
            &mut gpu_memory,
            &mut gpu,
            &mut render_condition_source,
        );

        assert!(cache.impl_.owner.is_some());
        assert!(cache.impl_.rasterizer.is_some());
        assert!(cache.impl_.device_memory.is_some());
        assert!(cache.impl_.runtime.is_some());
        assert!(cache.impl_.gpu_memory.is_some());
        assert!(cache.impl_.gpu.is_some());
        assert!(cache.impl_.render_condition_source.is_some());
    }
}

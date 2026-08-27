// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's `src/video_core/query_cache/query_cache.h`.
//!
//! Contains the shared streamer owners `GuestStreamer` and `StubStreamer`, the
//! guest sync payload `SyncValuesStruct`, and `QueryCacheBaseImpl`, the inner
//! owner state that belongs to `QueryCacheBase`.

use std::collections::VecDeque;
use std::sync::Mutex;

use super::query_base::{GuestQuery, QueryBase, QueryFlagBits, VAddr};
use super::query_cache_base::{LookupData, QueryCacheBase, QueryLocation};
use super::query_stream::{SimpleStreamer, StreamerInterface, StreamerInterfaceBase};
use super::types::{ComparisonMode, MAX_QUERY_TYPES};
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::memory_manager::MemoryManager;
use crate::rasterizer_interface::RasterizerInterface;

/// Guest sync payload written back by runtime-owned sync operations.
///
/// Maps to C++ `SyncValuesStruct`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncValuesStruct {
    pub address: VAddr,
    pub value: u64,
    pub size: u64,
}

impl SyncValuesStruct {
    pub const GENERATES_BASE_BUFFER: bool = true;
}

/// Runtime hook for `GuestStreamer<Traits>::SyncWrites`.
///
/// This preserves the upstream ownership boundary: the shared `GuestStreamer`
/// owner builds `SyncValuesStruct` payloads, and the backend runtime consumes
/// them.
pub trait SyncValuesRuntime {
    fn sync_values(&mut self, sync_values: Vec<SyncValuesStruct>);
}

impl<T> SyncValuesRuntime for &mut T
where
    T: SyncValuesRuntime + ?Sized,
{
    fn sync_values(&mut self, sync_values: Vec<SyncValuesStruct>) {
        (**self).sync_values(sync_values);
    }
}

/// Shared runtime hooks consumed by `QueryCacheBase`.
///
/// This preserves upstream ownership in the shared query-cache files while
/// allowing backend runtimes to provide the required barriers / segment hooks.
pub trait QueryCacheRuntimeHandle {
    fn bind_3d_engine(&mut self) {}
    fn barriers(&mut self, _is_prebarrier: bool) {}
    fn end_host_conditional_rendering(&mut self) {}
    fn pause_host_conditional_rendering(&mut self) {}
    fn resume_host_conditional_rendering(&mut self) {}
    fn host_conditional_rendering_compare_value(
        &mut self,
        _object_1: LookupData,
        _qc_dirty: bool,
    ) -> bool {
        false
    }
    fn host_conditional_rendering_compare_values(
        &mut self,
        _object_1: LookupData,
        _object_2: LookupData,
        _qc_dirty: bool,
        _equal_check: bool,
    ) -> bool {
        false
    }
}

/// Shared device-memory writer surface used by query guest writeback paths.
///
/// Upstream owns a concrete `MaxwellDeviceMemoryManager&`. Rust uses a trait
/// bridge here so tests and partial backend wiring can provide the same owner
/// behavior without moving the logic out of this file.
pub trait DeviceMemoryWriter {
    fn write_u32(&mut self, addr: u64, value: u32);
    fn write_u64(&mut self, addr: u64, value: u64);
}

/// Shared GPU address translation owner used by `QueryCacheBase::counter_report`.
///
/// Upstream owns `Tegra::MemoryManager& gpu_memory` through `ChannelSetupCaches`.
pub trait GpuAddressTranslator {
    fn gpu_to_cpu_address(&self, gpu_addr: u64) -> Option<u64>;
}

/// Shared GPU tick source used by timestamped query writeback.
///
/// Upstream owns `Tegra::GPU& gpu`.
pub trait GpuTickSource {
    fn get_ticks(&self) -> u64;
}

/// Shared render-enable state consumed by `QueryCacheBase::accelerate_host_conditional_rendering`.
///
/// Upstream reads this from `maxwell3d->regs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderConditionState {
    pub override_mode: u32,
    pub comparison_mode: ComparisonMode,
    pub address: u64,
}

/// Shared owner for render-enable state.
///
/// Upstream gets this through `ChannelSetupCaches<ChannelInfo>::maxwell3d`.
pub trait RenderConditionStateSource {
    fn render_condition_state(&self) -> RenderConditionState;
}

impl DeviceMemoryWriter for MaxwellDeviceMemoryManager {
    fn write_u32(&mut self, addr: u64, value: u32) {
        MaxwellDeviceMemoryManager::write_u32(self, addr, value);
    }

    fn write_u64(&mut self, addr: u64, value: u64) {
        MaxwellDeviceMemoryManager::write_u64(self, addr, value);
    }
}

impl GpuAddressTranslator for MemoryManager {
    fn gpu_to_cpu_address(&self, gpu_addr: u64) -> Option<u64> {
        MemoryManager::gpu_to_cpu_address(self, gpu_addr)
    }
}

/// Shared guest-backed streamer owner.
///
/// Maps to C++ `GuestStreamer<Traits>`.
pub struct GuestStreamer<R> {
    pub simple_streamer: SimpleStreamer<GuestQuery>,
    pub runtime: R,
    pub pending_sync: VecDeque<usize>,
}

impl<R> GuestStreamer<R> {
    pub fn new(id: usize, runtime: R) -> Self {
        Self {
            simple_streamer: SimpleStreamer::new(id),
            runtime,
            pending_sync: VecDeque::new(),
        }
    }
}

impl<R> StreamerInterface for GuestStreamer<R>
where
    R: SyncValuesRuntime,
{
    fn get_query(&self, id: usize) -> Option<&QueryBase> {
        self.simple_streamer.get_query(id).map(|query| &query.base)
    }

    fn get_query_mut(&mut self, id: usize) -> Option<&mut QueryBase> {
        self.simple_streamer
            .get_query_mut(id)
            .map(|query| &mut query.base)
    }

    fn write_counter(
        &mut self,
        address: VAddr,
        has_timestamp: bool,
        value: u32,
        _subreport: Option<u32>,
    ) -> usize {
        let new_id =
            self.simple_streamer
                .build_query(GuestQuery::new(has_timestamp, address, value as u64));
        self.pending_sync.push_back(new_id);
        new_id
    }

    fn has_pending_sync(&self) -> bool {
        !self.pending_sync.is_empty()
    }

    fn sync_writes(&mut self) {
        if self.pending_sync.is_empty() {
            return;
        }
        let mut sync_values = Vec::with_capacity(self.pending_sync.len());
        for &pending_id in &self.pending_sync {
            let Some(query) = self.simple_streamer.slot_queries.get_mut(pending_id) else {
                continue;
            };
            if query.base.flags.intersects(QueryFlagBits::IS_REWRITTEN)
                || query.base.flags.intersects(QueryFlagBits::IS_INVALIDATED)
            {
                continue;
            }
            query.base.flags |= QueryFlagBits::IS_HOST_SYNCED;
            sync_values.push(SyncValuesStruct {
                address: query.base.guest_address,
                value: query.base.value,
                size: if query.base.flags.intersects(QueryFlagBits::HAS_TIMESTAMP) {
                    8
                } else {
                    4
                },
            });
        }
        self.pending_sync.clear();
        if !sync_values.is_empty() {
            self.runtime.sync_values(sync_values);
        }
    }

    fn free(&mut self, query_id: usize) {
        self.simple_streamer.free(query_id);
    }

    fn base(&self) -> &StreamerInterfaceBase {
        &self.simple_streamer.base
    }

    fn base_mut(&mut self) -> &mut StreamerInterfaceBase {
        &mut self.simple_streamer.base
    }
}

/// Shared fixed-value streamer owner.
///
/// Maps to C++ `StubStreamer<Traits>`.
pub struct StubStreamer<R> {
    pub guest_streamer: GuestStreamer<R>,
    pub stub_value: u32,
}

impl<R> StubStreamer<R> {
    pub fn new(id: usize, runtime: R, stub_value: u32) -> Self {
        Self {
            guest_streamer: GuestStreamer::new(id, runtime),
            stub_value,
        }
    }
}

impl<R> StreamerInterface for StubStreamer<R>
where
    R: SyncValuesRuntime,
{
    fn get_query(&self, id: usize) -> Option<&QueryBase> {
        self.guest_streamer.get_query(id)
    }

    fn get_query_mut(&mut self, id: usize) -> Option<&mut QueryBase> {
        self.guest_streamer.get_query_mut(id)
    }

    fn write_counter(
        &mut self,
        address: VAddr,
        has_timestamp: bool,
        _value: u32,
        subreport: Option<u32>,
    ) -> usize {
        self.guest_streamer
            .write_counter(address, has_timestamp, self.stub_value, subreport)
    }

    fn has_pending_sync(&self) -> bool {
        self.guest_streamer.has_pending_sync()
    }

    fn sync_writes(&mut self) {
        self.guest_streamer.sync_writes();
    }

    fn free(&mut self, query_id: usize) {
        self.guest_streamer.free(query_id);
    }

    fn base(&self) -> &StreamerInterfaceBase {
        self.guest_streamer.base()
    }

    fn base_mut(&mut self) -> &mut StreamerInterfaceBase {
        self.guest_streamer.base_mut()
    }
}

/// Shared inner owner state for `QueryCacheBase`.
///
/// Maps to C++ `QueryCacheBase<Traits>::QueryCacheBaseImpl`.
pub struct QueryCacheBaseImpl {
    pub owner: Option<*mut QueryCacheBase>,
    pub rasterizer: Option<*mut dyn RasterizerInterface>,
    pub device_memory: Option<*mut dyn DeviceMemoryWriter>,
    pub runtime: Option<*mut dyn QueryCacheRuntimeHandle>,
    pub gpu_memory: Option<*mut dyn GpuAddressTranslator>,
    pub gpu: Option<*mut dyn GpuTickSource>,
    pub render_condition_source: Option<*mut dyn RenderConditionStateSource>,
    pub streamer_mask: u64,
    pub streamers: [Option<*mut dyn StreamerInterface>; MAX_QUERY_TYPES],
    pub flush_guard: Mutex<()>,
    pub flushes_pending: VecDeque<u64>,
    pub pending_unregister: Vec<QueryLocation>,
}

impl QueryCacheBaseImpl {
    pub fn new() -> Self {
        Self {
            owner: None,
            rasterizer: None,
            device_memory: None,
            runtime: None,
            gpu_memory: None,
            gpu: None,
            render_condition_source: None,
            streamer_mask: 0,
            streamers: std::array::from_fn(|_| None),
            flush_guard: Mutex::new(()),
            flushes_pending: VecDeque::new(),
            pending_unregister: Vec::new(),
        }
    }

    pub fn bind_owner(&mut self, owner: *mut QueryCacheBase) {
        self.owner = Some(owner);
    }

    pub fn bind_rasterizer(&mut self, rasterizer: &mut dyn RasterizerInterface) {
        let rasterizer_ptr: *mut dyn RasterizerInterface = rasterizer;
        let erased_ptr: *mut dyn RasterizerInterface =
            unsafe { std::mem::transmute(rasterizer_ptr) };
        self.rasterizer = Some(erased_ptr);
    }

    pub fn bind_device_memory(&mut self, device_memory: &mut dyn DeviceMemoryWriter) {
        let device_memory_ptr: *mut dyn DeviceMemoryWriter = device_memory;
        let erased_ptr: *mut dyn DeviceMemoryWriter =
            unsafe { std::mem::transmute(device_memory_ptr) };
        self.device_memory = Some(erased_ptr);
    }

    pub fn bind_runtime(&mut self, runtime: &mut dyn QueryCacheRuntimeHandle) {
        let runtime_ptr: *mut dyn QueryCacheRuntimeHandle = runtime;
        let erased_ptr: *mut dyn QueryCacheRuntimeHandle =
            unsafe { std::mem::transmute(runtime_ptr) };
        self.runtime = Some(erased_ptr);
    }

    pub fn bind_gpu_memory(&mut self, gpu_memory: &mut dyn GpuAddressTranslator) {
        let gpu_memory_ptr: *mut dyn GpuAddressTranslator = gpu_memory;
        let erased_ptr: *mut dyn GpuAddressTranslator =
            unsafe { std::mem::transmute(gpu_memory_ptr) };
        self.gpu_memory = Some(erased_ptr);
    }

    pub fn bind_gpu(&mut self, gpu: &mut dyn GpuTickSource) {
        let gpu_ptr: *mut dyn GpuTickSource = gpu;
        let erased_ptr: *mut dyn GpuTickSource = unsafe { std::mem::transmute(gpu_ptr) };
        self.gpu = Some(erased_ptr);
    }

    pub fn bind_render_condition_source(
        &mut self,
        render_condition_source: &mut dyn RenderConditionStateSource,
    ) {
        let source_ptr: *mut dyn RenderConditionStateSource = render_condition_source;
        let erased_ptr: *mut dyn RenderConditionStateSource =
            unsafe { std::mem::transmute(source_ptr) };
        self.render_condition_source = Some(erased_ptr);
    }

    pub fn rasterizer_mut(&mut self) -> Option<&mut dyn RasterizerInterface> {
        let rasterizer = self.rasterizer.as_mut()?;
        Some(unsafe { &mut **rasterizer })
    }

    pub fn device_memory_mut(&mut self) -> Option<&mut dyn DeviceMemoryWriter> {
        let device_memory = self.device_memory.as_mut()?;
        Some(unsafe { &mut **device_memory })
    }

    pub fn runtime_mut(&mut self) -> Option<&mut dyn QueryCacheRuntimeHandle> {
        let runtime = self.runtime.as_mut()?;
        Some(unsafe { &mut **runtime })
    }

    pub fn gpu_memory(&self) -> Option<&dyn GpuAddressTranslator> {
        let gpu_memory = self.gpu_memory.as_ref()?;
        Some(unsafe { &**gpu_memory })
    }

    pub fn gpu_mut(&mut self) -> Option<&mut dyn GpuTickSource> {
        let gpu = self.gpu.as_mut()?;
        Some(unsafe { &mut **gpu })
    }

    pub fn render_condition_source(&self) -> Option<&dyn RenderConditionStateSource> {
        let render_condition_source = self.render_condition_source.as_ref()?;
        Some(unsafe { &**render_condition_source })
    }

    pub fn register_streamer(
        &mut self,
        query_type_index: usize,
        streamer: &mut dyn StreamerInterface,
    ) {
        let streamer_id = streamer.get_id();
        debug_assert!(
            streamer_id < MAX_QUERY_TYPES,
            "streamer id {} must fit in MAX_QUERY_TYPES",
            streamer_id
        );
        let streamer_ptr: *mut dyn StreamerInterface = streamer;
        let erased_ptr: *mut dyn StreamerInterface = unsafe { std::mem::transmute(streamer_ptr) };
        self.streamers[query_type_index] = Some(erased_ptr);
        self.streamer_mask |= 1u64 << streamer_id;
    }

    fn streamer_for_id(&self, streamer_id: usize) -> Option<&dyn StreamerInterface> {
        self.streamers.iter().flatten().find_map(|streamer| {
            let streamer = unsafe { &**streamer };
            (streamer.get_id() == streamer_id).then_some(streamer)
        })
    }

    fn streamer_for_id_mut(&mut self, streamer_id: usize) -> Option<&mut dyn StreamerInterface> {
        self.streamers.iter_mut().flatten().find_map(|streamer| {
            let streamer = unsafe { &mut **streamer };
            (streamer.get_id() == streamer_id).then_some(streamer)
        })
    }

    pub fn get_streamer_for_query_type(
        &self,
        query_type_index: usize,
    ) -> Option<&dyn StreamerInterface> {
        let streamer = self.streamers.get(query_type_index)?.as_ref()?;
        Some(unsafe { &**streamer })
    }

    pub fn get_streamer_for_query_type_mut(
        &mut self,
        query_type_index: usize,
    ) -> Option<&mut dyn StreamerInterface> {
        let streamer = self.streamers.get_mut(query_type_index)?.as_mut()?;
        Some(unsafe { &mut **streamer })
    }

    pub fn obtain_query(&self, location: QueryLocation) -> Option<&QueryBase> {
        let (streamer_slot, query_id) = location.unpack();
        self.get_streamer_for_query_type(streamer_slot)?
            .get_query(query_id)
    }

    pub fn obtain_query_mut(&mut self, location: QueryLocation) -> Option<&mut QueryBase> {
        let (streamer_slot, query_id) = location.unpack();
        self.get_streamer_for_query_type_mut(streamer_slot)?
            .get_query_mut(query_id)
    }

    pub fn free_query_location(&mut self, location: QueryLocation) {
        let (streamer_slot, query_id) = location.unpack();
        if let Some(streamer) = self.get_streamer_for_query_type_mut(streamer_slot) {
            streamer.free(query_id);
        }
    }

    pub fn for_each_streamer_in<F>(&self, mut mask: u64, mut func: F)
    where
        F: FnMut(&dyn StreamerInterface) -> bool,
    {
        while mask != 0 {
            let position = mask.trailing_zeros() as usize;
            mask &= !(1u64 << position);
            let Some(streamer) = self.streamer_for_id(position) else {
                continue;
            };
            if func(streamer) {
                return;
            }
        }
    }

    pub fn for_each_streamer<F>(&self, func: F)
    where
        F: FnMut(&dyn StreamerInterface) -> bool,
    {
        self.for_each_streamer_in(self.streamer_mask, func);
    }

    pub fn for_each_streamer_in_mut<F>(&mut self, mut mask: u64, mut func: F)
    where
        F: FnMut(&mut dyn StreamerInterface) -> bool,
    {
        while mask != 0 {
            let position = mask.trailing_zeros() as usize;
            mask &= !(1u64 << position);
            let Some(streamer) = self.streamer_for_id_mut(position) else {
                continue;
            };
            if func(streamer) {
                return;
            }
        }
    }

    pub fn for_each_streamer_mut<F>(&mut self, func: F)
    where
        F: FnMut(&mut dyn StreamerInterface) -> bool,
    {
        self.for_each_streamer_in_mut(self.streamer_mask, func);
    }
}

impl Default for QueryCacheBaseImpl {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct CollectRuntime {
        sync_values: Vec<SyncValuesStruct>,
    }

    impl SyncValuesRuntime for CollectRuntime {
        fn sync_values(&mut self, sync_values: Vec<SyncValuesStruct>) {
            self.sync_values.extend(sync_values);
        }
    }

    #[test]
    fn guest_streamer_sync_writes_forward_runtime_payloads() {
        let mut runtime = CollectRuntime::default();
        {
            let mut streamer = GuestStreamer::new(7, &mut runtime);
            let short_id = streamer.write_counter(0x1000, false, 0x1234, None);
            let long_id = streamer.write_counter(0x2000, true, 0x5678, None);
            streamer
                .get_query_mut(short_id)
                .unwrap()
                .flags
                .insert(QueryFlagBits::IS_REWRITTEN);

            streamer.sync_writes();

            assert!(!streamer.has_pending_sync());
            assert!(streamer
                .get_query(long_id)
                .unwrap()
                .flags
                .intersects(QueryFlagBits::IS_HOST_SYNCED));
        }
        assert_eq!(
            runtime.sync_values,
            vec![SyncValuesStruct {
                address: 0x2000,
                value: 0x5678,
                size: 8,
            }]
        );
    }

    #[test]
    fn stub_streamer_overrides_counter_value() {
        let mut runtime = CollectRuntime::default();
        let mut streamer = StubStreamer::new(3, &mut runtime, 0xDEAD_BEEF);
        streamer.write_counter(0x3000, false, 0x1111_2222, None);
        streamer.sync_writes();

        assert_eq!(runtime.sync_values.len(), 1);
        assert_eq!(runtime.sync_values[0].value, 0xDEAD_BEEF);
    }

    #[test]
    fn query_cache_base_impl_registers_streamers_into_mask() {
        let mut impl_state = QueryCacheBaseImpl::new();
        let mut runtime_a = CollectRuntime::default();
        let mut runtime_b = CollectRuntime::default();
        let mut streamer_a = GuestStreamer::new(2, &mut runtime_a);
        let mut streamer_b = GuestStreamer::new(4, &mut runtime_b);
        impl_state.register_streamer(2, &mut streamer_a);
        impl_state.register_streamer(4, &mut streamer_b);

        let mut seen = Vec::new();
        impl_state.for_each_streamer(|streamer| {
            seen.push(streamer.get_id());
            false
        });

        assert_eq!(
            impl_state.get_streamer_for_query_type(2).unwrap().get_id(),
            2
        );
        assert_eq!(
            impl_state.get_streamer_for_query_type(4).unwrap().get_id(),
            4
        );
        assert_eq!(seen, vec![2, 4]);
    }

    #[test]
    fn query_cache_base_impl_obtain_query_uses_streamer_owner() {
        let mut runtime = CollectRuntime::default();
        let mut streamer = GuestStreamer::new(1, &mut runtime);
        let query_id = streamer.write_counter(0x4000, true, 0x55AA, None);

        let mut impl_state = QueryCacheBaseImpl::new();
        impl_state.register_streamer(1, &mut streamer);

        let location = QueryLocation::new(1, query_id as u32);
        assert_eq!(
            impl_state.obtain_query(location).unwrap().guest_address,
            0x4000
        );
        impl_state.obtain_query_mut(location).unwrap().value = 0x1234;
        assert_eq!(impl_state.obtain_query(location).unwrap().value, 0x1234);
    }
}

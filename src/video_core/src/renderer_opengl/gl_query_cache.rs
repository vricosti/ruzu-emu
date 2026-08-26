// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/video_core/renderer_opengl/gl_query_cache.h and gl_query_cache.cpp
//!
//! OpenGL query cache — manages GPU occlusion/primitives queries.

use std::sync::Arc;

use parking_lot::Mutex;

use super::gl_resource_manager::OGLQuery;
use crate::control::channel_state::ChannelState;
use crate::query_cache_top::{
    AsyncJobId, CachedQueryBase, CounterHandle, HostCounterBase, LegacyCachedQuery,
    QueryCacheLegacy, QueryType, NUM_QUERY_TYPES,
};

#[cfg(test)]
use crate::query_cache_top::{AsyncJob, NULL_ASYNC_JOB_ID};

/// Map a query type to the corresponding GL target.
///
/// Corresponds to the anonymous `GetTarget()` in gl_query_cache.cpp.
fn get_target(query_type: QueryType) -> u32 {
    match query_type {
        QueryType::SamplesPassed => gl::SAMPLES_PASSED,
        QueryType::PrimitivesGenerated => gl::PRIMITIVES_GENERATED,
        QueryType::TfbPrimitivesWritten => gl::TRANSFORM_FEEDBACK_PRIMITIVES_WRITTEN,
        QueryType::Count => {
            log::error!(
                "Query type {:?} is not a concrete GL query target",
                query_type
            );
            0
        }
    }
}

/// OpenGL query cache.
///
/// Corresponds to `OpenGL::QueryCache`.
pub struct QueryCache {
    /// Per-type pool of reusable query objects.
    query_pools: Arc<Mutex<[Vec<OGLQuery>; NUM_QUERY_TYPES]>>,
    legacy: QueryCacheLegacy<CachedQuery, HostCounterHandle>,
}

impl QueryCache {
    /// Create a new query cache.
    ///
    /// Corresponds to `QueryCache::QueryCache()`.
    pub fn new() -> Self {
        let mut cache = Self {
            query_pools: Arc::new(Mutex::new(std::array::from_fn(|_| Vec::new()))),
            legacy: QueryCacheLegacy::new(),
        };
        cache.enable_counters();
        cache
    }

    /// Allocate a query object for the given type.
    ///
    /// Corresponds to `QueryCache::AllocateQuery()`.
    pub fn allocate_query(&mut self, query_type: QueryType) -> OGLQuery {
        Self::allocate_query_from_pool(&self.query_pools, query_type)
    }

    fn allocate_query_from_pool(
        query_pools: &Arc<Mutex<[Vec<OGLQuery>; NUM_QUERY_TYPES]>>,
        query_type: QueryType,
    ) -> OGLQuery {
        if let Some(query) = query_pools.lock()[query_type as usize].pop() {
            return query;
        }
        let mut query = OGLQuery::new();
        query.create(get_target(query_type));
        query
    }

    /// Return a query object to the pool.
    ///
    /// Corresponds to `QueryCache::Reserve()`.
    pub fn reserve(&mut self, query_type: QueryType, query: OGLQuery) {
        self.query_pools.lock()[query_type as usize].push(query);
    }

    /// Port of `QueryCacheLegacy::CreateChannel`.
    pub fn create_channel(&mut self, channel: &ChannelState) {
        self.legacy.create_channel(channel);
    }

    /// Port of `QueryCacheLegacy::BindToChannel`.
    pub fn bind_to_channel(&mut self, channel_id: i32) {
        self.legacy.bind_to_channel(channel_id);
    }

    /// Port of `QueryCacheLegacy::EraseChannel`.
    pub fn erase_channel(&mut self, channel_id: i32) {
        self.legacy.erase_channel(channel_id);
    }

    /// Port of `QueryCacheLegacy::EnableCounters`.
    pub fn enable_counters(&mut self) {
        let query_pools = Arc::clone(&self.query_pools);
        let mut make_counter = move |dependency, query_type| {
            Arc::new(Mutex::new(HostCounter::new(
                &query_pools,
                dependency,
                query_type,
            )))
        };
        self.legacy.enable_counters(&mut make_counter);
    }

    /// Port of `QueryCacheLegacy::ResetCounter`.
    pub fn reset_counter(&mut self, query_type: QueryType, any_command_queued: bool) {
        let query_pools = Arc::clone(&self.query_pools);
        let mut make_counter = move |dependency, query_type| {
            Arc::new(Mutex::new(HostCounter::new(
                &query_pools,
                dependency,
                query_type,
            )))
        };
        self.legacy
            .reset_counter(query_type, any_command_queued, &mut make_counter);
    }

    /// Port of `QueryCacheLegacy::DisableStreams`.
    pub fn disable_streams(&mut self, any_command_queued: bool) {
        self.legacy.disable_streams(any_command_queued);
    }

    pub fn invalidate_region(&mut self, addr: u64, size: usize, any_command_queued: bool) {
        let query_pools = Arc::clone(&self.query_pools);
        let mut make_counter = move |dependency, query_type| {
            Arc::new(Mutex::new(HostCounter::new(
                &query_pools,
                dependency,
                query_type,
            )))
        };
        self.legacy
            .invalidate_region(addr, size, any_command_queued, &mut make_counter);
    }

    pub fn flush_region(&mut self, addr: u64, size: usize, any_command_queued: bool) {
        let query_pools = Arc::clone(&self.query_pools);
        let mut make_counter = move |dependency, query_type| {
            Arc::new(Mutex::new(HostCounter::new(
                &query_pools,
                dependency,
                query_type,
            )))
        };
        self.legacy
            .flush_region(addr, size, any_command_queued, &mut make_counter);
    }

    pub fn commit_async_flushes(&mut self) {
        self.legacy.commit_async_flushes();
    }

    pub fn has_uncommitted_flushes(&self) -> bool {
        self.legacy.has_uncommitted_flushes()
    }

    pub fn should_wait_async_flushes(&self) -> bool {
        self.legacy.should_wait_async_flushes()
    }

    pub fn pop_async_flushes(&mut self, any_command_queued: bool) {
        let query_pools = Arc::clone(&self.query_pools);
        let mut make_counter = move |dependency, query_type| {
            Arc::new(Mutex::new(HostCounter::new(
                &query_pools,
                dependency,
                query_type,
            )))
        };
        self.legacy
            .pop_async_flushes(any_command_queued, &mut make_counter);
    }

    pub fn query(
        &mut self,
        gpu_addr: u64,
        query_type: QueryType,
        timestamp: Option<u64>,
        any_command_queued: bool,
        sync_operation: impl FnOnce(Box<dyn FnOnce() + Send>),
        invalidate_query_cache_writeback: impl FnOnce(u64, u64) + Send + 'static,
    ) {
        let query_pools = Arc::clone(&self.query_pools);
        let mut make_counter = move |dependency, query_type| {
            Arc::new(Mutex::new(HostCounter::new(
                &query_pools,
                dependency,
                query_type,
            )))
        };
        self.legacy.query(
            gpu_addr,
            query_type,
            timestamp,
            any_command_queued,
            &mut make_counter,
            sync_operation,
            invalidate_query_cache_writeback,
        );
    }
}

#[cfg(test)]
impl QueryCache {
    pub(crate) fn new_for_test() -> Self {
        Self {
            query_pools: Arc::new(Mutex::new(std::array::from_fn(|_| Vec::new()))),
            legacy: QueryCacheLegacy::new(),
        }
    }
}

type HostCounterHandle = Arc<Mutex<HostCounter>>;

impl CounterHandle for HostCounterHandle {
    fn query(&self, r#async: bool) -> u64 {
        self.lock().query(r#async)
    }

    fn wait_pending(&self) -> bool {
        self.lock().wait_pending()
    }

    fn depth(&self) -> u64 {
        self.lock().depth()
    }

    fn end_query(&self, any_command_queued: bool) {
        self.lock().end_query(any_command_queued);
    }
}

/// A host-side query counter.
///
/// Corresponds to `OpenGL::HostCounter`.
pub struct HostCounter {
    query_type: QueryType,
    query: Option<OGLQuery>,
    query_pools: Arc<Mutex<[Vec<OGLQuery>; NUM_QUERY_TYPES]>>,
    base: HostCounterBase<HostCounterHandle>,
}

impl HostCounter {
    /// Create and begin a new host counter.
    pub fn new(
        query_pools: &Arc<Mutex<[Vec<OGLQuery>; NUM_QUERY_TYPES]>>,
        dependency: Option<HostCounterHandle>,
        query_type: QueryType,
    ) -> Self {
        let query = QueryCache::allocate_query_from_pool(query_pools, query_type);
        unsafe {
            gl::BeginQuery(get_target(query_type), query.handle);
        }
        Self {
            query_type,
            query: Some(query),
            query_pools: Arc::clone(query_pools),
            base: HostCounterBase::new(dependency),
        }
    }

    /// End the query.
    pub fn end_query(&self, any_command_queued: bool) {
        if !any_command_queued {
            unsafe {
                gl::Flush();
            }
        }
        unsafe {
            gl::EndQuery(get_target(self.query_type));
        }
    }

    /// Block and read the query result.
    fn blocking_query(query: &OGLQuery, _async: bool) -> u64 {
        let mut value: i64 = 0;
        unsafe {
            gl::GetQueryObjecti64v(query.handle, gl::QUERY_RESULT, &mut value);
        }
        value as u64
    }

    pub fn query(&mut self, r#async: bool) -> u64 {
        let query = self.query.as_ref().expect("query must be allocated");
        self.base.query(r#async, |async_query| {
            Self::blocking_query(query, async_query)
        })
    }

    pub fn wait_pending(&self) -> bool {
        self.base.wait_pending()
    }

    pub fn depth(&self) -> u64 {
        self.base.depth()
    }
}

impl Drop for HostCounter {
    fn drop(&mut self) {
        if let Some(query) = self.query.take() {
            self.query_pools.lock()[self.query_type as usize].push(query);
        }
    }
}

/// A cached query mapped to guest memory.
///
/// Corresponds to `OpenGL::CachedQuery`.
pub struct CachedQuery {
    query_type: QueryType,
    base: CachedQueryBase<HostCounterHandle>,
}

impl CachedQuery {
    /// Create a new cached query.
    pub fn new(query_type: QueryType, cpu_addr: u64, host_ptr: *mut u8) -> Self {
        Self {
            query_type,
            base: CachedQueryBase::new(cpu_addr, host_ptr),
        }
    }

    pub fn cpu_addr(&self) -> u64 {
        self.base.cpu_addr
    }

    pub fn size_in_bytes(&self) -> u64 {
        self.base.size_in_bytes()
    }

    fn flush_base<F>(
        base: &mut CachedQueryBase<HostCounterHandle>,
        query_type: QueryType,
        cache: &mut QueryCacheLegacy<Self, HostCounterHandle>,
        any_command_queued: bool,
        make_counter: &mut F,
    ) -> u64
    where
        F: FnMut(Option<HostCounterHandle>, QueryType) -> HostCounterHandle,
    {
        let slice_counter = base.wait_pending() && cache.stream(query_type).is_enabled();
        if slice_counter {
            cache.disable_stream(query_type, any_command_queued);
        }
        // Eden's OpenGL override intentionally ignores its `async` argument
        // and calls `CachedQueryBase::Flush()` with the default `false`.
        let value = base.flush(false);
        if slice_counter {
            cache.enable_stream(query_type, make_counter);
        }
        value
    }
}

impl LegacyCachedQuery<HostCounterHandle> for CachedQuery {
    fn new(query_type: QueryType, cpu_addr: u64, host_ptr: *mut u8) -> Self {
        Self::new(query_type, cpu_addr, host_ptr)
    }

    fn cpu_addr(&self) -> u64 {
        self.cpu_addr()
    }

    fn size_in_bytes(&self) -> u64 {
        self.size_in_bytes()
    }

    fn assigned_async_job(&self) -> AsyncJobId {
        self.base.async_job()
    }

    fn set_async_job(&mut self, async_job_id: AsyncJobId) {
        self.base.set_async_job(async_job_id);
    }

    fn bind_counter<F>(
        &mut self,
        counter: Option<HostCounterHandle>,
        timestamp: Option<u64>,
        cache: &mut QueryCacheLegacy<Self, HostCounterHandle>,
        any_command_queued: bool,
        make_counter: &mut F,
    ) -> Option<(AsyncJobId, u64)>
    where
        F: FnMut(Option<HostCounterHandle>, QueryType) -> HostCounterHandle,
    {
        let query_type = self.query_type;
        self.base.bind_counter(counter, timestamp, |base| {
            Self::flush_base(base, query_type, cache, any_command_queued, make_counter)
        })
    }

    fn flush<F>(
        &mut self,
        cache: &mut QueryCacheLegacy<Self, HostCounterHandle>,
        _async: bool,
        any_command_queued: bool,
        make_counter: &mut F,
    ) -> u64
    where
        F: FnMut(Option<HostCounterHandle>, QueryType) -> HostCounterHandle,
    {
        Self::flush_base(
            &mut self.base,
            self.query_type,
            cache,
            any_command_queued,
            make_counter,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::channel_state::ChannelState;
    use crate::control::channel_state_cache::ChannelCacheAccessor;
    use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
    use crate::memory_manager::MemoryManager;
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
    fn count_query_target_uses_upstream_soft_fallback() {
        assert_eq!(get_target(QueryType::Count), 0);
    }

    #[test]
    fn bind_to_channel_wires_channel_memory_manager_through_legacy_owner() {
        let mut cache = QueryCache::new_for_test();
        let mm = Arc::new(ParkingMutex::new(MemoryManager::new(17)));
        let mut channel = ChannelState::new(5);
        channel.program_id = 0x1234;
        channel.memory_manager = Some(Arc::clone(&mm));

        cache.create_channel(&channel);
        cache.bind_to_channel(channel.bind_id);

        let bound = cache
            .legacy
            .channel_caches
            .current_channel_state()
            .and_then(ChannelCacheAccessor::gpu_memory_arc)
            .expect("channel memory manager should be bound");
        assert!(Arc::ptr_eq(&bound, &mm));
        assert_eq!(cache.legacy.channel_caches.program_id, 0x1234);

        cache.erase_channel(channel.bind_id);
        assert!(cache
            .legacy
            .channel_caches
            .current_channel_state()
            .is_none());
    }

    #[test]
    fn erasing_an_unbound_channel_preserves_bound_memory_manager() {
        let mut cache = QueryCache::new_for_test();
        let bound_mm = Arc::new(ParkingMutex::new(MemoryManager::new(17)));
        let other_mm = Arc::new(ParkingMutex::new(MemoryManager::new(18)));
        let mut bound_channel = ChannelState::new(5);
        bound_channel.memory_manager = Some(Arc::clone(&bound_mm));
        let mut other_channel = ChannelState::new(6);
        other_channel.memory_manager = Some(other_mm);

        cache.create_channel(&bound_channel);
        cache.create_channel(&other_channel);
        cache.bind_to_channel(bound_channel.bind_id);
        cache.erase_channel(other_channel.bind_id);

        let current = cache
            .legacy
            .channel_caches
            .current_channel_state()
            .and_then(ChannelCacheAccessor::gpu_memory_arc)
            .expect("bound memory manager must survive another channel's release");
        assert!(Arc::ptr_eq(&current, &bound_mm));
    }

    #[test]
    fn async_flush_queue_tracks_pending_and_collects_query_values() {
        let mut cache = QueryCache::new_for_test();
        let cpu_addr = 0x5510_6000u64;
        let page = cpu_addr >> 12;
        let mut query_backing = [0xffu8; 16];
        let async_job_id = cache.legacy.slot_async_jobs.lock().insert(AsyncJob {
            collected: false,
            value: 99,
            query_location: cpu_addr,
            timestamp: Some(0x1234),
        });

        cache.legacy.cached_queries.insert(
            page,
            vec![CachedQuery {
                query_type: QueryType::SamplesPassed,
                base: CachedQueryBase {
                    cpu_addr,
                    host_ptr: query_backing.as_mut_ptr(),
                    counter: None,
                    timestamp: Some(0x1234),
                    assigned_async_job: async_job_id,
                },
            }],
        );
        cache.legacy.uncommitted_flushes = Some(vec![async_job_id]);

        assert!(cache.has_uncommitted_flushes());
        cache.commit_async_flushes();
        assert!(!cache.has_uncommitted_flushes());
        assert!(cache.should_wait_async_flushes());

        cache.pop_async_flushes(false);

        assert!(!cache.should_wait_async_flushes());
        assert!(cache
            .legacy
            .cached_queries
            .get(&page)
            .map(|queries| queries.is_empty())
            .unwrap_or(true));
        let async_jobs = cache.legacy.slot_async_jobs.lock();
        let job = async_jobs.get(async_job_id);
        assert!(job.collected);
        assert_eq!(job.value, 0);
        assert_eq!(&query_backing[0..8], &0u64.to_ne_bytes());
    }

    #[test]
    fn sync_flush_writes_immediate_guest_value_and_timestamp() {
        let mut cache = QueryCache::new_for_test();
        let (mm, mut backing) = make_query_memory_manager(0x5038_50000, 0x5510_6000, 0x1000);

        let mut query = CachedQuery {
            query_type: QueryType::SamplesPassed,
            base: CachedQueryBase {
                cpu_addr: 0x5510_6000,
                host_ptr: backing.as_mut_ptr(),
                counter: None,
                timestamp: Some(0x1122_3344_5566_7788),
                assigned_async_job: NULL_ASYNC_JOB_ID,
            },
        };

        // `MemoryManager::ReadBlock` reaches this flush while already holding
        // the channel memory-manager mutex. Eden writes through the retained
        // host pointer, so the flush must not try to acquire that mutex again.
        let _memory_manager_guard = mm.lock();
        let mut unexpected_counter = |_, _| panic!("counter factory must remain unused");
        let value = LegacyCachedQuery::flush(
            &mut query,
            &mut cache.legacy,
            false,
            false,
            &mut unexpected_counter,
        );
        assert_eq!(value, 0);

        assert_eq!(&backing[0..8], &0u64.to_le_bytes());
        assert_eq!(&backing[8..16], &0x1122_3344_5566_7788u64.to_le_bytes());
    }

    #[test]
    fn flush_region_only_touches_overlapping_pages() {
        let mut cache = QueryCache::new_for_test();
        let hit_page = 0x5510_6000u64 >> 12;
        let cold_page = 0x6610_6000u64 >> 12;
        let mut hit_backing = [0u8; 8];
        let async_job_id = cache.legacy.slot_async_jobs.lock().insert(AsyncJob {
            collected: false,
            value: 0,
            query_location: 0x5510_6000,
            timestamp: None,
        });
        cache.legacy.cached_queries.insert(
            hit_page,
            vec![CachedQuery {
                query_type: QueryType::SamplesPassed,
                base: CachedQueryBase {
                    cpu_addr: 0x5510_6000,
                    host_ptr: hit_backing.as_mut_ptr(),
                    counter: None,
                    timestamp: None,
                    assigned_async_job: async_job_id,
                },
            }],
        );
        cache.legacy.cached_queries.insert(
            cold_page,
            vec![CachedQuery::new(
                QueryType::SamplesPassed,
                0x6610_6000,
                std::ptr::null_mut(),
            )],
        );

        cache.flush_region(0x5510_6000, 8, false);

        assert!(cache
            .legacy
            .cached_queries
            .get(&hit_page)
            .is_some_and(Vec::is_empty));
        assert!(cache.legacy.cached_queries.contains_key(&cold_page));
    }

    #[test]
    fn flush_region_removes_query_without_async_job_after_soft_assert() {
        let mut cache = QueryCache::new_for_test();
        let cpu_addr = 0x5510_6000u64;
        let page = cpu_addr >> 12;
        let mut backing = [0u8; 8];
        cache.legacy.cached_queries.insert(
            page,
            vec![CachedQuery {
                query_type: QueryType::SamplesPassed,
                base: CachedQueryBase {
                    cpu_addr,
                    host_ptr: backing.as_mut_ptr(),
                    counter: None,
                    timestamp: None,
                    assigned_async_job: NULL_ASYNC_JOB_ID,
                },
            }],
        );

        cache.flush_region(cpu_addr, 8, false);

        assert!(cache
            .legacy
            .cached_queries
            .get(&page)
            .is_some_and(Vec::is_empty));
    }

    #[test]
    fn commit_async_flushes_preserves_empty_batches_as_non_waiting() {
        let mut cache = QueryCache::new_for_test();
        cache.commit_async_flushes();
        assert!(!cache.should_wait_async_flushes());
        cache.pop_async_flushes(false);
        assert!(!cache.should_wait_async_flushes());
    }
}

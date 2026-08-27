// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/query_cache.h
//!
//! Shared legacy query-cache owners used by backend query cache implementations.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use common::slot_vector::{SlotId, SlotVector};
use parking_lot::ReentrantMutex;

use crate::control::channel_state::ChannelState;
use crate::control::channel_state_cache::{ChannelCacheAccessor, ChannelInfo, ChannelSetupCaches};
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;

/// Query types supported by the GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum QueryType {
    SamplesPassed = 0,
    PrimitivesGenerated = 1,
    TfbPrimitivesWritten = 2,
    Count = 3,
}

/// Number of query types.
pub const NUM_QUERY_TYPES: usize = QueryType::Count as usize;

/// Slot ID for async jobs.
pub type AsyncJobId = SlotId;

/// Null async job ID.
pub const NULL_ASYNC_JOB_ID: AsyncJobId = SlotId { index: 0 };

/// Async flush bookkeeping.
#[derive(Debug, Clone, Default)]
pub struct AsyncJob {
    pub collected: bool,
    pub value: u64,
    pub query_location: u64,
    pub timestamp: Option<u64>,
}

/// Shared operations required by `CounterStreamBase`, `HostCounterBase`, and
/// `CachedQueryBase` to manipulate backend-specific host counter handles.
pub trait CounterHandle: Clone {
    fn query(&self, r#async: bool) -> u64;
    fn wait_pending(&self) -> bool;
    fn depth(&self) -> u64;
    fn end_query(&self, any_command_queued: bool);
}

/// Shared dependency/result state for backend host counters.
pub struct HostCounterBase<H: CounterHandle> {
    pub dependency: Option<H>,
    pub result: Option<u64>,
    pub depth: u64,
    pub base_result: u64,
}

impl<H: CounterHandle> HostCounterBase<H> {
    pub fn new(mut dependency: Option<H>) -> Self {
        let mut depth = dependency
            .as_ref()
            .map(|dep| dep.depth().wrapping_add(1))
            .unwrap_or(0);
        let mut base_result = 0;
        if depth > 96 {
            depth = 0;
            if let Some(dep) = dependency.take() {
                base_result = dep.query(false);
            }
        }
        Self {
            dependency,
            result: None,
            depth,
            base_result,
        }
    }

    pub fn query<F>(&mut self, r#async: bool, blocking_query: F) -> u64
    where
        F: FnOnce(bool) -> u64,
    {
        if let Some(result) = self.result {
            return result;
        }
        let mut value = blocking_query(r#async).wrapping_add(self.base_result);
        if let Some(dep) = self.dependency.take() {
            value = value.wrapping_add(dep.query(false));
        }
        self.result = Some(value);
        value
    }

    pub fn wait_pending(&self) -> bool {
        self.result.is_some()
    }

    pub fn depth(&self) -> u64 {
        self.depth
    }
}

/// Shared guest-mapped query state.
pub struct CachedQueryBase<H: CounterHandle> {
    pub cpu_addr: u64,
    pub host_ptr: *mut u8,
    pub counter: Option<H>,
    pub timestamp: Option<u64>,
    pub assigned_async_job: AsyncJobId,
}

// Upstream retains the same non-owning writable pointer in `CachedQueryBase`.
// The mapped guest backing outlives cached queries and cache invalidation removes
// queries before that backing is released.
unsafe impl<H: CounterHandle + Send> Send for CachedQueryBase<H> {}

impl<H: CounterHandle> CachedQueryBase<H> {
    pub fn new(cpu_addr: u64, host_ptr: *mut u8) -> Self {
        Self {
            cpu_addr,
            host_ptr,
            counter: None,
            timestamp: None,
            assigned_async_job: NULL_ASYNC_JOB_ID,
        }
    }

    pub fn bind_counter<F>(
        &mut self,
        counter: Option<H>,
        timestamp: Option<u64>,
        flush_existing: F,
    ) -> Option<(AsyncJobId, u64)>
    where
        F: FnOnce(&mut Self) -> u64,
    {
        let result = if self.counter.is_some() {
            let async_job_id = self.assigned_async_job;
            Some((async_job_id, flush_existing(self)))
        } else {
            None
        };
        self.counter = counter;
        self.timestamp = timestamp;
        result
    }

    /// Flush the query value to its retained guest host pointer.
    ///
    /// Maps to upstream `CachedQueryBase::Flush`.
    pub fn flush(&mut self, r#async: bool) -> u64 {
        let value = self
            .counter
            .as_ref()
            .map(|counter| counter.query(r#async))
            .unwrap_or(0);
        if r#async {
            return value;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(
                (&value as *const u64).cast::<u8>(),
                self.host_ptr,
                std::mem::size_of::<u64>(),
            );
            if let Some(timestamp) = self.timestamp {
                std::ptr::copy_nonoverlapping(
                    (&timestamp as *const u64).cast::<u8>(),
                    self.host_ptr.add(8),
                    std::mem::size_of::<u64>(),
                );
            }
        }
        value
    }

    pub fn size_in_bytes(&self) -> u64 {
        Self::size_in_bytes_with_timestamp(self.timestamp.is_some())
    }

    /// Rust cannot overload `SizeInBytes`; this is its static boolean form.
    pub const fn size_in_bytes_with_timestamp(with_timestamp: bool) -> u64 {
        if with_timestamp {
            16
        } else {
            8
        }
    }

    pub fn set_async_job(&mut self, assigned_async_job: AsyncJobId) {
        self.assigned_async_job = assigned_async_job;
    }

    pub fn async_job(&self) -> AsyncJobId {
        self.assigned_async_job
    }

    pub fn wait_pending(&self) -> bool {
        self.counter
            .as_ref()
            .is_some_and(CounterHandle::wait_pending)
    }
}

/// Backend operations required by upstream's
/// `QueryCacheLegacy<QueryCache, CachedQuery, CounterStream, HostCounter>`.
///
/// The generic cache owns registration, region iteration, stream lifecycle and
/// async-job ordering. Only the backend-specific cached-query flush remains in
/// the renderer counterpart, as it does in Eden.
pub trait LegacyCachedQuery<H: CounterHandle>: Sized {
    fn new(query_type: QueryType, cpu_addr: u64, host_ptr: *mut u8) -> Self;
    fn cpu_addr(&self) -> u64;
    fn size_in_bytes(&self) -> u64;
    fn assigned_async_job(&self) -> AsyncJobId;
    fn set_async_job(&mut self, async_job_id: AsyncJobId);

    fn bind_counter<F>(
        &mut self,
        counter: Option<H>,
        timestamp: Option<u64>,
        cache: &mut QueryCacheLegacy<Self, H>,
        any_command_queued: bool,
        make_counter: &mut F,
    ) -> Option<(AsyncJobId, u64)>
    where
        F: FnMut(Option<H>, QueryType) -> H;

    fn flush<F>(
        &mut self,
        cache: &mut QueryCacheLegacy<Self, H>,
        r#async: bool,
        any_command_queued: bool,
        make_counter: &mut F,
    ) -> u64
    where
        F: FnMut(Option<H>, QueryType) -> H;
}

/// Shared per-query-type stream state.
pub struct CounterStreamBase<H: CounterHandle> {
    pub query_type: QueryType,
    pub current: Option<H>,
    pub last: Option<H>,
}

impl<H: CounterHandle> CounterStreamBase<H> {
    pub fn new(query_type: QueryType) -> Self {
        Self {
            query_type,
            current: None,
            last: None,
        }
    }

    /// Port of `CounterStreamBase::Reset`.
    pub fn reset<F>(&mut self, any_command_queued: bool, make_counter: &mut F)
    where
        F: FnMut(Option<H>, QueryType) -> H,
    {
        if let Some(current) = self.current.take() {
            current.end_query(any_command_queued);
            self.current = Some(make_counter(None, self.query_type));
        }
        self.last = None;
    }

    /// Port of `CounterStreamBase::Current`.
    pub fn current<F>(&mut self, any_command_queued: bool, make_counter: &mut F) -> Option<H>
    where
        F: FnMut(Option<H>, QueryType) -> H,
    {
        let current = self.current.take()?;
        current.end_query(any_command_queued);
        self.last = Some(current);
        self.current = Some(make_counter(self.last.clone(), self.query_type));
        self.last.clone()
    }

    pub fn is_enabled(&self) -> bool {
        self.current.is_some()
    }

    /// Port of `CounterStreamBase::Enable`.
    pub fn enable<F>(&mut self, make_counter: &mut F)
    where
        F: FnMut(Option<H>, QueryType) -> H,
    {
        if self.current.is_none() {
            self.current = Some(make_counter(self.last.clone(), self.query_type));
        }
    }

    pub fn disable(&mut self, any_command_queued: bool) {
        if let Some(current) = self.current.as_ref() {
            current.end_query(any_command_queued);
        }
        self.last = self.current.take();
    }
}

/// Shared `QueryCacheLegacy<QueryCache, CachedQuery, CounterStream, HostCounter>`
/// state owned by backend query caches.
pub struct QueryCacheLegacy<Q, H: CounterHandle> {
    pub mutex: Arc<ReentrantMutex<()>>,
    pub channel_caches: ChannelSetupCaches<ChannelInfo>,
    pub cached_queries: HashMap<u64, Vec<Q>>,
    pub streams: [CounterStreamBase<H>; NUM_QUERY_TYPES],
    pub slot_async_jobs: Arc<parking_lot::Mutex<SlotVector<AsyncJob>>>,
    pub uncommitted_flushes: Option<Vec<AsyncJobId>>,
    pub committed_flushes: VecDeque<Option<Vec<AsyncJobId>>>,
}

impl<Q, H: CounterHandle> QueryCacheLegacy<Q, H> {
    pub fn new() -> Self {
        let mut slot_async_jobs = SlotVector::new();
        let null_id = slot_async_jobs.insert(AsyncJob::default());
        debug_assert_eq!(null_id, NULL_ASYNC_JOB_ID);
        Self {
            mutex: Arc::new(ReentrantMutex::new(())),
            channel_caches: ChannelSetupCaches::new(),
            cached_queries: HashMap::new(),
            streams: [
                CounterStreamBase::new(QueryType::SamplesPassed),
                CounterStreamBase::new(QueryType::PrimitivesGenerated),
                CounterStreamBase::new(QueryType::TfbPrimitivesWritten),
            ],
            slot_async_jobs: Arc::new(parking_lot::Mutex::new(slot_async_jobs)),
            uncommitted_flushes: None,
            committed_flushes: VecDeque::new(),
        }
    }

    pub fn stream(&self, query_type: QueryType) -> &CounterStreamBase<H> {
        &self.streams[query_type as usize]
    }

    pub fn stream_mut(&mut self, query_type: QueryType) -> &mut CounterStreamBase<H> {
        &mut self.streams[query_type as usize]
    }

    pub fn create_channel(&mut self, channel: &ChannelState) {
        self.channel_caches.create_channel(channel);
    }

    pub fn bind_to_channel(&mut self, id: i32) {
        self.channel_caches.bind_to_channel(id);
    }

    pub fn erase_channel(&mut self, id: i32) {
        self.channel_caches.erase_channel(id);
    }

    /// Port of `QueryCacheLegacy::EnableCounters`.
    pub fn enable_counters<F>(&mut self, make_counter: &mut F)
    where
        F: FnMut(Option<H>, QueryType) -> H,
    {
        let mutex = Arc::clone(&self.mutex);
        let _lock = mutex.lock();
        for query_type in [
            QueryType::SamplesPassed,
            QueryType::PrimitivesGenerated,
            QueryType::TfbPrimitivesWritten,
        ] {
            self.enable_stream(query_type, make_counter);
        }
    }

    /// Port of `QueryCacheLegacy::ResetCounter`.
    pub fn reset_counter<F>(
        &mut self,
        query_type: QueryType,
        any_command_queued: bool,
        make_counter: &mut F,
    ) where
        F: FnMut(Option<H>, QueryType) -> H,
    {
        let mutex = Arc::clone(&self.mutex);
        let _lock = mutex.lock();
        self.stream_mut(query_type)
            .reset(any_command_queued, make_counter);
    }

    /// Port of `QueryCacheLegacy::DisableStreams`.
    pub fn disable_streams(&mut self, any_command_queued: bool) {
        let mutex = Arc::clone(&self.mutex);
        let _lock = mutex.lock();
        for stream in &mut self.streams {
            stream.disable(any_command_queued);
        }
    }

    /// Port of `CounterStreamBase::Enable`.
    pub fn enable_stream<F>(&mut self, query_type: QueryType, make_counter: &mut F)
    where
        F: FnMut(Option<H>, QueryType) -> H,
    {
        self.stream_mut(query_type).enable(make_counter);
    }

    /// Port of `CounterStreamBase::Disable`.
    pub fn disable_stream(&mut self, query_type: QueryType, any_command_queued: bool) {
        self.stream_mut(query_type).disable(any_command_queued);
    }

    /// Port of `CounterStreamBase::Current`.
    pub fn current_counter<F>(
        &mut self,
        query_type: QueryType,
        any_command_queued: bool,
        make_counter: &mut F,
    ) -> Option<H>
    where
        F: FnMut(Option<H>, QueryType) -> H,
    {
        self.stream_mut(query_type)
            .current(any_command_queued, make_counter)
    }

    pub fn commit_async_flushes(&mut self) {
        let mutex = Arc::clone(&self.mutex);
        let _lock = mutex.lock();
        self.committed_flushes
            .push_back(self.uncommitted_flushes.take());
    }

    pub fn has_uncommitted_flushes(&self) -> bool {
        let _lock = self.mutex.lock();
        self.uncommitted_flushes.is_some()
    }

    pub fn should_wait_async_flushes(&self) -> bool {
        let _lock = self.mutex.lock();
        matches!(self.committed_flushes.front(), Some(Some(_)))
    }
}

impl<Q, H: CounterHandle> Default for QueryCacheLegacy<Q, H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Q, H> QueryCacheLegacy<Q, H>
where
    Q: LegacyCachedQuery<H>,
    H: CounterHandle,
{
    /// Port of `QueryCacheLegacy::InvalidateRegion`.
    pub fn invalidate_region<F>(
        &mut self,
        addr: u64,
        size: usize,
        any_command_queued: bool,
        make_counter: &mut F,
    ) where
        F: FnMut(Option<H>, QueryType) -> H,
    {
        let mutex = Arc::clone(&self.mutex);
        let _lock = mutex.lock();
        self.flush_and_remove_region(addr, size, false, any_command_queued, make_counter);
    }

    /// Port of `QueryCacheLegacy::FlushRegion`.
    pub fn flush_region<F>(
        &mut self,
        addr: u64,
        size: usize,
        any_command_queued: bool,
        make_counter: &mut F,
    ) where
        F: FnMut(Option<H>, QueryType) -> H,
    {
        let mutex = Arc::clone(&self.mutex);
        let _lock = mutex.lock();
        self.flush_and_remove_region(addr, size, false, any_command_queued, make_counter);
    }

    /// Port of `QueryCacheLegacy::InvalidateRegion` and `FlushRegion`'s shared
    /// `FlushAndRemoveRegion` owner.
    fn flush_and_remove_region<F>(
        &mut self,
        addr: u64,
        size: usize,
        r#async: bool,
        any_command_queued: bool,
        make_counter: &mut F,
    ) where
        F: FnMut(Option<H>, QueryType) -> H,
    {
        let addr_begin = addr;
        let addr_end = addr_begin.wrapping_add(size as u64);
        let page_begin = addr_begin >> 12;
        let page_end = addr_end >> 12;
        for page in page_begin..=page_end {
            let Some(mut page_queries) = self.cached_queries.remove(&page) else {
                continue;
            };
            let mut kept = Vec::with_capacity(page_queries.len());
            for mut query in page_queries.drain(..) {
                let cache_begin = query.cpu_addr();
                let cache_end = cache_begin.wrapping_add(query.size_in_bytes());
                if cache_begin < addr_end && addr_begin < cache_end {
                    let async_job_id = query.assigned_async_job();
                    let value = query.flush(self, r#async, any_command_queued, make_counter);
                    if async_job_id == NULL_ASYNC_JOB_ID {
                        log::error!("flushed cached query does not own an async job");
                        continue;
                    }
                    let mut async_jobs = self.slot_async_jobs.lock();
                    let async_job = async_jobs.get_mut(async_job_id);
                    async_job.collected = true;
                    async_job.value = value;
                    query.set_async_job(NULL_ASYNC_JOB_ID);
                } else {
                    kept.push(query);
                }
            }
            // `std::erase_if(contents, ...)` leaves Eden's map entry in place
            // when every cached query on the page was removed.
            self.cached_queries.insert(page, kept);
        }
    }

    /// Port of `QueryCacheLegacy::PopAsyncFlushes`.
    pub fn pop_async_flushes<F>(&mut self, any_command_queued: bool, make_counter: &mut F)
    where
        F: FnMut(Option<H>, QueryType) -> H,
    {
        let mutex = Arc::clone(&self.mutex);
        let _lock = mutex.lock();
        let Some(flush_list) = self.committed_flushes.front().cloned() else {
            return;
        };
        let Some(flush_list) = flush_list else {
            self.committed_flushes.pop_front();
            return;
        };
        for async_job_id in flush_list {
            let (should_collect, query_location) = {
                let async_jobs = self.slot_async_jobs.lock();
                let async_job = async_jobs.get(async_job_id);
                (!async_job.collected, async_job.query_location)
            };
            if should_collect {
                self.flush_and_remove_region(
                    query_location,
                    2,
                    true,
                    any_command_queued,
                    make_counter,
                );
            }
        }
        self.committed_flushes.pop_front();
    }

    /// Port of `QueryCacheLegacy::Register`.
    fn register(
        &mut self,
        query_type: QueryType,
        cpu_addr: u64,
        host_ptr: *mut u8,
        _timestamp: bool,
    ) -> (u64, usize) {
        let page = cpu_addr >> 12;
        let contents = self.cached_queries.entry(page).or_default();
        contents.push(Q::new(query_type, cpu_addr, host_ptr));
        (page, contents.len() - 1)
    }

    /// Port of `QueryCacheLegacy::TryGet`.
    fn try_get(&self, cpu_addr: u64) -> Option<(u64, usize)> {
        let page = cpu_addr >> 12;
        let index = self
            .cached_queries
            .get(&page)?
            .iter()
            .position(|query| query.cpu_addr() == cpu_addr)?;
        Some((page, index))
    }

    /// Port of `QueryCacheLegacy::AsyncFlushQuery`.
    fn async_flush_query<I>(
        &mut self,
        page: u64,
        query_index: usize,
        timestamp: Option<u64>,
        device_memory: Arc<MaxwellDeviceMemoryManager>,
        invalidate_query_cache_writeback: I,
    ) -> Box<dyn FnOnce() + Send>
    where
        I: FnOnce(u64, u64) + Send + 'static,
    {
        let new_async_job_id = self.slot_async_jobs.lock().insert(AsyncJob::default());
        let query_location = {
            let query = &mut self
                .cached_queries
                .get_mut(&page)
                .expect("cached query page must exist")[query_index];
            query.set_async_job(new_async_job_id);
            query.cpu_addr()
        };
        {
            let mut async_jobs = self.slot_async_jobs.lock();
            let async_job = async_jobs.get_mut(new_async_job_id);
            async_job.query_location = query_location;
            async_job.collected = false;
        }
        self.uncommitted_flushes
            .get_or_insert_with(Vec::new)
            .push(new_async_job_id);

        let async_jobs = Arc::clone(&self.slot_async_jobs);
        let mutex = Arc::clone(&self.mutex);
        Box::new(move || {
            let (value, address) = {
                let _lock = mutex.lock();
                let async_job = async_jobs.lock().take(new_async_job_id);
                (async_job.value, async_job.query_location)
            };
            if let Some(timestamp) = timestamp {
                device_memory
                    .smmu_write_block_unsafe(address.wrapping_add(8), &timestamp.to_ne_bytes());
                device_memory.smmu_write_block_unsafe(address, &value.to_ne_bytes());
                invalidate_query_cache_writeback(address, 16);
            } else {
                device_memory.smmu_write_block_unsafe(address, &(value as u32).to_ne_bytes());
                invalidate_query_cache_writeback(address, 4);
            }
        })
    }

    /// Port of `QueryCacheLegacy::Query`.
    pub fn query<F, S, I>(
        &mut self,
        gpu_addr: u64,
        query_type: QueryType,
        timestamp: Option<u64>,
        any_command_queued: bool,
        make_counter: &mut F,
        sync_operation: S,
        invalidate_query_cache_writeback: I,
    ) where
        F: FnMut(Option<H>, QueryType) -> H,
        S: FnOnce(Box<dyn FnOnce() + Send>),
        I: FnOnce(u64, u64) + Send + 'static,
    {
        let mutex = Arc::clone(&self.mutex);
        let lock = mutex.lock();
        let Some(memory_manager) = self
            .channel_caches
            .current_channel_state()
            .and_then(ChannelCacheAccessor::gpu_memory_arc)
        else {
            log::error!("QueryCacheLegacy::Query called without a bound channel memory manager");
            return;
        };
        let (cpu_addr, host_ptr, device_memory) = {
            let memory_manager = memory_manager.lock();
            let Some(cpu_addr) = memory_manager.gpu_to_cpu_address(gpu_addr) else {
                log::error!(
                    "QueryCacheLegacy::Query assertion failed: GPU address {gpu_addr:#018x} has no CPU mapping"
                );
                return;
            };
            (
                cpu_addr,
                memory_manager.get_pointer(gpu_addr),
                Arc::clone(memory_manager.device_memory()),
            )
        };

        let (page, index) = self
            .try_get(cpu_addr)
            .unwrap_or_else(|| self.register(query_type, cpu_addr, host_ptr, timestamp.is_some()));

        let previous = self.current_counter(query_type, any_command_queued, make_counter);
        let mut query = self
            .cached_queries
            .get_mut(&page)
            .expect("cached query page must exist")
            .remove(index);
        if let Some((async_job_id, value)) =
            query.bind_counter(previous, timestamp, self, any_command_queued, make_counter)
        {
            let mut async_jobs = self.slot_async_jobs.lock();
            let async_job = async_jobs.get_mut(async_job_id);
            async_job.collected = true;
            async_job.value = value;
            query.set_async_job(NULL_ASYNC_JOB_ID);
        }

        self.cached_queries
            .get_mut(&page)
            .expect("cached query page must exist")
            .insert(index, query);
        let operation = self.async_flush_query(
            page,
            index,
            timestamp,
            device_memory,
            invalidate_query_cache_writeback,
        );
        drop(lock);
        sync_operation(operation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug)]
    struct TestCounter {
        ended: std::sync::Arc<std::sync::Mutex<Vec<bool>>>,
        queried: std::sync::Arc<std::sync::Mutex<Vec<bool>>>,
        value: u64,
        depth: u64,
    }

    impl CounterHandle for TestCounter {
        fn query(&self, r#async: bool) -> u64 {
            self.queried.lock().unwrap().push(r#async);
            self.value
        }

        fn wait_pending(&self) -> bool {
            true
        }

        fn depth(&self) -> u64 {
            self.depth
        }

        fn end_query(&self, any_command_queued: bool) {
            self.ended.lock().unwrap().push(any_command_queued);
        }
    }

    struct TestCachedQuery {
        cpu_addr: u64,
        size: u64,
        async_job_id: AsyncJobId,
        observed_commands: std::sync::Arc<std::sync::Mutex<Vec<bool>>>,
        observed_committed_front: std::sync::Arc<std::sync::Mutex<Vec<bool>>>,
    }

    impl LegacyCachedQuery<TestCounter> for TestCachedQuery {
        fn new(_query_type: QueryType, cpu_addr: u64, _host_ptr: *mut u8) -> Self {
            Self {
                cpu_addr,
                size: 8,
                async_job_id: NULL_ASYNC_JOB_ID,
                observed_commands: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                observed_committed_front: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        fn cpu_addr(&self) -> u64 {
            self.cpu_addr
        }

        fn size_in_bytes(&self) -> u64 {
            self.size
        }

        fn assigned_async_job(&self) -> AsyncJobId {
            self.async_job_id
        }

        fn set_async_job(&mut self, async_job_id: AsyncJobId) {
            self.async_job_id = async_job_id;
        }

        fn bind_counter<F>(
            &mut self,
            _counter: Option<TestCounter>,
            _timestamp: Option<u64>,
            _cache: &mut QueryCacheLegacy<Self, TestCounter>,
            _any_command_queued: bool,
            _make_counter: &mut F,
        ) -> Option<(AsyncJobId, u64)>
        where
            F: FnMut(Option<TestCounter>, QueryType) -> TestCounter,
        {
            None
        }

        fn flush<F>(
            &mut self,
            cache: &mut QueryCacheLegacy<Self, TestCounter>,
            _async: bool,
            any_command_queued: bool,
            _make_counter: &mut F,
        ) -> u64
        where
            F: FnMut(Option<TestCounter>, QueryType) -> TestCounter,
        {
            self.observed_commands
                .lock()
                .unwrap()
                .push(any_command_queued);
            self.observed_committed_front
                .lock()
                .unwrap()
                .push(cache.committed_flushes.front().is_some());
            0
        }
    }

    #[test]
    fn counter_stream_current_ends_and_rotates() {
        let ended = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let queried = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let current = TestCounter {
            ended: ended.clone(),
            queried: queried.clone(),
            value: 1,
            depth: 0,
        };
        let next = TestCounter {
            ended,
            queried,
            value: 2,
            depth: 1,
        };
        let mut stream = CounterStreamBase::new(QueryType::SamplesPassed);
        stream.current = Some(current.clone());
        let mut make_counter = |dependency: Option<TestCounter>, query_type| {
            assert_eq!(dependency.as_ref().map(|counter| counter.value), Some(1));
            assert_eq!(query_type, QueryType::SamplesPassed);
            next.clone()
        };
        let previous = stream
            .current(true, &mut make_counter)
            .expect("previous query");
        assert_eq!(previous.value, 1);
        assert_eq!(stream.last.as_ref().map(|counter| counter.value), Some(1));
        assert_eq!(
            stream.current.as_ref().map(|counter| counter.value),
            Some(2)
        );
        assert_eq!(*current.ended.lock().unwrap(), vec![true]);
    }

    #[test]
    fn host_counter_base_collapses_deep_dependencies() {
        let ended = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let queried = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let dependency = TestCounter {
            ended,
            queried: queried.clone(),
            value: 9,
            depth: 96,
        };
        let mut base = HostCounterBase::new(Some(dependency));
        let value = base.query(false, |_| 3);
        assert_eq!(value, 12);
        assert_eq!(*queried.lock().unwrap(), vec![false]);
        assert_eq!(base.depth(), 0);
    }

    #[test]
    fn host_counter_dependencies_are_queried_synchronously() {
        let ended = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let queried = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let dependency = TestCounter {
            ended,
            queried: queried.clone(),
            value: 9,
            depth: 0,
        };
        let mut base = HostCounterBase::new(Some(dependency));
        assert_eq!(base.query(true, |_| 3), 12);
        assert_eq!(*queried.lock().unwrap(), vec![false]);
    }

    #[test]
    fn host_counter_accumulation_preserves_upstream_u64_wraparound() {
        let ended = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let queried = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let dependency = TestCounter {
            ended,
            queried,
            value: u64::MAX,
            depth: 0,
        };
        let mut base = HostCounterBase::new(Some(dependency));
        assert_eq!(base.query(false, |_| 2), 1);
    }

    #[test]
    fn async_jobs_reserve_null_slot_and_reuse_released_slots() {
        let cache = QueryCacheLegacy::<TestCachedQuery, TestCounter>::new();
        let mut jobs = cache.slot_async_jobs.lock();
        assert!(jobs.contains(NULL_ASYNC_JOB_ID));

        let first = jobs.insert(AsyncJob::default());
        assert_eq!(first.index, 1);
        let _ = jobs.take(first);
        let reused = jobs.insert(AsyncJob::default());
        assert_eq!(reused, first);
    }

    #[test]
    fn async_flush_uses_current_rasterizer_command_state() {
        let observed_commands = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed_committed_front = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let cpu_addr = 0x5510_6000;
        let page = cpu_addr >> 12;
        let mut cache = QueryCacheLegacy::<TestCachedQuery, TestCounter>::new();
        let async_job_id = cache.slot_async_jobs.lock().insert(AsyncJob {
            collected: false,
            value: 0,
            query_location: cpu_addr,
            timestamp: None,
        });
        cache.cached_queries.insert(
            page,
            vec![TestCachedQuery {
                cpu_addr,
                size: 8,
                async_job_id,
                observed_commands: std::sync::Arc::clone(&observed_commands),
                observed_committed_front: std::sync::Arc::clone(&observed_committed_front),
            }],
        );
        cache.committed_flushes.push_back(Some(vec![async_job_id]));
        let mut unused_counter = |_, _| panic!("counter factory must remain unused");

        cache.pop_async_flushes(true, &mut unused_counter);

        assert_eq!(*observed_commands.lock().unwrap(), vec![true]);
        assert_eq!(*observed_committed_front.lock().unwrap(), vec![true]);
        assert!(cache.committed_flushes.is_empty());
    }
}

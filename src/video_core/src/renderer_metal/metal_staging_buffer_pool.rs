// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared-memory staging allocations retired against the Metal GPU timeline.

use std::sync::Arc;

use crate::buffer_cache::buffer_cache_base::BufferCacheAsyncBuffer;

use super::metal_buffer::{MetalBuffer, MetalBufferError};
use super::metal_device::MetalDevice;
use super::metal_scheduler::{MetalScheduler, MetalSchedulerError};

const MAX_ALIGNMENT: usize = 256;
const MAX_STREAM_BUFFER_SIZE: usize = 128 * 1024 * 1024;
const NUM_SYNCS: usize = 16;
const NUM_LEVELS: usize = usize::BITS as usize;
const DELETIONS_PER_TICK: usize = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StagingBufferUsage {
    DeviceLocal,
    #[default]
    Upload,
    Download,
}

pub struct StagingBufferRef {
    pub buffer: Arc<MetalBuffer>,
    pub offset: usize,
    pub size: usize,
    pub usage: StagingBufferUsage,
    pub log2_level: u32,
    pub index: u64,
    deferred: bool,
}

impl BufferCacheAsyncBuffer for StagingBufferRef {
    fn offset(&self) -> u64 {
        self.offset as u64
    }

    fn mapped_span(&self) -> &[u8] {
        assert_ne!(self.usage, StagingBufferUsage::DeviceLocal);
        if self.size == 0 {
            return &[];
        }
        unsafe {
            std::slice::from_raw_parts(self.buffer.contents_ptr().add(self.offset), self.size)
        }
    }

    fn mapped_span_mut(&mut self) -> &mut [u8] {
        assert_ne!(self.usage, StagingBufferUsage::DeviceLocal);
        if self.size == 0 {
            return &mut [];
        }
        unsafe {
            std::slice::from_raw_parts_mut(self.buffer.contents_ptr().add(self.offset), self.size)
        }
    }

    #[cfg(test)]
    fn empty_for_test() -> Self {
        let device = MetalDevice::new().expect("Metal device");
        Self {
            buffer: Arc::new(MetalBuffer::new(&device, 4).expect("Metal buffer")),
            offset: 0,
            size: 0,
            usage: StagingBufferUsage::Upload,
            log2_level: 0,
            index: 0,
            deferred: false,
        }
    }
}

struct CachedBuffer {
    buffer: Arc<MetalBuffer>,
    usage: StagingBufferUsage,
    log2_level: u32,
    tick: u64,
    index: u64,
    deferred: bool,
}

impl CachedBuffer {
    fn reference(&self, size: usize) -> StagingBufferRef {
        StagingBufferRef {
            buffer: Arc::clone(&self.buffer),
            offset: 0,
            size,
            usage: self.usage,
            log2_level: self.log2_level,
            index: self.index,
            deferred: self.deferred,
        }
    }
}

#[derive(Default)]
struct StagingBuffers {
    entries: Vec<CachedBuffer>,
    delete_index: usize,
    iterate_index: usize,
}

type StagingBuffersCache = [StagingBuffers; NUM_LEVELS];

#[derive(Debug, thiserror::Error)]
pub enum MetalStagingBufferError {
    #[error(transparent)]
    Buffer(#[from] MetalBufferError),
    #[error(transparent)]
    Scheduler(#[from] MetalSchedulerError),
    #[error("deferred Metal staging allocation {0} is not owned by this pool")]
    MissingDeferred(u64),
}

/// Metal counterpart of Eden's `StagingBufferPool`.
///
/// The pool receives the scheduler at each operation. This is the Rust
/// ownership equivalent of Eden's stable `Scheduler&` member and avoids
/// retaining a pointer that would dangle when a renderer is moved.
pub struct MetalStagingBufferPool {
    device: MetalDevice,
    stream_buffer: Arc<MetalBuffer>,
    stream_iterator: usize,
    stream_used_iterator: usize,
    stream_free_iterator: usize,
    stream_sync_ticks: [u64; NUM_SYNCS],
    device_local_cache: StagingBuffersCache,
    upload_cache: StagingBuffersCache,
    download_cache: StagingBuffersCache,
    current_delete_level: usize,
    next_index: u64,
}

impl MetalStagingBufferPool {
    pub fn new(device: &MetalDevice) -> Result<Self, MetalStagingBufferError> {
        Ok(Self {
            device: device.clone(),
            stream_buffer: Arc::new(MetalBuffer::new_stream(device, MAX_STREAM_BUFFER_SIZE)?),
            stream_iterator: 0,
            stream_used_iterator: 0,
            stream_free_iterator: 0,
            stream_sync_ticks: [0; NUM_SYNCS],
            device_local_cache: std::array::from_fn(|_| StagingBuffers::default()),
            upload_cache: std::array::from_fn(|_| StagingBuffers::default()),
            download_cache: std::array::from_fn(|_| StagingBuffers::default()),
            current_delete_level: 0,
            next_index: 0,
        })
    }

    pub fn request(
        &mut self,
        scheduler: &mut MetalScheduler,
        size: usize,
        usage: StagingBufferUsage,
        deferred: bool,
    ) -> Result<StagingBufferRef, MetalStagingBufferError> {
        if !deferred
            && usage == StagingBufferUsage::Upload
            && size <= MAX_STREAM_BUFFER_SIZE / NUM_SYNCS
        {
            return self.get_stream_buffer(scheduler, size);
        }
        self.get_staging_buffer(scheduler, size, usage, deferred)
    }

    pub fn request_upload_buffer(
        &mut self,
        scheduler: &mut MetalScheduler,
        size: usize,
        deferred: bool,
    ) -> Result<StagingBufferRef, MetalStagingBufferError> {
        self.request(scheduler, size, StagingBufferUsage::Upload, deferred)
    }

    pub fn request_download_buffer(
        &mut self,
        scheduler: &mut MetalScheduler,
        size: usize,
        deferred: bool,
    ) -> Result<StagingBufferRef, MetalStagingBufferError> {
        self.request(scheduler, size, StagingBufferUsage::Download, deferred)
    }

    pub fn free_deferred(
        &mut self,
        scheduler: &MetalScheduler,
        allocation: &mut StagingBufferRef,
    ) -> Result<(), MetalStagingBufferError> {
        let entries = &mut self.cache_mut(allocation.usage)[allocation.log2_level as usize].entries;
        let entry = entries
            .iter_mut()
            .find(|entry| entry.index == allocation.index)
            .ok_or(MetalStagingBufferError::MissingDeferred(allocation.index))?;
        if !entry.deferred || !allocation.deferred {
            return Err(MetalStagingBufferError::MissingDeferred(allocation.index));
        }
        entry.tick = scheduler.current_tick();
        entry.deferred = false;
        allocation.deferred = false;
        Ok(())
    }

    pub fn tick_frame(
        &mut self,
        scheduler: &mut MetalScheduler,
    ) -> Result<(), MetalStagingBufferError> {
        self.current_delete_level = (self.current_delete_level + 1) % NUM_LEVELS;
        let known_tick = scheduler.known_gpu_tick()?;
        self.release_level(StagingBufferUsage::DeviceLocal, known_tick);
        self.release_level(StagingBufferUsage::Upload, known_tick);
        self.release_level(StagingBufferUsage::Download, known_tick);
        Ok(())
    }

    fn get_stream_buffer(
        &mut self,
        scheduler: &mut MetalScheduler,
        size: usize,
    ) -> Result<StagingBufferRef, MetalStagingBufferError> {
        if self.are_regions_active(
            scheduler,
            self.region(self.stream_free_iterator) + 1,
            self.region(self.stream_iterator.saturating_add(size))
                .saturating_add(1)
                .min(NUM_SYNCS),
        )? {
            return self.get_staging_buffer(scheduler, size, StagingBufferUsage::Upload, false);
        }

        let current_tick = scheduler.current_tick();
        self.fill_sync_regions(
            self.region(self.stream_used_iterator),
            self.region(self.stream_iterator),
            current_tick,
        );
        self.stream_used_iterator = self.stream_iterator;
        self.stream_free_iterator = self
            .stream_free_iterator
            .max(self.stream_iterator.saturating_add(size));

        if self.stream_iterator.saturating_add(size) >= MAX_STREAM_BUFFER_SIZE {
            self.fill_sync_regions(
                self.region(self.stream_used_iterator),
                NUM_SYNCS,
                current_tick,
            );
            self.stream_used_iterator = 0;
            self.stream_iterator = 0;
            self.stream_free_iterator = size;

            if self.are_regions_active(scheduler, 0, self.region(size) + 1)? {
                return self.get_staging_buffer(scheduler, size, StagingBufferUsage::Upload, false);
            }
        }

        let offset = self.stream_iterator;
        self.stream_iterator = align_up(self.stream_iterator.saturating_add(size), MAX_ALIGNMENT);
        Ok(StagingBufferRef {
            buffer: Arc::clone(&self.stream_buffer),
            offset,
            size,
            usage: StagingBufferUsage::Upload,
            log2_level: 0,
            index: 0,
            deferred: false,
        })
    }

    fn are_regions_active(
        &self,
        scheduler: &mut MetalScheduler,
        region_begin: usize,
        region_end: usize,
    ) -> Result<bool, MetalStagingBufferError> {
        if region_begin >= region_end || region_begin >= NUM_SYNCS {
            return Ok(false);
        }
        let gpu_tick = scheduler.known_gpu_tick()?;
        Ok(
            self.stream_sync_ticks[region_begin..region_end.min(NUM_SYNCS)]
                .iter()
                .any(|tick| gpu_tick < *tick),
        )
    }

    fn fill_sync_regions(&mut self, begin: usize, end: usize, tick: u64) {
        if begin < end && begin < NUM_SYNCS {
            self.stream_sync_ticks[begin..end.min(NUM_SYNCS)].fill(tick);
        }
    }

    fn get_staging_buffer(
        &mut self,
        scheduler: &mut MetalScheduler,
        size: usize,
        usage: StagingBufferUsage,
        deferred: bool,
    ) -> Result<StagingBufferRef, MetalStagingBufferError> {
        if let Some(reference) = self.try_get_reserved_buffer(scheduler, size, usage, deferred)? {
            return Ok(reference);
        }
        self.create_staging_buffer(scheduler, size, usage, deferred)
    }

    fn try_get_reserved_buffer(
        &mut self,
        scheduler: &mut MetalScheduler,
        size: usize,
        usage: StagingBufferUsage,
        deferred: bool,
    ) -> Result<Option<StagingBufferRef>, MetalStagingBufferError> {
        let log2_level = log2_ceil(size.max(1));
        let known_tick = scheduler.known_gpu_tick()?;
        let current_tick = scheduler.current_tick();
        let level = &mut self.cache_mut(usage)[log2_level as usize];
        let len = level.entries.len();
        let start = level.iterate_index.min(len);
        let found = (start..len).chain(0..start).find(|index| {
            let entry = &level.entries[*index];
            !entry.deferred && entry.tick <= known_tick
        });
        let Some(index) = found else {
            return Ok(None);
        };
        level.iterate_index = index + 1;
        let entry = &mut level.entries[index];
        entry.tick = if deferred { u64::MAX } else { current_tick };
        entry.deferred = deferred;
        Ok(Some(entry.reference(size)))
    }

    fn create_staging_buffer(
        &mut self,
        scheduler: &MetalScheduler,
        size: usize,
        usage: StagingBufferUsage,
        deferred: bool,
    ) -> Result<StagingBufferRef, MetalStagingBufferError> {
        let log2_level = log2_ceil(size.max(1));
        let capacity = 1usize << log2_level;
        let buffer = match usage {
            StagingBufferUsage::DeviceLocal => MetalBuffer::new_private(&self.device, capacity)?,
            StagingBufferUsage::Upload | StagingBufferUsage::Download => {
                MetalBuffer::new(&self.device, capacity)?
            }
        };
        let entry = CachedBuffer {
            buffer: Arc::new(buffer),
            usage,
            log2_level,
            tick: if deferred {
                u64::MAX
            } else {
                scheduler.current_tick()
            },
            index: self.next_index,
            deferred,
        };
        self.next_index = self.next_index.wrapping_add(1);
        let reference = entry.reference(size);
        self.cache_mut(usage)[log2_level as usize]
            .entries
            .push(entry);
        Ok(reference)
    }

    fn cache_mut(&mut self, usage: StagingBufferUsage) -> &mut StagingBuffersCache {
        match usage {
            StagingBufferUsage::DeviceLocal => &mut self.device_local_cache,
            StagingBufferUsage::Upload => &mut self.upload_cache,
            StagingBufferUsage::Download => &mut self.download_cache,
        }
    }

    fn release_level(&mut self, usage: StagingBufferUsage, known_tick: u64) {
        let current_delete_level = self.current_delete_level;
        let level = &mut self.cache_mut(usage)[current_delete_level];
        let old_size = level.entries.len();
        let begin = level.delete_index.min(old_size);
        let end = (begin + DELETIONS_PER_TICK).min(old_size);
        let mut index = begin;
        let mut remaining = end - begin;
        while remaining != 0 && index < level.entries.len() {
            if level.entries[index].tick <= known_tick {
                level.entries.remove(index);
            } else {
                index += 1;
            }
            remaining -= 1;
        }
        let new_size = level.entries.len();
        level.delete_index = level.delete_index.saturating_add(DELETIONS_PER_TICK);
        if level.delete_index >= new_size {
            level.delete_index = 0;
        }
        if level.iterate_index > new_size {
            level.iterate_index = 0;
        }
    }

    fn region(&self, iterator: usize) -> usize {
        iterator / (MAX_STREAM_BUFFER_SIZE / NUM_SYNCS)
    }
}

fn align_up(value: usize, alignment: usize) -> usize {
    value.saturating_add(alignment - 1) & !(alignment - 1)
}

fn log2_ceil(value: usize) -> u32 {
    usize::BITS - value.saturating_sub(1).leading_zeros()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_uploads_are_suballocated_from_the_stream_buffer() {
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut pool = MetalStagingBufferPool::new(&device).unwrap();
        let first = pool
            .request_upload_buffer(&mut scheduler, 17, false)
            .unwrap();
        let second = pool
            .request_upload_buffer(&mut scheduler, 17, false)
            .unwrap();
        assert_eq!(first.index, 0);
        assert_eq!(second.index, 0);
        assert_eq!(first.offset, 0);
        assert_eq!(second.offset, MAX_ALIGNMENT);
        assert_eq!(first.size, 17);
    }

    #[test]
    fn deferred_allocations_require_explicit_release() {
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut pool = MetalStagingBufferPool::new(&device).unwrap();
        let mut allocation = pool
            .request_download_buffer(&mut scheduler, 1024, true)
            .unwrap();
        assert!(allocation.deferred);
        pool.free_deferred(&scheduler, &mut allocation).unwrap();
        assert!(!allocation.deferred);
    }

    #[test]
    fn size_classes_match_upstream_log2_ceil() {
        assert_eq!(log2_ceil(1), 0);
        assert_eq!(log2_ceil(4), 2);
        assert_eq!(log2_ceil(5), 3);
        assert_eq!(log2_ceil(1024), 10);
    }
}

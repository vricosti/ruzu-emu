// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal buffer allocation and copy operations.

use objc2::rc::Retained;
use std::collections::VecDeque;
use std::ops::Range;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use parking_lot::Mutex;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLBlitCommandEncoder, MTLBuffer, MTLDevice, MTLResourceOptions, MTLTexture,
    MTLTextureDescriptor, MTLTextureType, MTLTextureUsage,
};
use thiserror::Error;

use crate::surface::{bytes_per_block, PixelFormat};

use super::metal_device::MetalDevice;
use super::metal_format::surface_format;
use super::metal_scheduler::{MetalScheduler, MetalSchedulerError};

const WRITE_HISTORY_CAPACITY: usize = 64;

// Recording revisions, not completion ticks. A missing interval must never
// certify unchanged contents. GPU command ordering remains the caller's job.
struct WriteHistory {
    floor: u64,
    latest: u64,
    ranges: VecDeque<(u64, Range<usize>)>,
}

impl WriteHistory {
    fn new(revision: u64) -> Self {
        Self { floor: revision, latest: revision, ranges: VecDeque::with_capacity(WRITE_HISTORY_CAPACITY) }
    }

    fn record(&mut self, revision: u64, range: Option<Range<usize>>) {
        let discontinuous = self.latest.checked_add(1) != Some(revision);
        if revision == u64::MAX || discontinuous || range.is_none() {
            self.latest = self.latest.max(revision);
            // An out-of-order notification can invalidate a snapshot already
            // tagged with latest. Require a later revision before certifying it.
            self.floor = if discontinuous { self.latest.saturating_add(1) } else { self.latest };
            self.ranges.clear();
            return;
        }
        self.latest = revision;
        if self.ranges.len() == WRITE_HISTORY_CAPACITY {
            self.floor = self.ranges.pop_front().unwrap().0;
        }
        self.ranges.push_back((revision, range.unwrap()));
    }

    fn unchanged(&self, since: u64, current: u64, range: Range<usize>) -> bool {
        current != u64::MAX && self.latest == current && since >= self.floor && since <= current
            && !self.ranges.iter().any(|(revision, written)| {
                *revision > since && written.start < range.end && range.start < written.end
                    && !written.is_empty() && !range.is_empty()
            })
    }
}

#[derive(Debug, Error)]
pub enum MetalBufferError {
    #[error("Metal failed to allocate a {0}-byte buffer")]
    AllocationFailed(usize),
    #[error("Metal buffer size {requested} exceeds device limit {maximum}")]
    AllocationTooLarge { requested: usize, maximum: usize },
    #[error("buffer range {offset}..{end} exceeds allocation size {length}")]
    RangeOutOfBounds {
        offset: usize,
        end: usize,
        length: usize,
    },
    #[error(transparent)]
    Scheduler(#[from] MetalSchedulerError),
    #[error("pixel format {0:?} cannot be represented by a native Metal buffer texture")]
    UnsupportedTextureFormat(PixelFormat),
    #[error("Metal texture-buffer offset {offset} is not aligned to {alignment} bytes")]
    TextureOffsetAlignment { offset: usize, alignment: usize },
    #[error("Metal failed to create a texture-buffer view")]
    TextureViewCreationFailed,
}

/// A shared-storage allocation usable by both the guest upload path and Metal.
///
/// Apple Silicon has unified memory, so `MTLStorageModeShared` is the native
/// zero-copy representation for guest buffer cache allocations. Metal's
/// tracked hazards preserve command-buffer ordering without Vulkan-style
/// access masks.
pub struct MetalBuffer {
    buffer: Retained<ProtocolObject<dyn MTLBuffer>>,
    length: usize,
    content_generation: AtomicU64,
    write_history: OnceLock<Mutex<WriteHistory>>,
}

impl MetalBuffer {
    pub fn new(device: &MetalDevice, length: usize) -> Result<Self, MetalBufferError> {
        Self::new_with_options(
            device,
            length,
            MTLResourceOptions::StorageModeShared
                | MTLResourceOptions::CPUCacheModeDefaultCache
                | MTLResourceOptions::HazardTrackingModeTracked,
        )
    }

    pub fn new_stream(device: &MetalDevice, length: usize) -> Result<Self, MetalBufferError> {
        Self::new_with_options(
            device,
            length,
            MTLResourceOptions::StorageModeShared
                | MTLResourceOptions::CPUCacheModeWriteCombined
                | MTLResourceOptions::HazardTrackingModeUntracked,
        )
    }

    pub fn new_private(device: &MetalDevice, length: usize) -> Result<Self, MetalBufferError> {
        Self::new_with_options(
            device,
            length,
            MTLResourceOptions::StorageModePrivate | MTLResourceOptions::HazardTrackingModeTracked,
        )
    }

    fn new_with_options(
        device: &MetalDevice,
        length: usize,
        options: MTLResourceOptions,
    ) -> Result<Self, MetalBufferError> {
        // Metal rejects zero-byte buffers. The common cache's null object still
        // needs a bindable native resource, matching Eden's reserved null
        // buffer behavior.
        let allocation_length = Self::checked_allocation_length(length, device.profile().max_buffer_length)?;
        let buffer = device
            .device()
            .newBufferWithLength_options(allocation_length, options)
            .ok_or(MetalBufferError::AllocationFailed(allocation_length))?;
        Ok(Self {
            buffer,
            length: allocation_length,
            content_generation: AtomicU64::new(0),
            write_history: OnceLock::new(),
        })
    }

    fn checked_allocation_length(length: usize, maximum: usize) -> Result<usize, MetalBufferError> {
        let requested = length.max(4);
        if requested > maximum {
            return Err(MetalBufferError::AllocationTooLarge { requested, maximum });
        }
        Ok(requested)
    }

    pub fn handle(&self) -> &ProtocolObject<dyn MTLBuffer> {
        &self.buffer
    }

    pub fn length(&self) -> usize {
        self.length
    }

    pub fn raw_handle(&self) -> u64 {
        Retained::as_ptr(&self.buffer) as usize as u64
    }

    /// Invalidates derived buffer data at recording time, not GPU completion.
    /// Common-cache GPU write declarations forward here through set_write_tick;
    /// native blit writers that bypass encode_copy must call this explicitly.
    pub(crate) fn mark_content_modified(&self) {
        self.record_write(None);
    }

    pub(crate) fn mark_content_range_modified(&self, offset: u64, size: u64) {
        let range = offset.checked_add(size).filter(|end| *end <= self.length as u64)
            .map(|end| offset as usize..end as usize);
        self.record_write(range);
    }

    fn record_write(&self, range: Option<Range<usize>>) {
        let previous = self.content_generation.fetch_update(Ordering::Relaxed, Ordering::Relaxed,
            |generation| Some(generation.saturating_add(1))).unwrap();
        if let Some(history) = self.write_history.get() {
            history.lock().record(previous.saturating_add(1), range);
        }
    }

    pub fn enable_write_history(&self) {
        self.write_history.get_or_init(|| Mutex::new(WriteHistory::new(self.content_generation())));
    }

    pub fn region_unchanged_since(&self, revision: u64, offset: usize, size: usize) -> bool {
        let Some(end) = offset.checked_add(size).filter(|end| *end <= self.length) else { return false; };
        self.write_history.get().is_some_and(|history| {
            history.lock().unchanged(revision, self.content_generation(), offset..end)
        })
    }

    pub(crate) fn content_generation(&self) -> u64 {
        self.content_generation.load(Ordering::Relaxed)
    }

    pub(crate) fn contents_ptr(&self) -> *mut u8 {
        self.buffer.contents().as_ptr().cast::<u8>()
    }

    pub(crate) fn retained_handle(&self) -> Retained<ProtocolObject<dyn MTLBuffer>> {
        self.buffer.clone()
    }

    /// Materialize a Maxwell texel-buffer descriptor as a native Metal
    /// texture-buffer view. The view retains the parent buffer allocation.
    pub fn new_texture_view(
        &self,
        device: &MetalDevice,
        format: PixelFormat,
        offset: usize,
        size: usize,
        writable: bool,
    ) -> Result<Retained<ProtocolObject<dyn MTLTexture>>, MetalBufferError> {
        self.checked_range(offset, size)?;
        let metal_format = surface_format(format)
            .filter(|format| !format.requires_conversion)
            .ok_or(MetalBufferError::UnsupportedTextureFormat(format))?;
        let alignment = device
            .device()
            .minimumTextureBufferAlignmentForPixelFormat(metal_format.pixel_format)
            .max(1);
        if offset % alignment != 0 {
            return Err(MetalBufferError::TextureOffsetAlignment { offset, alignment });
        }
        let bytes_per_element = bytes_per_block(format).max(1) as usize;
        let element_count = size.div_ceil(bytes_per_element).max(1);
        let descriptor = MTLTextureDescriptor::new();
        descriptor.setTextureType(MTLTextureType::TypeTextureBuffer);
        descriptor.setPixelFormat(metal_format.pixel_format);
        unsafe {
            descriptor.setWidth(element_count);
        }
        descriptor.setUsage(
            MTLTextureUsage::ShaderRead
                | if writable {
                    MTLTextureUsage::ShaderWrite
                } else {
                    MTLTextureUsage::Unknown
                },
        );
        self.buffer
            .newTextureWithDescriptor_offset_bytesPerRow(&descriptor, offset, 0)
            .ok_or(MetalBufferError::TextureViewCreationFailed)
    }

    pub fn write(&self, offset: usize, data: &[u8]) -> Result<(), MetalBufferError> {
        let range = self.checked_range(offset, data.len())?;
        self.mark_content_range_modified(offset as u64, data.len() as u64);
        unsafe {
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                self.buffer
                    .contents()
                    .as_ptr()
                    .cast::<u8>()
                    .add(range.start),
                data.len(),
            );
        }
        Ok(())
    }

    pub fn read(&self, offset: usize, data: &mut [u8]) -> Result<(), MetalBufferError> {
        let range = self.checked_range(offset, data.len())?;
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.buffer
                    .contents()
                    .as_ptr()
                    .cast::<u8>()
                    .add(range.start),
                data.as_mut_ptr(),
                data.len(),
            );
        }
        Ok(())
    }

    pub fn encode_copy(
        &self,
        scheduler: &mut MetalScheduler,
        destination: &MetalBuffer,
        source_offset: usize,
        destination_offset: usize,
        size: usize,
    ) -> Result<(), MetalBufferError> {
        self.encode_copy_impl(scheduler, destination, source_offset, destination_offset, size, false)
    }

    pub(crate) fn encode_upload_copy(
        &self,
        scheduler: &mut MetalScheduler,
        destination: &MetalBuffer,
        source_offset: usize,
        destination_offset: usize,
        size: usize,
    ) -> Result<(), MetalBufferError> {
        self.encode_copy_impl(scheduler, destination, source_offset, destination_offset, size, true)
    }

    fn encode_copy_impl(
        &self,
        scheduler: &mut MetalScheduler,
        destination: &MetalBuffer,
        source_offset: usize,
        destination_offset: usize,
        size: usize,
        upload_prefix: bool,
    ) -> Result<(), MetalBufferError> {
        self.checked_range(source_offset, size)?;
        destination.checked_range(destination_offset, size)?;
        destination.mark_content_range_modified(destination_offset as u64, size as u64);
        let record = |encoder: &ProtocolObject<dyn objc2_metal::MTLBlitCommandEncoder>| unsafe {
            encoder.copyFromBuffer_sourceOffset_toBuffer_destinationOffset_size(
                self.handle(),
                source_offset,
                destination.handle(),
                destination_offset,
                size,
            )
        };
        if upload_prefix {
            scheduler.with_upload_blit_encoder(record)?;
        } else {
            scheduler.with_blit_encoder(record)?;
        }
        Ok(())
    }

    fn checked_range(
        &self,
        offset: usize,
        size: usize,
    ) -> Result<std::ops::Range<usize>, MetalBufferError> {
        let end = offset
            .checked_add(size)
            .ok_or(MetalBufferError::RangeOutOfBounds {
                offset,
                end: usize::MAX,
                length: self.length,
            })?;
        if end > self.length {
            return Err(MetalBufferError::RangeOutOfBounds {
                offset,
                end,
                length: self.length,
            });
        }
        Ok(offset..end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer_metal::metal_scheduler::MetalScheduler;

    #[test]
    fn allocation_length_respects_device_limit_without_truncating() {
        for length in [0, 1, 2, 3, 4] {
            assert_eq!(MetalBuffer::checked_allocation_length(length, 4).unwrap(), 4);
        }
        for length in [5, 255, 256] {
            assert_eq!(MetalBuffer::checked_allocation_length(length, 256).unwrap(), length);
        }
        for (length, maximum, requested) in [(257, 256, 257), (usize::MAX, 256, usize::MAX), (0, 3, 4)] {
            assert!(matches!(MetalBuffer::checked_allocation_length(length, maximum),
                Err(MetalBufferError::AllocationTooLarge { requested: actual, maximum: limit })
                    if actual == requested && limit == maximum));
        }
        assert_eq!(MetalBuffer::checked_allocation_length(usize::MAX, usize::MAX).unwrap(), usize::MAX);
    }

    #[test]
    fn content_generation_invalidates_on_recording_and_never_wraps() {
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let source = MetalBuffer::new(&device, 4).unwrap();
        let destination = MetalBuffer::new(&device, 4).unwrap();
        assert_eq!(source.content_generation(), 0);
        source.write(0, &[1, 2, 3, 4]).unwrap();
        assert_eq!(source.content_generation(), 1);
        source.encode_copy(&mut scheduler, &destination, 0, 0, 4).unwrap();
        assert_eq!(destination.content_generation(), 1);
        assert_eq!(scheduler.completed_tick(), 0);
        destination.content_generation.store(u64::MAX - 1, Ordering::Relaxed);
        destination.mark_content_modified();
        destination.mark_content_modified();
        assert_eq!(destination.content_generation(), u64::MAX);
        scheduler.finish_all().unwrap();
    }

    #[test]
    fn write_history_covers_boundaries_gaps_eviction_and_saturation() {
        let mut history = WriteHistory::new(5);
        history.record(6, Some(4..8));
        assert!(history.unchanged(5, 6, 0..4));
        assert!(history.unchanged(5, 6, 8..12));
        assert!(!history.unchanged(5, 6, 3..5));
        assert!(!history.unchanged(4, 6, 0..4));
        assert!(!history.unchanged(7, 6, 0..4));
        assert!(!history.unchanged(6, 7, 0..4));
        history.record(7, None);
        assert!(!history.unchanged(6, 7, 0..4));
        assert!(history.unchanged(7, 7, 0..4));
        history.record(9, Some(4..8));
        assert!(!history.unchanged(8, 9, 0..4));
        assert!(!history.unchanged(9, 9, 0..4));
        history.record(8, Some(0..4));
        assert!(!history.unchanged(8, 9, 4..8));
        assert!(!history.unchanged(9, 9, 4..8));
        for revision in 10..=74 { history.record(revision, Some(4..8)); }
        assert_eq!(history.ranges.len(), WRITE_HISTORY_CAPACITY);
        assert!(!history.unchanged(9, 74, 0..4));
        assert!(history.unchanged(10, 74, 0..4));
        history.record(u64::MAX, Some(4..8));
        assert!(!history.unchanged(u64::MAX, u64::MAX, 0..4));
    }

    #[test]
    fn tracked_buffer_writes_keep_disjoint_ranges_and_reject_unknown_ranges() {
        let device = MetalDevice::new().unwrap();
        let source = MetalBuffer::new(&device, 16).unwrap();
        source.write(0, &[1; 16]).unwrap();
        assert!(source.write_history.get().is_none());
        source.enable_write_history();
        let revision = source.content_generation();
        source.write(4, &[2; 4]).unwrap();
        assert!(source.region_unchanged_since(revision, 0, 4));
        assert!(!source.region_unchanged_since(revision, 4, 4));
        assert!(!source.region_unchanged_since(revision, usize::MAX, 2));
        assert!(!source.region_unchanged_since(revision, 15, 2));
        source.mark_content_range_modified(u64::MAX, 1);
        assert!(!source.region_unchanged_since(revision, 0, 4));
        let revision = source.content_generation();
        source.mark_content_modified();
        assert!(!source.region_unchanged_since(revision, 0, 4));
    }

    #[test]
    fn copies_shared_buffers_on_the_native_metal_queue() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut scheduler = MetalScheduler::new(&device);
        let source = MetalBuffer::new(&device, 32).expect("source buffer allocation");
        let destination = MetalBuffer::new(&device, 32).expect("destination buffer allocation");
        source.write(4, &[1, 2, 3, 4, 5]).unwrap();

        source
            .encode_copy(&mut scheduler, &destination, 4, 12, 5)
            .unwrap();
        source
            .encode_copy(&mut scheduler, &destination, 5, 20, 4)
            .unwrap();
        // Switching encoder kinds must end the shared blit encoder before
        // opening compute on the same command buffer.
        scheduler.with_compute_encoder(|_| {}).unwrap();
        scheduler.finish_all().unwrap();

        let mut copied = [0; 5];
        destination.read(12, &mut copied).unwrap();
        assert_eq!(copied, [1, 2, 3, 4, 5]);
        destination.read(20, &mut copied[..4]).unwrap();
        assert_eq!(&copied[..4], [2, 3, 4, 5]);
    }

    #[test]
    fn rejects_out_of_bounds_cpu_and_gpu_ranges() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let buffer = MetalBuffer::new(&device, 16).unwrap();
        assert!(matches!(
            buffer.write(15, &[1, 2]),
            Err(MetalBufferError::RangeOutOfBounds { .. })
        ));
    }
}

// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal query ownership.
//!
//! This is the Metal counterpart of Eden's
//! `renderer_vulkan/vk_query_cache.{h,cpp}`. Metal visibility counters are
//! written to one shared result buffer. Each draw uses a distinct aligned
//! slot, so reports can sum all draws since the matching counter reset without
//! touching storage that is still in flight.

use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLBuffer, MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLVisibilityResultMode,
};
use thiserror::Error;

use common::settings;

use crate::memory_manager::MemoryManager;
use crate::query_cache::types::{QueryPropertiesFlags, QueryType};
use crate::renderer_base::GpuTicksGetter;

use super::metal_buffer::{MetalBuffer, MetalBufferError};
use super::metal_device::MetalDevice;
use super::metal_scheduler::{MetalScheduler, MetalSchedulerError};

const QUERY_SLOT_SIZE: usize = std::mem::size_of::<u64>();
// Metal limits visibility-result offsets to 256 KiB minus one 64-bit result.
const QUERY_SLOT_COUNT: usize = (256 * 1024) / QUERY_SLOT_SIZE;

#[derive(Debug, Error)]
pub enum MetalQueryCacheError {
    #[error(transparent)]
    Buffer(#[from] MetalBufferError),
    #[error(transparent)]
    Scheduler(#[from] MetalSchedulerError),
}

pub type MetalQueryOperation = Box<dyn FnOnce() + Send>;

pub enum MetalQueryReport {
    Complete,
    SignalFence(MetalQueryOperation),
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

type MetalVisibilitySnapshot = Vec<(MetalVisibilityBufferHandle, Vec<usize>)>;

pub struct MetalQueryCache {
    device: MetalDevice,
    visibility_buffer: MetalBuffer,
    next_slot: usize,
    zpass_slots: Vec<usize>,
    completed_zpass_banks: Vec<MetalVisibilityBank>,
}

impl MetalQueryCache {
    pub fn new(device: &MetalDevice) -> Result<Self, MetalQueryCacheError> {
        Ok(Self {
            device: device.clone(),
            visibility_buffer: MetalBuffer::new(device, QUERY_SLOT_COUNT * QUERY_SLOT_SIZE)?,
            next_slot: 0,
            zpass_slots: Vec::new(),
            completed_zpass_banks: Vec::new(),
        })
    }

    pub fn reset_counter(&mut self, query_type: u32) {
        if is_zpass_query(query_type) {
            self.zpass_slots.clear();
            self.completed_zpass_banks.clear();
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
        _scheduler: &mut MetalScheduler,
        memory_manager: Option<std::sync::Arc<parking_lot::Mutex<MemoryManager>>>,
        gpu_ticks_getter: Option<GpuTicksGetter>,
        gpu_addr: u64,
        query_type: u32,
        flags: QueryPropertiesFlags,
        payload: u32,
    ) -> Result<MetalQueryReport, MetalQueryCacheError> {
        if query_type == QueryType::Payload as u32 {
            let operation = query_write_operation(
                memory_manager,
                gpu_ticks_getter,
                gpu_addr,
                payload as u64,
                flags.contains(QueryPropertiesFlags::HAS_TIMEOUT),
            );
            let gpu_level_high = settings::is_gpu_level_high(&settings::values());
            return Ok(match payload_report_action(flags, gpu_level_high) {
                PayloadReportAction::Immediate => {
                    operation();
                    MetalQueryReport::Complete
                }
                PayloadReportAction::SignalFence => MetalQueryReport::SignalFence(operation),
                PayloadReportAction::SyncOperation => MetalQueryReport::SyncOperation(operation),
            });
        }

        let operation = if is_zpass_query(query_type) {
            zpass_query_write_operation(
                self.visibility_snapshot(),
                memory_manager,
                gpu_ticks_getter,
                gpu_addr,
                flags.contains(QueryPropertiesFlags::HAS_TIMEOUT),
            )
        } else {
            query_write_operation(
                memory_manager,
                gpu_ticks_getter,
                gpu_addr,
                self.query_value(query_type, payload),
                flags.contains(QueryPropertiesFlags::HAS_TIMEOUT),
            )
        };
        Ok(if flags.contains(QueryPropertiesFlags::IS_A_FENCE) {
            MetalQueryReport::SignalFence(operation)
        } else {
            MetalQueryReport::SyncOperation(operation)
        })
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

    fn visibility_snapshot(&self) -> MetalVisibilitySnapshot {
        let mut snapshot = Vec::with_capacity(self.completed_zpass_banks.len() + 1);
        snapshot.extend(self.completed_zpass_banks.iter().map(|bank| {
            (
                MetalVisibilityBufferHandle {
                    buffer: bank.buffer.retained_handle(),
                },
                bank.offsets.clone(),
            )
        }));
        if !self.zpass_slots.is_empty() {
            snapshot.push((
                MetalVisibilityBufferHandle {
                    buffer: self.visibility_buffer.retained_handle(),
                },
                self.zpass_slots.clone(),
            ));
        }
        snapshot
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
    memory_manager: Option<std::sync::Arc<parking_lot::Mutex<MemoryManager>>>,
    gpu_ticks_getter: Option<GpuTicksGetter>,
    gpu_addr: u64,
    value: u64,
    has_timestamp: bool,
) -> MetalQueryOperation {
    Box::new(move || {
        let Some(memory_manager) = memory_manager else {
            return;
        };
        let memory_manager = memory_manager.lock();
        if has_timestamp {
            let ticks = gpu_ticks_getter.map_or(0, |getter| getter());
            memory_manager.write_block_unsafe(gpu_addr + 8, &ticks.to_le_bytes());
            memory_manager.write_block_unsafe(gpu_addr, &value.to_le_bytes());
        } else {
            memory_manager.write_block_unsafe(gpu_addr, &(value as u32).to_le_bytes());
        }
    })
}

fn zpass_query_write_operation(
    snapshot: MetalVisibilitySnapshot,
    memory_manager: Option<std::sync::Arc<parking_lot::Mutex<MemoryManager>>>,
    gpu_ticks_getter: Option<GpuTicksGetter>,
    gpu_addr: u64,
    has_timestamp: bool,
) -> MetalQueryOperation {
    Box::new(move || {
        let value = visibility_snapshot_value(&snapshot);
        query_write_operation(
            memory_manager,
            gpu_ticks_getter,
            gpu_addr,
            value,
            has_timestamp,
        )();
    })
}

fn visibility_snapshot_value(snapshot: &MetalVisibilitySnapshot) -> u64 {
    snapshot.iter().fold(0u64, |total, (buffer, offsets)| {
        let contents = buffer.buffer.contents().as_ptr().cast::<u8>();
        offsets.iter().fold(total, |bank_total, &offset| unsafe {
            bank_total.wrapping_add(contents.add(offset).cast::<u64>().read())
        })
    })
}

fn is_zpass_query(query_type: u32) -> bool {
    query_type == QueryType::ZPassPixelCount as u32
        || query_type == QueryType::ZPassPixelCount64 as u32
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let snapshot = cache.visibility_snapshot();

        cache.reset_counter(QueryType::ZPassPixelCount64 as u32);

        assert_eq!(visibility_snapshot_value(&snapshot), 18);
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
        assert_eq!(visibility_snapshot_value(&cache.visibility_snapshot()), 18);
    }
}

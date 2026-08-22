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
use objc2_metal::{MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLVisibilityResultMode};
use thiserror::Error;

use crate::memory_manager::MemoryManager;
use crate::query_cache::types::{QueryPropertiesFlags, QueryType};
use crate::renderer_base::GpuTicksGetter;

use super::metal_buffer::{MetalBuffer, MetalBufferError};
use super::metal_device::MetalDevice;
use super::metal_scheduler::{MetalScheduler, MetalSchedulerError};

const QUERY_SLOT_SIZE: usize = std::mem::size_of::<u64>();
const QUERY_SLOT_COUNT: usize = 64 * 1024;

#[derive(Debug, Error)]
pub enum MetalQueryCacheError {
    #[error(transparent)]
    Buffer(#[from] MetalBufferError),
    #[error(transparent)]
    Scheduler(#[from] MetalSchedulerError),
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

pub struct MetalQueryCache {
    visibility_buffer: MetalBuffer,
    next_slot: usize,
    zpass_slots: Vec<usize>,
    zpass_accumulated: u64,
}

impl MetalQueryCache {
    pub fn new(device: &MetalDevice) -> Result<Self, MetalQueryCacheError> {
        Ok(Self {
            visibility_buffer: MetalBuffer::new(device, QUERY_SLOT_COUNT * QUERY_SLOT_SIZE)?,
            next_slot: 0,
            zpass_slots: Vec::new(),
            zpass_accumulated: 0,
        })
    }

    pub fn reset_counter(&mut self, query_type: u32) {
        if is_zpass_query(query_type) {
            self.zpass_slots.clear();
            self.zpass_accumulated = 0;
        }
    }

    pub fn prepare_draw(
        &mut self,
        scheduler: &mut MetalScheduler,
        zpass_enabled: bool,
    ) -> Result<Option<MetalVisibilityQuery>, MetalQueryCacheError> {
        if !zpass_enabled {
            return Ok(None);
        }
        if self.next_slot == QUERY_SLOT_COUNT {
            scheduler.finish_all()?;
            self.zpass_accumulated = self
                .zpass_accumulated
                .wrapping_add(self.current_zpass_value());
            self.next_slot = 0;
            self.zpass_slots.clear();
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
        memory_manager: Option<&parking_lot::Mutex<MemoryManager>>,
        gpu_ticks_getter: Option<&GpuTicksGetter>,
        gpu_addr: u64,
        query_type: u32,
        flags: QueryPropertiesFlags,
        payload: u32,
    ) -> Result<(), MetalQueryCacheError> {
        scheduler.finish_all()?;
        let value = self.query_value(query_type, payload);
        let Some(memory_manager) = memory_manager else {
            return Ok(());
        };
        let memory_manager = memory_manager.lock();
        if flags.contains(QueryPropertiesFlags::HAS_TIMEOUT) {
            let ticks = gpu_ticks_getter.map_or(0, |getter| getter());
            memory_manager.write_block_unsafe(gpu_addr + 8, &ticks.to_le_bytes());
            memory_manager.write_block_unsafe(gpu_addr, &value.to_le_bytes());
        } else {
            memory_manager.write_block_unsafe(gpu_addr, &(value as u32).to_le_bytes());
        }
        Ok(())
    }

    fn query_value(&self, query_type: u32, payload: u32) -> u64 {
        if is_zpass_query(query_type) {
            return self
                .zpass_accumulated
                .wrapping_add(self.current_zpass_value());
        }
        if query_type == QueryType::Payload as u32 {
            return payload as u64;
        }
        if query_type == QueryType::StreamingPrimitivesNeededMinusSucceeded as u32 {
            return 0;
        }
        1
    }

    fn current_zpass_value(&self) -> u64 {
        self.zpass_slots.iter().fold(0u64, |total, &offset| {
            let value = unsafe {
                self.visibility_buffer
                    .contents_ptr()
                    .add(offset)
                    .cast::<u64>()
                    .read()
            };
            total.wrapping_add(value)
        })
    }
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
}

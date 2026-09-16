// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/core/hle/service/nvnflinger/hardware_composer.h
//! Port of zuyu/src/core/hle/service/nvnflinger/hardware_composer.cpp

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::buffer_item::BufferItem;
use super::display::{Display, Layer};
use super::hwc_layer::HwcLayer;
use super::ui::fence::Fence;
use crate::hle::service::nvdrv::devices::nvdisp_disp0::NvDispDisp0;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

type ConsumerId = i32;
type ReleaseFrameNumber = u64;

static HWC_TRACE_COUNT: AtomicU32 = AtomicU32::new(0);
static HWC_EMPTY_ACQUIRE_TRACE_COUNT: AtomicU64 = AtomicU64::new(0);

fn should_trace_hwc() -> bool {
    std::env::var_os("RUZU_TRACE_HWC").is_some()
}

fn should_trace_hwc_dense() -> bool {
    std::env::var_os("RUZU_TRACE_HWC_DENSE").is_some()
}

fn should_emit_hwc_acquire_status(status: super::status::Status) -> bool {
    if status == super::status::Status::NoError {
        return true;
    }
    if std::env::var_os("RUZU_TRACE_HWC_ACQUIRE_SPAM").is_some() {
        return true;
    }

    let count = HWC_EMPTY_ACQUIRE_TRACE_COUNT.fetch_add(1, Ordering::Relaxed);
    count < 64 || count.is_power_of_two()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheStatus {
    NoBufferAvailable,
    BufferAcquired,
    CachedBufferReused,
}

#[derive(Default)]
struct Framebuffer {
    item: BufferItem,
    release_frame_number: ReleaseFrameNumber,
    last_acquire_frame: u64,
    is_acquired: bool,
}

fn normalize_swap_interval(mut out_speed_scale: Option<&mut f32>, mut swap_interval: i32) -> i32 {
    if swap_interval <= 0 {
        if let Some(out_speed_scale) = out_speed_scale.as_deref_mut() {
            *out_speed_scale = 2.0 * (1 - swap_interval) as f32;
        }
        swap_interval = 1;
    }

    if swap_interval >= 5 {
        if let Some(out_speed_scale) = out_speed_scale.as_deref_mut() {
            *out_speed_scale = swap_interval as f32 / 100.0;
        }
        swap_interval = 1;
    }

    swap_interval
}

pub struct HardwareComposer {
    frame_number: u64,
    framebuffers: BTreeMap<ConsumerId, Framebuffer>,
}

impl HardwareComposer {
    pub fn new() -> Self {
        Self {
            frame_number: 0,
            framebuffers: BTreeMap::new(),
        }
    }

    pub fn compose_locked(
        &mut self,
        out_speed_scale: &mut f32,
        display: &Display,
        nvdisp: &NvDispDisp0,
    ) -> u32 {
        let mut composition_stack = Vec::with_capacity(display.stack.layers.len());
        *out_speed_scale = 1.0;

        nvdisp.wait_for_composite();
        self.release_framebuffers_locked(display);

        let mut swap_interval: Option<i32> = None;
        let mut has_acquired_buffer = false;

        for layer in &display.stack.layers {
            let (consumer_id, is_overlay) = {
                let layer = layer.lock().unwrap();
                (layer.consumer_id, layer.is_overlay)
            };
            let should_try_acquire = if is_overlay {
                true
            } else {
                self.framebuffers
                    .get(&consumer_id)
                    .is_none_or(|framebuffer| {
                        !framebuffer.is_acquired
                            || self
                                .frame_number
                                .wrapping_sub(framebuffer.last_acquire_frame)
                                >= normalize_swap_interval(None, framebuffer.item.swap_interval)
                                    as u64
                    })
            };
            let result = if should_try_acquire {
                self.cache_framebuffer_locked(layer, consumer_id)
            } else if self
                .framebuffers
                .get(&consumer_id)
                .is_some_and(|framebuffer| framebuffer.is_acquired)
            {
                CacheStatus::CachedBufferReused
            } else {
                CacheStatus::NoBufferAvailable
            };

            if should_trace_hwc() {
                let count = HWC_TRACE_COUNT.fetch_add(1, Ordering::Relaxed);
                if count < 64 {
                    log::info!(
                        "HWC::compose layer={} cache_result={:?} frame_number={}",
                        consumer_id,
                        result,
                        self.frame_number
                    );
                }
            }

            if result == CacheStatus::NoBufferAvailable {
                continue;
            }

            if result == CacheStatus::BufferAcquired {
                has_acquired_buffer = true;
            }

            let Some(framebuffer) = self.framebuffers.get(&consumer_id) else {
                continue;
            };
            let item = &framebuffer.item;
            let Some(graphic_buffer) = item.graphic_buffer.as_ref() else {
                continue;
            };

            let layer_guard = layer.lock().unwrap();
            if layer_guard.visible {
                if common::trace::is_enabled(common::trace::cat::HWC) {
                    common::trace::emit_raw(
                        common::trace::cat::HWC,
                        &[
                            2,
                            self.frame_number,
                            consumer_id as u64,
                            graphic_buffer.get_buffer_id() as u64,
                            graphic_buffer.get_offset() as u64,
                            graphic_buffer.get_width() as u64,
                            graphic_buffer.get_height() as u64,
                            graphic_buffer.get_stride() as u64,
                            graphic_buffer.get_external_format() as u64,
                            item.transform.bits() as u64,
                            item.crop.left as u64,
                            item.crop.top as u64,
                            item.crop.right as u64,
                            item.crop.bottom as u64,
                        ],
                    );
                }
                composition_stack.push(HwcLayer {
                    buffer_handle: graphic_buffer.get_buffer_id(),
                    offset: graphic_buffer.get_offset(),
                    format: graphic_buffer.get_external_format(),
                    width: graphic_buffer.get_width(),
                    height: graphic_buffer.get_height(),
                    stride: graphic_buffer.get_stride(),
                    z_index: layer_guard.z_index,
                    blending: layer_guard.blending,
                    transform:
                        super::buffer_transform_flags::BufferTransformFlags::from_bits_retain(
                            item.transform.bits(),
                        ),
                    crop_rect: item.crop,
                    acquire_fence: item.fence,
                    layer_stack_mask: layer_guard.layer_stack_mask,
                });
                if should_trace_hwc_dense() {
                    log::info!(
                        "[HWC_LAYER] consumer={} frame={} buffer={} offset=0x{:X} size={}x{} stride={} fmt={:?} transform=0x{:X} crop=({}, {}, {}, {})",
                        consumer_id,
                        self.frame_number,
                        graphic_buffer.get_buffer_id(),
                        graphic_buffer.get_offset(),
                        graphic_buffer.get_width(),
                        graphic_buffer.get_height(),
                        graphic_buffer.get_stride(),
                        graphic_buffer.get_external_format(),
                        item.transform.bits(),
                        item.crop.left,
                        item.crop.top,
                        item.crop.right,
                        item.crop.bottom,
                    );
                }
            }

            if layer_guard.is_overlay {
                continue;
            }

            let item_swap_interval =
                normalize_swap_interval(Some(out_speed_scale), item.swap_interval);
            swap_interval = Some(match swap_interval {
                Some(current) => current.min(item_swap_interval),
                None => item_swap_interval,
            });
        }

        if has_acquired_buffer && !composition_stack.is_empty() {
            composition_stack.sort_by_key(|layer| layer.z_index);
            super::diagnostics::record_hwc(
                "compose_submit",
                [
                    self.frame_number,
                    composition_stack.len() as u64,
                    display.id,
                    swap_interval.unwrap_or(1) as u64,
                    0,
                    0,
                ],
            );
            if should_trace_hwc_dense() {
                log::info!(
                    "[HWC_DENSE] composite_begin frame_number={} layers={} swap_interval={}",
                    self.frame_number,
                    composition_stack.len(),
                    swap_interval.unwrap_or(1)
                );
            }
            nvdisp.composite(&composition_stack);
            if should_trace_hwc_dense() {
                log::info!(
                    "[HWC_DENSE] composite_end frame_number={} layers={}",
                    self.frame_number,
                    composition_stack.len()
                );
            }
        }

        self.frame_number += 1;
        1
    }

    fn release_framebuffers_locked(&mut self, display: &Display) {
        for (layer_id, framebuffer) in &mut self.framebuffers {
            if should_trace_hwc_dense() {
                log::info!(
                    "[HWC_DENSE] release_check consumer={} frame_number={} release_frame_number={} is_acquired={}",
                    layer_id,
                    self.frame_number,
                    framebuffer.release_frame_number,
                    framebuffer.is_acquired
                );
            }
            if !framebuffer.is_acquired {
                continue;
            }

            let Some(layer) = display.stack.find_layer(*layer_id) else {
                if should_trace_hwc_dense() {
                    log::info!("[HWC_DENSE] release_skip_no_layer consumer={}", layer_id);
                }
                continue;
            };

            let layer = layer.lock().unwrap();
            if !layer.is_overlay && framebuffer.release_frame_number > self.frame_number {
                continue;
            }
            let status = layer
                .buffer_item_consumer
                .release_buffer(&framebuffer.item, &Fence::no_fence());
            if should_trace_hwc_dense() {
                log::info!(
                    "[HWC_DENSE] release_done consumer={} slot={} frame={} status={:?}",
                    layer_id,
                    framebuffer.item.slot,
                    framebuffer.item.frame_number,
                    status
                );
            }
            framebuffer.is_acquired = false;
        }
    }

    pub fn remove_layer_locked(&mut self, display: &Display, consumer_id: ConsumerId) {
        let Some(framebuffer) = self.framebuffers.remove(&consumer_id) else {
            return;
        };

        if !framebuffer.is_acquired {
            return;
        }

        if let Some(layer) = display.stack.find_layer(consumer_id) {
            layer
                .lock()
                .unwrap()
                .buffer_item_consumer
                .release_buffer(&framebuffer.item, &Fence::no_fence());
        }
    }

    fn try_acquire_framebuffer_locked(
        layer: &Arc<Mutex<Layer>>,
        framebuffer: &mut Framebuffer,
        frame_number: u64,
    ) -> bool {
        let layer = layer.lock().unwrap();
        let consumer_id = layer.consumer_id;
        let status = layer
            .buffer_item_consumer
            .acquire_buffer(&mut framebuffer.item, 0, false);
        if should_trace_hwc() {
            let count = HWC_TRACE_COUNT.fetch_add(1, Ordering::Relaxed);
            if count < 64 {
                log::info!(
                    "HWC::try_acquire layer={} status={:?} slot={} frame={}",
                    consumer_id,
                    status,
                    framebuffer.item.slot,
                    framebuffer.item.frame_number
                );
            }
        }
        if status != super::status::Status::NoError {
            super::diagnostics::record_hwc(
                "acquire_fail",
                [
                    consumer_id as i64 as u64,
                    status as i32 as u64,
                    framebuffer.item.slot as i64 as u64,
                    framebuffer.item.frame_number,
                    framebuffer.release_frame_number,
                    u64::from(framebuffer.is_acquired),
                ],
            );
            if common::trace::is_enabled(common::trace::cat::HWC)
                && should_emit_hwc_acquire_status(status)
            {
                common::trace::emit_raw(
                    common::trace::cat::HWC,
                    &[
                        1,
                        0,
                        consumer_id as u64,
                        status as i32 as u64,
                        framebuffer.item.slot as u64,
                        framebuffer.item.frame_number,
                        framebuffer.item.swap_interval as u64,
                        framebuffer.release_frame_number,
                        u64::from(framebuffer.is_acquired),
                    ],
                );
            }
            return false;
        }

        let swap_interval = if layer.is_overlay {
            1
        } else {
            normalize_swap_interval(None, framebuffer.item.swap_interval)
        };
        framebuffer.release_frame_number = frame_number + swap_interval as u64;
        framebuffer.last_acquire_frame = frame_number;
        framebuffer.is_acquired = true;
        super::diagnostics::record_hwc(
            "acquire_ok",
            [
                consumer_id as i64 as u64,
                status as i32 as u64,
                framebuffer.item.slot as i64 as u64,
                framebuffer.item.frame_number,
                framebuffer.release_frame_number,
                framebuffer.item.swap_interval as u64,
            ],
        );
        if common::trace::is_enabled(common::trace::cat::HWC) {
            common::trace::emit_raw(
                common::trace::cat::HWC,
                &[
                    1,
                    0,
                    consumer_id as u64,
                    status as i32 as u64,
                    framebuffer.item.slot as u64,
                    framebuffer.item.frame_number,
                    framebuffer.item.swap_interval as u64,
                    framebuffer.release_frame_number,
                    u64::from(framebuffer.is_acquired),
                ],
            );
        }
        true
    }

    fn cache_framebuffer_locked(
        &mut self,
        layer: &Arc<Mutex<Layer>>,
        consumer_id: ConsumerId,
    ) -> CacheStatus {
        let frame_number = self.frame_number;
        let result = if let Some(framebuffer) = self.framebuffers.get_mut(&consumer_id) {
            if framebuffer.is_acquired {
                CacheStatus::CachedBufferReused
            } else if Self::try_acquire_framebuffer_locked(layer, framebuffer, frame_number) {
                CacheStatus::BufferAcquired
            } else {
                CacheStatus::CachedBufferReused
            }
        } else {
            let mut framebuffer = Framebuffer::default();
            if Self::try_acquire_framebuffer_locked(layer, &mut framebuffer, frame_number) {
                self.framebuffers.insert(consumer_id, framebuffer);
                CacheStatus::BufferAcquired
            } else {
                CacheStatus::NoBufferAvailable
            }
        };
        record_hwc_cache_status(consumer_id, result);
        result
    }
}

// =============================================================================
// RUZU_PROFILE_HWC_CACHE: per-consumer histogram of cache_framebuffer_locked
// CacheStatus returns. Pinpoints whether the compositor is actually consuming
// new buffers (BufferAcquired) or stuck reusing cached entries
// (CachedBufferReused) -- the latter means the producer's DequeueBuffer can't
// proceed because buffers aren't being released back to it.
// =============================================================================

#[derive(Default, Clone, Copy)]
struct HwcCacheCounters {
    no_buffer_available: u64,
    buffer_acquired: u64,
    cached_buffer_reused: u64,
}

static HWC_CACHE_PROFILE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<ConsumerId, HwcCacheCounters>>,
> = std::sync::OnceLock::new();

fn hwc_cache_profile_enabled() -> bool {
    std::env::var_os("RUZU_PROFILE_HWC_CACHE").is_some()
}

fn record_hwc_cache_status(consumer_id: ConsumerId, status: CacheStatus) {
    if !hwc_cache_profile_enabled() {
        return;
    }
    let map =
        HWC_CACHE_PROFILE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut g = map.lock().unwrap();
    let entry = g.entry(consumer_id).or_default();
    match status {
        CacheStatus::NoBufferAvailable => entry.no_buffer_available += 1,
        CacheStatus::BufferAcquired => entry.buffer_acquired += 1,
        CacheStatus::CachedBufferReused => entry.cached_buffer_reused += 1,
    }
}

pub fn dump_hwc_cache_profile() {
    let Some(map) = HWC_CACHE_PROFILE.get() else {
        return;
    };
    let entries: Vec<(ConsumerId, HwcCacheCounters)> = {
        let g = map.lock().unwrap();
        g.iter().map(|(k, v)| (*k, *v)).collect()
    };
    if entries.is_empty() {
        return;
    }
    eprintln!("[HWC_CACHE_PROFILE] per-consumer CacheStatus distribution:");
    for (cid, c) in entries.iter() {
        let total = c.no_buffer_available + c.buffer_acquired + c.cached_buffer_reused;
        if total == 0 {
            continue;
        }
        let pct_acquired = 100.0 * c.buffer_acquired as f64 / total as f64;
        let pct_reused = 100.0 * c.cached_buffer_reused as f64 / total as f64;
        let pct_none = 100.0 * c.no_buffer_available as f64 / total as f64;
        eprintln!(
            "[HWC_CACHE_PROFILE]   consumer={:<3} total={:<7} BufferAcquired={:<7} ({:.1}%)  CachedBufferReused={:<7} ({:.1}%)  NoBufferAvailable={:<7} ({:.1}%)",
            cid,
            total,
            c.buffer_acquired,
            pct_acquired,
            c.cached_buffer_reused,
            pct_reused,
            c.no_buffer_available,
            pct_none,
        );
    }
}

impl Default for HardwareComposer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_swap_interval;

    #[test]
    fn normalize_swap_interval_turns_nonpositive_into_speed_scale() {
        let mut speed_scale = 1.0;
        let interval = normalize_swap_interval(Some(&mut speed_scale), 0);

        assert_eq!(interval, 1);
        assert_eq!(speed_scale, 2.0);
    }

    #[test]
    fn normalize_swap_interval_turns_large_interval_into_precise_speed_scale() {
        let mut speed_scale = 1.0;
        let interval = normalize_swap_interval(Some(&mut speed_scale), 50);

        assert_eq!(interval, 1);
        assert_eq!(speed_scale, 0.5);
    }
}

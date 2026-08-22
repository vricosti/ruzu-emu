// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-FileCopyrightText: Copyright 2014 The Android Open Source Project
// SPDX-License-Identifier: GPL-3.0-or-later
// Parts of this implementation were based on:
// https://cs.android.com/android/platform/superproject/+/android-5.1.1_r38:frameworks/native/include/gui/BufferQueueProducer.h

//! Port of zuyu/src/core/hle/service/nvnflinger/buffer_queue_producer.h
//!
//! The BufferQueueProducer is the producer-side interface for buffer queues.
//! It implements IBinder for binder transactions from the application.
//!
//! Full method implementations (DequeueBuffer, QueueBuffer, etc.) depend on
//! the complete BufferQueueCore + NvMap + kernel event infrastructure.
//! The struct and method signatures are fully ported; method bodies that
//! require complex interactions with other subsystems use todo!().

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use common::math_util::Rectangle;

use crate::hle::kernel::k_event::KEvent;
use crate::hle::kernel::k_readable_event::KReadableEvent;
use crate::hle::kernel::k_scheduler::KScheduler;
use crate::hle::service::kernel_helpers::ServiceContext;
use crate::hle::service::nvdrv::core::nvmap::NvMap;
use crate::hle::service::os::event::Event;

use super::binder::IBinder;
use super::buffer_queue_core::BufferQueueCore;
use super::buffer_slot::BufferSlot;
use super::graphic_buffer_producer::{QueueBufferInput, QueueBufferOutput};
use super::parcel::{InputParcel, OutputParcel};
use super::pixel_format::PixelFormat;
use super::producer_listener::IProducerListener;
use super::status::{Status, StatusCode};
use super::ui::fence::Fence;
use super::ui::graphic_buffer::{GraphicBuffer, NvGraphicBuffer};
use super::window::{
    NativeWindow, NativeWindowApi, NativeWindowScalingMode, NativeWindowTransform,
};
use crate::hle::kernel::k_process::ProcessLock;

static BQP_TRACE_COUNT: AtomicU32 = AtomicU32::new(0);
static BQP_RING_SEQ: AtomicU64 = AtomicU64::new(0);

fn should_trace_bqp() -> bool {
    std::env::var_os("RUZU_TRACE_BQP").is_some()
}

fn trace_bqp(args: std::fmt::Arguments<'_>) {
    if !should_trace_bqp() {
        return;
    }
    let count = BQP_TRACE_COUNT.fetch_add(1, Ordering::Relaxed);
    if count < 96 {
        log::info!("{}", args);
    }
}

fn trace_bqp_ring(args: &[u64]) {
    if common::trace::is_enabled(common::trace::cat::BQP) {
        common::trace::emit_raw(common::trace::cat::BQP, args);
    }
}

fn current_bqp_tid() -> u64 {
    crate::hle::kernel::kernel::get_current_thread_id_fast().unwrap_or(0)
}

fn next_bqp_seq() -> u64 {
    BQP_RING_SEQ.fetch_add(1, Ordering::Relaxed)
}

fn trace_dequeue_return_for_present(seq: u64, status: Status, slot: i32, flags: i32) {
    if std::env::var_os("RUZU_TRACE_PRESENT").is_none() {
        return;
    }
    log::info!(
        "[BQP_DEQUEUE_RET] seq={} status={:?} slot={} flags=0x{:X}",
        seq,
        status,
        slot,
        flags
    );
}

fn stop_unimplemented_transact(code: u32, name: &str) -> ! {
    panic!(
        "BufferQueueProducer::transact unimplemented transaction {} ({})",
        code, name
    );
}

fn stop_unimplemented_connect_listener() -> ! {
    panic!("BufferQueueProducer::transact Connect listener is unimplemented");
}

pub struct BufferQueueProducer {
    service_context: Arc<Mutex<ServiceContext>>,
    core: Arc<BufferQueueCore>,
    buffer_wait_event_handle: u32,
    sticky_transform: Mutex<u32>,
    next_callback_ticket: Mutex<i32>,
    current_callback_ticket: Mutex<i32>,
    callback_condition: Condvar,
    nvmap: Arc<NvMap>,
}

impl BufferQueueProducer {
    pub fn new(
        service_context: Arc<Mutex<ServiceContext>>,
        core: Arc<BufferQueueCore>,
        nvmap: Arc<NvMap>,
    ) -> Self {
        let buffer_wait_event_handle = service_context
            .lock()
            .unwrap()
            .create_event("BufferQueue:WaitEvent".to_string());
        Self {
            service_context,
            core,
            buffer_wait_event_handle,
            sticky_transform: Mutex::new(0),
            next_callback_ticket: Mutex::new(0),
            current_callback_ticket: Mutex::new(0),
            callback_condition: Condvar::new(),
            nvmap,
        }
    }

    fn buffer_wait_event(&self) -> Arc<Event> {
        self.service_context
            .lock()
            .unwrap()
            .get_event(self.buffer_wait_event_handle)
            .expect("BufferQueueProducer must keep its wait event alive")
    }

    fn signal_buffer_wait_event(&self) {
        let event = self.buffer_wait_event();
        let readable_object_id = event.kernel_object_id().unwrap_or(0);
        trace_bqp_ring(&[
            20,
            next_bqp_seq(),
            readable_object_id,
            u64::from(event.is_signaled()),
            current_bqp_tid(),
        ]);
        event.signal();
    }

    fn wait_for_free_slot_then_relock<'a>(
        &'a self,
        async_flag: bool,
        found: &mut i32,
        return_flags: &mut StatusCode,
        mut inner: std::sync::MutexGuard<'a, super::buffer_queue_core::BufferQueueCoreInner>,
    ) -> (
        Status,
        std::sync::MutexGuard<'a, super::buffer_queue_core::BufferQueueCoreInner>,
    ) {
        let mut try_again = true;

        while try_again {
            if inner.is_abandoned {
                log::error!("BufferQueueProducer: BufferQueue has been abandoned");
                return (Status::NoInit, inner);
            }

            let max_buffer_count = inner.get_max_buffer_count_locked(async_flag);
            if async_flag
                && inner.override_max_buffer_count != 0
                && inner.override_max_buffer_count < max_buffer_count
            {
                *found = BufferQueueCore::INVALID_BUFFER_SLOT;
                return (Status::BadValue, inner);
            }

            for slot in max_buffer_count as usize..super::buffer_queue_defs::NUM_BUFFER_SLOTS {
                debug_assert!(
                    inner.slots[slot].buffer_state == super::buffer_slot::BufferState::Free
                );
                if inner.slots[slot].graphic_buffer.is_some()
                    && inner.slots[slot].buffer_state == super::buffer_slot::BufferState::Free
                    && !inner.slots[slot].is_preallocated
                {
                    inner.free_buffer_locked(slot as i32);
                    *return_flags |= StatusCode::RELEASE_ALL_BUFFERS;
                }
            }

            *found = BufferQueueCore::INVALID_BUFFER_SLOT;
            let mut dequeued_count = 0;
            let mut acquired_count = 0;
            for slot in 0..max_buffer_count as usize {
                match inner.slots[slot].buffer_state {
                    super::buffer_slot::BufferState::Dequeued => {
                        dequeued_count += 1;
                    }
                    super::buffer_slot::BufferState::Acquired => {
                        acquired_count += 1;
                    }
                    super::buffer_slot::BufferState::Free => {
                        if *found == BufferQueueCore::INVALID_BUFFER_SLOT
                            || inner.slots[slot].frame_number
                                < inner.slots[*found as usize].frame_number
                        {
                            *found = slot as i32;
                        }
                    }
                    _ => {}
                }
            }

            if inner.override_max_buffer_count == 0 && dequeued_count != 0 {
                log::error!(
                    "BufferQueueProducer: can't dequeue multiple buffers without setting the buffer count"
                );
                return (Status::InvalidOperation, inner);
            }

            if inner.buffer_has_been_queued {
                let new_undequeued_count = max_buffer_count - (dequeued_count + 1);
                let min_undequeued_count = inner.get_min_undequeued_buffer_count_locked(async_flag);
                if new_undequeued_count < min_undequeued_count {
                    log::error!(
                        "BufferQueueProducer: min undequeued buffer count({}) exceeded (dequeued={} undequeued={})",
                        min_undequeued_count,
                        dequeued_count,
                        new_undequeued_count
                    );
                    return (Status::InvalidOperation, inner);
                }
            }

            let too_many_buffers = inner.queue.len() > max_buffer_count as usize;
            if too_many_buffers {
                log::error!(
                    "BufferQueueProducer: queue size is {}, waiting",
                    inner.queue.len()
                );
            }
            try_again = (*found == BufferQueueCore::INVALID_BUFFER_SLOT) || too_many_buffers;
            if try_again && std::env::var_os("RUZU_TRACE_BQP_DEQUEUE_BLOCK").is_some() {
                log::info!(
                    "[BQP_DEQUEUE_BLOCK] found={} too_many={} max_count={} dequeued={} acquired={} queue_len={} cannot_block={} override={}",
                    *found, too_many_buffers, max_buffer_count, dequeued_count, acquired_count,
                    inner.queue.len(), inner.dequeue_buffer_cannot_block,
                    inner.override_max_buffer_count,
                );
            }
            if try_again {
                if inner.dequeue_buffer_cannot_block
                    && acquired_count <= inner.max_acquired_buffer_count
                {
                    return (Status::WouldBlock, inner);
                }
                inner = self.core.wait_for_dequeue_condition(inner);
            }
        }

        (Status::NoError, inner)
    }

    pub fn request_buffer(&self, slot: i32) -> (Status, Option<Arc<GraphicBuffer>>) {
        record_bqp_event(BqpEvent::RequestBuffer);
        super::diagnostics::record_bqp("request_buffer", [slot as i64 as u64, 0, 0, 0, 0, 0]);
        trace_bqp_ring(&[7, next_bqp_seq(), slot as i64 as u64, current_bqp_tid()]);
        trace_bqp(format_args!("BQP::request_buffer slot={}", slot));
        let mut inner = self.core.mutex.lock().unwrap();
        if inner.is_abandoned {
            log::error!("BufferQueueProducer: BufferQueue has been abandoned");
            return (Status::NoInit, None);
        }
        if slot < 0 || slot as usize >= super::buffer_queue_defs::NUM_BUFFER_SLOTS {
            log::error!("BufferQueueProducer: slot {} out of range", slot);
            return (Status::BadValue, None);
        } else if inner.slots[slot as usize].buffer_state
            != super::buffer_slot::BufferState::Dequeued
        {
            log::error!(
                "BufferQueueProducer: slot {} is not owned by producer (state={:?})",
                slot,
                inner.slots[slot as usize].buffer_state
            );
            return (Status::BadValue, None);
        }
        inner.slots[slot as usize].request_buffer_called = true;
        let buf = inner.slots[slot as usize].graphic_buffer.clone();
        if let Some(graphic_buffer) = buf.as_ref() {
            trace_bqp(format_args!(
                "BQP::request_buffer -> magic=0x{:08X} w={} h={} stride={} fmt={:?} usage=0x{:X} buffer_id={} ext_fmt={:?} handle={} offset={}",
                graphic_buffer.buffer.magic,
                graphic_buffer.get_width(),
                graphic_buffer.get_height(),
                graphic_buffer.get_stride(),
                graphic_buffer.get_format(),
                graphic_buffer.get_usage(),
                graphic_buffer.get_buffer_id(),
                graphic_buffer.get_external_format(),
                graphic_buffer.get_handle(),
                graphic_buffer.get_offset()
            ));
        } else {
            trace_bqp(format_args!("BQP::request_buffer -> None"));
        }
        (Status::NoError, buf)
    }

    pub fn set_buffer_count(&self, buffer_count: i32) -> Status {
        record_bqp_event(BqpEvent::SetBufferCount);
        super::diagnostics::record_bqp(
            "set_buffer_count",
            [buffer_count as i64 as u64, 0, 0, 0, 0, 0],
        );
        trace_bqp(format_args!("BQP::set_buffer_count count={}", buffer_count));
        log::info!("[BQP_SET_COUNT] buffer_count={}", buffer_count);
        let mut inner = self.core.mutex.lock().unwrap();
        inner = self.core.wait_while_allocating_locked(inner);

        if inner.is_abandoned {
            log::error!("BufferQueueProducer: BufferQueue has been abandoned");
            return Status::NoInit;
        }

        if buffer_count > super::buffer_queue_defs::NUM_BUFFER_SLOTS as i32 {
            log::error!(
                "BufferQueueProducer: buffer_count {} too large",
                buffer_count
            );
            return Status::BadValue;
        }

        for slot in 0..super::buffer_queue_defs::NUM_BUFFER_SLOTS {
            if inner.slots[slot].buffer_state == super::buffer_slot::BufferState::Dequeued {
                log::error!("BufferQueueProducer: buffer owned by producer");
                return Status::BadValue;
            }
        }

        if buffer_count == 0 {
            inner.override_max_buffer_count = 0;
            self.core.signal_dequeue_condition();
            drop(inner);
            return Status::NoError;
        }

        let min_buffer_slots = inner.get_min_max_buffer_count_locked(false);
        if buffer_count < min_buffer_slots {
            log::error!(
                "BufferQueueProducer: requested buffer count {} is less than minimum {}",
                buffer_count,
                min_buffer_slots
            );
            return Status::BadValue;
        }

        if inner.get_preallocated_buffer_count_locked() <= 0 {
            inner.free_all_buffers_locked();
        }

        inner.override_max_buffer_count = buffer_count;
        let listener = inner.consumer_listener.clone();
        self.core.signal_dequeue_condition();
        self.signal_buffer_wait_event();
        drop(inner);
        if let Some(listener) = listener {
            listener.on_buffers_released();
        }

        Status::NoError
    }

    pub fn dequeue_buffer(
        &self,
        async_flag: bool,
        mut width: u32,
        mut height: u32,
        mut format: PixelFormat,
        mut usage: u32,
    ) -> (StatusCode, i32, Fence) {
        record_bqp_event(BqpEvent::DequeueBuffer);
        let bqp_seq = next_bqp_seq();
        super::diagnostics::record_bqp(
            "dequeue_enter",
            [
                bqp_seq,
                u64::from(async_flag),
                width as u64,
                height as u64,
                format as u64,
                usage as u64,
            ],
        );
        trace_bqp_ring(&[
            1,
            bqp_seq,
            u64::from(async_flag),
            width as u64,
            height as u64,
            format as u64,
            usage as u64,
            current_bqp_tid(),
        ]);
        trace_bqp(format_args!(
            "BQP::dequeue_buffer async={} w={} h={} format={:?} usage=0x{:X}",
            async_flag, width, height, format, usage
        ));
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNT: AtomicU64 = AtomicU64::new(0);
            let n = COUNT.fetch_add(1, Ordering::Relaxed);
            if n < 16 || n.is_power_of_two() {
                log::info!(
                    "[BQP_DEQUEUE] #{} dequeue_buffer entry async={}",
                    n,
                    async_flag
                );
            }
        }
        if (width != 0 && height == 0) || (width == 0 && height != 0) {
            log::error!(
                "BufferQueueProducer: invalid size: w={} h={}",
                width,
                height
            );
            trace_dequeue_return_for_present(bqp_seq, Status::BadValue, -1, 0);
            return (Status::BadValue.into(), -1, Fence::default());
        }

        let mut return_flags = StatusCode::NO_ERROR;
        let attached_by_consumer;
        let out_slot;
        let out_fence;
        {
            let mut inner = self.core.mutex.lock().unwrap();
            inner = self.core.wait_while_allocating_locked(inner);

            if format == PixelFormat::NoFormat {
                format = inner.default_buffer_format;
            }
            usage |= inner.consumer_usage_bit;

            let mut found = 0;
            let (status, mut inner) = self.wait_for_free_slot_then_relock(
                async_flag,
                &mut found,
                &mut return_flags,
                inner,
            );
            if status != Status::NoError {
                trace_bqp_ring(&[
                    2,
                    bqp_seq,
                    status as i32 as u64,
                    -1i64 as u64,
                    0,
                    current_bqp_tid(),
                ]);
                trace_dequeue_return_for_present(bqp_seq, status, -1, return_flags.raw());
                return (status.into(), -1, Fence::default());
            }

            if found == BufferQueueCore::INVALID_BUFFER_SLOT {
                log::error!("BufferQueueProducer: no available buffer slots");
                trace_bqp_ring(&[
                    2,
                    bqp_seq,
                    Status::Busy as i32 as u64,
                    -1i64 as u64,
                    0,
                    current_bqp_tid(),
                ]);
                trace_dequeue_return_for_present(bqp_seq, Status::Busy, -1, return_flags.raw());
                return (Status::Busy.into(), -1, Fence::default());
            }

            out_slot = found;
            attached_by_consumer = inner.slots[found as usize].attached_by_consumer;

            if width == 0 && height == 0 {
                width = inner.default_width;
                height = inner.default_height;
            }

            inner.slots[found as usize].buffer_state = super::buffer_slot::BufferState::Dequeued;

            let needs_reallocation = match inner.slots[found as usize].graphic_buffer.as_ref() {
                Some(buffer) => {
                    buffer.get_width() != width
                        || buffer.get_height() != height
                        || buffer.get_format() != format
                        || (buffer.get_usage() & usage) != usage
                }
                None => true,
            };

            if needs_reallocation {
                inner.slots[found as usize].acquire_called = false;
                inner.slots[found as usize].graphic_buffer = None;
                inner.slots[found as usize].request_buffer_called = false;
                inner.slots[found as usize].fence = Fence::no_fence();
                return_flags |= StatusCode::BUFFER_NEEDS_REALLOCATION;
            }

            out_fence = inner.slots[found as usize].fence;
            inner.slots[found as usize].fence = Fence::no_fence();
        }

        if return_flags.contains(StatusCode::BUFFER_NEEDS_REALLOCATION) {
            log::debug!(
                "BufferQueueProducer::dequeue_buffer allocating a new buffer for slot {}",
                out_slot
            );
            let graphic_buffer = Arc::new(GraphicBuffer::new(width, height, format, usage));
            {
                let mut inner = self.core.mutex.lock().unwrap();
                if inner.is_abandoned {
                    log::error!("BufferQueueProducer: BufferQueue has been abandoned");
                    trace_bqp_ring(&[
                        2,
                        bqp_seq,
                        Status::NoInit as i32 as u64,
                        -1i64 as u64,
                        0,
                        current_bqp_tid(),
                    ]);
                    trace_dequeue_return_for_present(bqp_seq, Status::NoInit, -1, 0);
                    return (Status::NoInit.into(), -1, Fence::default());
                }
                inner.slots[out_slot as usize].frame_number = u32::MAX as u64;
                inner.slots[out_slot as usize].graphic_buffer = Some(graphic_buffer);
            }
        }

        if attached_by_consumer {
            return_flags |= StatusCode::BUFFER_NEEDS_REALLOCATION;
        }

        log::debug!(
            "BufferQueueProducer::dequeue_buffer returning slot={} flags={}",
            out_slot,
            return_flags.raw()
        );
        trace_bqp(format_args!(
            "BQP::dequeue_buffer -> slot={} flags={}",
            out_slot,
            return_flags.raw()
        ));
        // log::info!(
        //     "[BQP_DEQUEUE_RET] slot={} flags=0x{:X}",
        //     out_slot,
        //     return_flags.raw()
        // );
        trace_bqp_ring(&[
            2,
            bqp_seq,
            Status::NoError as i32 as u64,
            out_slot as i64 as u64,
            return_flags.raw() as i64 as u64,
            current_bqp_tid(),
        ]);
        super::diagnostics::record_bqp(
            "dequeue_return",
            [
                bqp_seq,
                Status::NoError as i32 as u64,
                out_slot as i64 as u64,
                return_flags.raw() as i64 as u64,
                current_bqp_tid(),
                0,
            ],
        );
        trace_dequeue_return_for_present(bqp_seq, Status::NoError, out_slot, return_flags.raw());
        (return_flags, out_slot, out_fence)
    }

    pub fn detach_buffer(&self, slot: i32) -> Status {
        let mut inner = self.core.mutex.lock().unwrap();

        if inner.is_abandoned {
            log::error!("BufferQueueProducer: BufferQueue has been abandoned");
            return Status::NoInit;
        }

        if slot < 0 || slot as usize >= super::buffer_queue_defs::NUM_BUFFER_SLOTS {
            log::error!(
                "BufferQueueProducer::detach_buffer: slot {} out of range",
                slot
            );
            return Status::BadValue;
        }

        let s = slot as usize;
        if inner.slots[s].buffer_state != super::buffer_slot::BufferState::Dequeued {
            log::error!(
                "BufferQueueProducer::detach_buffer: slot {} is not owned by producer (state={:?})",
                slot,
                inner.slots[s].buffer_state
            );
            return Status::BadValue;
        }

        if !inner.slots[s].request_buffer_called {
            log::error!(
                "BufferQueueProducer::detach_buffer: buffer in slot {} has not been requested",
                slot
            );
            return Status::BadValue;
        }

        inner.free_buffer_locked(slot);
        self.core.signal_dequeue_condition();

        Status::NoError
    }

    pub fn detach_next_buffer(&self) -> (Status, Option<Arc<GraphicBuffer>>, Fence) {
        let mut inner = self.core.mutex.lock().unwrap();
        inner = self.core.wait_while_allocating_locked(inner);

        if inner.is_abandoned {
            log::error!("BufferQueueProducer: BufferQueue has been abandoned");
            return (Status::NoInit, None, Fence::default());
        }

        let mut found = BufferQueueCore::INVALID_BUFFER_SLOT;
        for s in 0..super::buffer_queue_defs::NUM_BUFFER_SLOTS {
            if inner.slots[s].buffer_state == super::buffer_slot::BufferState::Free
                && inner.slots[s].graphic_buffer.is_some()
                && (found == BufferQueueCore::INVALID_BUFFER_SLOT
                    || inner.slots[s].frame_number < inner.slots[found as usize].frame_number)
            {
                found = s as i32;
            }
        }

        if found == BufferQueueCore::INVALID_BUFFER_SLOT {
            return (Status::NoMemory, None, Fence::default());
        }

        let s = found as usize;
        let buffer = inner.slots[s].graphic_buffer.clone();
        let fence = inner.slots[s].fence;
        inner.free_buffer_locked(found);

        (Status::NoError, buffer, fence)
    }

    pub fn attach_buffer(&self, buffer: Option<Arc<GraphicBuffer>>) -> (StatusCode, i32) {
        let Some(buffer) = buffer else {
            log::error!("BufferQueueProducer::attach_buffer: cannot attach null buffer");
            return (Status::BadValue.into(), -1);
        };

        let mut inner = self.core.mutex.lock().unwrap();
        inner = self.core.wait_while_allocating_locked(inner);

        let mut return_flags = StatusCode::NO_ERROR;
        let mut found = 0;
        let (status, mut inner) =
            self.wait_for_free_slot_then_relock(false, &mut found, &mut return_flags, inner);
        if status != Status::NoError {
            return (status.into(), -1);
        }

        if found == BufferQueueCore::INVALID_BUFFER_SLOT {
            log::error!("BufferQueueProducer::attach_buffer: no available buffer slots");
            return (Status::Busy.into(), -1);
        }

        let s = found as usize;
        inner.slots[s].graphic_buffer = Some(buffer);
        inner.slots[s].buffer_state = super::buffer_slot::BufferState::Dequeued;
        inner.slots[s].fence = Fence::no_fence();
        inner.slots[s].request_buffer_called = true;

        (return_flags, found)
    }

    pub fn queue_buffer(&self, slot: i32, input: &QueueBufferInput) -> (Status, QueueBufferOutput) {
        record_bqp_event(BqpEvent::QueueBuffer);
        let bqp_seq = next_bqp_seq();
        super::diagnostics::record_bqp(
            "queue_enter",
            [bqp_seq, slot as i64 as u64, current_bqp_tid(), 0, 0, 0],
        );
        trace_bqp_ring(&[3, bqp_seq, slot as i64 as u64, current_bqp_tid()]);
        trace_bqp(format_args!("BQP::queue_buffer slot={}", slot));
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNT: AtomicU64 = AtomicU64::new(0);
            let n = COUNT.fetch_add(1, Ordering::Relaxed);
            // RUZU_TRACE_BQP_QUEUE_DENSE=1 — log every QueueBuffer call (not just
            let dense = std::env::var_os("RUZU_TRACE_BQP_QUEUE_DENSE").is_some();
            if dense || n < 16 || n.is_power_of_two() {
                log::info!("[BQP_QUEUE] #{} queue_buffer slot={}", n, slot);
            }
        }
        record_bqp_slot(slot);
        let (
            timestamp,
            is_auto_timestamp,
            crop,
            scaling_mode,
            transform,
            sticky_transform,
            async_flag,
            swap_interval,
            fence,
        ) = input.deflate();

        match scaling_mode {
            NativeWindowScalingMode::Freeze
            | NativeWindowScalingMode::ScaleToWindow
            | NativeWindowScalingMode::ScaleCrop
            | NativeWindowScalingMode::NoScaleCrop
            | NativeWindowScalingMode::PreserveAspectRatio => {}
        }

        let mut inner = self.core.mutex.lock().unwrap();

        if inner.is_abandoned {
            trace_bqp_ring(&[5, bqp_seq, Status::NoInit as i32 as u64, slot as i64 as u64]);
            return (Status::NoInit, QueueBufferOutput::new());
        }

        let max_buffer_count = inner.get_max_buffer_count_locked(async_flag);
        if async_flag
            && inner.override_max_buffer_count != 0
            && inner.override_max_buffer_count < max_buffer_count
        {
            trace_bqp_ring(&[
                5,
                bqp_seq,
                Status::BadValue as i32 as u64,
                slot as i64 as u64,
            ]);
            return (Status::BadValue, QueueBufferOutput::new());
        }

        if slot < 0 || slot >= max_buffer_count {
            trace_bqp_ring(&[
                5,
                bqp_seq,
                Status::BadValue as i32 as u64,
                slot as i64 as u64,
            ]);
            return (Status::BadValue, QueueBufferOutput::new());
        }

        let s = slot as usize;
        if inner.slots[s].buffer_state != super::buffer_slot::BufferState::Dequeued {
            log::error!(
                "BufferQueueProducer::queue_buffer: slot {} not dequeued (state={:?})",
                slot,
                inner.slots[s].buffer_state
            );
            trace_bqp_ring(&[
                5,
                bqp_seq,
                Status::BadValue as i32 as u64,
                slot as i64 as u64,
            ]);
            return (Status::BadValue, QueueBufferOutput::new());
        }
        if !inner.slots[s].request_buffer_called {
            log::error!(
                "BufferQueueProducer::queue_buffer: slot {} queued without RequestBuffer",
                slot
            );
            trace_bqp_ring(&[
                5,
                bqp_seq,
                Status::BadValue as i32 as u64,
                slot as i64 as u64,
            ]);
            return (Status::BadValue, QueueBufferOutput::new());
        }

        let Some(graphic_buffer) = inner.slots[s].graphic_buffer.clone() else {
            log::error!(
                "BufferQueueProducer::queue_buffer: slot {} missing graphic buffer",
                slot
            );
            trace_bqp_ring(&[
                5,
                bqp_seq,
                Status::BadValue as i32 as u64,
                slot as i64 as u64,
            ]);
            return (Status::BadValue, QueueBufferOutput::new());
        };
        let buffer_rect = Rectangle::new(
            0,
            0,
            graphic_buffer.get_width() as i32,
            graphic_buffer.get_height() as i32,
        );
        let cropped_rect = Rectangle::new(
            crop.left.max(buffer_rect.left),
            crop.top.max(buffer_rect.top),
            crop.right.min(buffer_rect.right),
            crop.bottom.min(buffer_rect.bottom),
        );
        if cropped_rect != crop {
            log::error!(
                "BufferQueueProducer::queue_buffer: crop {:?} not contained in slot {} buffer {:?}",
                crop,
                slot,
                buffer_rect
            );
            trace_bqp_ring(&[
                5,
                bqp_seq,
                Status::BadValue as i32 as u64,
                slot as i64 as u64,
            ]);
            return (Status::BadValue, QueueBufferOutput::new());
        }

        inner.frame_counter += 1;
        inner.slots[s].buffer_state = super::buffer_slot::BufferState::Queued;
        inner.slots[s].frame_number = inner.frame_counter;
        inner.slots[s].queue_time = timestamp;
        inner.slots[s].presentation_time =
            common::cpu_features::G_WALL_CLOCK.get_time_ns().as_nanos() as i64;
        inner.slots[s].fence = fence;

        let frame_num = inner.frame_counter;
        let item = super::buffer_item::BufferItem {
            slot,
            graphic_buffer: Some(graphic_buffer.clone()),
            fence,
            crop,
            transform: NativeWindowTransform::from_bits_retain(
                transform.bits() & !NativeWindowTransform::INVERSE_DISPLAY.bits(),
            ),
            scaling_mode: scaling_mode as u32,
            timestamp,
            is_auto_timestamp,
            frame_number: frame_num,
            swap_interval,
            is_droppable: inner.dequeue_buffer_cannot_block || async_flag,
            acquire_called: inner.slots[s].acquire_called,
            transform_to_display_inverse: transform
                .contains(NativeWindowTransform::INVERSE_DISPLAY),
        };
        let mut callback_item = super::buffer_item::BufferItem {
            graphic_buffer: item.graphic_buffer.clone(),
            fence: item.fence,
            crop: item.crop,
            transform: item.transform,
            scaling_mode: item.scaling_mode,
            timestamp: item.timestamp,
            is_auto_timestamp: item.is_auto_timestamp,
            frame_number: item.frame_number,
            slot: item.slot,
            is_droppable: item.is_droppable,
            acquire_called: item.acquire_called,
            transform_to_display_inverse: item.transform_to_display_inverse,
            swap_interval: item.swap_interval,
        };
        let mut frame_available_listener = None;
        let mut frame_replaced_listener = None;

        *self.sticky_transform.lock().unwrap() = sticky_transform;

        if inner.queue.is_empty() {
            inner.queue.push(item);
            frame_available_listener = inner.consumer_listener.clone();
        } else {
            let front_is_droppable = inner.queue[0].is_droppable;
            if front_is_droppable {
                let (front_slot, front_frame_number, front_still_tracking) = {
                    let front = &inner.queue[0];
                    (front.slot, front.frame_number, inner.still_tracking(front))
                };
                if front_still_tracking {
                    inner.slots[front_slot as usize].buffer_state =
                        super::buffer_slot::BufferState::Free;
                    if *common::settings::values().enable_buffer_history.get_value() {
                        self.core.update_history(
                            front_frame_number,
                            super::buffer_slot::BufferState::Free,
                        );
                    }
                    inner.slots[front_slot as usize].frame_number = 0;
                }
                inner.queue[0] = item;
                frame_replaced_listener = inner.consumer_listener.clone();
            } else {
                inner.queue.push(item);
                frame_available_listener = inner.consumer_listener.clone();
            }
        }
        if *common::settings::values().enable_buffer_history.get_value() {
            self.core.push_history(
                inner.frame_counter,
                inner.slots[s].queue_time,
                inner.slots[s].presentation_time,
                super::buffer_slot::BufferState::Queued,
            );
        }
        inner.buffer_has_been_queued = true;
        let queue_len = inner.queue.len() as u64;
        trace_bqp_ring(&[
            4,
            bqp_seq,
            slot as i64 as u64,
            frame_num,
            queue_len,
            u64::from(inner.dequeue_buffer_cannot_block || async_flag),
            u64::from(inner.slots[s].acquire_called),
            current_bqp_tid(),
        ]);
        super::diagnostics::record_bqp(
            "queue_commit",
            [
                bqp_seq,
                slot as i64 as u64,
                frame_num,
                queue_len,
                u64::from(inner.dequeue_buffer_cannot_block || async_flag),
                u64::from(inner.slots[s].acquire_called),
            ],
        );

        let mut output = QueueBufferOutput::new();
        output.inflate(
            inner.default_width,
            inner.default_height,
            inner.transform_hint,
            inner.queue.len() as u32,
        );

        let callback_ticket = {
            let mut next_callback_ticket = self.next_callback_ticket.lock().unwrap();
            let ticket = *next_callback_ticket;
            *next_callback_ticket += 1;
            ticket
        };

        self.core.signal_dequeue_condition();
        drop(inner);
        callback_item.graphic_buffer = None;
        callback_item.slot = super::buffer_item::BufferItem::INVALID_BUFFER_SLOT;

        let mut current_callback_ticket = self.current_callback_ticket.lock().unwrap();
        while callback_ticket != *current_callback_ticket {
            current_callback_ticket = self
                .callback_condition
                .wait(current_callback_ticket)
                .unwrap();
        }

        if let Some(listener) = frame_available_listener {
            listener.on_frame_available(&callback_item);
        } else if let Some(listener) = frame_replaced_listener {
            listener.on_frame_replaced(&callback_item);
        }

        *current_callback_ticket += 1;
        self.callback_condition.notify_all();
        drop(current_callback_ticket);

        log::debug!(
            "BufferQueueProducer::queue_buffer slot={} frame={}",
            slot,
            self.core.mutex.lock().unwrap().frame_counter
        );
        trace_bqp(format_args!(
            "BQP::queue_buffer -> status={:?} slot={}",
            Status::NoError,
            slot
        ));
        trace_bqp_ring(&[
            5,
            bqp_seq,
            Status::NoError as i32 as u64,
            slot as i64 as u64,
            current_bqp_tid(),
        ]);
        super::diagnostics::record_bqp(
            "queue_return",
            [
                bqp_seq,
                Status::NoError as i32 as u64,
                slot as i64 as u64,
                current_bqp_tid(),
                0,
                0,
            ],
        );
        (Status::NoError, output)
    }

    pub fn cancel_buffer(&self, slot: i32, fence: &Fence) {
        record_bqp_event(BqpEvent::CancelBuffer);
        super::diagnostics::record_bqp(
            "cancel_buffer",
            [slot as i64 as u64, current_bqp_tid(), 0, 0, 0, 0],
        );
        trace_bqp_ring(&[6, next_bqp_seq(), slot as i64 as u64, current_bqp_tid()]);
        let mut inner = self.core.mutex.lock().unwrap();
        if inner.is_abandoned {
            return;
        }
        if slot < 0 || slot as usize >= super::buffer_queue_defs::NUM_BUFFER_SLOTS {
            return;
        }
        inner.slots[slot as usize].buffer_state = super::buffer_slot::BufferState::Free;
        inner.slots[slot as usize].frame_number = 0;
        inner.slots[slot as usize].fence = *fence;
        self.core.signal_dequeue_condition();
        self.signal_buffer_wait_event();
        drop(inner);
    }

    pub fn query(&self, what: NativeWindow) -> (Status, i32) {
        record_bqp_event(BqpEvent::Query);
        let inner = self.core.mutex.lock().unwrap();

        if inner.is_abandoned {
            return (Status::NoInit, 0);
        }

        let value = match what {
            NativeWindow::Width => inner.default_width as i32,
            NativeWindow::Height => inner.default_height as i32,
            NativeWindow::Format => inner.default_buffer_format as i32,
            NativeWindow::MinUndequeedBuffers => {
                inner.get_min_undequeued_buffer_count_locked(false)
            }
            NativeWindow::StickyTransform => *self.sticky_transform.lock().unwrap() as i32,
            NativeWindow::ConsumerRunningBehind => (inner.queue.len() > 1) as i32,
            NativeWindow::ConsumerUsageBits => inner.consumer_usage_bit as i32,
            _ => {
                log::warn!("BufferQueueProducer::query unhandled: {:?}", what);
                return (Status::BadValue, 0);
            }
        };

        (Status::NoError, value)
    }

    pub fn connect(
        &self,
        listener: Option<Arc<dyn IProducerListener>>,
        api: NativeWindowApi,
        producer_controlled_by_app: bool,
    ) -> (Status, QueueBufferOutput) {
        record_bqp_event(BqpEvent::Connect);
        super::diagnostics::record_bqp(
            "connect",
            [
                api as i32 as u64,
                u64::from(producer_controlled_by_app),
                current_bqp_tid(),
                0,
                0,
                0,
            ],
        );
        trace_bqp(format_args!(
            "BQP::connect api={:?} producer_controlled_by_app={}",
            api, producer_controlled_by_app
        ));
        let mut inner = self.core.mutex.lock().unwrap();

        if inner.is_abandoned {
            return (Status::NoInit, QueueBufferOutput::new());
        }
        if inner.consumer_listener.is_none() {
            log::error!("BufferQueueProducer: BufferQueue has no consumer");
            return (Status::NoInit, QueueBufferOutput::new());
        }

        if inner.connected_api != NativeWindowApi::NoConnectedApi {
            log::error!(
                "BufferQueueProducer: already connected (api={:?})",
                inner.connected_api
            );
            return (Status::BadValue, QueueBufferOutput::new());
        }

        match api {
            NativeWindowApi::Egl
            | NativeWindowApi::Cpu
            | NativeWindowApi::Media
            | NativeWindowApi::Camera => {
                inner.connected_api = api;
                inner.connected_producer_listener = listener;
            }
            _ => {
                log::error!("BufferQueueProducer: unknown api {:?}", api);
                return (Status::BadValue, QueueBufferOutput::new());
            }
        }

        inner.buffer_has_been_queued = false;
        inner.dequeue_buffer_cannot_block =
            inner.consumer_controlled_by_app && producer_controlled_by_app;

        let mut output = QueueBufferOutput::new();
        output.inflate(
            inner.default_width,
            inner.default_height,
            inner.transform_hint,
            inner.queue.len() as u32,
        );

        (Status::NoError, output)
    }

    pub fn disconnect(&self, api: NativeWindowApi) -> Status {
        record_bqp_event(BqpEvent::Disconnect);
        super::diagnostics::record_bqp(
            "disconnect",
            [api as i32 as u64, current_bqp_tid(), 0, 0, 0, 0],
        );
        let mut status = Status::NoError;
        let listener;

        let mut inner = self.core.mutex.lock().unwrap();
        inner = self.core.wait_while_allocating_locked(inner);

        if inner.is_abandoned {
            return Status::NoError;
        }

        match api {
            NativeWindowApi::Egl
            | NativeWindowApi::Cpu
            | NativeWindowApi::Media
            | NativeWindowApi::Camera => {
                if inner.connected_api == api {
                    inner.queue.clear();
                    inner.free_all_buffers_locked();
                    inner.connected_producer_listener = None;
                    inner.connected_api = NativeWindowApi::NoConnectedApi;
                    self.core.signal_dequeue_condition();
                    self.signal_buffer_wait_event();
                    listener = inner.consumer_listener.clone();
                } else {
                    log::error!(
                        "BufferQueueProducer: still connected to another API {:?} (requested={:?})",
                        inner.connected_api,
                        api
                    );
                    status = Status::BadValue;
                    listener = None;
                }
            }
            _ => {
                log::error!("BufferQueueProducer: unknown API {:?}", api);
                status = Status::BadValue;
                listener = None;
            }
        }
        drop(inner);

        if let Some(listener) = listener {
            listener.on_buffers_released();
        }

        status
    }

    pub fn set_preallocated_buffer(
        &self,
        slot: i32,
        buffer: Option<Arc<NvGraphicBuffer>>,
    ) -> Status {
        record_bqp_event(BqpEvent::SetPreallocatedBuffer);
        let (buffer_handle, buffer_width, buffer_height, buffer_size) =
            buffer.as_ref().map_or((0, 0, 0, 0), |buf| {
                (
                    buf.get_handle() as u64,
                    buf.get_width() as u64,
                    buf.get_height() as u64,
                    buf.get_stride() as u64,
                )
            });
        super::diagnostics::record_bqp(
            "set_preallocated",
            [
                slot as i64 as u64,
                buffer_handle,
                buffer_width,
                buffer_height,
                buffer_size,
                current_bqp_tid(),
            ],
        );
        if let Some(buf) = buffer.as_ref() {
            trace_bqp(format_args!(
                "BQP::set_preallocated_buffer slot={} magic=0x{:08X} w={} h={} stride={} fmt={:?} usage=0x{:X} buffer_id={} ext_fmt={:?} handle={} offset={}",
                slot,
                buf.magic,
                buf.get_width(),
                buf.get_height(),
                buf.get_stride(),
                buf.get_format(),
                buf.get_usage(),
                buf.get_buffer_id(),
                buf.get_external_format(),
                buf.get_handle(),
                buf.get_offset()
            ));
        } else {
            trace_bqp(format_args!(
                "BQP::set_preallocated_buffer slot={} None",
                slot
            ));
        }
        if slot < 0 || slot as usize >= super::buffer_queue_defs::NUM_BUFFER_SLOTS {
            return Status::BadValue;
        }

        let mut inner = self.core.mutex.lock().unwrap();
        let s = slot as usize;
        inner.slots[s] = BufferSlot::default();
        inner.slots[s].graphic_buffer = Some(Arc::new(GraphicBuffer::from_optional_nv_buffer(
            Arc::clone(&self.nvmap),
            buffer.as_deref(),
        )));
        inner.slots[s].fence = Fence::no_fence();
        inner.slots[s].frame_number = 0;

        if let Some(buf) = buffer {
            inner.slots[s].is_preallocated = true;
            inner.override_max_buffer_count = inner.get_preallocated_buffer_count_locked();
            inner.default_width = buf.get_width();
            inner.default_height = buf.get_height();
            inner.default_buffer_format = buf.get_format();
        }

        self.core.signal_dequeue_condition();
        self.signal_buffer_wait_event();
        drop(inner);

        Status::NoError
    }
}

impl IBinder for BufferQueueProducer {
    fn transact(&self, code: u32, parcel_data: &[u8], parcel_reply: &mut [u8], _flags: u32) {
        #[repr(u32)]
        enum TransactionId {
            RequestBuffer = 1,
            SetBufferCount = 2,
            DequeueBuffer = 3,
            DetachBuffer = 4,
            DetachNextBuffer = 5,
            AttachBuffer = 6,
            QueueBuffer = 7,
            CancelBuffer = 8,
            Query = 9,
            Connect = 10,
            Disconnect = 11,
            AllocateBuffers = 13,
            SetPreallocatedBuffer = 14,
            GetBufferHistory = 17,
        }

        let mut status = Status::NoError;
        let mut dequeue_status: Option<StatusCode> = None;
        let mut parcel_in = InputParcel::new(parcel_data);
        let mut parcel_out = OutputParcel::new();

        match code {
            x if x == TransactionId::Connect as u32 => {
                let enable_listener = parcel_in.read::<u8>() != 0;
                let api = match parcel_in.read::<i32>() {
                    0 => NativeWindowApi::NoConnectedApi,
                    1 => NativeWindowApi::Egl,
                    2 => NativeWindowApi::Cpu,
                    3 => NativeWindowApi::Media,
                    4 => NativeWindowApi::Camera,
                    _ => NativeWindowApi::NoConnectedApi,
                };
                let producer_controlled_by_app = parcel_in.read::<u8>() != 0;

                if enable_listener {
                    stop_unimplemented_connect_listener();
                }

                let (new_status, output) = self.connect(None, api, producer_controlled_by_app);
                status = new_status;
                parcel_out.write(&output);
            }
            x if x == TransactionId::SetPreallocatedBuffer as u32 => {
                let slot = parcel_in.read::<i32>();
                let buffer = parcel_in.read_object::<NvGraphicBuffer>().map(Arc::new);
                status = self.set_preallocated_buffer(slot, buffer);
            }
            x if x == TransactionId::DequeueBuffer as u32 => {
                let is_async = parcel_in.read::<u8>() != 0;
                let width = parcel_in.read::<u32>();
                let height = parcel_in.read::<u32>();
                let pixel_format = parcel_in.read::<PixelFormat>();
                let usage = parcel_in.read::<u32>();

                let (new_status, slot, fence) =
                    self.dequeue_buffer(is_async, width, height, pixel_format, usage);
                dequeue_status = Some(new_status);
                status = match new_status.raw() {
                    0 => Status::NoError,
                    1 => Status::BUFFER_NEEDS_REALLOCATION,
                    2 => Status::RELEASE_ALL_BUFFERS,
                    3 => Status::BUFFER_NEEDS_REALLOCATION,
                    -11 => Status::WouldBlock,
                    -12 => Status::NoMemory,
                    -16 => Status::Busy,
                    -19 => Status::NoInit,
                    -22 => Status::BadValue,
                    -38 => Status::InvalidOperation,
                    _ => Status::NoError,
                };
                parcel_out.write(&slot);
                parcel_out.write_flattened_object(Some(&fence));
            }
            x if x == TransactionId::RequestBuffer as u32 => {
                let slot = parcel_in.read::<i32>();
                let (new_status, buf) = self.request_buffer(slot);
                status = new_status;
                parcel_out.write_flattened_object(buf.as_ref().map(|g| &g.buffer));
            }
            x if x == TransactionId::QueueBuffer as u32 => {
                let slot = parcel_in.read::<i32>();
                let input = parcel_in.read_flattened::<QueueBufferInput>();
                let (new_status, output) = self.queue_buffer(slot, &input);
                status = new_status;
                parcel_out.write(&output);
            }
            x if x == TransactionId::Query as u32 => {
                let what_raw = parcel_in.read::<i32>();
                match what_raw {
                    0 => {
                        let (new_status, value) = self.query(NativeWindow::Width);
                        status = new_status;
                        parcel_out.write(&value);
                    }
                    1 => {
                        let (new_status, value) = self.query(NativeWindow::Height);
                        status = new_status;
                        parcel_out.write(&value);
                    }
                    2 => {
                        let (new_status, value) = self.query(NativeWindow::Format);
                        status = new_status;
                        parcel_out.write(&value);
                    }
                    3 => {
                        let (new_status, value) = self.query(NativeWindow::MinUndequeedBuffers);
                        status = new_status;
                        parcel_out.write(&value);
                    }
                    9 => {
                        let (new_status, value) = self.query(NativeWindow::ConsumerRunningBehind);
                        status = new_status;
                        parcel_out.write(&value);
                    }
                    10 => {
                        let (new_status, value) = self.query(NativeWindow::ConsumerUsageBits);
                        status = new_status;
                        parcel_out.write(&value);
                    }
                    11 => {
                        let (new_status, value) = self.query(NativeWindow::StickyTransform);
                        status = new_status;
                        parcel_out.write(&value);
                    }
                    _ => {
                        log::error!(
                            "BufferQueueProducer::transact Query unknown what={}",
                            what_raw
                        );
                        status = Status::BadValue;
                    }
                }
            }
            x if x == TransactionId::CancelBuffer as u32 => {
                let slot = parcel_in.read::<i32>();
                let fence = parcel_in.read_flattened::<Fence>();
                self.cancel_buffer(slot, &fence);
            }
            x if x == TransactionId::Disconnect as u32 => {
                let api = match parcel_in.read::<i32>() {
                    0 => NativeWindowApi::NoConnectedApi,
                    1 => NativeWindowApi::Egl,
                    2 => NativeWindowApi::Cpu,
                    3 => NativeWindowApi::Media,
                    4 => NativeWindowApi::Camera,
                    _ => {
                        log::error!("BufferQueueProducer::transact Disconnect unknown api");
                        NativeWindowApi::NoConnectedApi
                    }
                };
                status = self.disconnect(api);
            }
            x if x == TransactionId::DetachBuffer as u32 => {
                let slot = parcel_in.read::<i32>();
                status = self.detach_buffer(slot);
            }
            x if x == TransactionId::SetBufferCount as u32 => {
                let buffer_count = parcel_in.read::<i32>();
                status = self.set_buffer_count(buffer_count);
            }
            x if x == TransactionId::GetBufferHistory as u32 => {
                if *common::settings::values().enable_buffer_history.get_value() {
                    log::debug!("BufferQueueProducer::transact GetBufferHistory");
                    let request = parcel_in.read::<i32>();
                    if request <= 0 {
                        parcel_out.write(&Status::BadValue);
                        parcel_out.write(&0i32);
                    } else {
                        let mut snapshot = {
                            let history = self.core.buffer_history.lock().unwrap();
                            history.map.values().copied().collect::<Vec<_>>()
                        };
                        snapshot.sort_by(|a, b| b.frame_number.cmp(&a.frame_number));
                        let limit = request.min(snapshot.len() as i32);
                        parcel_out.write(&Status::NoError);
                        parcel_out.write(&limit);
                        for info in snapshot.iter().take(limit as usize) {
                            parcel_out.write(info);
                        }
                    }
                } else {
                    log::debug!("BufferQueueProducer::transact GetBufferHistory (STUBBED)");
                }
            }
            x if x == TransactionId::DetachNextBuffer as u32 => {
                stop_unimplemented_transact(code, "DetachNextBuffer");
            }
            x if x == TransactionId::AttachBuffer as u32 => {
                stop_unimplemented_transact(code, "AttachBuffer");
            }
            x if x == TransactionId::AllocateBuffers as u32 => {
                stop_unimplemented_transact(code, "AllocateBuffers");
            }
            _ => {
                stop_unimplemented_transact(code, "Unknown");
            }
        }

        let status_to_write = dequeue_status.map_or(status as i32, StatusCode::raw);
        parcel_out.write(&status_to_write);
        let serialized = parcel_out.serialize();
        let copy_len = std::cmp::min(parcel_reply.len(), serialized.len());
        parcel_reply[..copy_len].copy_from_slice(&serialized[..copy_len]);
    }

    fn get_native_handle(&self, _type_id: u32) -> Option<Arc<Mutex<KReadableEvent>>> {
        let readable_event = self.buffer_wait_event().readable_event();
        let readable_object_id = readable_event
            .as_ref()
            .map(|event| event.lock().unwrap().object_id)
            .unwrap_or(0);
        trace_bqp_ring(&[21, next_bqp_seq(), readable_object_id, current_bqp_tid()]);
        readable_event
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn register_native_handle_owner(
        &self,
        process: Arc<ProcessLock>,
        _scheduler: Arc<Mutex<KScheduler>>,
    ) {
        let event = self.buffer_wait_event();
        if event.readable_event().is_none() {
            let Some(kernel) = crate::hle::kernel::kernel::get_kernel_ref() else {
                return;
            };
            let event_object_id = kernel.create_new_object_id() as u64;
            let readable_event_object_id = kernel.create_new_object_id() as u64;
            let owner_process_id = process.lock().unwrap().get_process_id();
            let signaled = event.is_signaled();

            let mut event_owner = KEvent::new();
            let mut readable_event = KReadableEvent::new();
            event_owner.initialize(owner_process_id, readable_event_object_id);
            readable_event.initialize(event_object_id, readable_event_object_id);
            if signaled {
                readable_event
                    .is_signaled
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }

            let event_owner = Arc::new(Mutex::new(event_owner));
            let readable_event = Arc::new(Mutex::new(readable_event));
            {
                let mut process_guard = process.lock().unwrap();
                process_guard.register_event_object(event_object_id, Arc::clone(&event_owner));
                process_guard.register_readable_event_object(
                    readable_event_object_id,
                    Arc::clone(&readable_event),
                );
            }
            event.attach_kernel_event_owner(
                event_owner,
                Arc::clone(&readable_event),
                Arc::clone(&process),
            );
            trace_bqp_ring(&[
                22,
                next_bqp_seq(),
                event_object_id,
                readable_event_object_id,
                readable_event
                    .lock()
                    .unwrap()
                    .is_signaled
                    .load(Ordering::Relaxed) as u64,
                current_bqp_tid(),
            ]);
        }
    }
}

impl Drop for BufferQueueProducer {
    fn drop(&mut self) {
        self.service_context
            .lock()
            .unwrap()
            .close_event(self.buffer_wait_event_handle);
    }
}

// =============================================================================
// BQP slot histogram (RUZU_PROFILE_BQP_SLOTS=1)
// =============================================================================
//
// Tracks which slot indices are passed to queue_buffer. If the same slot is
// queued over and over, the game is pumping cached frames (e.g. stuck on a

static BQP_SLOT_COUNTS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<i32, u64>>> =
    std::sync::OnceLock::new();
static BQP_EVENT_COUNTS: [std::sync::atomic::AtomicU64; 9] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];

#[repr(usize)]
#[derive(Clone, Copy)]
enum BqpEvent {
    Connect = 0,
    SetPreallocatedBuffer = 1,
    SetBufferCount = 2,
    DequeueBuffer = 3,
    RequestBuffer = 4,
    QueueBuffer = 5,
    CancelBuffer = 6,
    Query = 7,
    Disconnect = 8,
}

impl BqpEvent {
    const ALL: [Self; 9] = [
        Self::Connect,
        Self::SetPreallocatedBuffer,
        Self::SetBufferCount,
        Self::DequeueBuffer,
        Self::RequestBuffer,
        Self::QueueBuffer,
        Self::CancelBuffer,
        Self::Query,
        Self::Disconnect,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Connect => "Connect",
            Self::SetPreallocatedBuffer => "SetPreallocatedBuffer",
            Self::SetBufferCount => "SetBufferCount",
            Self::DequeueBuffer => "DequeueBuffer",
            Self::RequestBuffer => "RequestBuffer",
            Self::QueueBuffer => "QueueBuffer",
            Self::CancelBuffer => "CancelBuffer",
            Self::Query => "Query",
            Self::Disconnect => "Disconnect",
        }
    }
}

fn record_bqp_event(event: BqpEvent) {
    if std::env::var_os("RUZU_PROFILE_BQP_SLOTS").is_none() {
        return;
    }
    BQP_EVENT_COUNTS[event as usize].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn record_bqp_slot(slot: i32) {
    if std::env::var_os("RUZU_PROFILE_BQP_SLOTS").is_none() {
        return;
    }
    let map =
        BQP_SLOT_COUNTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut g = map.lock().unwrap();
    *g.entry(slot).or_insert(0) += 1;
}

pub fn dump_bqp_slot_profile() {
    if std::env::var_os("RUZU_PROFILE_BQP_SLOTS").is_none() {
        return;
    }
    eprintln!("[BQP_PROFILE] call counts:");
    for event in BqpEvent::ALL {
        let count = BQP_EVENT_COUNTS[event as usize].load(std::sync::atomic::Ordering::Relaxed);
        eprintln!("[BQP_PROFILE]   {:<24} {}", event.name(), count);
    }
    let Some(map) = BQP_SLOT_COUNTS.get() else {
        return;
    };
    let entries: Vec<(i32, u64)> = {
        let g = map.lock().unwrap();
        g.iter().map(|(k, v)| (*k, *v)).collect()
    };
    if entries.is_empty() {
        return;
    }
    let mut entries = entries;
    entries.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let total: u64 = entries.iter().map(|(_, n)| n).sum();
    eprintln!(
        "[BQP_SLOT_PROFILE] {} QueueBuffer calls across {} unique slots:",
        total,
        entries.len()
    );
    for (slot, n) in entries.iter() {
        let pct = (*n as f64 / total as f64) * 100.0;
        eprintln!(
            "[BQP_SLOT_PROFILE]   slot={:<4} count={:<6} ({:.1}%)",
            slot, n, pct
        );
    }
}

#[cfg(test)]
mod tests {
    use common::math_util::Rectangle;

    use crate::hle::kernel::k_process::KProcess;
    use crate::hle::service::kernel_helpers::ServiceContext;
    use crate::hle::service::nvdrv::core::container::Container;

    use super::super::buffer_item::BufferItem;
    use super::super::consumer_listener::IConsumerListener;
    use super::super::graphic_buffer_producer::QueueBufferInput;
    use super::super::pixel_format::PixelFormat;
    use super::*;

    fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
        match payload.downcast::<String>() {
            Ok(message) => *message,
            Err(payload) => match payload.downcast::<&'static str>() {
                Ok(message) => (*message).to_owned(),
                Err(_) => "non-string panic".to_owned(),
            },
        }
    }

    struct TestConsumerListener;

    impl IConsumerListener for TestConsumerListener {
        fn on_frame_available(&self, _item: &BufferItem) {}
        fn on_frame_replaced(&self, _item: &BufferItem) {}
        fn on_buffers_released(&self) {}
        fn on_sideband_stream_changed(&self) {}
    }

    fn install_test_consumer(core: &Arc<BufferQueueCore>) {
        core.mutex.lock().unwrap().consumer_listener = Some(Arc::new(TestConsumerListener));
    }

    fn test_nvmap() -> Arc<NvMap> {
        Container::new().get_nv_map_file_handle()
    }

    fn test_service_context() -> Arc<Mutex<ServiceContext>> {
        Arc::new(Mutex::new(ServiceContext::new(
            "BufferQueueProducerTest".to_string(),
        )))
    }

    fn enable_buffer_history_for_test() -> (bool, bool) {
        let previous = {
            let values = common::settings::values();
            (
                *values.enable_buffer_history.get_value_global(),
                values.enable_buffer_history.using_global(),
            )
        };
        let mut values = common::settings::values_mut();
        values.enable_buffer_history.set_global(true);
        values.enable_buffer_history.set_value(true);
        previous
    }

    fn restore_buffer_history_after_test(previous: (bool, bool)) {
        let mut values = common::settings::values_mut();
        values.enable_buffer_history.setting.set_value(previous.0);
        values.enable_buffer_history.set_global(previous.1);
    }

    fn get_buffer_history_request(count: i32) -> Vec<u8> {
        let mut parcel = Vec::new();
        parcel.extend_from_slice(&16u32.to_ne_bytes());
        parcel.extend_from_slice(&16u32.to_ne_bytes());
        parcel.extend_from_slice(&0u32.to_ne_bytes());
        parcel.extend_from_slice(&32u32.to_ne_bytes());
        parcel.extend_from_slice(&0u32.to_ne_bytes());
        parcel.extend_from_slice(&0u32.to_ne_bytes());
        parcel.extend_from_slice(&0u16.to_ne_bytes());
        parcel.extend_from_slice(&0u16.to_ne_bytes());
        parcel.extend_from_slice(&count.to_ne_bytes());
        parcel
    }

    fn read_i32(bytes: &[u8], offset: usize) -> i32 {
        i32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn read_u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_ne_bytes(bytes[offset..offset + 8].try_into().unwrap())
    }

    #[test]
    fn get_native_handle_returns_persistent_buffer_wait_event() {
        let core = BufferQueueCore::new();
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let scheduler = Arc::new(Mutex::new(KScheduler::new(0)));
        let producer = BufferQueueProducer::new(test_service_context(), core, test_nvmap());
        producer.register_native_handle_owner(process, scheduler);

        let first = producer.get_native_handle(0).unwrap();
        let second = producer.get_native_handle(15).unwrap();

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn connect_sets_nonblocking_flag_from_core_and_producer_control() {
        let core = BufferQueueCore::new();
        core.mutex.lock().unwrap().consumer_controlled_by_app = true;
        install_test_consumer(&core);
        let producer = BufferQueueProducer::new(test_service_context(), core.clone(), test_nvmap());

        let (status, _) = producer.connect(None, NativeWindowApi::Egl, true);
        assert_eq!(status, Status::NoError);
        assert!(core.mutex.lock().unwrap().dequeue_buffer_cannot_block);
    }

    #[test]
    fn disconnect_signals_buffer_wait_event() {
        let core = BufferQueueCore::new();
        install_test_consumer(&core);
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let scheduler = Arc::new(Mutex::new(KScheduler::new(0)));
        let producer = BufferQueueProducer::new(test_service_context(), core, test_nvmap());
        producer.register_native_handle_owner(process, scheduler);
        let event = producer.get_native_handle(0).unwrap();
        assert!(!event.lock().unwrap().is_signaled());

        let (status, _) = producer.connect(None, NativeWindowApi::Egl, false);
        assert_eq!(status, Status::NoError);
        assert_eq!(producer.disconnect(NativeWindowApi::Egl), Status::NoError);

        assert!(event.lock().unwrap().is_signaled());
    }

    #[test]
    fn disconnect_after_abandon_is_noop_success() {
        let core = BufferQueueCore::new();
        install_test_consumer(&core);
        core.mutex.lock().unwrap().is_abandoned = true;
        let producer = BufferQueueProducer::new(test_service_context(), core, test_nvmap());

        assert_eq!(producer.disconnect(NativeWindowApi::Egl), Status::NoError);
    }

    #[test]
    fn set_preallocated_buffer_signals_wait_event_and_updates_defaults() {
        let core = BufferQueueCore::new();
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let scheduler = Arc::new(Mutex::new(KScheduler::new(0)));
        let producer = BufferQueueProducer::new(test_service_context(), core.clone(), test_nvmap());
        producer.register_native_handle_owner(process, scheduler);
        let event = producer.get_native_handle(0).unwrap();
        let buffer = Arc::new(NvGraphicBuffer::new(1280, 720, PixelFormat::Rgba8888, 0));

        assert_eq!(
            producer.set_preallocated_buffer(0, Some(buffer)),
            Status::NoError
        );
        assert!(event.lock().unwrap().is_signaled());

        let inner = core.mutex.lock().unwrap();
        assert_eq!(inner.default_width, 1280);
        assert_eq!(inner.default_height, 720);
        assert_eq!(inner.override_max_buffer_count, 1);
    }

    #[test]
    fn set_preallocated_buffer_resets_slot_and_uses_no_fence() {
        let core = BufferQueueCore::new();
        let producer = BufferQueueProducer::new(test_service_context(), core.clone(), test_nvmap());

        {
            let mut inner = core.mutex.lock().unwrap();
            inner.slots[0].request_buffer_called = true;
            inner.slots[0].frame_number = 99;
            inner.slots[0].is_preallocated = true;
            inner.slots[0].fence = Fence {
                num_fences: 1,
                ..Fence::default()
            };
        }

        assert_eq!(producer.set_preallocated_buffer(0, None), Status::NoError);

        let inner = core.mutex.lock().unwrap();
        let graphic_buffer = inner.slots[0].graphic_buffer.as_ref().unwrap();
        assert_eq!(graphic_buffer.get_buffer_id(), 0);
        assert_eq!(graphic_buffer.get_handle(), 0);
        assert!(!inner.slots[0].request_buffer_called);
        assert_eq!(inner.slots[0].frame_number, 0);
        assert!(!inner.slots[0].is_preallocated);
        assert_eq!(inner.slots[0].fence.num_fences, 0);
        assert_eq!(inner.slots[0].fence.fences[0].id, -1);
    }

    #[test]
    fn queue_buffer_marks_core_as_having_queued_buffers() {
        let core = BufferQueueCore::new();
        install_test_consumer(&core);
        let producer = BufferQueueProducer::new(test_service_context(), core.clone(), test_nvmap());
        let buffer = Arc::new(NvGraphicBuffer::new(16, 16, PixelFormat::Rgba8888, 0));
        assert_eq!(
            producer.set_preallocated_buffer(0, Some(buffer)),
            Status::NoError
        );

        let (status, slot, _fence) =
            producer.dequeue_buffer(false, 16, 16, PixelFormat::Rgba8888, 0);
        assert_eq!(status, Status::NoError.into());
        assert_eq!(slot, 0);
        let (status, _buffer) = producer.request_buffer(slot);
        assert_eq!(status, Status::NoError);

        let (status, _) = producer.queue_buffer(slot, &QueueBufferInput::default());
        assert_eq!(status, Status::NoError);
        assert!(core.mutex.lock().unwrap().buffer_has_been_queued);
    }

    #[test]
    fn queue_buffer_rejects_slot_without_request_buffer() {
        let core = BufferQueueCore::new();
        install_test_consumer(&core);
        let producer = BufferQueueProducer::new(test_service_context(), core.clone(), test_nvmap());
        let buffer = Arc::new(NvGraphicBuffer::new(16, 16, PixelFormat::Rgba8888, 0));
        assert_eq!(
            producer.set_preallocated_buffer(0, Some(buffer)),
            Status::NoError
        );

        let (status, slot, _fence) =
            producer.dequeue_buffer(false, 16, 16, PixelFormat::Rgba8888, 0);
        assert_eq!(status, Status::NoError.into());

        let (status, _) = producer.queue_buffer(slot, &QueueBufferInput::default());
        assert_eq!(status, Status::BadValue);
    }

    #[test]
    fn detach_buffer_requires_request_buffer_and_frees_requested_slot() {
        let core = BufferQueueCore::new();
        install_test_consumer(&core);
        let producer = BufferQueueProducer::new(test_service_context(), core.clone(), test_nvmap());
        let buffer = Arc::new(NvGraphicBuffer::new(16, 16, PixelFormat::Rgba8888, 0));
        assert_eq!(
            producer.set_preallocated_buffer(0, Some(buffer)),
            Status::NoError
        );

        let (status, slot, _fence) =
            producer.dequeue_buffer(false, 16, 16, PixelFormat::Rgba8888, 0);
        assert_eq!(status, Status::NoError.into());
        assert_eq!(producer.detach_buffer(slot), Status::BadValue);

        let (status, _buffer) = producer.request_buffer(slot);
        assert_eq!(status, Status::NoError);
        assert_eq!(producer.detach_buffer(slot), Status::NoError);

        let inner = core.mutex.lock().unwrap();
        assert_eq!(
            inner.slots[slot as usize].buffer_state,
            super::super::buffer_slot::BufferState::Free
        );
        assert!(inner.slots[slot as usize].graphic_buffer.is_none());
    }

    #[test]
    fn detach_next_buffer_returns_oldest_free_graphic_buffer() {
        let core = BufferQueueCore::new();
        let producer = BufferQueueProducer::new(test_service_context(), core.clone(), test_nvmap());
        {
            let mut inner = core.mutex.lock().unwrap();
            inner.slots[0].graphic_buffer = Some(Arc::new(GraphicBuffer::new(
                16,
                16,
                PixelFormat::Rgba8888,
                0,
            )));
            inner.slots[0].frame_number = 10;
            inner.slots[1].graphic_buffer = Some(Arc::new(GraphicBuffer::new(
                32,
                32,
                PixelFormat::Rgba8888,
                0,
            )));
            inner.slots[1].frame_number = 5;
        }

        let (status, buffer, _fence) = producer.detach_next_buffer();
        assert_eq!(status, Status::NoError);
        assert_eq!(buffer.unwrap().get_width(), 32);
        assert!(core.mutex.lock().unwrap().slots[1].graphic_buffer.is_none());
    }

    #[test]
    fn attach_buffer_places_buffer_in_dequeued_requested_slot() {
        let core = BufferQueueCore::new();
        let producer = BufferQueueProducer::new(test_service_context(), core.clone(), test_nvmap());
        core.mutex.lock().unwrap().override_max_buffer_count = 1;

        let (status, slot) = producer.attach_buffer(Some(Arc::new(GraphicBuffer::new(
            64,
            32,
            PixelFormat::Rgba8888,
            0,
        ))));
        assert_eq!(status, StatusCode::NO_ERROR);
        assert_eq!(slot, 0);

        let inner = core.mutex.lock().unwrap();
        assert_eq!(
            inner.slots[0].buffer_state,
            super::super::buffer_slot::BufferState::Dequeued
        );
        assert!(inner.slots[0].request_buffer_called);
        assert_eq!(
            inner.slots[0].graphic_buffer.as_ref().unwrap().get_width(),
            64
        );
    }

    #[test]
    fn queue_buffer_rejects_crop_outside_buffer_bounds() {
        let core = BufferQueueCore::new();
        install_test_consumer(&core);
        let producer = BufferQueueProducer::new(test_service_context(), core.clone(), test_nvmap());
        let buffer = Arc::new(NvGraphicBuffer::new(16, 16, PixelFormat::Rgba8888, 0));
        assert_eq!(
            producer.set_preallocated_buffer(0, Some(buffer)),
            Status::NoError
        );

        let (status, slot, _fence) =
            producer.dequeue_buffer(false, 16, 16, PixelFormat::Rgba8888, 0);
        assert_eq!(status, Status::NoError.into());
        let (status, _buffer) = producer.request_buffer(slot);
        assert_eq!(status, Status::NoError);

        let mut input = QueueBufferInput::default();
        input.crop = Rectangle::new(0, 0, 32, 32);
        let (status, _) = producer.queue_buffer(slot, &input);
        assert_eq!(status, Status::BadValue);
    }

    #[test]
    fn queue_buffer_accepts_empty_default_crop() {
        let core = BufferQueueCore::new();
        install_test_consumer(&core);
        let producer = BufferQueueProducer::new(test_service_context(), core.clone(), test_nvmap());
        let buffer = Arc::new(NvGraphicBuffer::new(16, 16, PixelFormat::Rgba8888, 0));
        assert_eq!(
            producer.set_preallocated_buffer(0, Some(buffer)),
            Status::NoError
        );

        let (status, slot, _fence) =
            producer.dequeue_buffer(false, 16, 16, PixelFormat::Rgba8888, 0);
        assert_eq!(status, Status::NoError.into());
        let (status, _buffer) = producer.request_buffer(slot);
        assert_eq!(status, Status::NoError);

        let (status, _) = producer.queue_buffer(slot, &QueueBufferInput::default());
        assert_eq!(status, Status::NoError);
    }

    #[test]
    fn queue_buffer_records_enabled_history_with_host_presentation_time() {
        let previous = enable_buffer_history_for_test();
        let core = BufferQueueCore::new();
        install_test_consumer(&core);
        let producer = BufferQueueProducer::new(test_service_context(), core.clone(), test_nvmap());
        let buffer = Arc::new(NvGraphicBuffer::new(16, 16, PixelFormat::Rgba8888, 0));
        assert_eq!(
            producer.set_preallocated_buffer(0, Some(buffer)),
            Status::NoError
        );
        let (status, slot, _) = producer.dequeue_buffer(false, 16, 16, PixelFormat::Rgba8888, 0);
        assert_eq!(status, Status::NoError.into());
        assert_eq!(producer.request_buffer(slot).0, Status::NoError);

        let mut input = QueueBufferInput::default();
        input.timestamp = 123_456;
        assert_eq!(producer.queue_buffer(slot, &input).0, Status::NoError);

        let history = core.buffer_history.lock().unwrap();
        let info = history.map.get(&1).expect("queued frame history");
        assert_eq!(info.queue_time, 123_456);
        assert!(info.presentation_time > 0);
        assert_eq!(info.state, super::super::buffer_slot::BufferState::Queued);
        drop(history);
        restore_buffer_history_after_test(previous);
    }

    #[test]
    fn get_buffer_history_transaction_returns_newest_first_with_exact_payload() {
        let previous = enable_buffer_history_for_test();
        let core = BufferQueueCore::new();
        core.push_history(1, 10, 100, super::super::buffer_slot::BufferState::Queued);
        core.push_history(2, 20, 200, super::super::buffer_slot::BufferState::Free);
        core.push_history(3, 30, 300, super::super::buffer_slot::BufferState::Acquired);
        let producer = BufferQueueProducer::new(test_service_context(), core, test_nvmap());
        let parcel = get_buffer_history_request(2);
        let mut reply = [0u8; 256];

        IBinder::transact(&producer, 17, &parcel, &mut reply, 0);

        let data_offset = u32::from_ne_bytes(reply[4..8].try_into().unwrap()) as usize;
        assert_eq!(read_i32(&reply, data_offset), Status::NoError as i32);
        assert_eq!(read_i32(&reply, data_offset + 4), 2);
        assert_eq!(read_u64(&reply, data_offset + 8), 3);
        assert_eq!(read_u64(&reply, data_offset + 40), 2);
        assert_eq!(&reply[data_offset + 36..data_offset + 40], &[0; 4]);
        assert_eq!(&reply[data_offset + 68..data_offset + 72], &[0; 4]);
        assert_eq!(read_i32(&reply, data_offset + 72), Status::NoError as i32);
        restore_buffer_history_after_test(previous);
    }

    #[test]
    fn queue_buffer_preserves_unnamed_rotation_bits() {
        let core = BufferQueueCore::new();
        install_test_consumer(&core);
        let producer = BufferQueueProducer::new(test_service_context(), core.clone(), test_nvmap());
        let buffer = Arc::new(NvGraphicBuffer::new(16, 16, PixelFormat::Rgba8888, 0));
        assert_eq!(
            producer.set_preallocated_buffer(0, Some(buffer)),
            Status::NoError
        );

        let (status, slot, _fence) =
            producer.dequeue_buffer(false, 16, 16, PixelFormat::Rgba8888, 0);
        assert_eq!(status, Status::NoError.into());
        let (status, _buffer) = producer.request_buffer(slot);
        assert_eq!(status, Status::NoError);

        let mut input = QueueBufferInput::default();
        input.transform = NativeWindowTransform::from_bits_retain(0x0B);
        let (status, _) = producer.queue_buffer(slot, &input);
        assert_eq!(status, Status::NoError);
        assert_eq!(core.mutex.lock().unwrap().queue[0].transform.bits(), 0x03);
    }

    #[test]
    fn query_reports_sticky_transform_and_consumer_running_behind() {
        let core = BufferQueueCore::new();
        let producer = BufferQueueProducer::new(test_service_context(), core.clone(), test_nvmap());
        *producer.sticky_transform.lock().unwrap() = NativeWindowTransform::INVERSE_DISPLAY.bits();
        {
            let mut inner = core.mutex.lock().unwrap();
            inner.queue.push(BufferItem::default());
            inner.queue.push(BufferItem::default());
        }

        let (status, sticky_transform) = producer.query(NativeWindow::StickyTransform);
        assert_eq!(status, Status::NoError);
        assert_eq!(
            sticky_transform,
            NativeWindowTransform::INVERSE_DISPLAY.bits() as i32
        );

        let (status, running_behind) = producer.query(NativeWindow::ConsumerRunningBehind);
        assert_eq!(status, Status::NoError);
        assert_eq!(running_behind, 1);
    }

    #[test]
    fn request_buffer_rejects_slot_not_owned_by_producer() {
        let core = BufferQueueCore::new();
        let producer = BufferQueueProducer::new(test_service_context(), core.clone(), test_nvmap());
        core.mutex.lock().unwrap().slots[0].graphic_buffer = Some(Arc::new(GraphicBuffer::new(
            16,
            16,
            PixelFormat::Rgba8888,
            0,
        )));

        let (status, buffer) = producer.request_buffer(0);
        assert_eq!(status, Status::BadValue);
        assert!(buffer.is_none());
    }

    #[test]
    fn dequeue_buffer_requires_explicit_buffer_count_for_second_dequeue() {
        let core = BufferQueueCore::new();
        let producer = BufferQueueProducer::new(test_service_context(), core.clone(), test_nvmap());
        assert_eq!(
            producer.set_preallocated_buffer(
                0,
                Some(Arc::new(NvGraphicBuffer::new(
                    16,
                    16,
                    PixelFormat::Rgba8888,
                    0
                )))
            ),
            Status::NoError
        );
        assert_eq!(
            producer.set_preallocated_buffer(
                1,
                Some(Arc::new(NvGraphicBuffer::new(
                    16,
                    16,
                    PixelFormat::Rgba8888,
                    0
                )))
            ),
            Status::NoError
        );

        let (status, slot, _) = producer.dequeue_buffer(false, 16, 16, PixelFormat::Rgba8888, 0);
        assert_eq!(status, Status::NoError.into());
        assert_eq!(slot, 0);

        let (status, slot, _) = producer.dequeue_buffer(false, 16, 16, PixelFormat::Rgba8888, 0);
        assert_eq!(status, Status::InvalidOperation.into());
        assert_eq!(slot, -1);
    }

    #[test]
    fn dequeue_buffer_sets_reallocation_flag_for_empty_slot() {
        let core = BufferQueueCore::new();
        let producer = BufferQueueProducer::new(test_service_context(), core.clone(), test_nvmap());
        core.mutex.lock().unwrap().override_max_buffer_count = 1;

        let (status, slot, _) = producer.dequeue_buffer(false, 32, 32, PixelFormat::Rgba8888, 0);
        assert_eq!(status, StatusCode::BUFFER_NEEDS_REALLOCATION);
        assert_eq!(slot, 0);

        let inner = core.mutex.lock().unwrap();
        assert!(inner.slots[0].graphic_buffer.is_some());
        assert_eq!(
            inner.slots[0].buffer_state,
            super::super::buffer_slot::BufferState::Dequeued
        );
        assert!(!inner.slots[0].request_buffer_called);
    }

    #[test]
    fn unimplemented_transact_panics_instead_of_returning_silent_status() {
        let core = BufferQueueCore::new();
        let producer = BufferQueueProducer::new(test_service_context(), core, test_nvmap());
        let mut parcel = Vec::new();
        parcel.extend_from_slice(&8u32.to_ne_bytes());
        parcel.extend_from_slice(&16u32.to_ne_bytes());
        parcel.extend_from_slice(&0u32.to_ne_bytes());
        parcel.extend_from_slice(&24u32.to_ne_bytes());
        parcel.extend_from_slice(&0u32.to_ne_bytes());
        parcel.extend_from_slice(&0u32.to_ne_bytes());
        parcel.extend_from_slice(&0u16.to_ne_bytes());
        parcel.extend_from_slice(&0u16.to_ne_bytes());
        let mut reply = [0u8; 64];

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            IBinder::transact(&producer, 6, &parcel, &mut reply, 0);
        }));

        assert_eq!(
            panic_message(result.unwrap_err()),
            "BufferQueueProducer::transact unimplemented transaction 6 (AttachBuffer)"
        );
    }

    #[test]
    fn connect_listener_transact_panics_like_upstream_unimplemented_if() {
        let core = BufferQueueCore::new();
        install_test_consumer(&core);
        let producer = BufferQueueProducer::new(test_service_context(), core, test_nvmap());
        let mut parcel = Vec::new();
        parcel.extend_from_slice(&20u32.to_ne_bytes());
        parcel.extend_from_slice(&16u32.to_ne_bytes());
        parcel.extend_from_slice(&0u32.to_ne_bytes());
        parcel.extend_from_slice(&36u32.to_ne_bytes());
        parcel.extend_from_slice(&0u32.to_ne_bytes());
        parcel.extend_from_slice(&0u32.to_ne_bytes());
        parcel.extend_from_slice(&0u16.to_ne_bytes());
        parcel.extend_from_slice(&0u16.to_ne_bytes());
        parcel.extend_from_slice(&1u8.to_ne_bytes());
        parcel.extend_from_slice(&[0u8; 3]);
        parcel.extend_from_slice(&(NativeWindowApi::Egl as i32).to_ne_bytes());
        parcel.extend_from_slice(&0u8.to_ne_bytes());
        parcel.extend_from_slice(&[0u8; 3]);
        let mut reply = [0u8; 64];

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            IBinder::transact(&producer, 10, &parcel, &mut reply, 0);
        }));

        assert_eq!(
            panic_message(result.unwrap_err()),
            "BufferQueueProducer::transact Connect listener is unimplemented"
        );
    }
}

// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-FileCopyrightText: Copyright 2014 The Android Open Source Project
// SPDX-License-Identifier: GPL-3.0-or-later
// Parts of this implementation were based on:
// https://cs.android.com/android/platform/superproject/+/android-5.1.1_r38:frameworks/native/include/gui/BufferQueueConsumer.h
// https://cs.android.com/android/platform/superproject/+/android-5.1.1_r38:frameworks/native/libs/gui/BufferQueueConsumer.cpp

//! Port of zuyu/src/core/hle/service/nvnflinger/buffer_queue_consumer.h
//! Port of zuyu/src/core/hle/service/nvnflinger/buffer_queue_consumer.cpp

use std::sync::Arc;

use crate::hle::kernel::k_readable_event::KReadableEvent;

use super::binder::IBinder;
use super::buffer_item::BufferItem;
use super::buffer_queue_core::BufferQueueCore;
use super::buffer_queue_defs::NUM_BUFFER_SLOTS;
use super::buffer_slot::BufferState;
use super::consumer_listener::IConsumerListener;
use super::parcel::{InputParcel, OutputParcel};
use super::status::Status;
use super::ui::fence::Fence;

pub struct BufferQueueConsumer {
    core: Arc<BufferQueueCore>,
}

fn stop_unimplemented_transact(code: u32, flags: u32, name: &str) -> ! {
    panic!(
        "BufferQueueConsumer::transact unimplemented transaction {} ({}) flags={}",
        code, name, flags
    );
}

impl BufferQueueConsumer {
    pub fn new(core: Arc<BufferQueueCore>) -> Self {
        Self { core }
    }

    pub fn acquire_buffer(&self, out_buffer: &mut BufferItem, expected_present_ns: i64) -> Status {
        let mut inner = self.core.mutex.lock().unwrap();

        // Check that the consumer doesn't currently have the maximum number of buffers acquired.
        let num_acquired: i32 = inner
            .slots
            .iter()
            .filter(|s| s.buffer_state == BufferState::Acquired)
            .count() as i32;

        if num_acquired >= inner.max_acquired_buffer_count + 1 {
            log::error!(
                "BufferQueueConsumer: max acquired buffer count reached: {} (max {})",
                num_acquired,
                inner.max_acquired_buffer_count
            );
            return Status::InvalidOperation;
        }

        // Check if the queue is empty.
        if inner.queue.is_empty() {
            return Status::NoBufferAvailable;
        }

        // If expected_present is specified, we may not want to return a buffer yet.
        if expected_present_ns != 0 {
            const MAX_REASONABLE_NSEC: i64 = 1_000_000_000; // 1 second

            // Drop old buffers that are past their time
            while inner.queue.len() > 1 && !inner.queue[0].is_auto_timestamp {
                let desired_present = inner.queue[1].timestamp;
                if desired_present < expected_present_ns - MAX_REASONABLE_NSEC
                    || desired_present > expected_present_ns
                {
                    log::debug!(
                        "BufferQueueConsumer: nodrop desire={} expect={}",
                        desired_present,
                        expected_present_ns
                    );
                    break;
                }

                log::debug!(
                    "BufferQueueConsumer: drop desire={} expect={} size={}",
                    desired_present,
                    expected_present_ns,
                    inner.queue.len()
                );

                if inner.still_tracking(&inner.queue[0]) {
                    let slot = inner.queue[0].slot as usize;
                    inner.slots[slot].buffer_state = BufferState::Free;
                }
                inner.queue.remove(0);
            }

            // See if the front buffer is ready to be acquired.
            let desired_present = inner.queue[0].timestamp;
            if desired_present > expected_present_ns
                && desired_present < expected_present_ns + MAX_REASONABLE_NSEC
            {
                log::debug!(
                    "BufferQueueConsumer: defer desire={} expect={}",
                    desired_present,
                    expected_present_ns
                );
                return Status::PresentLater;
            }

            log::debug!(
                "BufferQueueConsumer: accept desire={} expect={}",
                desired_present,
                expected_present_ns
            );
        }

        let front = inner.queue.remove(0);
        let slot = front.slot as usize;

        *out_buffer = BufferItem {
            graphic_buffer: front.graphic_buffer.clone(),
            fence: front.fence,
            crop: front.crop,
            transform: front.transform,
            scaling_mode: front.scaling_mode,
            timestamp: front.timestamp,
            is_auto_timestamp: front.is_auto_timestamp,
            frame_number: front.frame_number,
            slot: front.slot,
            is_droppable: front.is_droppable,
            acquire_called: front.acquire_called,
            transform_to_display_inverse: front.transform_to_display_inverse,
            swap_interval: front.swap_interval,
        };

        log::debug!("BufferQueueConsumer: acquiring slot={}", slot);

        // If the front buffer is still being tracked, update its slot state
        if inner.still_tracking(&front) {
            inner.slots[slot].acquire_called = true;
            inner.slots[slot].needs_cleanup_on_release = false;
            inner.slots[slot].buffer_state = BufferState::Acquired;
        }

        // If the buffer has previously been acquired by the consumer, set graphic_buffer to None
        // to avoid unnecessarily remapping this buffer on the consumer side.
        if out_buffer.acquire_called {
            out_buffer.graphic_buffer = None;
        }

        // Signal that a slot might be free. Upstream calls SignalDequeueCondition
        // while core->mutex is still held in AcquireBuffer.
        self.core.signal_dequeue_condition();
        drop(inner);

        Status::NoError
    }

    pub fn release_buffer(&self, slot: i32, frame_number: u64, _release_fence: &Fence) -> Status {
        if slot < 0 || slot >= NUM_BUFFER_SLOTS as i32 {
            log::error!("BufferQueueConsumer: slot {} out of range", slot);
            return Status::BadValue;
        }

        let listener;
        {
            let mut inner = self.core.mutex.lock().unwrap();

            // If the frame number has changed because the buffer has been reallocated,
            // we can ignore this ReleaseBuffer for the old buffer.
            if frame_number != inner.slots[slot as usize].frame_number {
                return Status::StaleBufferSlot;
            }

            // Make sure this buffer hasn't been queued while acquired by the consumer.
            for item in &inner.queue {
                if item.slot == slot {
                    log::error!(
                        "BufferQueueConsumer: buffer slot {} pending release is currently queued",
                        slot
                    );
                    return Status::BadValue;
                }
            }

            if inner.slots[slot as usize].buffer_state == BufferState::Acquired {
                // TODO: for now, avoid resetting the fence, so that when we next return this
                // slot to the producer, it can wait for its own fence to pass. We should fix this
                // by properly waiting for the fence in the BufferItemConsumer.
                // inner.slots[slot as usize].fence = *_release_fence;
                inner.slots[slot as usize].buffer_state = BufferState::Free;
                listener = inner.connected_producer_listener.clone();
                log::debug!("BufferQueueConsumer: releasing slot {}", slot);
            } else if inner.slots[slot as usize].needs_cleanup_on_release {
                log::debug!(
                    "BufferQueueConsumer: releasing a stale buffer slot {} (state = {:?})",
                    slot,
                    inner.slots[slot as usize].buffer_state
                );
                inner.slots[slot as usize].needs_cleanup_on_release = false;
                return Status::StaleBufferSlot;
            } else {
                log::error!(
                    "BufferQueueConsumer: attempted to release buffer slot {} but its state was {:?}",
                    slot,
                    inner.slots[slot as usize].buffer_state
                );
                return Status::BadValue;
            }

            self.core.signal_dequeue_condition();
        }

        // Call back without lock held
        if let Some(ref l) = listener {
            l.on_buffer_released();
        }

        Status::NoError
    }

    pub fn connect(
        &self,
        consumer_listener: Arc<dyn IConsumerListener>,
        controlled_by_app: bool,
    ) -> Status {
        let mut inner = self.core.mutex.lock().unwrap();
        if inner.is_abandoned {
            log::error!("BufferQueueConsumer: BufferQueue has been abandoned");
            return Status::NoInit;
        }
        inner.consumer_listener = Some(consumer_listener);
        inner.consumer_controlled_by_app = controlled_by_app;
        Status::NoError
    }

    pub fn disconnect(&self) -> Status {
        log::debug!("BufferQueueConsumer::disconnect called");
        let mut inner = self.core.mutex.lock().unwrap();

        if inner.consumer_listener.is_none() {
            log::error!("BufferQueueConsumer: no consumer is connected");
            return Status::BadValue;
        }

        inner.is_abandoned = true;
        inner.consumer_listener = None;
        inner.queue.clear();
        inner.free_all_buffers_locked();
        self.core.signal_dequeue_condition();
        drop(inner);
        Status::NoError
    }

    pub fn get_released_buffers(&self, out_slot_mask: &mut u64) {
        let inner = self.core.mutex.lock().unwrap();

        if inner.is_abandoned {
            log::error!("BufferQueueConsumer: BufferQueue has been abandoned");
            *out_slot_mask = 0;
            return;
        }

        let mut mask: u64 = 0;
        for s in 0..NUM_BUFFER_SLOTS {
            if !inner.slots[s].acquire_called {
                mask |= 1u64 << s;
            }
        }

        // Remove from the mask queued buffers for which acquire has been called
        for item in &inner.queue {
            if item.acquire_called {
                mask &= !(1u64 << item.slot);
            }
        }

        log::debug!("BufferQueueConsumer: returning mask {}", mask);
        *out_slot_mask = mask;
    }
}

impl IBinder for BufferQueueConsumer {
    fn transact(&self, code: u32, parcel_data: &[u8], parcel_reply: &mut [u8], flags: u32) {
        #[repr(u32)]
        enum TransactionId {
            AcquireBuffer = 1,
            DetachBuffer = 2,
            AttachBuffer = 3,
            ReleaseBuffer = 4,
            ConsumerConnect = 5,
            ConsumerDisconnect = 6,
            GetReleasedBuffers = 7,
            SetDefaultBufferSize = 8,
            SetDefaultMaxBufferCount = 9,
            DisableAsyncBuffer = 10,
            SetMaxAcquiredBufferCount = 11,
            SetConsumerName = 12,
            SetDefaultBufferFormat = 13,
            SetConsumerUsageBits = 14,
            SetTransformHint = 15,
            GetSidebandStream = 16,
            Unknown18 = 18,
            Unknown20 = 20,
        }

        let mut status = Status::NoError;
        let mut parcel_in = InputParcel::new(parcel_data);
        let mut parcel_out = OutputParcel::new();

        match code {
            x if x == TransactionId::AcquireBuffer as u32 => {
                let mut item = BufferItem::default();
                let present_when = parcel_in.read::<i64>();
                status = self.acquire_buffer(&mut item, present_when);
                let _ = status;
                stop_unimplemented_transact(code, flags, "AcquireBuffer");
            }
            x if x == TransactionId::ReleaseBuffer as u32 => {
                let slot = parcel_in.read::<i32>();
                let frame_number = parcel_in.read::<u64>();
                let release_fence = parcel_in.read_flattened::<Fence>();
                status = self.release_buffer(slot, frame_number, &release_fence);
            }
            x if x == TransactionId::GetReleasedBuffers as u32 => {
                let mut slot_mask = 0u64;
                self.get_released_buffers(&mut slot_mask);
                parcel_out.write(&slot_mask);
            }
            x if x == TransactionId::DetachBuffer as u32
                || x == TransactionId::AttachBuffer as u32
                || x == TransactionId::ConsumerConnect as u32
                || x == TransactionId::ConsumerDisconnect as u32
                || x == TransactionId::SetDefaultBufferSize as u32
                || x == TransactionId::SetDefaultMaxBufferCount as u32
                || x == TransactionId::DisableAsyncBuffer as u32
                || x == TransactionId::SetMaxAcquiredBufferCount as u32
                || x == TransactionId::SetConsumerName as u32
                || x == TransactionId::SetDefaultBufferFormat as u32
                || x == TransactionId::SetConsumerUsageBits as u32
                || x == TransactionId::SetTransformHint as u32
                || x == TransactionId::GetSidebandStream as u32
                || x == TransactionId::Unknown18 as u32
                || x == TransactionId::Unknown20 as u32 =>
            {
                let _ = status;
                stop_unimplemented_transact(code, flags, "Unsupported");
            }
            _ => {
                let _ = status;
                stop_unimplemented_transact(code, flags, "Unknown");
            }
        }

        parcel_out.write(&status);
        let serialized = parcel_out.serialize();
        let copy_len = std::cmp::min(parcel_reply.len(), serialized.len());
        parcel_reply[..copy_len].copy_from_slice(&serialized[..copy_len]);
    }

    fn get_native_handle(&self, type_id: u32) -> Option<Arc<std::sync::Mutex<KReadableEvent>>> {
        panic!(
            "BufferQueueConsumer::get_native_handle called type_id={}",
            type_id
        );
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::service::nvdrv::nvdata::NvFence;

    fn consumer() -> BufferQueueConsumer {
        BufferQueueConsumer::new(BufferQueueCore::new())
    }

    fn parcel(payload: &[u8]) -> Vec<u8> {
        let data_offset = 16u32;
        let mut data = Vec::new();
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.resize(12, 0);
        data.extend_from_slice(payload);

        let mut out = Vec::new();
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data_offset.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(data_offset + data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    #[test]
    fn release_buffer_keeps_the_acquire_fence_like_upstream() {
        let core = BufferQueueCore::new();
        {
            let mut inner = core.mutex.lock().unwrap();
            inner.slots[0].buffer_state = BufferState::Acquired;
            inner.slots[0].frame_number = 7;
            inner.slots[0].fence = Fence {
                num_fences: 1,
                fences: [NvFence { id: 3, value: 11 }; 4],
            };
        }
        let consumer = BufferQueueConsumer::new(Arc::clone(&core));
        let release_fence = Fence {
            num_fences: 1,
            fences: [NvFence { id: 9, value: 27 }; 4],
        };

        assert_eq!(
            consumer.release_buffer(0, 7, &release_fence),
            Status::NoError
        );

        let inner = core.mutex.lock().unwrap();
        assert_eq!(inner.slots[0].buffer_state, BufferState::Free);
        assert_eq!(inner.slots[0].fence.num_fences, 1);
        assert_eq!(inner.slots[0].fence.fences[0].id, 3);
        assert_eq!(inner.slots[0].fence.fences[0].value, 11);
    }

    #[test]
    #[should_panic(expected = "BufferQueueConsumer::transact unimplemented transaction 1")]
    fn acquire_buffer_transact_stops_after_unimplemented_flattening_like_upstream() {
        let consumer = consumer();
        let present_when = parcel(&0i64.to_le_bytes());
        let mut reply = [0u8; 64];

        consumer.transact(1, &present_when, &mut reply, 0);
    }

    #[test]
    #[should_panic(expected = "BufferQueueConsumer::transact unimplemented transaction 5")]
    fn unsupported_consumer_transact_stops_like_upstream_assert() {
        let consumer = consumer();
        let input = parcel(&[]);
        let mut reply = [0u8; 64];

        consumer.transact(5, &input, &mut reply, 0);
    }

    #[test]
    #[should_panic(expected = "BufferQueueConsumer::transact unimplemented transaction 99")]
    fn unknown_consumer_transact_stops_like_upstream_assert() {
        let consumer = consumer();
        let input = parcel(&[]);
        let mut reply = [0u8; 64];

        consumer.transact(99, &input, &mut reply, 0);
    }
}

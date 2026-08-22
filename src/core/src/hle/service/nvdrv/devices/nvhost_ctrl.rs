// SPDX-FileCopyrightText: 2021 yuzu Emulator Project
// SPDX-FileCopyrightText: 2021 Skyline Team and Contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/core/hle/service/nvdrv/devices/nvhost_ctrl.h
//! Port of zuyu/src/core/hle/service/nvdrv/devices/nvhost_ctrl.cpp

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::hle::kernel::k_readable_event::KReadableEvent;
use crate::hle::service::nvdrv::core::container::SessionId;
use crate::hle::service::nvdrv::core::syncpoint_manager::SyncpointManager;
use crate::hle::service::nvdrv::devices::nvdevice::NvDevice;
use crate::hle::service::nvdrv::devices::nvmap::{read_struct, write_struct};
use crate::hle::service::nvdrv::nvdata::*;
use crate::hle::service::nvdrv::nvdrv::EventInterface;

/// Union for SyncpointEventValue bit fields.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SyncpointEventValue {
    pub raw: u32,
}

impl SyncpointEventValue {
    pub fn partial_slot(&self) -> u32 {
        self.raw & 0xF
    }

    pub fn syncpoint_id(&self) -> u32 {
        (self.raw >> 4) & 0x0FFFFFFF
    }

    pub fn slot(&self) -> u16 {
        self.raw as u16
    }

    pub fn syncpoint_id_for_allocation(&self) -> u16 {
        ((self.raw >> 16) & 0x0FFF) as u16
    }

    pub fn event_allocated(&self) -> bool {
        ((self.raw >> 28) & 1) != 0
    }
}

struct InternalEvent {
    readable_event: Option<Arc<Mutex<KReadableEvent>>>,
    // Shared with the syncpoint-manager wait callback. Upstream's callback
    // captures `this` and touches `events[slot].status` directly without
    // taking NvEventsLock; the Rust port keeps the table behind a Mutex, so
    // the callback must hold its own reference to the per-slot atomic to stay
    // lock-free (RegisterHostAction fires the callback synchronously on the
    // caller's thread when the fence is already signalled — taking the events
    // mutex there self-deadlocks, as the caller holds it).
    status: Arc<AtomicU32>,
    fails: u32,
    assigned_syncpt: u32,
    assigned_value: u32,
    registered: bool,
    wait_handle: Option<u64>,
}

impl Default for InternalEvent {
    fn default() -> Self {
        Self {
            readable_event: None,
            status: Arc::new(AtomicU32::new(EventState::Available as u32)),
            fails: 0,
            assigned_syncpt: 0,
            assigned_value: 0,
            registered: false,
            wait_handle: None,
        }
    }
}

impl InternalEvent {
    fn is_being_used(&self) -> bool {
        let current_status = self.status.load(Ordering::Acquire);
        current_status == EventState::Waiting as u32
            || current_status == EventState::Cancelling as u32
            || current_status == EventState::Signalling as u32
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IocSyncptReadParams {
    pub id: u32,
    pub value: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IocSyncptIncrParams {
    pub id: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IocSyncptWaitParams {
    pub id: u32,
    pub thresh: u32,
    pub timeout: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IocCtrlEventClearParams {
    pub event_id: SyncpointEventValue,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IocCtrlEventWaitParams {
    pub fence: NvFence,
    pub timeout: u32,
    pub value: SyncpointEventValue,
}
const _: () = assert!(std::mem::size_of::<IocCtrlEventWaitParams>() == 16);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IocCtrlEventRegisterParams {
    pub user_event_id: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IocCtrlEventUnregisterParams {
    pub user_event_id: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IocCtrlEventUnregisterBatchParams {
    pub user_events: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct IocGetConfigParams {
    pub domain_str: [u8; 0x41],
    pub param_str: [u8; 0x41],
    pub config_str: [u8; 0x101],
}
const _: () = assert!(std::mem::size_of::<IocGetConfigParams>() == 387);

impl Default for IocGetConfigParams {
    fn default() -> Self {
        Self {
            domain_str: [0u8; 0x41],
            param_str: [0u8; 0x41],
            config_str: [0u8; 0x101],
        }
    }
}

/// nvhost_ctrl device.
pub struct NvHostCtrl {
    events_interface: Arc<EventInterface>,
    syncpoint_manager: *const SyncpointManager,
    events: Arc<Mutex<Vec<InternalEvent>>>,
    events_mask: Mutex<u64>,
}

// Safety: NvHostCtrl is only accessed from service thread.
unsafe impl Send for NvHostCtrl {}
unsafe impl Sync for NvHostCtrl {}

impl NvHostCtrl {
    fn should_trace_event_wait() -> bool {
        std::env::var_os("RUZU_TRACE_NVHOST_CTRL_WAIT").is_some()
    }

    fn should_stderr_event_wait(value: u32) -> bool {
        std::env::var("RUZU_TRACE_SYNCPOINT_AFTER")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .map(|threshold| value >= threshold)
            .unwrap_or(false)
    }

    fn trace_ioctl(command: Ioctl, stage: &str) {
        if Self::should_trace_event_wait() {
            log::info!(
                "nvhost_ctrl::ioctl stage={} raw=0x{:08X} group=0x{:X} cmd=0x{:X}",
                stage,
                command.raw,
                command.group(),
                command.cmd()
            );
        }
    }

    fn trace_event_wait_record(
        stage: u64,
        params: &IocCtrlEventWaitParams,
        is_allocation: bool,
        slot: u32,
        result: NvResult,
        min_value: u32,
        target: u32,
    ) {
        if !common::trace::is_enabled(common::trace::cat::NVHOST_CTRL_WAIT) {
            return;
        }

        let result_id = match result {
            NvResult::Success => 0,
            NvResult::Timeout => 1,
            NvResult::BadParameter => 2,
            NvResult::Busy => 3,
            _ => 0xFFFF,
        };
        common::trace::emit_raw(
            common::trace::cat::NVHOST_CTRL_WAIT,
            &[
                stage,
                params.fence.id as u32 as u64,
                params.fence.value as u64,
                params.timeout as u64,
                u64::from(is_allocation),
                slot as u64,
                params.value.raw as u64,
                result_id,
                min_value as u64,
                target as u64,
            ],
        );
    }

    fn trace_event_record(
        stage: u64,
        slot: u32,
        value_raw: u32,
        syncpoint_id: u32,
        assigned_value: u32,
        status: u32,
        object_id: u64,
    ) {
        if !common::trace::is_enabled(common::trace::cat::NVHOST_CTRL_WAIT) {
            return;
        }

        common::trace::emit_raw(
            common::trace::cat::NVHOST_CTRL_WAIT,
            &[
                stage,
                syncpoint_id as u64,
                assigned_value as u64,
                0,
                0,
                slot as u64,
                value_raw as u64,
                status as u64,
                object_id,
                assigned_value as u64,
            ],
        );
    }

    fn bytes_to_cstr(bytes: &[u8]) -> String {
        let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..len]).into_owned()
    }

    pub fn new(
        events_interface: Arc<EventInterface>,
        syncpoint_manager: &SyncpointManager,
    ) -> Self {
        let mut events = Vec::with_capacity(MAX_NV_EVENTS as usize);
        for _ in 0..MAX_NV_EVENTS {
            events.push(InternalEvent::default());
        }
        Self {
            events_interface,
            syncpoint_manager: syncpoint_manager as *const _,
            events: Arc::new(Mutex::new(events)),
            events_mask: Mutex::new(0),
        }
    }

    fn syncpoint_manager(&self) -> &SyncpointManager {
        unsafe { &*self.syncpoint_manager }
    }

    fn wait_host_stalled(&self, fence_id: u32, target_value: u32) {
        let system_ptr = self.events_interface.system().get() as *const crate::core::System
            as *mut crate::core::System;
        // SAFETY: `SystemRef` is valid for the service lifetime. This mirrors
        // upstream `system.StallApplication(); WaitHost(...); system.UnstallApplication();`
        // from the matching owner file.
        let stall_guard = unsafe { (*system_ptr).stall_application() };
        self.syncpoint_manager().wait_host(fence_id, target_value);
        unsafe { (*system_ptr).unstall_application() };
        drop(stall_guard);
    }

    fn create_nv_event(&self, events: &mut [InternalEvent], mask: &mut u64, event_id: u32) {
        let event = &mut events[event_id as usize];
        event.readable_event = Some(
            self.events_interface
                .create_event(&format!("NVCTRL::NvEvent_{}", event_id)),
        );
        event
            .status
            .store(EventState::Available as u32, Ordering::Release);
        event.registered = true;
        event.fails = 0;
        event.assigned_syncpt = 0;
        event.assigned_value = 0;
        event.wait_handle = None;
        *mask |= 1u64 << event_id;
        let object_id = event
            .readable_event
            .as_ref()
            .map(|readable_event| readable_event.lock().unwrap().object_id)
            .unwrap_or(0);
        Self::trace_event_record(
            11,
            event_id,
            event_id,
            event.assigned_syncpt,
            event.assigned_value,
            event.status.load(Ordering::Acquire),
            object_id,
        );
    }

    fn free_nv_event(&self, events: &mut [InternalEvent], mask: &mut u64, event_id: u32) {
        let event = &mut events[event_id as usize];
        if let Some(readable_event) = event.readable_event.take() {
            self.events_interface.free_event(readable_event);
        }
        event
            .status
            .store(EventState::Available as u32, Ordering::Release);
        event.registered = false;
        event.assigned_syncpt = 0;
        event.assigned_value = 0;
        event.wait_handle = None;
        event.fails = 0;
        *mask &= !(1u64 << event_id);
    }

    fn free_event_locked(
        &self,
        events: &mut [InternalEvent],
        mask: &mut u64,
        slot: u32,
    ) -> NvResult {
        if slot >= MAX_NV_EVENTS {
            return NvResult::BadParameter;
        }

        let event = &events[slot as usize];
        if !event.registered {
            return NvResult::Success;
        }

        if event.is_being_used() {
            return NvResult::Busy;
        }

        self.free_nv_event(events, mask, slot);
        NvResult::Success
    }

    fn find_free_nv_event(
        &self,
        events: &mut [InternalEvent],
        mask: &mut u64,
        syncpoint_id: u32,
    ) -> u32 {
        let mut slot = MAX_NV_EVENTS;
        let mut free_slot = MAX_NV_EVENTS;

        for i in 0..MAX_NV_EVENTS {
            let event = &events[i as usize];
            if event.registered {
                if !event.is_being_used() {
                    slot = i;
                    if event.assigned_syncpt == syncpoint_id {
                        return slot;
                    }
                }
            } else if free_slot == MAX_NV_EVENTS {
                free_slot = i;
            }
        }

        if free_slot < MAX_NV_EVENTS {
            self.create_nv_event(events, mask, free_slot);
            return free_slot;
        }

        if slot < MAX_NV_EVENTS {
            return slot;
        }

        log::error!("Failed to allocate an NV event");
        0
    }

    pub fn nv_os_get_config_u32(&self, params: &mut IocGetConfigParams) -> NvResult {
        log::debug!(
            "nvhost_ctrl::NvOsGetConfigU32 called, domain='{}' param='{}' config='{}'",
            Self::bytes_to_cstr(&params.domain_str),
            Self::bytes_to_cstr(&params.param_str),
            Self::bytes_to_cstr(&params.config_str)
        );
        NvResult::ConfigVarNotFound
    }

    pub fn ioc_ctrl_event_wait(
        &self,
        params: &mut IocCtrlEventWaitParams,
        is_allocation: bool,
    ) -> NvResult {
        if Self::should_trace_event_wait() {
            log::info!(
                "nvhost_ctrl::IocCtrlEventWait begin syncpt_id={} threshold={} timeout={} is_allocation={} value_raw_in=0x{:08X}",
                params.fence.id,
                params.fence.value,
                params.timeout,
                is_allocation,
                params.value.raw
            );
        }
        Self::trace_event_wait_record(1, params, is_allocation, u32::MAX, NvResult::Timeout, 0, 0);
        log::debug!(
            "nvhost_ctrl::IocCtrlEventWait syncpt_id={}, threshold={}, timeout={}, is_allocation={}",
            params.fence.id,
            params.fence.value,
            params.timeout,
            is_allocation
        );
        if Self::should_stderr_event_wait(params.fence.value) {
            eprintln!(
                "[NVHOST_CTRL_WAIT] begin syncpt_id={} target={} timeout={} allocation={} value_raw=0x{:08X}",
                params.fence.id,
                params.fence.value,
                params.timeout,
                is_allocation,
                params.value.raw
            );
        }

        let fence_id = params.fence.id as u32;
        let existing_event_id = params.value.raw;
        let reset_non_allocation_fails = |this: &Self| {
            if !is_allocation && existing_event_id < MAX_NV_EVENTS {
                let mut events = this.events.lock().unwrap();
                events[existing_event_id as usize].fails = 0;
            }
        };

        if fence_id >= MAX_SYNC_POINTS {
            reset_non_allocation_fails(self);
            Self::trace_event_wait_record(
                9,
                params,
                is_allocation,
                u32::MAX,
                NvResult::BadParameter,
                0,
                params.fence.value,
            );
            return NvResult::BadParameter;
        }

        if params.fence.value == 0 {
            if !self
                .syncpoint_manager()
                .is_syncpoint_allocated(params.fence.id as u32)
            {
                log::warn!("Unallocated syncpt_id={}", params.fence.id);
            } else {
                params.value.raw = self.syncpoint_manager().read_syncpoint_min_value(fence_id);
            }
            if Self::should_trace_event_wait() {
                log::info!(
                    "nvhost_ctrl::IocCtrlEventWait zero-threshold syncpt_id={} value_raw_out=0x{:08X}",
                    fence_id,
                    params.value.raw
                );
            }
            Self::trace_event_wait_record(
                2,
                params,
                is_allocation,
                u32::MAX,
                NvResult::Success,
                params.value.raw,
                params.fence.value,
            );
            reset_non_allocation_fails(self);
            if Self::should_stderr_event_wait(params.fence.value) {
                eprintln!(
                    "[NVHOST_CTRL_WAIT] signalled-immediate syncpt_id={} target={} value_raw=0x{:08X}",
                    fence_id, params.fence.value, params.value.raw
                );
            }
            return NvResult::Success;
        }

        if self.syncpoint_manager().is_fence_signalled(&params.fence) {
            params.value.raw = self.syncpoint_manager().read_syncpoint_min_value(fence_id);
            if Self::should_trace_event_wait() {
                log::info!(
                    "nvhost_ctrl::IocCtrlEventWait signalled-immediate syncpt_id={} value_raw_out=0x{:08X}",
                    fence_id,
                    params.value.raw
                );
            }
            Self::trace_event_wait_record(
                3,
                params,
                is_allocation,
                u32::MAX,
                NvResult::Success,
                params.value.raw,
                params.fence.value,
            );
            reset_non_allocation_fails(self);
            if Self::should_stderr_event_wait(params.fence.value) {
                eprintln!(
                    "[NVHOST_CTRL_WAIT] signalled-after-update syncpt_id={} target={} value_raw=0x{:08X}",
                    fence_id, params.fence.value, params.value.raw
                );
            }
            return NvResult::Success;
        }

        let new_value = self.syncpoint_manager().update_min(fence_id);
        if self.syncpoint_manager().is_fence_signalled(&params.fence) {
            params.value.raw = new_value;
            if Self::should_trace_event_wait() {
                log::info!(
                    "nvhost_ctrl::IocCtrlEventWait signalled-after-update syncpt_id={} value_raw_out=0x{:08X}",
                    fence_id,
                    params.value.raw
                );
            }
            Self::trace_event_wait_record(
                4,
                params,
                is_allocation,
                u32::MAX,
                NvResult::Success,
                new_value,
                params.fence.value,
            );
            reset_non_allocation_fails(self);
            return NvResult::Success;
        }

        let result = {
            // Upstream uses one NvEventsLock() for the event table and mask.
            // Rust stores them in two mutexes; keep the acquisition order
            // identical to register/unregister/free_event paths.
            let mut mask = self.events_mask.lock().unwrap();
            let mut events = self.events.lock().unwrap();
            let slot = if is_allocation {
                params.value.raw = 0;
                self.find_free_nv_event(&mut events, &mut mask, fence_id)
            } else {
                params.value.raw
            };

            let target_value = params.fence.value;

            if slot >= MAX_NV_EVENTS {
                Self::trace_event_wait_record(
                    9,
                    params,
                    is_allocation,
                    slot,
                    NvResult::BadParameter,
                    new_value,
                    target_value,
                );
                return NvResult::BadParameter;
            }

            if params.timeout == 0 {
                if events[slot as usize].fails > 2 {
                    events[slot as usize].fails = 0;
                    self.wait_host_stalled(fence_id, target_value);
                    params.value.raw = target_value;
                    if Self::should_stderr_event_wait(target_value) {
                        eprintln!(
                            "[NVHOST_CTRL_WAIT] fallback-timeout0-success slot={} syncpt_id={} target={} value_raw=0x{:08X}",
                            slot, fence_id, target_value, params.value.raw
                        );
                    }
                    if Self::should_trace_event_wait() {
                        log::info!(
                            "nvhost_ctrl::IocCtrlEventWait timeout=0 fallback-success slot={} syncpt_id={} target={} value_raw_out=0x{:08X}",
                            slot,
                            fence_id,
                            target_value,
                            params.value.raw
                        );
                    }
                    Self::trace_event_wait_record(
                        5,
                        params,
                        is_allocation,
                        slot,
                        NvResult::Success,
                        params.value.raw,
                        target_value,
                    );
                    NvResult::Success
                } else {
                    if Self::should_trace_event_wait() {
                        log::info!(
                            "nvhost_ctrl::IocCtrlEventWait timeout=0 returning-timeout slot={} syncpt_id={} target={}",
                            slot,
                            fence_id,
                            target_value
                        );
                    }
                    Self::trace_event_wait_record(
                        6,
                        params,
                        is_allocation,
                        slot,
                        NvResult::Timeout,
                        new_value,
                        target_value,
                    );
                    NvResult::Timeout
                }
            } else {
                if !events[slot as usize].registered {
                    Self::trace_event_wait_record(
                        9,
                        params,
                        is_allocation,
                        slot,
                        NvResult::BadParameter,
                        new_value,
                        target_value,
                    );
                    return NvResult::BadParameter;
                }
                if events[slot as usize].is_being_used() {
                    Self::trace_event_wait_record(
                        10,
                        params,
                        is_allocation,
                        slot,
                        NvResult::Busy,
                        new_value,
                        target_value,
                    );
                    return NvResult::BadParameter;
                }

                if events[slot as usize].fails > 2 {
                    events[slot as usize].fails = 0;
                    self.wait_host_stalled(fence_id, target_value);
                    params.value.raw = target_value;
                    if Self::should_stderr_event_wait(target_value) {
                        eprintln!(
                            "[NVHOST_CTRL_WAIT] fallback-wait-success slot={} syncpt_id={} target={} value_raw=0x{:08X}",
                            slot, fence_id, target_value, params.value.raw
                        );
                    }
                    if Self::should_trace_event_wait() {
                        log::info!(
                            "nvhost_ctrl::IocCtrlEventWait wait-fallback-success slot={} syncpt_id={} target={} value_raw_out=0x{:08X}",
                            slot,
                            fence_id,
                            target_value,
                            params.value.raw
                        );
                    }
                    Self::trace_event_wait_record(
                        7,
                        params,
                        is_allocation,
                        slot,
                        NvResult::Success,
                        params.value.raw,
                        target_value,
                    );
                    NvResult::Success
                } else {
                    let event = &mut events[slot as usize];
                    params.value.raw = 0;
                    event
                        .status
                        .store(EventState::Waiting as u32, Ordering::Release);
                    event.assigned_syncpt = fence_id;
                    event.assigned_value = target_value;
                    if is_allocation {
                        params.value.raw = slot | ((fence_id & 0x0FFF) << 16) | (1 << 28);
                    } else {
                        params.value.raw = slot | (fence_id << 4);
                    }
                    // Upstream's callback captures `this`+`slot` and touches only
                    // `events[slot].status` (atomic) and its kevent — it never takes
                    // NvEventsLock. `RegisterHostAction` fires the callback
                    // synchronously on this thread when the fence is already
                    // signalled, and we are holding the events mutex here, so the
                    // callback must not lock the events table (self-deadlock).
                    let status_ref = Arc::clone(&event.status);
                    let callback_readable_event = event.readable_event.clone();
                    event.wait_handle = self.syncpoint_manager().register_host_action(
                        fence_id,
                        target_value,
                        Box::new(move || {
                            if status_ref.swap(EventState::Signalling as u32, Ordering::AcqRel)
                                == EventState::Waiting as u32
                            {
                                if let Some(readable_event) = &callback_readable_event {
                                    let object_id = {
                                        let mut readable_event = readable_event.lock().unwrap();
                                        let object_id = readable_event.object_id;
                                        readable_event.signal_from_host();
                                        object_id
                                    };
                                    Self::trace_event_record(
                                        12,
                                        slot,
                                        slot,
                                        fence_id,
                                        target_value,
                                        EventState::Signalling as u32,
                                        object_id,
                                    );
                                }
                            }
                            status_ref.store(EventState::Signalled as u32, Ordering::Release);
                        }),
                    );

                    if Self::should_trace_event_wait() {
                        log::info!(
                            "nvhost_ctrl::IocCtrlEventWait armed slot={} syncpt_id={} target={} value_raw_out=0x{:08X}",
                            slot,
                            fence_id,
                            target_value,
                            params.value.raw
                        );
                    }
                    if Self::should_stderr_event_wait(target_value) {
                        eprintln!(
                            "[NVHOST_CTRL_WAIT] armed slot={} syncpt_id={} target={} value_raw=0x{:08X}",
                            slot, fence_id, target_value, params.value.raw
                        );
                    }

                    Self::trace_event_wait_record(
                        8,
                        params,
                        is_allocation,
                        slot,
                        NvResult::Timeout,
                        new_value,
                        target_value,
                    );

                    NvResult::Timeout
                }
            }
        };

        result
    }

    pub fn ioc_ctrl_event_register(&self, params: &mut IocCtrlEventRegisterParams) -> NvResult {
        let event_id = params.user_event_id;
        log::debug!(
            "nvhost_ctrl::IocCtrlEventRegister called, user_event_id: {:X}",
            event_id
        );
        if event_id >= MAX_NV_EVENTS {
            return NvResult::BadParameter;
        }

        let mut mask = self.events_mask.lock().unwrap();
        let mut events = self.events.lock().unwrap();
        if events[event_id as usize].registered {
            let result = self.free_event_locked(&mut events, &mut mask, event_id);
            if result != NvResult::Success {
                return result;
            }
        }

        self.create_nv_event(&mut events, &mut mask, event_id);

        NvResult::Success
    }

    pub fn ioc_ctrl_event_unregister(&self, params: &mut IocCtrlEventUnregisterParams) -> NvResult {
        let event_id = params.user_event_id & 0x00FF;
        log::debug!(
            "nvhost_ctrl::IocCtrlEventUnregister called, user_event_id: {:X}",
            event_id
        );

        if event_id >= MAX_NV_EVENTS {
            return NvResult::BadParameter;
        }

        let mut mask = self.events_mask.lock().unwrap();
        let mut events = self.events.lock().unwrap();
        self.free_event_locked(&mut events, &mut mask, event_id)
    }

    pub fn ioc_ctrl_event_unregister_batch(
        &self,
        params: &mut IocCtrlEventUnregisterBatchParams,
    ) -> NvResult {
        let mut event_mask = params.user_events;
        log::debug!(
            "nvhost_ctrl::IocCtrlEventUnregisterBatch called, event_mask: {:X}",
            event_mask
        );

        let mut mask = self.events_mask.lock().unwrap();
        let mut events = self.events.lock().unwrap();
        while event_mask != 0 {
            let event_id = event_mask.trailing_zeros() as u32;
            event_mask &= !(1u64 << event_id);

            let result = self.free_event_locked(&mut events, &mut mask, event_id);
            if result != NvResult::Success {
                return result;
            }
        }

        NvResult::Success
    }

    pub fn ioc_ctrl_clear_event_wait(&self, params: &mut IocCtrlEventClearParams) -> NvResult {
        let event_id = params.event_id.slot() as u32;
        log::debug!(
            "nvhost_ctrl::IocCtrlClearEventWait called, event_id: {:X}",
            event_id
        );

        if event_id >= MAX_NV_EVENTS {
            return NvResult::BadParameter;
        }

        let mut events = self.events.lock().unwrap();
        let event = &mut events[event_id as usize];

        if !event.registered {
            return NvResult::Success;
        }

        if event
            .status
            .swap(EventState::Cancelling as u32, Ordering::AcqRel)
            == EventState::Waiting as u32
        {
            if let Some(wait_handle) = event.wait_handle.take() {
                self.syncpoint_manager()
                    .deregister_host_action(event.assigned_syncpt, wait_handle);
            }
            self.syncpoint_manager().update_min(event.assigned_syncpt);
        }
        event.fails += 1;
        event
            .status
            .store(EventState::Cancelled as u32, Ordering::Release);
        if let Some(readable_event) = &event.readable_event {
            let _ = readable_event.lock().unwrap().clear();
        }

        NvResult::Success
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        IocCtrlEventClearParams, IocCtrlEventRegisterParams, IocCtrlEventWaitParams, NvHostCtrl,
    };
    use crate::core::{System, SystemRef};
    use crate::hle::service::nvdrv::core::syncpoint_manager::SyncpointManager;
    use crate::hle::service::nvdrv::devices::nvdevice::NvDevice;
    use crate::hle::service::nvdrv::nvdata::{NvFence, NvResult};
    use crate::hle::service::nvdrv::nvdrv::EventInterface;

    #[test]
    fn event_wait_allocation_populates_allocated_event_value() {
        let system = System::new_for_test();
        let events = Arc::new(EventInterface::new(SystemRef::from_ref(&system)));
        let syncpoints = SyncpointManager::new();
        let ctrl = NvHostCtrl::new(events, &syncpoints);
        let fence_id = syncpoints.allocate_syncpoint(false);

        let mut params = IocCtrlEventWaitParams {
            fence: NvFence {
                id: fence_id as i32,
                value: 5,
            },
            timeout: 1,
            ..Default::default()
        };

        let result = ctrl.ioc_ctrl_event_wait(&mut params, true);

        assert_eq!(result, NvResult::Timeout);
        assert!(params.value.event_allocated());
        assert_eq!(
            params.value.syncpoint_id_for_allocation() as u32,
            fence_id & 0x0FFF
        );
        assert!(ctrl.query_event(params.value.raw).is_some());

        std::mem::forget(system);
    }

    #[test]
    fn clear_event_wait_cancels_registered_slot() {
        let system = System::new_for_test();
        let events = Arc::new(EventInterface::new(SystemRef::from_ref(&system)));
        let syncpoints = SyncpointManager::new();
        let ctrl = NvHostCtrl::new(events, &syncpoints);
        let mut register = IocCtrlEventRegisterParams { user_event_id: 2 };
        assert_eq!(
            ctrl.ioc_ctrl_event_register(&mut register),
            NvResult::Success
        );

        let mut clear = IocCtrlEventClearParams {
            event_id: super::SyncpointEventValue { raw: 2 },
        };

        assert_eq!(
            ctrl.ioc_ctrl_clear_event_wait(&mut clear),
            NvResult::Success
        );

        std::mem::forget(system);
    }

    #[test]
    fn event_wait_non_allocation_fallback_wait_host_resets_fails_and_returns_target() {
        let system = System::new_for_test();
        let events = Arc::new(EventInterface::new(SystemRef::from_ref(&system)));
        let syncpoints = SyncpointManager::new();
        let ctrl = NvHostCtrl::new(events, &syncpoints);
        let fence_id = syncpoints.allocate_syncpoint(false);

        let mut register = IocCtrlEventRegisterParams { user_event_id: 2 };
        assert_eq!(
            ctrl.ioc_ctrl_event_register(&mut register),
            NvResult::Success
        );

        {
            let mut events = ctrl.events.lock().unwrap();
            events[2].fails = 3;
        }

        let mut params = IocCtrlEventWaitParams {
            fence: NvFence {
                id: fence_id as i32,
                value: 5,
            },
            timeout: 1,
            value: super::SyncpointEventValue { raw: 2 },
        };

        let result = ctrl.ioc_ctrl_event_wait(&mut params, false);

        assert_eq!(result, NvResult::Success);
        assert_eq!(params.value.raw, 5);
        {
            let events = ctrl.events.lock().unwrap();
            assert_eq!(events[2].fails, 0);
        }

        std::mem::forget(system);
    }

    #[test]
    fn event_wait_non_allocation_immediate_success_resets_fails() {
        let system = System::new_for_test();
        let events = Arc::new(EventInterface::new(SystemRef::from_ref(&system)));
        let syncpoints = SyncpointManager::new();
        let ctrl = NvHostCtrl::new(events, &syncpoints);
        let fence_id = syncpoints.allocate_syncpoint(false);

        let mut register = IocCtrlEventRegisterParams { user_event_id: 2 };
        assert_eq!(
            ctrl.ioc_ctrl_event_register(&mut register),
            NvResult::Success
        );

        {
            let mut events = ctrl.events.lock().unwrap();
            events[2].fails = 2;
        }

        syncpoints.increment_syncpoint_max_ext(fence_id, 5);
        syncpoints.signal_syncpoint(fence_id);

        let mut params = IocCtrlEventWaitParams {
            fence: NvFence {
                id: fence_id as i32,
                value: 5,
            },
            timeout: 1,
            value: super::SyncpointEventValue { raw: 2 },
        };

        let result = ctrl.ioc_ctrl_event_wait(&mut params, false);

        assert_eq!(result, NvResult::Success);
        assert_eq!(params.value.raw, 5);
        {
            let events = ctrl.events.lock().unwrap();
            assert_eq!(events[2].fails, 0);
        }

        std::mem::forget(system);
    }

    #[test]
    fn event_wait_non_allocation_armed_timeout_preserves_fails() {
        let system = System::new_for_test();
        let events = Arc::new(EventInterface::new(SystemRef::from_ref(&system)));
        let syncpoints = SyncpointManager::new();
        let ctrl = NvHostCtrl::new(events, &syncpoints);
        let fence_id = syncpoints.allocate_syncpoint(false);

        let mut register = IocCtrlEventRegisterParams { user_event_id: 2 };
        assert_eq!(
            ctrl.ioc_ctrl_event_register(&mut register),
            NvResult::Success
        );

        {
            let mut events = ctrl.events.lock().unwrap();
            events[2].fails = 2;
        }

        let mut params = IocCtrlEventWaitParams {
            fence: NvFence {
                id: fence_id as i32,
                value: 5,
            },
            timeout: 1,
            value: super::SyncpointEventValue { raw: 2 },
        };

        let result = ctrl.ioc_ctrl_event_wait(&mut params, false);

        assert_eq!(result, NvResult::Timeout);
        {
            let events = ctrl.events.lock().unwrap();
            assert_eq!(events[2].fails, 2);
        }

        std::mem::forget(system);
    }

    #[test]
    fn query_event_returns_registered_event_for_matching_syncpoint() {
        let system = System::new_for_test();
        let events = Arc::new(EventInterface::new(SystemRef::from_ref(&system)));
        let syncpoints = SyncpointManager::new();
        let ctrl = NvHostCtrl::new(events, &syncpoints);

        {
            let mut events_guard = ctrl.events.lock().unwrap();
            let mut mask = ctrl.events_mask.lock().unwrap();
            ctrl.create_nv_event(&mut events_guard, &mut mask, 3);
            events_guard[3].assigned_syncpt = 7;
        }

        let encoded_event_id = 3 | (7 << 16) | (1 << 28);
        assert!(ctrl.query_event(encoded_event_id).is_some());

        std::mem::forget(system);
    }
}

impl NvDevice for NvHostCtrl {
    fn ioctl1(&self, _fd: DeviceFD, command: Ioctl, input: &[u8], output: &mut [u8]) -> NvResult {
        Self::trace_ioctl(command, "ioctl1");
        match command.group() {
            0x0 => match command.cmd() {
                0x1b => {
                    let mut params: IocGetConfigParams = read_struct(input);
                    let r = self.nv_os_get_config_u32(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x1c => {
                    let mut params: IocCtrlEventClearParams = read_struct(input);
                    let r = self.ioc_ctrl_clear_event_wait(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x1d => {
                    let mut params: IocCtrlEventWaitParams = read_struct(input);
                    let r = self.ioc_ctrl_event_wait(&mut params, true);
                    write_struct(output, &params);
                    r
                }
                0x1e => {
                    let mut params: IocCtrlEventWaitParams = read_struct(input);
                    let r = self.ioc_ctrl_event_wait(&mut params, false);
                    write_struct(output, &params);
                    r
                }
                0x1f => {
                    let mut params: IocCtrlEventRegisterParams = read_struct(input);
                    let r = self.ioc_ctrl_event_register(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x20 => {
                    let mut params: IocCtrlEventUnregisterParams = read_struct(input);
                    let r = self.ioc_ctrl_event_unregister(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x21 => {
                    let mut params: IocCtrlEventUnregisterBatchParams = read_struct(input);
                    let r = self.ioc_ctrl_event_unregister_batch(&mut params);
                    write_struct(output, &params);
                    r
                }
                _ => {
                    log::error!("Unimplemented ioctl={:08X}", command.raw);
                    NvResult::NotImplemented
                }
            },
            _ => {
                log::error!("Unimplemented ioctl={:08X}", command.raw);
                NvResult::NotImplemented
            }
        }
    }

    fn ioctl2(
        &self,
        _fd: DeviceFD,
        command: Ioctl,
        _input: &[u8],
        _inline_input: &[u8],
        _output: &mut [u8],
    ) -> NvResult {
        Self::trace_ioctl(command, "ioctl2");
        log::error!("Unimplemented ioctl={:08X}", command.raw);
        NvResult::NotImplemented
    }

    fn ioctl3(
        &self,
        _fd: DeviceFD,
        command: Ioctl,
        _input: &[u8],
        _output: &mut [u8],
        _inline_output: &mut [u8],
    ) -> NvResult {
        log::error!("Unimplemented ioctl={:08X}", command.raw);
        NvResult::NotImplemented
    }

    fn on_open(&self, _session_id: SessionId, _fd: DeviceFD) {}
    fn on_close(&self, _fd: DeviceFD) {}

    fn query_event(&self, event_id: u32) -> Option<Arc<Mutex<KReadableEvent>>> {
        let desired_event = SyncpointEventValue { raw: event_id };
        let allocated = desired_event.event_allocated();
        let slot = if allocated {
            desired_event.partial_slot()
        } else {
            desired_event.slot() as u32
        };

        if slot >= MAX_NV_EVENTS {
            log::error!("Event slot {} out of range", slot);
            return None;
        }

        let syncpoint_id = if allocated {
            desired_event.syncpoint_id_for_allocation() as u32
        } else {
            desired_event.syncpoint_id()
        };

        let events = self.events.lock().unwrap();
        let event = &events[slot as usize];
        if event.registered && event.assigned_syncpt == syncpoint_id {
            let object_id = event
                .readable_event
                .as_ref()
                .map(|readable_event| readable_event.lock().unwrap().object_id)
                .unwrap_or(0);
            Self::trace_event_record(
                13,
                slot,
                event_id,
                syncpoint_id,
                event.assigned_value,
                event.status.load(Ordering::Acquire),
                object_id,
            );
            return event.readable_event.clone();
        }

        Self::trace_event_record(
            14,
            slot,
            event_id,
            syncpoint_id,
            event.assigned_value,
            event.status.load(Ordering::Acquire),
            0,
        );
        log::error!("Slot:{}, SyncpointID:{}, requested", slot, syncpoint_id);
        None
    }
}

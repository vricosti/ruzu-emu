// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ns/system_update_interface.h
//! Port of zuyu/src/core/hle/service/ns/system_update_interface.cpp
//!
//! ISystemUpdateInterface — "ns:su" service.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::ns_types::BackgroundNetworkUpdateState;
use crate::hle::result::{ResultCode, RESULT_SUCCESS, RESULT_UNKNOWN};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::os::event::Event;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for ISystemUpdateInterface.
///
/// Corresponds to the function table in upstream system_update_interface.cpp.
pub mod commands {
    pub const GET_BACKGROUND_NETWORK_UPDATE_STATE: u32 = 0;
    pub const OPEN_SYSTEM_UPDATE_CONTROL: u32 = 1;
    pub const NOTIFY_EX_FAT_DRIVER_REQUIRED: u32 = 2;
    pub const CLEAR_EX_FAT_DRIVER_STATUS_FOR_DEBUG: u32 = 3;
    pub const REQUEST_BACKGROUND_NETWORK_UPDATE: u32 = 4;
    pub const NOTIFY_BACKGROUND_NETWORK_UPDATE: u32 = 5;
    pub const NOTIFY_EX_FAT_DRIVER_DOWNLOADED_FOR_DEBUG: u32 = 6;
    pub const GET_SYSTEM_UPDATE_NOTIFICATION_EVENT_FOR_CONTENT_DELIVERY: u32 = 9;
    pub const NOTIFY_SYSTEM_UPDATE_FOR_CONTENT_DELIVERY: u32 = 10;
    pub const PREPARE_SHUTDOWN: u32 = 11;
    pub const UNKNOWN_12: u32 = 12;
    pub const UNKNOWN_13: u32 = 13;
    pub const UNKNOWN_14: u32 = 14;
    pub const UNKNOWN_15: u32 = 15;
    pub const DESTROY_SYSTEM_UPDATE_TASK: u32 = 16;
    pub const REQUEST_SEND_SYSTEM_UPDATE: u32 = 17;
    pub const GET_SEND_SYSTEM_UPDATE_PROGRESS: u32 = 18;
}

/// ISystemUpdateInterface.
///
/// Corresponds to `ISystemUpdateInterface` in upstream.
pub struct ISystemUpdateInterface {
    // The existing Event bridge materializes the readable kernel endpoint when
    // first requested; the interface owns its lifetime, as upstream does.
    update_notification_event: Event,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ISystemUpdateInterface {
    pub fn new() -> Self {
        let handlers = build_handler_map(&[
            (
                commands::GET_BACKGROUND_NETWORK_UPDATE_STATE,
                Some(Self::get_background_network_update_state_handler),
                "GetBackgroundNetworkUpdateState",
            ),
            (
                commands::OPEN_SYSTEM_UPDATE_CONTROL,
                Some(Self::open_system_update_control_handler),
                "OpenSystemUpdateControl",
            ),
            (
                commands::NOTIFY_EX_FAT_DRIVER_REQUIRED,
                None,
                "NotifyExFatDriverRequired",
            ),
            (
                commands::CLEAR_EX_FAT_DRIVER_STATUS_FOR_DEBUG,
                None,
                "ClearExFatDriverStatusForDebug",
            ),
            (
                commands::REQUEST_BACKGROUND_NETWORK_UPDATE,
                None,
                "RequestBackgroundNetworkUpdate",
            ),
            (
                commands::NOTIFY_BACKGROUND_NETWORK_UPDATE,
                None,
                "NotifyBackgroundNetworkUpdate",
            ),
            (
                commands::NOTIFY_EX_FAT_DRIVER_DOWNLOADED_FOR_DEBUG,
                None,
                "NotifyExFatDriverDownloadedForDebug",
            ),
            (
                commands::GET_SYSTEM_UPDATE_NOTIFICATION_EVENT_FOR_CONTENT_DELIVERY,
                Some(Self::get_system_update_notification_event_for_content_delivery_handler),
                "GetSystemUpdateNotificationEventForContentDelivery",
            ),
            (
                commands::NOTIFY_SYSTEM_UPDATE_FOR_CONTENT_DELIVERY,
                None,
                "NotifySystemUpdateForContentDelivery",
            ),
            (commands::PREPARE_SHUTDOWN, None, "PrepareShutdown"),
            (commands::UNKNOWN_12, None, "Unknown12"),
            (commands::UNKNOWN_13, None, "Unknown13"),
            (commands::UNKNOWN_14, None, "Unknown14"),
            (commands::UNKNOWN_15, None, "Unknown15"),
            (
                commands::DESTROY_SYSTEM_UPDATE_TASK,
                None,
                "DestroySystemUpdateTask",
            ),
            (
                commands::REQUEST_SEND_SYSTEM_UPDATE,
                None,
                "RequestSendSystemUpdate",
            ),
            (
                commands::GET_SEND_SYSTEM_UPDATE_PROGRESS,
                None,
                "GetSendSystemUpdateProgress",
            ),
        ]);
        Self {
            update_notification_event: Event::new(),
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn get_background_network_update_state_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let state = service.get_background_network_update_state().unwrap();
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(state as u32);
    }

    fn open_system_update_control_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let control = service.open_system_update_control().unwrap();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(Arc::new(control));
    }

    fn get_system_update_notification_event_for_content_delivery(&self) -> &Event {
        log::warn!("(STUBBED) GetSystemUpdateNotificationEventForContentDelivery called");
        &self.update_notification_event
    }

    fn get_system_update_notification_event_for_content_delivery_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let event = service.get_system_update_notification_event_for_content_delivery();
        let Some(object_id) = event.copy_object_id(ctx) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    /// GetBackgroundNetworkUpdateState (cmd 0).
    ///
    /// Corresponds to upstream `ISystemUpdateInterface::GetBackgroundNetworkUpdateState`.
    pub fn get_background_network_update_state(
        &self,
    ) -> Result<BackgroundNetworkUpdateState, ResultCode> {
        log::warn!("(STUBBED) GetBackgroundNetworkUpdateState called");
        Ok(BackgroundNetworkUpdateState::None)
    }

    /// OpenSystemUpdateControl (cmd 1).
    ///
    /// Corresponds to upstream `ISystemUpdateInterface::OpenSystemUpdateControl`.
    pub fn open_system_update_control(
        &self,
    ) -> Result<super::system_update_control::ISystemUpdateControl, ResultCode> {
        log::warn!("(STUBBED) OpenSystemUpdateControl called");
        Ok(super::system_update_control::ISystemUpdateControl::new())
    }
}

impl SessionRequestHandler for ISystemUpdateInterface {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ns::ISystemUpdateInterface"
    }
}

impl ServiceFramework for ISystemUpdateInterface {
    fn get_service_name(&self) -> &str {
        "ns::ISystemUpdateInterface"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::kernel::k_process::{KProcess, ProcessLock};
    use crate::hle::kernel::k_readable_event::KReadableEvent;
    use crate::hle::kernel::k_thread::{KThread, KThreadLock};
    use crate::hle::service::hle_ipc::KAutoObjectRef;
    use std::sync::Mutex;

    #[test]
    fn notification_requests_copy_the_same_unsignaled_event() {
        let service = ISystemUpdateInterface::new();
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let readable = Arc::new(Mutex::new(KReadableEvent::new()));
        readable.lock().unwrap().initialize(1, 2);
        service
            .update_notification_event
            .attach_kernel_event(readable.clone(), process.clone());
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
        for _ in 0..2 {
            let mut ctx = HLERequestContext::new_with_thread(thread.clone(), 0);
            service.handlers[&9].handler_callback.unwrap()(&service, &mut ctx);
            assert!(matches!(
                ctx.outgoing_copy_objects.as_slice(),
                [KAutoObjectRef::ObjectId(2)]
            ));
            assert!(!readable.lock().unwrap().is_signaled());
        }
        assert_eq!(
            service.get_background_network_update_state().unwrap(),
            BackgroundNetworkUpdateState::None
        );
        assert!(service.handlers[&0].handler_callback.is_some());
        assert!(service.handlers[&1].handler_callback.is_some());
        assert!(service.handlers[&10].handler_callback.is_none());
    }
}

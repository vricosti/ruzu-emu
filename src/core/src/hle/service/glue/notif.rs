// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/glue/notif.h
//! Port of zuyu/src/core/hle/service/glue/notif.cpp

use crate::hle::result::{ResultCode, RESULT_SUCCESS, RESULT_UNKNOWN};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::os::event::Event;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// nn::notification::AlarmSettingId
pub type AlarmSettingId = u16;

pub type ApplicationParameter = [u8; 0x400];

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct DailyAlarmSetting {
    pub hour: i8,
    pub minute: i8,
}

const _: () = assert!(core::mem::size_of::<DailyAlarmSetting>() == 0x2);

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct WeeklyScheduleAlarmSetting {
    pub _padding: [u8; 0xA],
    pub day_of_week: [DailyAlarmSetting; 0x7],
}

const _: () = assert!(core::mem::size_of::<WeeklyScheduleAlarmSetting>() == 0x18);

impl Default for WeeklyScheduleAlarmSetting {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}

/// nn::notification::AlarmSetting
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct AlarmSetting {
    pub alarm_setting_id: AlarmSettingId,
    pub kind: u8,
    pub muted: u8,
    pub _padding1: [u8; 0x4],
    pub account_id: [u8; 16],
    pub application_id: u64,
    pub _padding2: [u8; 0x8],
    pub schedule: WeeklyScheduleAlarmSetting,
}

const _: () = assert!(core::mem::size_of::<AlarmSetting>() == 0x40);

impl Default for AlarmSetting {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NotificationChannel {
    Unknown0 = 0,
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct NotificationPresentationSetting {
    pub _padding: [u8; 0x10],
}

const _: () = assert!(core::mem::size_of::<NotificationPresentationSetting>() == 0x10);

const MAX_ALARMS: usize = 8;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_ipc_returns_explicit_empty_count_and_presentation() {
        let service = INotificationServices::new();
        assert_eq!(service.handlers.len(), 20);
        for (command, expected_words) in [(520, 1), (1510, 4)] {
            let mut ctx = HLERequestContext::new();
            ctx.command_buffer_mut().fill(0xa5a5a5a5);
            service.handlers[&command].handler_callback.unwrap()(&service, &mut ctx);
            assert_eq!(ctx.command_buffer()[6], 0);
            assert_eq!(
                &ctx.command_buffer()[8..8 + expected_words],
                vec![0; expected_words]
            );
        }
        assert!(service.handlers[&1010].handler_callback.is_none());
        assert_eq!(INotificationServicesForApplication::new().handlers.len(), 6);
    }

    #[test]
    fn alarm_store_matches_limits_and_parameter_output() {
        let mut store = NotificationServiceImpl::new();
        let mut out = [0xa5; 0x410];
        assert_eq!(
            store.load_application_parameter(&mut out, 9).0,
            RESULT_UNKNOWN
        );
        assert_eq!(out, [0xa5; 0x410]);
        for id in 0..9 {
            assert_eq!(
                store.register_alarm_setting(&AlarmSetting::default(), &[]),
                (RESULT_SUCCESS, id)
            );
        }
        assert_eq!(
            store
                .register_alarm_setting(&AlarmSetting::default(), &[])
                .0,
            RESULT_UNKNOWN
        );
        assert_eq!(
            store.load_application_parameter(&mut out, 0),
            (RESULT_SUCCESS, 0x400)
        );
        assert_eq!(&out[..0x400], &[0; 0x400]);
        assert_eq!(&out[0x400..], &[0xa5; 16]);
        let mut alarms = [AlarmSetting::default(); 2];
        assert_eq!(store.list_alarm_settings(&mut alarms), (RESULT_SUCCESS, 2));
        assert_eq!(alarms[1].alarm_setting_id, 1);
        store.delete_alarm_setting(0);
        assert_eq!(
            store.load_application_parameter(&mut out, 0).0,
            RESULT_UNKNOWN
        );
        assert_eq!(std::mem::offset_of!(AlarmSetting, schedule), 40);
        assert_eq!(std::mem::size_of::<AlarmSetting>(), 64);
    }
}

/// NotificationServiceImpl: shared alarm management logic.
///
/// Corresponds to `NotificationServiceImpl` in upstream `notif.cpp`.
pub struct NotificationServiceImpl {
    pub alarms: Vec<AlarmSetting>,
    pub last_alarm_setting_id: AlarmSettingId,
}

impl NotificationServiceImpl {
    pub fn new() -> Self {
        Self {
            alarms: Vec::new(),
            last_alarm_setting_id: 0,
        }
    }

    pub fn register_alarm_setting(
        &mut self,
        alarm_setting: &AlarmSetting,
        application_parameter: &[u8],
    ) -> (ResultCode, AlarmSettingId) {
        if self.alarms.len() > MAX_ALARMS {
            log::error!("Alarm limit reached");
            return (RESULT_UNKNOWN, 0);
        }
        assert!(application_parameter.len() <= 0x400);

        let mut new_alarm = *alarm_setting;
        new_alarm.alarm_setting_id = self.last_alarm_setting_id;
        self.last_alarm_setting_id = self.last_alarm_setting_id.wrapping_add(1);
        self.alarms.push(new_alarm);

        log::warn!(
            "(STUBBED) register_alarm_setting called, setting_id={}, kind={}, muted={}",
            new_alarm.alarm_setting_id,
            new_alarm.kind,
            new_alarm.muted,
        );

        (RESULT_SUCCESS, new_alarm.alarm_setting_id)
    }

    pub fn update_alarm_setting(
        &mut self,
        alarm_setting: &AlarmSetting,
        application_parameter: &[u8],
    ) -> ResultCode {
        assert!(application_parameter.len() <= 0x400);
        if let Some(index) = self.get_alarm_from_id(alarm_setting.alarm_setting_id) {
            self.alarms[index] = *alarm_setting;
        }
        log::warn!("(STUBBED) update_alarm_setting called");
        RESULT_SUCCESS
    }

    pub fn list_alarm_settings(&self, out_alarms: &mut [AlarmSetting]) -> (ResultCode, i32) {
        let count = std::cmp::min(out_alarms.len(), self.alarms.len());
        for i in 0..count {
            out_alarms[i] = self.alarms[i];
        }
        log::info!(
            "list_alarm_settings called, alarm_count={}",
            self.alarms.len()
        );
        (RESULT_SUCCESS, count as i32)
    }

    pub fn delete_alarm_setting(&mut self, alarm_setting_id: AlarmSettingId) -> ResultCode {
        self.alarms
            .retain(|a| a.alarm_setting_id != alarm_setting_id);
        log::info!(
            "delete_alarm_setting called, alarm_setting_id={}",
            alarm_setting_id
        );
        RESULT_SUCCESS
    }

    pub fn initialize(&mut self, _aruid: u64) -> ResultCode {
        log::warn!("(STUBBED) initialize called");
        RESULT_SUCCESS
    }

    pub fn load_application_parameter(
        &self,
        out: &mut [u8],
        id: AlarmSettingId,
    ) -> (ResultCode, u32) {
        if self.get_alarm_from_id(id).is_none() {
            return (RESULT_UNKNOWN, 0);
        }
        let count = out.len().min(0x400);
        out[..count].fill(0);
        (RESULT_SUCCESS, 0x400)
    }

    fn get_alarm_from_id(&self, id: AlarmSettingId) -> Option<usize> {
        self.alarms.iter().position(|alarm| alarm.alarm_setting_id == id)
    }
}

pub struct INotificationServicesForApplication {
    pub impl_: Mutex<NotificationServiceImpl>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}
impl INotificationServicesForApplication {
    pub fn new() -> Self {
        Self {
            impl_: Mutex::new(NotificationServiceImpl::new()),
            handlers: build_handler_map(&[
                (
                    500,
                    Some(Self::register_alarm_setting_handler),
                    "RegisterAlarmSetting",
                ),
                (
                    510,
                    Some(Self::update_alarm_setting_handler),
                    "UpdateAlarmSetting",
                ),
                (
                    520,
                    Some(Self::list_alarm_settings_handler),
                    "ListAlarmSettings",
                ),
                (
                    530,
                    Some(Self::load_application_parameter_handler),
                    "LoadApplicationParameter",
                ),
                (
                    540,
                    Some(Self::delete_alarm_setting_handler),
                    "DeleteAlarmSetting",
                ),
                (1000, Some(Self::initialize_handler), "Initialize"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
    fn register_alarm_setting_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let data = ctx.read_buffer_a(0);
        assert!(data.len() >= 0x40);
        // AlarmSetting has only integer/byte-array fields and explicit padding.
        let alarm = unsafe { std::ptr::read_unaligned(data.as_ptr().cast::<AlarmSetting>()) };
        let (result, id) = service
            .impl_
            .lock()
            .unwrap()
            .register_alarm_setting(&alarm, &ctx.read_buffer_a(1));
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_u32(id as u32);
    }
    fn update_alarm_setting_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let data = ctx.read_buffer_a(0);
        assert!(data.len() >= 0x40);
        let alarm = unsafe { std::ptr::read_unaligned(data.as_ptr().cast::<AlarmSetting>()) };
        let result = service
            .impl_
            .lock()
            .unwrap()
            .update_alarm_setting(&alarm, &ctx.read_buffer_a(1));
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }
    fn list_alarm_settings_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let mut alarms = vec![AlarmSetting::default(); ctx.get_write_buffer_size(0) / 0x40];
        let (result, count) = service
            .impl_
            .lock()
            .unwrap()
            .list_alarm_settings(&mut alarms);
        // Entire records are initialized, including explicit reserved bytes.
        let bytes = unsafe {
            std::slice::from_raw_parts(alarms.as_ptr().cast::<u8>(), count as usize * 0x40)
        };
        ctx.write_buffer(bytes, 0);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_u32(count as u32);
    }
    fn load_application_parameter_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let id = RequestParser::new(ctx).pop_u32() as u16;
        let mut output = vec![0; ctx.get_write_buffer_size(0).min(0x400)];
        let (result, size) = service
            .impl_
            .lock()
            .unwrap()
            .load_application_parameter(&mut output, id);
        if result == RESULT_SUCCESS {
            ctx.write_buffer(&output, 0);
        }
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_u32(size);
    }
    fn delete_alarm_setting_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let id = RequestParser::new(ctx).pop_u32() as u16;
        let result = service.impl_.lock().unwrap().delete_alarm_setting(id);
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }
    fn initialize_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let aruid = RequestParser::new(ctx).pop_u64();
        let result = service.impl_.lock().unwrap().initialize(aruid);
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }
}

impl SessionRequestHandler for INotificationServicesForApplication {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }
    fn service_name(&self) -> &str {
        "notif:a"
    }
}
impl ServiceFramework for INotificationServicesForApplication {
    fn get_service_name(&self) -> &str {
        "notif:a"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct INotificationServices {
    pub impl_: Mutex<NotificationServiceImpl>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}
impl INotificationServices {
    pub fn new() -> Self {
        Self {
            impl_: Mutex::new(NotificationServiceImpl::new()),
            handlers: build_handler_map(&[
                (
                    500,
                    Some(Self::register_alarm_setting_handler),
                    "RegisterAlarmSetting",
                ),
                (
                    510,
                    Some(Self::update_alarm_setting_handler),
                    "UpdateAlarmSetting",
                ),
                (
                    520,
                    Some(Self::list_alarm_settings_handler),
                    "ListAlarmSettings",
                ),
                (
                    530,
                    Some(Self::load_application_parameter_handler),
                    "LoadApplicationParameter",
                ),
                (
                    540,
                    Some(Self::delete_alarm_setting_handler),
                    "DeleteAlarmSetting",
                ),
                (1000, Some(Self::initialize_handler), "Initialize"),
                (1010, None, "ListNotifications"),
                (1020, None, "DeleteNotification"),
                (1030, None, "ClearNotifications"),
                (
                    1040,
                    Some(Self::open_notification_system_event_accessor_handler),
                    "OpenNotificationSystemEventAccessor",
                ),
                (1500, None, "SetNotificationPresentationSetting"),
                (
                    1510,
                    Some(Self::get_notification_presentation_setting_handler),
                    "GetNotificationPresentationSetting",
                ),
                (2000, None, "GetAlarmSetting"),
                (2001, None, "GetAlarmSettingWithApplicationParameter"),
                (2010, None, "MuteAlarmSetting"),
                (2020, None, "IsAlarmSettingReady"),
                (8000, None, "RegisterAppletResourceUserId"),
                (8010, None, "UnregisterAppletResourceUserId"),
                (8999, None, "GetCurrentTime"),
                (9000, None, "GetAlarmSettingNextNotificationTime"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
    fn register_alarm_setting_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let data = ctx.read_buffer_a(0);
        assert!(data.len() >= 0x40);
        // AlarmSetting has only integer/byte-array fields and explicit padding.
        let alarm = unsafe { std::ptr::read_unaligned(data.as_ptr().cast::<AlarmSetting>()) };
        let (result, id) = service
            .impl_
            .lock()
            .unwrap()
            .register_alarm_setting(&alarm, &ctx.read_buffer_a(1));
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_u32(id as u32);
    }
    fn update_alarm_setting_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let data = ctx.read_buffer_a(0);
        assert!(data.len() >= 0x40);
        let alarm = unsafe { std::ptr::read_unaligned(data.as_ptr().cast::<AlarmSetting>()) };
        let result = service
            .impl_
            .lock()
            .unwrap()
            .update_alarm_setting(&alarm, &ctx.read_buffer_a(1));
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }
    fn list_alarm_settings_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let mut alarms = vec![AlarmSetting::default(); ctx.get_write_buffer_size(0) / 0x40];
        let (result, count) = service
            .impl_
            .lock()
            .unwrap()
            .list_alarm_settings(&mut alarms);
        // Entire records are initialized, including explicit reserved bytes.
        let bytes = unsafe {
            std::slice::from_raw_parts(alarms.as_ptr().cast::<u8>(), count as usize * 0x40)
        };
        ctx.write_buffer(bytes, 0);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_u32(count as u32);
    }
    fn load_application_parameter_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let id = RequestParser::new(ctx).pop_u32() as u16;
        let mut output = vec![0; ctx.get_write_buffer_size(0).min(0x400)];
        let (result, size) = service
            .impl_
            .lock()
            .unwrap()
            .load_application_parameter(&mut output, id);
        if result == RESULT_SUCCESS {
            ctx.write_buffer(&output, 0);
        }
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_u32(size);
    }
    fn delete_alarm_setting_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let id = RequestParser::new(ctx).pop_u32() as u16;
        let result = service.impl_.lock().unwrap().delete_alarm_setting(id);
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }
    fn initialize_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let aruid = RequestParser::new(ctx).pop_u64();
        let result = service.impl_.lock().unwrap().initialize(aruid);
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }
    fn open_notification_system_event_accessor_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let _ = this;
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(Arc::new(INotificationSystemEventAccessor::new()));
    }
    fn get_notification_presentation_setting_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let _ = this;
        let _channel = RequestParser::new(ctx).pop_u8();
        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        for _ in 0..4 {
            rb.push_u32(0);
        }
    }
}

impl SessionRequestHandler for INotificationServices {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }
    fn service_name(&self) -> &str {
        "notif:s"
    }
}
impl ServiceFramework for INotificationServices {
    fn get_service_name(&self) -> &str {
        "notif:s"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct INotificationSystemEventAccessor {
    notification_event: Event,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}
impl INotificationSystemEventAccessor {
    pub fn new() -> Self {
        Self {
            // Existing lazy Event bridge owns the kernel endpoint.
            notification_event: Event::new(),
            handlers: build_handler_map(&[(
                0,
                Some(Self::get_system_event_handler),
                "GetSystemEvent",
            )]),
            handlers_tipc: BTreeMap::new(),
        }
    }
    fn get_system_event_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let Some(id) = service.notification_event.copy_object_id(ctx) else {
            ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_UNKNOWN);
            return;
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(id);
    }
}

impl SessionRequestHandler for INotificationSystemEventAccessor {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }
    fn service_name(&self) -> &str {
        "INotificationSystemEventAccessor"
    }
}
impl ServiceFramework for INotificationSystemEventAccessor {
    fn get_service_name(&self) -> &str {
        "INotificationSystemEventAccessor"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

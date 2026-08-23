// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/glue/time/time_zone.h
//! Port of zuyu/src/core/hle/service/glue/time/time_zone.cpp
//!
//! TimeZoneService: glue-layer timezone service that wraps PSC::Time::TimeZoneService
//! and adds timezone binary file loading support.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{
    HLERequestContext, SessionRequestHandler, SessionRequestHandlerPtr,
};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::psc::time::common::{
    CalendarAdditionalInfo, CalendarTime, LocationName, OperationEvent, RuleVersion,
    SteadyClockTimePoint,
};
use crate::hle::service::psc::time::errors::{
    RESULT_FAILED, RESULT_NOT_IMPLEMENTED, RESULT_PERMISSION_DENIED, RESULT_TIME_ZONE_NOT_FOUND,
};
use crate::hle::service::psc::time::time_zone::TzRule;
use crate::hle::service::psc::time::time_zone_service::TimeZoneService as PscTimeZoneService;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use crate::hle::service::set::system_settings_server::SystemSettingsService;
use crate::hle::service::sm::sm::ServiceManager;

use super::file_timestamp_worker::FileTimestampWorker;
use super::time_zone_binary::TimeZoneBinary;

/// IPC command IDs for Glue::Time::TimeZoneService.
///
/// Corresponds to the function table in upstream glue/time/time_zone.cpp constructor.
pub mod commands {
    pub const GET_DEVICE_LOCATION_NAME: u32 = 0;
    pub const SET_DEVICE_LOCATION_NAME: u32 = 1;
    pub const GET_TOTAL_LOCATION_NAME_COUNT: u32 = 2;
    pub const LOAD_LOCATION_NAME_LIST: u32 = 3;
    pub const LOAD_TIME_ZONE_RULE: u32 = 4;
    pub const GET_TIME_ZONE_RULE_VERSION: u32 = 5;
    pub const GET_DEVICE_LOCATION_NAME_AND_UPDATED_TIME: u32 = 6;
    pub const SET_DEVICE_LOCATION_NAME_WITH_TIME_ZONE_RULE: u32 = 7;
    pub const PARSE_TIME_ZONE_BINARY: u32 = 8;
    pub const GET_DEVICE_LOCATION_NAME_OPERATION_EVENT_READABLE_HANDLE: u32 = 20;
    pub const TO_CALENDAR_TIME: u32 = 100;
    pub const TO_CALENDAR_TIME_WITH_MY_RULE: u32 = 101;
    pub const TO_POSIX_TIME: u32 = 201;
    pub const TO_POSIX_TIME_WITH_MY_RULE: u32 = 202;
}

/// Glue-layer TimeZoneService.
///
/// Corresponds to `Glue::Time::TimeZoneService` in upstream glue/time/time_zone.h.
/// Wraps `PSC::Time::TimeZoneService` and adds timezone binary support.
///
/// Upstream holds:
/// - `m_wrapped_service` (shared_ptr<PSC::Time::TimeZoneService>)
/// - `m_file_timestamp_worker` (FileTimestampWorker&)
/// - `m_time_zone_binary` (TimeZoneBinary&)
/// - `m_operation_event` (OperationEvent)
/// - `m_set_sys` (shared_ptr<Set::ISystemSettingsServer>)
pub struct TimeZoneService {
    /// System settings singleton. Upstream: `m_set_sys`.
    set_sys: Arc<SystemSettingsService>,
    can_write_timezone_device_location: bool,
    /// Wrapped PSC timezone service. Upstream: `m_wrapped_service`.
    wrapped_service: Mutex<PscTimeZoneService>,
    /// TimeZone binary data provider. Upstream: `m_time_zone_binary`.
    time_zone_binary: Arc<Mutex<TimeZoneBinary>>,
    /// Filesystem timestamp updater. Upstream: `m_file_timestamp_worker`.
    file_timestamp_worker: Arc<Mutex<FileTimestampWorker>>,
    /// Mutex for location changes. Upstream: `m_mutex`.
    mutex: Mutex<()>,
    /// Upstream: `Service::PSC::Time::OperationEvent m_operation_event`.
    operation_event: Mutex<Option<OperationEvent>>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl TimeZoneService {
    fn get_set_sys_service_from_handler(
        set_sys_handler: SessionRequestHandlerPtr,
    ) -> Arc<SystemSettingsService> {
        assert!(
            set_sys_handler.as_any().is::<SystemSettingsService>(),
            "set:sys is not an ISystemSettingsServer"
        );
        unsafe { Arc::from_raw(Arc::into_raw(set_sys_handler) as *const SystemSettingsService) }
    }

    fn get_set_sys_service(system: crate::core::SystemRef) -> Arc<SystemSettingsService> {
        assert!(
            !system.is_null(),
            "Glue::Time::TimeZoneService requires a live Core::System"
        );
        let service_manager = system
            .get()
            .service_manager()
            .expect("Glue::Time::TimeZoneService requires the service manager");
        let set_sys_handler =
            ServiceManager::get_service_blocking(&service_manager, system, "set:sys");
        Self::get_set_sys_service_from_handler(set_sys_handler)
    }

    fn pop_location_name(rp: &mut RequestParser<'_>) -> LocationName {
        let raw_words = rp.pop_raw::<[u32; 9]>();
        unsafe { core::mem::transmute::<[u32; 9], LocationName>(raw_words) }
    }

    fn build_handlers() -> BTreeMap<u32, FunctionInfo> {
        build_handler_map(&[
            (
                commands::GET_DEVICE_LOCATION_NAME,
                Some(Self::get_device_location_name_handler),
                "GetDeviceLocationName",
            ),
            (
                commands::SET_DEVICE_LOCATION_NAME,
                Some(Self::set_device_location_name_handler),
                "SetDeviceLocationName",
            ),
            (
                commands::GET_TOTAL_LOCATION_NAME_COUNT,
                Some(Self::get_total_location_name_count_handler),
                "GetTotalLocationNameCount",
            ),
            (
                commands::LOAD_LOCATION_NAME_LIST,
                Some(Self::load_location_name_list_handler),
                "LoadLocationNameList",
            ),
            (
                commands::LOAD_TIME_ZONE_RULE,
                Some(Self::load_time_zone_rule_handler),
                "LoadTimeZoneRule",
            ),
            (
                commands::GET_TIME_ZONE_RULE_VERSION,
                Some(Self::get_time_zone_rule_version_handler),
                "GetTimeZoneRuleVersion",
            ),
            (
                commands::GET_DEVICE_LOCATION_NAME_AND_UPDATED_TIME,
                Some(Self::get_device_location_name_and_updated_time_handler),
                "GetDeviceLocationNameAndUpdatedTime",
            ),
            (
                commands::SET_DEVICE_LOCATION_NAME_WITH_TIME_ZONE_RULE,
                Some(Self::set_device_location_name_with_time_zone_rule_handler),
                "SetDeviceLocationNameWithTimeZoneRule",
            ),
            (
                commands::PARSE_TIME_ZONE_BINARY,
                Some(Self::parse_time_zone_binary_handler),
                "ParseTimeZoneBinary",
            ),
            (
                commands::GET_DEVICE_LOCATION_NAME_OPERATION_EVENT_READABLE_HANDLE,
                Some(Self::get_device_location_name_operation_event_readable_handle_handler),
                "GetDeviceLocationNameOperationEventReadableHandle",
            ),
            (
                commands::TO_CALENDAR_TIME,
                Some(Self::to_calendar_time_handler),
                "ToCalendarTime",
            ),
            (
                commands::TO_CALENDAR_TIME_WITH_MY_RULE,
                Some(Self::to_calendar_time_with_my_rule_handler),
                "ToCalendarTimeWithMyRule",
            ),
            (
                commands::TO_POSIX_TIME,
                Some(Self::to_posix_time_handler),
                "ToPosixTime",
            ),
            (
                commands::TO_POSIX_TIME_WITH_MY_RULE,
                Some(Self::to_posix_time_with_my_rule_handler),
                "ToPosixTimeWithMyRule",
            ),
        ])
    }

    pub fn new(system: crate::core::SystemRef, can_write_timezone_device_location: bool) -> Self {
        let mut tz_binary = TimeZoneBinary::new(system);
        let _ = tz_binary.mount();
        Self::with_wrapped_and_set_sys(
            can_write_timezone_device_location,
            PscTimeZoneService::new(can_write_timezone_device_location),
            Arc::new(Mutex::new(FileTimestampWorker::new())),
            Arc::new(Mutex::new(tz_binary)),
            Self::get_set_sys_service(system),
        )
    }

    /// Create with an existing wrapped PSC service and timezone binary.
    pub fn with_wrapped(
        system: crate::core::SystemRef,
        can_write_timezone_device_location: bool,
        wrapped_service: PscTimeZoneService,
        file_timestamp_worker: Arc<Mutex<FileTimestampWorker>>,
        time_zone_binary: Arc<Mutex<TimeZoneBinary>>,
    ) -> Self {
        Self::with_wrapped_and_set_sys(
            can_write_timezone_device_location,
            wrapped_service,
            file_timestamp_worker,
            time_zone_binary,
            Self::get_set_sys_service(system),
        )
    }

    fn with_wrapped_and_set_sys(
        can_write_timezone_device_location: bool,
        wrapped_service: PscTimeZoneService,
        file_timestamp_worker: Arc<Mutex<FileTimestampWorker>>,
        time_zone_binary: Arc<Mutex<TimeZoneBinary>>,
        set_sys: Arc<SystemSettingsService>,
    ) -> Self {
        Self {
            set_sys,
            can_write_timezone_device_location,
            wrapped_service: Mutex::new(wrapped_service),
            time_zone_binary,
            file_timestamp_worker,
            mutex: Mutex::new(()),
            operation_event: Mutex::new(None),
            handlers: Self::build_handlers(),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    /// GetDeviceLocationName (cmd 0).
    ///
    /// Corresponds to `TimeZoneService::GetDeviceLocationName` in upstream.
    /// Delegates to `m_wrapped_service->GetDeviceLocationName`.
    pub fn get_device_location_name(&self) -> Result<LocationName, ResultCode> {
        log::debug!("Glue::Time::TimeZoneService::GetDeviceLocationName called");
        self.wrapped_service
            .lock()
            .unwrap()
            .get_device_location_name()
    }

    /// SetDeviceLocationName (cmd 1).
    ///
    /// Corresponds to `TimeZoneService::SetDeviceLocationName` in upstream.
    /// Upstream validates the name against TimeZoneBinary, loads the rule,
    /// updates the PSC service, updates filesystem time, and saves to settings.
    pub fn set_device_location_name(&self, name: &LocationName) -> ResultCode {
        log::debug!("Glue::Time::TimeZoneService::SetDeviceLocationName called");
        if !self.can_write_timezone_device_location {
            return RESULT_PERMISSION_DENIED;
        }

        let tz_binary = self.time_zone_binary.lock().unwrap();
        if !tz_binary.is_valid(name) {
            return RESULT_TIME_ZONE_NOT_FOUND;
        }
        drop(tz_binary);

        let _lock = self.mutex.lock().unwrap();

        let mut tz_binary = self.time_zone_binary.lock().unwrap();
        let binary = match tz_binary.get_time_zone_rule(name) {
            Ok(b) => b,
            Err(rc) => return rc,
        };
        drop(tz_binary);

        let wrapped = self.wrapped_service.lock().unwrap();
        let rc = wrapped.set_device_location_name_with_time_zone_rule(name, &binary);
        if rc.is_error() {
            return rc;
        }

        self.file_timestamp_worker
            .lock()
            .unwrap()
            .set_filesystem_posix_time();

        let (persisted_name, updated_time) =
            match wrapped.get_device_location_name_and_updated_time() {
                Ok(values) => values,
                Err(rc) => return rc,
            };
        drop(wrapped);

        self.set_sys
            .set_device_time_zone_location_name(persisted_name);
        self.set_sys
            .set_device_time_zone_location_updated_time(updated_time);

        if let Some(operation_event) = self.operation_event.lock().unwrap().as_ref() {
            operation_event.signal();
        }

        RESULT_SUCCESS
    }

    /// GetTotalLocationNameCount (cmd 2).
    ///
    /// Corresponds to `TimeZoneService::GetTotalLocationNameCount` in upstream.
    /// Delegates to `m_wrapped_service->GetTotalLocationNameCount`.
    pub fn get_total_location_name_count(&self) -> Result<u32, ResultCode> {
        log::debug!("Glue::Time::TimeZoneService::GetTotalLocationNameCount called");
        self.wrapped_service
            .lock()
            .unwrap()
            .get_total_location_name_count()
    }

    /// LoadLocationNameList (cmd 3).
    ///
    /// Corresponds to `TimeZoneService::LoadLocationNameList` in upstream.
    /// Upstream delegates to `m_time_zone_binary.GetTimeZoneLocationList`.
    pub fn load_location_name_list(
        &self,
        index: u32,
        max_names: usize,
    ) -> Result<Vec<LocationName>, ResultCode> {
        log::debug!("Glue::Time::TimeZoneService::LoadLocationNameList called");
        let _lock = self.mutex.lock().unwrap();
        let mut tz_binary = self.time_zone_binary.lock().unwrap();
        tz_binary.get_time_zone_location_list(max_names, index)
    }

    /// LoadTimeZoneRule (cmd 4).
    ///
    /// Corresponds to `TimeZoneService::LoadTimeZoneRule` in upstream.
    /// Upstream loads the binary from TimeZoneBinary and calls
    /// `m_wrapped_service->ParseTimeZoneBinary(out_rule, binary)`.
    pub fn load_time_zone_rule(&self, name: &LocationName) -> Result<TzRule, ResultCode> {
        log::debug!("Glue::Time::TimeZoneService::LoadTimeZoneRule called");
        let _lock = self.mutex.lock().unwrap();
        let mut tz_binary = self.time_zone_binary.lock().unwrap();
        let binary = tz_binary.get_time_zone_rule(name)?;
        drop(tz_binary);

        let wrapped = self.wrapped_service.lock().unwrap();
        let mut rule = TzRule::default();
        let rc = wrapped.parse_time_zone_binary(&mut rule, &binary);
        if rc.is_error() {
            return Err(rc);
        }
        Ok(rule)
    }

    /// GetTimeZoneRuleVersion (cmd 5).
    ///
    /// Corresponds to `TimeZoneService::GetTimeZoneRuleVersion` in upstream.
    /// Delegates to `m_wrapped_service->GetTimeZoneRuleVersion`.
    pub fn get_time_zone_rule_version(&self) -> Result<RuleVersion, ResultCode> {
        log::debug!("Glue::Time::TimeZoneService::GetTimeZoneRuleVersion called");
        self.wrapped_service
            .lock()
            .unwrap()
            .get_time_zone_rule_version()
    }

    /// GetDeviceLocationNameAndUpdatedTime (cmd 6).
    ///
    /// Corresponds to `TimeZoneService::GetDeviceLocationNameAndUpdatedTime` in upstream.
    /// Delegates to `m_wrapped_service->GetDeviceLocationNameAndUpdatedTime`.
    pub fn get_device_location_name_and_updated_time(
        &self,
    ) -> Result<(LocationName, SteadyClockTimePoint), ResultCode> {
        log::debug!("Glue::Time::TimeZoneService::GetDeviceLocationNameAndUpdatedTime called");
        self.wrapped_service
            .lock()
            .unwrap()
            .get_device_location_name_and_updated_time()
    }

    /// ToCalendarTime (cmd 100).
    ///
    /// Corresponds to `TimeZoneService::ToCalendarTime` in upstream.
    /// Delegates to `m_wrapped_service->ToCalendarTime`.
    pub fn to_calendar_time(
        &self,
        time: i64,
        rule: &TzRule,
    ) -> Result<(CalendarTime, CalendarAdditionalInfo), ResultCode> {
        log::debug!("Glue::Time::TimeZoneService::ToCalendarTime: time={}", time);
        self.wrapped_service
            .lock()
            .unwrap()
            .to_calendar_time(time, rule)
    }

    /// ToCalendarTimeWithMyRule (cmd 101).
    ///
    /// Corresponds to `TimeZoneService::ToCalendarTimeWithMyRule` in upstream.
    /// Delegates to `m_wrapped_service->ToCalendarTimeWithMyRule`.
    pub fn to_calendar_time_with_my_rule(
        &self,
        time: i64,
    ) -> Result<(CalendarTime, CalendarAdditionalInfo), ResultCode> {
        log::debug!(
            "Glue::Time::TimeZoneService::ToCalendarTimeWithMyRule: time={}",
            time
        );
        self.wrapped_service
            .lock()
            .unwrap()
            .to_calendar_time_with_my_rule(time)
    }

    /// SetDeviceLocationNameWithTimeZoneRule (cmd 7).
    ///
    /// Corresponds to `TimeZoneService::SetDeviceLocationNameWithTimeZoneRule` in upstream.
    pub fn set_device_location_name_with_time_zone_rule(
        &self,
        _location_name: &LocationName,
        _binary: &[u8],
    ) -> ResultCode {
        log::debug!(
            "Glue::Time::TimeZoneService::SetDeviceLocationNameWithTimeZoneRule called. Not implemented!"
        );
        if !self.can_write_timezone_device_location {
            return RESULT_PERMISSION_DENIED;
        }
        RESULT_NOT_IMPLEMENTED
    }

    /// ParseTimeZoneBinary (cmd 8).
    ///
    /// Corresponds to `TimeZoneService::ParseTimeZoneBinary` in upstream.
    pub fn parse_time_zone_binary(&self, _rule: &mut TzRule, _binary: &[u8]) -> ResultCode {
        log::debug!("Glue::Time::TimeZoneService::ParseTimeZoneBinary called. Not implemented!");
        RESULT_NOT_IMPLEMENTED
    }

    /// ToPosixTime (cmd 201).
    ///
    /// Corresponds to `TimeZoneService::ToPosixTime` in upstream.
    /// Delegates to `m_wrapped_service->ToPosixTime`.
    pub fn to_posix_time(
        &self,
        calendar: &CalendarTime,
        rule: &TzRule,
        max_times: usize,
    ) -> Result<(u32, Vec<i64>), ResultCode> {
        log::debug!("Glue::Time::TimeZoneService::ToPosixTime called");
        let mut out_times = vec![0i64; max_times];
        let count =
            self.wrapped_service
                .lock()
                .unwrap()
                .to_posix_time(&mut out_times, calendar, rule)?;
        Ok((count, out_times))
    }

    /// ToPosixTimeWithMyRule (cmd 202).
    ///
    /// Corresponds to `TimeZoneService::ToPosixTimeWithMyRule` in upstream.
    /// Delegates to `m_wrapped_service->ToPosixTimeWithMyRule`.
    pub fn to_posix_time_with_my_rule(
        &self,
        calendar: &CalendarTime,
        max_times: usize,
    ) -> Result<(u32, Vec<i64>), ResultCode> {
        log::debug!("Glue::Time::TimeZoneService::ToPosixTimeWithMyRule called");
        let mut out_times = vec![0i64; max_times];
        let count = self
            .wrapped_service
            .lock()
            .unwrap()
            .to_posix_time_with_my_rule(&mut out_times, calendar)?;
        Ok((count, out_times))
    }

    pub fn get_device_location_name_operation_event_readable_handle(
        &self,
        ctx: &HLERequestContext,
    ) -> Result<u32, ResultCode> {
        log::debug!(
            "Glue::Time::TimeZoneService::GetDeviceLocationNameOperationEventReadableHandle called"
        );
        let operation_event = {
            let mut operation_event = self.operation_event.lock().unwrap();
            operation_event
                .get_or_insert_with(OperationEvent::new)
                .clone()
        };
        operation_event
            .get_event()
            .copy_handle(ctx)
            .ok_or(RESULT_FAILED)
    }

    fn get_device_location_name_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        match service.get_device_location_name() {
            Ok(name) => {
                let mut rb = ResponseBuilder::new(
                    ctx,
                    2 + (core::mem::size_of::<LocationName>() / 4) as u32,
                    0,
                    0,
                );
                rb.push_result(RESULT_SUCCESS);
                rb.push_raw(&name);
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    fn get_total_location_name_count_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        match service.get_total_location_name_count() {
            Ok(count) => {
                let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u32(count);
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    fn set_device_location_name_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let location_name = Self::pop_location_name(&mut rp);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(service.set_device_location_name(&location_name));
    }

    fn load_location_name_list_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let index = rp.pop_u32();
        let max_names = ctx.get_write_buffer_size(0) / core::mem::size_of::<LocationName>();
        match service.load_location_name_list(index, max_names) {
            Ok(names) => {
                let byte_len = core::mem::size_of_val(names.as_slice());
                let bytes =
                    unsafe { core::slice::from_raw_parts(names.as_ptr() as *const u8, byte_len) };
                ctx.write_buffer(bytes, 0);
                let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u32(names.len() as u32);
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    fn load_time_zone_rule_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let location_name = Self::pop_location_name(&mut rp);
        match service.load_time_zone_rule(&location_name) {
            Ok(rule) => {
                ctx.write_buffer(rule.as_bytes(), 0);
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(RESULT_SUCCESS);
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    fn get_time_zone_rule_version_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        match service.get_time_zone_rule_version() {
            Ok(version) => {
                let mut rb = ResponseBuilder::new(
                    ctx,
                    2 + (core::mem::size_of::<RuleVersion>() / 4) as u32,
                    0,
                    0,
                );
                rb.push_result(RESULT_SUCCESS);
                rb.push_raw(&version);
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    fn get_device_location_name_and_updated_time_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        match service.get_device_location_name_and_updated_time() {
            Ok((name, time_point)) => {
                let words = 2
                    + (core::mem::size_of::<LocationName>() / 4) as u32
                    + (core::mem::size_of::<SteadyClockTimePoint>() / 4) as u32;
                let mut rb = ResponseBuilder::new(ctx, words, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_raw(&name);
                rb.push_raw(&time_point);
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    fn to_calendar_time_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let time = rp.pop_i64();
        let rule = TzRule::from_prefix_bytes(&ctx.read_buffer(0));
        match service.to_calendar_time(time, &rule) {
            Ok((calendar, additional)) => {
                let words = 2
                    + (core::mem::size_of::<CalendarTime>() / 4) as u32
                    + (core::mem::size_of::<CalendarAdditionalInfo>() / 4) as u32;
                let mut rb = ResponseBuilder::new(ctx, words, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_raw(&calendar);
                rb.push_raw(&additional);
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    fn to_calendar_time_with_my_rule_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let time = rp.pop_i64();
        match service.to_calendar_time_with_my_rule(time) {
            Ok((calendar, additional)) => {
                let words = 2
                    + (core::mem::size_of::<CalendarTime>() / 4) as u32
                    + (core::mem::size_of::<CalendarAdditionalInfo>() / 4) as u32;
                let mut rb = ResponseBuilder::new(ctx, words, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_raw(&calendar);
                rb.push_raw(&additional);
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    fn set_device_location_name_with_time_zone_rule_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let location_name = Self::pop_location_name(&mut rp);
        let binary = ctx.read_buffer(0);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(
            service.set_device_location_name_with_time_zone_rule(&location_name, &binary),
        );
    }

    fn parse_time_zone_binary_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let binary = ctx.read_buffer(0);
        let mut rule = TzRule::default();
        let rc = service.parse_time_zone_binary(&mut rule, &binary);
        if rc.is_success() {
            ctx.write_buffer(rule.as_bytes(), 0);
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_SUCCESS);
        } else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(rc);
        }
    }

    fn get_device_location_name_operation_event_readable_handle_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        match service.get_device_location_name_operation_event_readable_handle(ctx) {
            Ok(handle) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_copy_objects(handle);
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    fn to_posix_time_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let calendar_time = rp.pop_raw::<CalendarTime>();
        let rule = TzRule::from_prefix_bytes(&ctx.read_buffer(0));
        let max_times = ctx.get_write_buffer_size(0) / core::mem::size_of::<i64>();
        match service.to_posix_time(&calendar_time, &rule, max_times) {
            Ok((count, out_times)) => {
                let byte_len = (count as usize).min(out_times.len()) * core::mem::size_of::<i64>();
                let bytes = unsafe {
                    core::slice::from_raw_parts(out_times.as_ptr() as *const u8, byte_len)
                };
                ctx.write_buffer(bytes, 0);
                let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u32(count);
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    fn to_posix_time_with_my_rule_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let calendar_time = rp.pop_raw::<CalendarTime>();
        let max_times = ctx.get_write_buffer_size(0) / core::mem::size_of::<i64>();
        match service.to_posix_time_with_my_rule(&calendar_time, max_times) {
            Ok((count, out_times)) => {
                let byte_len = (count as usize).min(out_times.len()) * core::mem::size_of::<i64>();
                let bytes = unsafe {
                    core::slice::from_raw_parts(out_times.as_ptr() as *const u8, byte_len)
                };
                ctx.write_buffer(bytes, 0);
                let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u32(count);
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }
}

impl SessionRequestHandler for TimeZoneService {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ITimeZoneService"
    }
}

impl ServiceFramework for TimeZoneService {
    fn get_service_name(&self) -> &str {
        "ITimeZoneService"
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
    use crate::core::{System, SystemRef};
    use crate::hle::service::psc::time::time_zone_service::TimeZoneService as PscTimeZoneService;

    fn isolated_service(can_write_timezone_device_location: bool) -> TimeZoneService {
        TimeZoneService::with_wrapped_and_set_sys(
            can_write_timezone_device_location,
            PscTimeZoneService::new(can_write_timezone_device_location),
            Arc::new(Mutex::new(FileTimestampWorker::new())),
            Arc::new(Mutex::new(TimeZoneBinary::new(SystemRef::null()))),
            Arc::new(SystemSettingsService::new_for_test()),
        )
    }

    #[test]
    fn exercised_handlers_are_registered() {
        let service = isolated_service(false);
        assert!(service
            .handlers()
            .get(&commands::GET_DEVICE_LOCATION_NAME)
            .and_then(|f| f.handler_callback)
            .is_some());
        assert!(service
            .handlers()
            .get(&commands::TO_CALENDAR_TIME_WITH_MY_RULE)
            .and_then(|f| f.handler_callback)
            .is_some());
        assert!(service
            .handlers()
            .get(&commands::LOAD_TIME_ZONE_RULE)
            .and_then(|f| f.handler_callback)
            .is_some());
        assert!(service
            .handlers()
            .get(&commands::TO_POSIX_TIME_WITH_MY_RULE)
            .and_then(|f| f.handler_callback)
            .is_some());
    }

    #[test]
    fn wrapped_service_retains_manager_owned_resources() {
        let file_timestamp_worker = Arc::new(Mutex::new(FileTimestampWorker::new()));
        let time_zone_binary = Arc::new(Mutex::new(TimeZoneBinary::new(
            crate::core::SystemRef::null(),
        )));
        let set_sys = Arc::new(SystemSettingsService::new_for_test());
        let service = TimeZoneService::with_wrapped_and_set_sys(
            false,
            PscTimeZoneService::new(false),
            Arc::clone(&file_timestamp_worker),
            Arc::clone(&time_zone_binary),
            Arc::clone(&set_sys),
        );

        assert!(Arc::ptr_eq(&set_sys, &service.set_sys));
        assert!(Arc::ptr_eq(
            &file_timestamp_worker,
            &service.file_timestamp_worker
        ));
        assert!(Arc::ptr_eq(&time_zone_binary, &service.time_zone_binary));
    }

    #[test]
    fn location_update_persists_settings_and_signals_operation_event() {
        std::thread::Builder::new()
            .name("Glue TimeZoneService parity test".to_string())
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let system = Box::new(System::new_for_test());
                let system_ref = SystemRef::from_ref(system.as_ref());
                let set_sys = Arc::new(SystemSettingsService::new_for_test());
                let set_sys_factory_owner = Arc::clone(&set_sys);
                let service_manager = system.service_manager().unwrap();
                assert_eq!(
                    service_manager.lock().unwrap().register_service(
                        "set:sys".to_string(),
                        64,
                        Box::new(move || -> SessionRequestHandlerPtr {
                            set_sys_factory_owner.clone()
                        }),
                    ),
                    RESULT_SUCCESS
                );

                let resolved = TimeZoneService::get_set_sys_service(system_ref);
                assert!(Arc::ptr_eq(&set_sys, &resolved));

                let mut time_zone_binary = TimeZoneBinary::new(system_ref);
                assert_eq!(time_zone_binary.mount(), RESULT_SUCCESS);
                let psc_time = Arc::new(Mutex::new(
                    crate::hle::service::psc::time::manager::TimeManager::new(Box::new(|| {
                        42_000_000_000
                    })),
                ));
                psc_time.lock().unwrap().time_zone.set_initialized();
                let service = TimeZoneService::with_wrapped(
                    system_ref,
                    true,
                    PscTimeZoneService::with_time_manager(true, psc_time),
                    Arc::new(Mutex::new(FileTimestampWorker::new())),
                    Arc::new(Mutex::new(time_zone_binary)),
                );
                assert!(Arc::ptr_eq(&set_sys, &service.set_sys));

                let operation_event = OperationEvent::new();
                *service.operation_event.lock().unwrap() = Some(operation_event.clone());
                assert!(!operation_event.get_event().is_signaled());

                let mut name = [0u8; 0x24];
                name[..7].copy_from_slice(b"Etc/GMT");
                assert_eq!(service.set_device_location_name(&name), RESULT_SUCCESS);

                assert_eq!(service.load_location_name_list(0, 1).unwrap().len(), 1);
                let calendar = CalendarTime {
                    year: 2024,
                    month: 1,
                    day: 1,
                    ..CalendarTime::default()
                };
                let utc_rule = service.load_time_zone_rule(&name).unwrap();
                let (count, out_times) = service.to_posix_time(&calendar, &utc_rule, 5).unwrap();
                assert_eq!(count, 1);
                assert_eq!(out_times.len(), 5);
                assert_eq!(out_times[0], 1_704_067_200);

                let (persisted_name, persisted_time) =
                    service.get_device_location_name_and_updated_time().unwrap();
                assert_eq!(persisted_time.time_point, 42);
                assert_eq!(set_sys.get_device_time_zone_location_name(), persisted_name);
                assert_eq!(
                    set_sys.get_device_time_zone_location_updated_time(),
                    persisted_time
                );
                assert!(operation_event.get_event().is_signaled());
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/psc/time/time_zone_service.h/.cpp
//!
//! ITimeZoneService: provides timezone queries and conversions.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::clocks::steady_clock_core;
use super::common::{
    CalendarAdditionalInfo, CalendarTime, LocationName, RuleVersion, SteadyClockTimePoint,
};
use super::errors::{RESULT_NOT_IMPLEMENTED, RESULT_PERMISSION_DENIED};
use super::manager::TimeManager;
use super::time_zone::TzRule;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command IDs for ITimeZoneService.
///
/// Corresponds to the function table in upstream time_zone_service.cpp constructor.
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

/// PSC TimeZoneService.
///
/// Corresponds to `PSC::Time::TimeZoneService` in upstream time_zone_service.h.
pub struct TimeZoneService {
    /// Shared owner of upstream `m_clock_core` and `m_time_zone` references.
    time: Arc<Mutex<TimeManager>>,
    can_write_timezone_device_location: bool,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl TimeZoneService {
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

    pub fn new(can_write_timezone_device_location: bool) -> Self {
        Self::with_time_manager(
            can_write_timezone_device_location,
            Arc::new(Mutex::new(TimeManager::new_default())),
        )
    }

    /// Create with the manager that owns upstream `m_clock_core` and `m_time_zone`.
    pub fn with_time_manager(
        can_write_timezone_device_location: bool,
        time: Arc<Mutex<TimeManager>>,
    ) -> Self {
        Self {
            time,
            can_write_timezone_device_location,
            handlers: Self::build_handlers(),
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// GetDeviceLocationName (cmd 0).
    pub fn get_device_location_name(&self) -> Result<LocationName, ResultCode> {
        log::debug!("PSC::Time::TimeZoneService::GetDeviceLocationName called");
        self.time.lock().unwrap().time_zone.get_location_name()
    }

    /// SetDeviceLocationName (cmd 1).
    pub fn set_device_location_name(&self, _location_name: &LocationName) -> ResultCode {
        log::debug!("PSC::Time::TimeZoneService::SetDeviceLocationName called. Not implemented!");
        if !self.can_write_timezone_device_location {
            return RESULT_PERMISSION_DENIED;
        }
        RESULT_NOT_IMPLEMENTED
    }

    /// GetTotalLocationNameCount (cmd 2).
    pub fn get_total_location_name_count(&self) -> Result<u32, ResultCode> {
        log::debug!("PSC::Time::TimeZoneService::GetTotalLocationNameCount called");
        self.time
            .lock()
            .unwrap()
            .time_zone
            .get_total_location_count()
    }

    /// LoadLocationNameList (cmd 3).
    pub fn load_location_name_list(&self) -> ResultCode {
        log::debug!("PSC::Time::TimeZoneService::LoadLocationNameList called. Not implemented!");
        RESULT_NOT_IMPLEMENTED
    }

    /// LoadTimeZoneRule (cmd 4).
    pub fn load_time_zone_rule(&self) -> ResultCode {
        log::debug!("PSC::Time::TimeZoneService::LoadTimeZoneRule called. Not implemented!");
        RESULT_NOT_IMPLEMENTED
    }

    /// GetTimeZoneRuleVersion (cmd 5).
    pub fn get_time_zone_rule_version(&self) -> Result<RuleVersion, ResultCode> {
        log::debug!("PSC::Time::TimeZoneService::GetTimeZoneRuleVersion called");
        self.time.lock().unwrap().time_zone.get_rule_version()
    }

    /// GetDeviceLocationNameAndUpdatedTime (cmd 6).
    pub fn get_device_location_name_and_updated_time(
        &self,
    ) -> Result<(LocationName, SteadyClockTimePoint), ResultCode> {
        log::debug!("PSC::Time::TimeZoneService::GetDeviceLocationNameAndUpdatedTime called");
        let time = self.time.lock().unwrap();
        let name = time.time_zone.get_location_name()?;
        let time_point = time.time_zone.get_time_point()?;
        Ok((name, time_point))
    }

    /// SetDeviceLocationNameWithTimeZoneRule (cmd 7).
    pub fn set_device_location_name_with_time_zone_rule(
        &self,
        location_name: &LocationName,
        binary: &[u8],
    ) -> ResultCode {
        log::debug!("PSC::Time::TimeZoneService::SetDeviceLocationNameWithTimeZoneRule called");
        if !self.can_write_timezone_device_location {
            return RESULT_PERMISSION_DENIED;
        }
        let mut time = self.time.lock().unwrap();
        let rc = time.time_zone.parse_binary(location_name, binary);
        if rc.is_error() {
            return rc;
        }
        let time_point =
            match steady_clock_core::get_current_time_point(&time.standard_steady_clock) {
                Ok(time_point) => time_point,
                Err(rc) => return rc,
            };
        time.time_zone.set_time_point(&time_point);
        RESULT_SUCCESS
    }

    /// ParseTimeZoneBinary (cmd 8).
    pub fn parse_time_zone_binary(&self, rule: &mut TzRule, binary: &[u8]) -> ResultCode {
        log::debug!("PSC::Time::TimeZoneService::ParseTimeZoneBinary called");
        self.time
            .lock()
            .unwrap()
            .time_zone
            .parse_binary_into(rule, binary)
    }

    /// GetDeviceLocationNameOperationEventReadableHandle (cmd 20).
    pub fn get_device_location_name_operation_event_readable_handle(&self) -> ResultCode {
        log::debug!("PSC::Time::TimeZoneService::GetDeviceLocationNameOperationEventReadableHandle called. Not implemented!");
        RESULT_NOT_IMPLEMENTED
    }

    /// ToCalendarTime (cmd 100).
    pub fn to_calendar_time(
        &self,
        time: i64,
        rule: &TzRule,
    ) -> Result<(CalendarTime, CalendarAdditionalInfo), ResultCode> {
        log::debug!("PSC::Time::TimeZoneService::ToCalendarTime: time={}", time);
        self.time
            .lock()
            .unwrap()
            .time_zone
            .to_calendar_time(time, rule)
    }

    /// ToCalendarTimeWithMyRule (cmd 101).
    pub fn to_calendar_time_with_my_rule(
        &self,
        time: i64,
    ) -> Result<(CalendarTime, CalendarAdditionalInfo), ResultCode> {
        log::debug!(
            "PSC::Time::TimeZoneService::ToCalendarTimeWithMyRule: time={}",
            time
        );
        self.time
            .lock()
            .unwrap()
            .time_zone
            .to_calendar_time_with_my_rule(time)
    }

    /// ToPosixTime (cmd 201).
    pub fn to_posix_time(
        &self,
        out_times: &mut [i64],
        calendar_time: &CalendarTime,
        rule: &TzRule,
    ) -> Result<u32, ResultCode> {
        log::debug!("PSC::Time::TimeZoneService::ToPosixTime called");
        self.time
            .lock()
            .unwrap()
            .time_zone
            .to_posix_time(out_times, calendar_time, rule)
    }

    /// ToPosixTimeWithMyRule (cmd 202).
    pub fn to_posix_time_with_my_rule(
        &self,
        out_times: &mut [i64],
        calendar_time: &CalendarTime,
    ) -> Result<u32, ResultCode> {
        log::debug!("PSC::Time::TimeZoneService::ToPosixTimeWithMyRule called");
        self.time
            .lock()
            .unwrap()
            .time_zone
            .to_posix_time_with_my_rule(out_times, calendar_time)
    }

    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
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

    fn set_device_location_name_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let location_name = Self::pop_location_name(&mut rp);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(service.set_device_location_name(&location_name));
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

    fn load_location_name_list_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let _index = rp.pop_u32();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(service.load_location_name_list());
    }

    fn load_time_zone_rule_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let _location_name = Self::pop_location_name(&mut rp);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(service.load_time_zone_rule());
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
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(service.get_device_location_name_operation_event_readable_handle());
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

    fn to_posix_time_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let calendar_time = rp.pop_raw::<CalendarTime>();
        let rule = TzRule::from_prefix_bytes(&ctx.read_buffer(0));
        let buffer_len = ctx.get_write_buffer_size(0) / core::mem::size_of::<i64>();
        let mut out_times = vec![0i64; buffer_len];
        match service.to_posix_time(&mut out_times, &calendar_time, &rule) {
            Ok(count) => {
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
        let buffer_len = ctx.get_write_buffer_size(0) / core::mem::size_of::<i64>();
        let mut out_times = vec![0i64; buffer_len];
        match service.to_posix_time_with_my_rule(&mut out_times, &calendar_time) {
            Ok(count) => {
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

    #[test]
    fn exercised_handlers_are_registered() {
        let service = TimeZoneService::new(false);
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
            .get(&commands::TO_POSIX_TIME_WITH_MY_RULE)
            .and_then(|f| f.handler_callback)
            .is_some());
    }

    #[test]
    fn services_share_timezone_and_capture_the_standard_steady_clock() {
        std::thread::Builder::new()
            .name("PSC TimeZoneService ownership test".to_string())
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let system = Box::new(crate::core::System::new_for_test());
                let system_ref = crate::core::SystemRef::from_ref(system.as_ref());
                let mut binary =
                    crate::hle::service::glue::time::time_zone_binary::TimeZoneBinary::new(
                        system_ref,
                    );
                assert_eq!(binary.mount(), RESULT_SUCCESS);

                let mut name = [0u8; 0x24];
                name[..7].copy_from_slice(b"Etc/GMT");
                let rule = binary.get_time_zone_rule(&name).unwrap();
                let time = Arc::new(Mutex::new(TimeManager::new(Box::new(|| 42_000_000_000))));
                time.lock().unwrap().time_zone.set_initialized();
                let writer = TimeZoneService::with_time_manager(true, Arc::clone(&time));
                let reader = TimeZoneService::with_time_manager(false, Arc::clone(&time));

                assert_eq!(
                    writer.set_device_location_name_with_time_zone_rule(&name, &rule),
                    RESULT_SUCCESS
                );

                let (stored_name, time_point) =
                    reader.get_device_location_name_and_updated_time().unwrap();
                assert_eq!(stored_name, name);
                assert_eq!(time_point.time_point, 42);
                assert_eq!(time_point.clock_source_id, [0; 16]);
                assert!(Arc::ptr_eq(&writer.time, &reader.time));
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

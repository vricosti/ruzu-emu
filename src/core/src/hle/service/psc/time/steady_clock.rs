// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/psc/time/steady_clock.h
//! Port of zuyu/src/core/hle/service/psc/time/steady_clock.cpp
//!
//! ISteadyClock: provides steady clock time point queries.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::clocks::steady_clock_core::{self, SteadyClockCoreImpl};
use super::common::SteadyClockTimePoint;
use super::errors::{RESULT_CLOCK_UNINITIALIZED, RESULT_PERMISSION_DENIED};
use super::manager::TimeManager;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command IDs for ISteadyClock.
///
/// Corresponds to the function table in upstream steady_clock.cpp constructor.
pub mod commands {
    pub const GET_CURRENT_TIME_POINT: u32 = 0;
    pub const GET_TEST_OFFSET: u32 = 2;
    pub const SET_TEST_OFFSET: u32 = 3;
    pub const GET_RTC_VALUE: u32 = 100;
    pub const IS_RTC_RESET_DETECTED: u32 = 101;
    pub const GET_SETUP_RESULT_VALUE: u32 = 102;
    pub const GET_INTERNAL_OFFSET: u32 = 200;
}

/// Live backing object for `SteadyClock`.
///
/// Upstream `SteadyClock` stores a reference to `TimeManager::m_standard_steady_clock`.
/// Keep the same lifetime shape here so every IPC observes the current clock core
/// instead of a snapshot captured when the sub-service was created.
pub trait SteadyClockBackend: Send + Sync {
    fn is_initialized(&self) -> bool;
    fn get_current_time_point(&self) -> Result<SteadyClockTimePoint, ResultCode>;
    fn get_test_offset(&self) -> i64;
    fn set_test_offset(&self, offset: i64);
    fn get_rtc_value(&self) -> Result<i64, ResultCode>;
    fn is_rtc_reset_detected(&self) -> bool;
    fn get_setup_result_value(&self) -> ResultCode;
    fn get_internal_offset(&self) -> i64;
}

pub struct TimeManagerSteadyClockBackend {
    time_manager: Arc<Mutex<TimeManager>>,
}

impl TimeManagerSteadyClockBackend {
    pub fn new(time_manager: Arc<Mutex<TimeManager>>) -> Self {
        Self { time_manager }
    }
}

impl SteadyClockBackend for TimeManagerSteadyClockBackend {
    fn is_initialized(&self) -> bool {
        self.time_manager
            .lock()
            .unwrap()
            .standard_steady_clock
            .lock()
            .unwrap()
            .state
            .is_initialized()
    }

    fn get_current_time_point(&self) -> Result<SteadyClockTimePoint, ResultCode> {
        let time = self.time_manager.lock().unwrap();
        let clock = time.standard_steady_clock.lock().unwrap();
        steady_clock_core::get_current_time_point(&*clock)
    }

    fn get_test_offset(&self) -> i64 {
        self.time_manager
            .lock()
            .unwrap()
            .standard_steady_clock
            .lock()
            .unwrap()
            .get_test_offset_impl()
    }

    fn set_test_offset(&self, offset: i64) {
        self.time_manager
            .lock()
            .unwrap()
            .standard_steady_clock
            .lock()
            .unwrap()
            .set_test_offset_impl(offset);
    }

    fn get_rtc_value(&self) -> Result<i64, ResultCode> {
        self.time_manager
            .lock()
            .unwrap()
            .standard_steady_clock
            .lock()
            .unwrap()
            .get_rtc_value_impl()
    }

    fn is_rtc_reset_detected(&self) -> bool {
        self.time_manager
            .lock()
            .unwrap()
            .standard_steady_clock
            .lock()
            .unwrap()
            .state
            .is_reset_detected()
    }

    fn get_setup_result_value(&self) -> ResultCode {
        self.time_manager
            .lock()
            .unwrap()
            .standard_steady_clock
            .lock()
            .unwrap()
            .get_setup_result_value_impl()
    }

    fn get_internal_offset(&self) -> i64 {
        self.time_manager
            .lock()
            .unwrap()
            .standard_steady_clock
            .lock()
            .unwrap()
            .get_internal_offset_impl()
    }
}

/// SteadyClock service interface.
///
/// Corresponds to `SteadyClock` in upstream steady_clock.h.
pub struct SteadyClock {
    can_write_steady_clock: bool,
    can_write_uninitialized_clock: bool,
    backend: Option<Arc<dyn SteadyClockBackend>>,
    // State for the clock core
    initialized: bool,
    test_offset: Mutex<i64>,
    internal_offset: i64,
    current_time_point: SteadyClockTimePoint,
    setup_result: ResultCode,
    rtc_reset_detected: bool,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl SteadyClock {
    pub fn new(can_write_steady_clock: bool, can_write_uninitialized_clock: bool) -> Self {
        Self::with_state(
            can_write_steady_clock,
            can_write_uninitialized_clock,
            false,
            0,
            0,
            SteadyClockTimePoint::default(),
            RESULT_SUCCESS,
            false,
        )
    }

    pub fn with_backend(
        can_write_steady_clock: bool,
        can_write_uninitialized_clock: bool,
        backend: Arc<dyn SteadyClockBackend>,
    ) -> Self {
        let mut this = Self::new(can_write_steady_clock, can_write_uninitialized_clock);
        this.backend = Some(backend);
        this
    }

    pub fn with_state(
        can_write_steady_clock: bool,
        can_write_uninitialized_clock: bool,
        initialized: bool,
        test_offset: i64,
        internal_offset: i64,
        current_time_point: SteadyClockTimePoint,
        setup_result: ResultCode,
        rtc_reset_detected: bool,
    ) -> Self {
        let handlers = build_handler_map(&[
            (
                commands::GET_CURRENT_TIME_POINT,
                Some(Self::get_current_time_point_handler),
                "GetCurrentTimePoint",
            ),
            (
                commands::GET_TEST_OFFSET,
                Some(Self::get_test_offset_handler),
                "GetTestOffset",
            ),
            (
                commands::SET_TEST_OFFSET,
                Some(Self::set_test_offset_handler),
                "SetTestOffset",
            ),
            (
                commands::GET_RTC_VALUE,
                Some(Self::get_rtc_value_handler),
                "GetRtcValue",
            ),
            (
                commands::IS_RTC_RESET_DETECTED,
                Some(Self::is_rtc_reset_detected_handler),
                "IsRtcResetDetected",
            ),
            (
                commands::GET_SETUP_RESULT_VALUE,
                Some(Self::get_setup_result_value_handler),
                "GetSetupResultValue",
            ),
            (
                commands::GET_INTERNAL_OFFSET,
                Some(Self::get_internal_offset_handler),
                "GetInternalOffset",
            ),
        ]);
        Self {
            can_write_steady_clock,
            can_write_uninitialized_clock,
            backend: None,
            initialized,
            test_offset: Mutex::new(test_offset),
            internal_offset,
            current_time_point,
            setup_result,
            rtc_reset_detected,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// Mark this clock as initialized (called during system setup).
    pub fn set_initialized(&mut self, initialized: bool) {
        self.initialized = initialized;
    }

    fn is_backend_initialized(&self) -> bool {
        match &self.backend {
            Some(backend) => backend.is_initialized(),
            None => self.initialized,
        }
    }

    fn check_initialized(&self) -> ResultCode {
        if !self.can_write_uninitialized_clock && !self.is_backend_initialized() {
            return RESULT_CLOCK_UNINITIALIZED;
        }
        RESULT_SUCCESS
    }

    /// GetCurrentTimePoint (cmd 0).
    ///
    /// Corresponds to `SteadyClock::GetCurrentTimePoint` in upstream steady_clock.cpp.
    pub fn get_current_time_point(&self) -> Result<SteadyClockTimePoint, ResultCode> {
        let check = self.check_initialized();
        if check.is_error() {
            return Err(check);
        }
        let time_point = match &self.backend {
            Some(backend) => backend.get_current_time_point(),
            None => Ok(self.current_time_point),
        }?;
        log::debug!(
            "SteadyClock::GetCurrentTimePoint: time_point={}",
            time_point.time_point
        );
        Ok(time_point)
    }

    /// GetTestOffset (cmd 2).
    ///
    /// Corresponds to `SteadyClock::GetTestOffset` in upstream steady_clock.cpp.
    pub fn get_test_offset(&self) -> Result<i64, ResultCode> {
        let check = self.check_initialized();
        if check.is_error() {
            return Err(check);
        }
        let offset = match &self.backend {
            Some(backend) => backend.get_test_offset(),
            None => *self.test_offset.lock().unwrap(),
        };
        log::debug!("SteadyClock::GetTestOffset: test_offset={}", offset);
        Ok(offset)
    }

    /// SetTestOffset (cmd 3).
    ///
    /// Corresponds to `SteadyClock::SetTestOffset` in upstream steady_clock.cpp.
    pub fn set_test_offset(&self, test_offset: i64) -> ResultCode {
        log::debug!("SteadyClock::SetTestOffset: test_offset={}", test_offset);
        if !self.can_write_steady_clock {
            return RESULT_PERMISSION_DENIED;
        }
        let check = self.check_initialized();
        if check.is_error() {
            return check;
        }
        match &self.backend {
            Some(backend) => backend.set_test_offset(test_offset),
            None => *self.test_offset.lock().unwrap() = test_offset,
        }
        RESULT_SUCCESS
    }

    /// GetRtcValue (cmd 100).
    ///
    /// Corresponds to `SteadyClock::GetRtcValue` in upstream steady_clock.cpp.
    pub fn get_rtc_value(&self) -> Result<i64, ResultCode> {
        let check = self.check_initialized();
        if check.is_error() {
            return Err(check);
        }
        let time_s = match &self.backend {
            Some(backend) => backend.get_rtc_value()?,
            None => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
        };
        log::debug!("SteadyClock::GetRtcValue: rtc_value={}", time_s);
        Ok(time_s)
    }

    /// IsRtcResetDetected (cmd 101).
    ///
    /// Corresponds to `SteadyClock::IsRtcResetDetected` in upstream steady_clock.cpp.
    pub fn is_rtc_reset_detected(&self) -> Result<bool, ResultCode> {
        let check = self.check_initialized();
        if check.is_error() {
            return Err(check);
        }
        log::debug!(
            "SteadyClock::IsRtcResetDetected: is_detected={}",
            match &self.backend {
                Some(backend) => backend.is_rtc_reset_detected(),
                None => self.rtc_reset_detected,
            }
        );
        Ok(match &self.backend {
            Some(backend) => backend.is_rtc_reset_detected(),
            None => self.rtc_reset_detected,
        })
    }

    /// GetSetupResultValue (cmd 102).
    ///
    /// Corresponds to `SteadyClock::GetSetupResultValue` in upstream steady_clock.cpp.
    pub fn get_setup_result_value(&self) -> Result<ResultCode, ResultCode> {
        let check = self.check_initialized();
        if check.is_error() {
            return Err(check);
        }
        log::debug!(
            "SteadyClock::GetSetupResultValue: result={:08X}",
            match &self.backend {
                Some(backend) => backend.get_setup_result_value(),
                None => self.setup_result,
            }
            .get_inner_value()
        );
        Ok(match &self.backend {
            Some(backend) => backend.get_setup_result_value(),
            None => self.setup_result,
        })
    }

    /// GetInternalOffset (cmd 200).
    ///
    /// Corresponds to `SteadyClock::GetInternalOffset` in upstream steady_clock.cpp.
    pub fn get_internal_offset(&self) -> Result<i64, ResultCode> {
        let check = self.check_initialized();
        if check.is_error() {
            return Err(check);
        }
        log::debug!(
            "SteadyClock::GetInternalOffset: internal_offset={}",
            match &self.backend {
                Some(backend) => backend.get_internal_offset(),
                None => self.internal_offset,
            }
        );
        Ok(match &self.backend {
            Some(backend) => backend.get_internal_offset(),
            None => self.internal_offset,
        })
    }

    fn get_current_time_point_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        match service.get_current_time_point() {
            Ok(time_point) => {
                let words = (core::mem::size_of::<SteadyClockTimePoint>() / 4) as u32;
                let mut rb =
                    crate::hle::service::ipc_helpers::ResponseBuilder::new(ctx, 2 + words, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_raw(&time_point);
            }
            Err(rc) => {
                let mut rb = crate::hle::service::ipc_helpers::ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    fn get_test_offset_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        match service.get_test_offset() {
            Ok(offset) => {
                let mut rb = crate::hle::service::ipc_helpers::ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_i64(offset);
            }
            Err(rc) => {
                let mut rb = crate::hle::service::ipc_helpers::ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    fn set_test_offset_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let mut rp = crate::hle::service::ipc_helpers::RequestParser::new(ctx);
        let offset = rp.pop_i64();
        let mut rb = crate::hle::service::ipc_helpers::ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(service.set_test_offset(offset));
    }

    fn get_rtc_value_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        match service.get_rtc_value() {
            Ok(value) => {
                let mut rb = crate::hle::service::ipc_helpers::ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_i64(value);
            }
            Err(rc) => {
                let mut rb = crate::hle::service::ipc_helpers::ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    fn is_rtc_reset_detected_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        match service.is_rtc_reset_detected() {
            Ok(detected) => {
                let mut rb = crate::hle::service::ipc_helpers::ResponseBuilder::new(ctx, 3, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u32(if detected { 1 } else { 0 });
            }
            Err(rc) => {
                let mut rb = crate::hle::service::ipc_helpers::ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    fn get_setup_result_value_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        match service.get_setup_result_value() {
            Ok(result) => {
                let mut rb = crate::hle::service::ipc_helpers::ResponseBuilder::new(ctx, 3, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u32(result.get_inner_value());
            }
            Err(rc) => {
                let mut rb = crate::hle::service::ipc_helpers::ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    fn get_internal_offset_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        match service.get_internal_offset() {
            Ok(offset) => {
                let mut rb = crate::hle::service::ipc_helpers::ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_i64(offset);
            }
            Err(rc) => {
                let mut rb = crate::hle::service::ipc_helpers::ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }
}

impl SessionRequestHandler for SteadyClock {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ISteadyClock"
    }
}

impl ServiceFramework for SteadyClock {
    fn get_service_name(&self) -> &str {
        "ISteadyClock"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

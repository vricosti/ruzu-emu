// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/glue/time/static.h
//! Port of zuyu/src/core/hle/service/glue/time/static.cpp

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::psc::time::common::{
    ClockSnapshot, StaticServiceSetupInfo, SteadyClockTimePoint, SystemClockContext, TimeType,
};
use crate::hle::service::psc::time::r#static as psc_static;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

use super::file_timestamp_worker::FileTimestampWorker;
use super::manager::TimeManager as GlueTimeManager;
use super::standard_steady_clock_resource::StandardSteadyClockResource;
use super::time_zone::TimeZoneService;
use super::time_zone_binary::TimeZoneBinary;

/// IPC command IDs for `Glue::Time::StaticService`.
pub mod commands {
    pub const GET_STANDARD_USER_SYSTEM_CLOCK: u32 = 0;
    pub const GET_STANDARD_NETWORK_SYSTEM_CLOCK: u32 = 1;
    pub const GET_STANDARD_STEADY_CLOCK: u32 = 2;
    pub const GET_TIME_ZONE_SERVICE: u32 = 3;
    pub const GET_STANDARD_LOCAL_SYSTEM_CLOCK: u32 = 4;
    pub const GET_EPHEMERAL_NETWORK_SYSTEM_CLOCK: u32 = 5;
    pub const GET_SHARED_MEMORY_NATIVE_HANDLE: u32 = 20;
    pub const SET_STANDARD_STEADY_CLOCK_INTERNAL_OFFSET: u32 = 50;
    pub const GET_STANDARD_STEADY_CLOCK_RTC_VALUE: u32 = 51;
    pub const IS_STANDARD_USER_SYSTEM_CLOCK_AUTOMATIC_CORRECTION_ENABLED: u32 = 100;
    pub const SET_STANDARD_USER_SYSTEM_CLOCK_AUTOMATIC_CORRECTION_ENABLED: u32 = 101;
    pub const GET_STANDARD_USER_SYSTEM_CLOCK_INITIAL_YEAR: u32 = 102;
    pub const IS_STANDARD_NETWORK_SYSTEM_CLOCK_ACCURACY_SUFFICIENT: u32 = 200;
    pub const GET_STANDARD_USER_SYSTEM_CLOCK_AUTOMATIC_CORRECTION_UPDATED_TIME: u32 = 201;
    pub const CALCULATE_MONOTONIC_SYSTEM_CLOCK_BASE_TIME_POINT: u32 = 300;
    pub const GET_CLOCK_SNAPSHOT: u32 = 400;
    pub const GET_CLOCK_SNAPSHOT_FROM_SYSTEM_CLOCK_CONTEXT: u32 = 401;
    pub const CALCULATE_STANDARD_USER_SYSTEM_CLOCK_DIFFERENCE_BY_USER: u32 = 500;
    pub const CALCULATE_SPAN_BETWEEN_STANDARD_USER_SYSTEM_CLOCKS: u32 = 501;
}

/// `Glue::Time::StaticService`.
pub struct StaticService {
    pub service_name: String,
    system: crate::core::SystemRef,
    pub setup_info: StaticServiceSetupInfo,
    wrapped_service: Mutex<psc_static::StaticService>,
    file_timestamp_worker: Arc<Mutex<FileTimestampWorker>>,
    standard_steady_clock_resource: Arc<Mutex<StandardSteadyClockResource>>,
    time_zone_binary: Arc<Mutex<TimeZoneBinary>>,
    /// Cached handle returned by GetSharedMemoryNativeHandle.
    /// Once registered in the process handle table it does not change.
    shared_memory_handle: Mutex<Option<u32>>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl StaticService {
    pub fn new(
        system: crate::core::SystemRef,
        setup_info: StaticServiceSetupInfo,
        name: &str,
        time_manager: Arc<Mutex<GlueTimeManager>>,
    ) -> Self {
        log::debug!("Glue::Time::StaticService::new called for '{name}'");

        if setup_info.can_write_local_clock
            && setup_info.can_write_user_clock
            && !setup_info.can_write_network_clock
            && setup_info.can_write_timezone_device_location
            && !setup_info.can_write_steady_clock
            && !setup_info.can_write_uninitialized_clock
        {
            log::debug!("  -> Admin variant");
        } else if !setup_info.can_write_local_clock
            && !setup_info.can_write_user_clock
            && !setup_info.can_write_network_clock
            && !setup_info.can_write_timezone_device_location
            && !setup_info.can_write_steady_clock
            && !setup_info.can_write_uninitialized_clock
        {
            log::debug!("  -> User variant");
        } else if !setup_info.can_write_local_clock
            && !setup_info.can_write_user_clock
            && !setup_info.can_write_network_clock
            && !setup_info.can_write_timezone_device_location
            && setup_info.can_write_steady_clock
            && !setup_info.can_write_uninitialized_clock
        {
            log::debug!("  -> Repair variant");
        } else {
            log::error!("  -> Unknown setup_info variant");
        }

        let handlers = build_handler_map(&[
            (commands::GET_STANDARD_USER_SYSTEM_CLOCK, Some(StaticService::get_standard_user_system_clock_handler), "GetStandardUserSystemClock"),
            (commands::GET_STANDARD_NETWORK_SYSTEM_CLOCK, Some(StaticService::get_standard_network_system_clock_handler), "GetStandardNetworkSystemClock"),
            (commands::GET_STANDARD_STEADY_CLOCK, Some(StaticService::get_standard_steady_clock_handler), "GetStandardSteadyClock"),
            (commands::GET_TIME_ZONE_SERVICE, Some(StaticService::get_time_zone_service_handler), "GetTimeZoneService"),
            (commands::GET_STANDARD_LOCAL_SYSTEM_CLOCK, Some(StaticService::get_standard_local_system_clock_handler), "GetStandardLocalSystemClock"),
            (commands::GET_EPHEMERAL_NETWORK_SYSTEM_CLOCK, Some(StaticService::get_ephemeral_network_system_clock_handler), "GetEphemeralNetworkSystemClock"),
            (commands::GET_SHARED_MEMORY_NATIVE_HANDLE, Some(StaticService::get_shared_memory_native_handle_handler), "GetSharedMemoryNativeHandle"),
            (commands::SET_STANDARD_STEADY_CLOCK_INTERNAL_OFFSET, Some(StaticService::set_standard_steady_clock_internal_offset_handler), "SetStandardSteadyClockInternalOffset"),
            (commands::GET_STANDARD_STEADY_CLOCK_RTC_VALUE, Some(StaticService::get_standard_steady_clock_rtc_value_handler), "GetStandardSteadyClockRtcValue"),
            (commands::IS_STANDARD_USER_SYSTEM_CLOCK_AUTOMATIC_CORRECTION_ENABLED, Some(StaticService::is_standard_user_system_clock_automatic_correction_enabled_handler), "IsStandardUserSystemClockAutomaticCorrectionEnabled"),
            (commands::SET_STANDARD_USER_SYSTEM_CLOCK_AUTOMATIC_CORRECTION_ENABLED, Some(StaticService::set_standard_user_system_clock_automatic_correction_enabled_handler), "SetStandardUserSystemClockAutomaticCorrectionEnabled"),
            (commands::GET_STANDARD_USER_SYSTEM_CLOCK_INITIAL_YEAR, Some(StaticService::get_standard_user_system_clock_initial_year_handler), "GetStandardUserSystemClockInitialYear"),
            (commands::IS_STANDARD_NETWORK_SYSTEM_CLOCK_ACCURACY_SUFFICIENT, Some(StaticService::is_standard_network_system_clock_accuracy_sufficient_handler), "IsStandardNetworkSystemClockAccuracySufficient"),
            (commands::GET_STANDARD_USER_SYSTEM_CLOCK_AUTOMATIC_CORRECTION_UPDATED_TIME, Some(StaticService::get_standard_user_system_clock_automatic_correction_updated_time_handler), "GetStandardUserSystemClockAutomaticCorrectionUpdatedTime"),
            (commands::CALCULATE_MONOTONIC_SYSTEM_CLOCK_BASE_TIME_POINT, Some(StaticService::calculate_monotonic_system_clock_base_time_point_handler), "CalculateMonotonicSystemClockBaseTimePoint"),
            (commands::GET_CLOCK_SNAPSHOT, Some(StaticService::get_clock_snapshot_handler), "GetClockSnapshot"),
            (commands::GET_CLOCK_SNAPSHOT_FROM_SYSTEM_CLOCK_CONTEXT, Some(StaticService::get_clock_snapshot_from_system_clock_context_handler), "GetClockSnapshotFromSystemClockContext"),
            (commands::CALCULATE_STANDARD_USER_SYSTEM_CLOCK_DIFFERENCE_BY_USER, Some(StaticService::calculate_standard_user_system_clock_difference_by_user_handler), "CalculateStandardUserSystemClockDifferenceByUser"),
            (commands::CALCULATE_SPAN_BETWEEN_STANDARD_USER_SYSTEM_CLOCKS, Some(StaticService::calculate_span_between_handler), "CalculateSpanBetweenStandardUserSystemClocks"),
        ]);

        let (
            wrapped_service,
            file_timestamp_worker,
            standard_steady_clock_resource,
            time_zone_binary,
        ) = {
            let manager = time_manager.lock().unwrap();
            (
                manager.make_static_service(setup_info),
                Arc::clone(&manager.file_timestamp_worker),
                Arc::clone(&manager.steady_clock_resource),
                Arc::clone(&manager.time_zone_binary),
            )
        };

        Self {
            service_name: name.to_string(),
            system,
            setup_info,
            wrapped_service: Mutex::new(wrapped_service),
            file_timestamp_worker,
            standard_steady_clock_resource,
            time_zone_binary,
            shared_memory_handle: Mutex::new(None),
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    // =========================================================================
    // IPC handler callbacks (ServiceFramework pattern)
    // =========================================================================

    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const StaticService) }
    }

    /// Helper to create a sub-service and push it as a domain object or move handle.
    fn push_sub_service(
        ctx: &mut HLERequestContext,
        sub_service: std::sync::Arc<dyn SessionRequestHandler>,
    ) {
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(sub_service);
    }

    /// GetStandardUserSystemClock (cmd 0) handler.
    ///
    /// Upstream delegates to m_wrapped_service->GetStandardUserSystemClock().
    fn get_standard_user_system_clock_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        log::debug!("Glue::Time::StaticService::GetStandardUserSystemClock called");
        let sub = service
            .wrapped_service
            .lock()
            .unwrap()
            .get_standard_user_system_clock();
        Self::push_sub_service(ctx, std::sync::Arc::new(sub));
    }

    /// GetStandardNetworkSystemClock (cmd 1) handler.
    fn get_standard_network_system_clock_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        log::debug!("Glue::Time::StaticService::GetStandardNetworkSystemClock called");
        let sub = service
            .wrapped_service
            .lock()
            .unwrap()
            .get_standard_network_system_clock();
        Self::push_sub_service(ctx, std::sync::Arc::new(sub));
    }

    /// GetStandardSteadyClock (cmd 2) handler.
    fn get_standard_steady_clock_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        log::debug!("Glue::Time::StaticService::GetStandardSteadyClock called");
        let sub = service.get_standard_steady_clock();
        Self::push_sub_service(ctx, std::sync::Arc::new(sub));
    }

    /// GetTimeZoneService (cmd 3) handler.
    ///
    /// Unlike the other sub-service commands, upstream creates a Glue::Time::TimeZoneService
    /// rather than delegating to the PSC wrapped service.
    fn get_time_zone_service_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        log::debug!("Glue::Time::StaticService::GetTimeZoneService called");
        match service.get_time_zone_service() {
            Ok(sub) => {
                Self::push_sub_service(ctx, std::sync::Arc::new(sub));
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    /// GetStandardLocalSystemClock (cmd 4) handler.
    fn get_standard_local_system_clock_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        log::debug!("Glue::Time::StaticService::GetStandardLocalSystemClock called");
        let sub = service
            .wrapped_service
            .lock()
            .unwrap()
            .get_standard_local_system_clock();
        Self::push_sub_service(ctx, std::sync::Arc::new(sub));
    }

    /// GetEphemeralNetworkSystemClock (cmd 5) handler.
    fn get_ephemeral_network_system_clock_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        log::debug!("Glue::Time::StaticService::GetEphemeralNetworkSystemClock called");
        let sub = service
            .wrapped_service
            .lock()
            .unwrap()
            .get_ephemeral_network_system_clock();
        Self::push_sub_service(ctx, std::sync::Arc::new(sub));
    }

    /// GetSharedMemoryNativeHandle (cmd 20) handler.
    ///
    /// Returns a copy handle to the KSharedMemory backing the lock-free time
    /// shared memory region. On first call, registers the KSharedMemory in the
    /// process handle table and caches the handle.
    fn get_shared_memory_native_handle_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const StaticService) };

        // Check if we already have a cached handle.
        let mut cached = service.shared_memory_handle.lock().unwrap();
        if let Some(handle) = *cached {
            log::debug!(
                "Glue::Time::StaticService::GetSharedMemoryNativeHandle -> cached handle={:#x}",
                handle
            );
            let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
            rb.push_result(RESULT_SUCCESS);
            rb.push_copy_objects(handle);
            return;
        }

        // First call: register the KSharedMemory in the process handle table.
        let handle = (|| -> Option<u32> {
            let thread = ctx.get_thread()?;
            let thread_guard = thread.lock().unwrap();
            let parent = thread_guard.parent.as_ref()?.upgrade()?;
            let mut process = parent.lock().unwrap();

            static NEXT_SHMEM_ID: std::sync::atomic::AtomicU64 =
                std::sync::atomic::AtomicU64::new(0x3000_0000);
            let object_id = NEXT_SHMEM_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

            let k_shmem = service
                .wrapped_service
                .lock()
                .unwrap()
                .get_shared_memory_arc()?;

            process.register_shared_memory_object(object_id, k_shmem);
            let handle = process.handle_table.add(object_id).ok()?;
            Some(handle)
        })();

        match handle {
            Some(h) => {
                *cached = Some(h);
                log::debug!(
                    "Glue::Time::StaticService::GetSharedMemoryNativeHandle -> new handle={:#x}",
                    h
                );
                let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_copy_objects(h);
            }
            None => {
                log::error!(
                    "Glue::Time::StaticService::GetSharedMemoryNativeHandle -> failed to create handle"
                );
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(RESULT_SUCCESS);
            }
        }
    }

    /// IsStandardUserSystemClockAutomaticCorrectionEnabled (cmd 100) handler.
    fn is_standard_user_system_clock_automatic_correction_enabled_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const StaticService) };
        match service.is_standard_user_system_clock_automatic_correction_enabled() {
            Ok(enabled) => {
                log::debug!(
                    "Glue::Time::StaticService::IsStandardUserSystemClockAutomaticCorrectionEnabled -> {}",
                    enabled
                );
                let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u32(if enabled { 1 } else { 0 });
            }
            Err(rc) => {
                log::warn!(
                    "Glue::Time::StaticService::IsStandardUserSystemClockAutomaticCorrectionEnabled -> error {:?}",
                    rc
                );
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    /// IsStandardNetworkSystemClockAccuracySufficient (cmd 200) handler.
    fn is_standard_network_system_clock_accuracy_sufficient_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const StaticService) };
        match service.is_standard_network_system_clock_accuracy_sufficient() {
            Ok(sufficient) => {
                log::debug!(
                    "Glue::Time::StaticService::IsStandardNetworkSystemClockAccuracySufficient -> {}",
                    sufficient
                );
                let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u32(if sufficient { 1 } else { 0 });
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    /// SetStandardSteadyClockInternalOffset (cmd 50) handler.
    fn set_standard_steady_clock_internal_offset_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let offset_ns = rp.pop_i64();
        let rc = service.set_standard_steady_clock_internal_offset(offset_ns);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(rc);
    }

    /// GetStandardSteadyClockRtcValue (cmd 51) handler.
    fn get_standard_steady_clock_rtc_value_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        match service.get_standard_steady_clock_rtc_value() {
            Ok(rtc_value) => {
                log::debug!(
                    "Glue::Time::StaticService::GetStandardSteadyClockRtcValue -> {}",
                    rtc_value
                );
                let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_i64(rtc_value);
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    /// SetStandardUserSystemClockAutomaticCorrectionEnabled (cmd 101) handler.
    fn set_standard_user_system_clock_automatic_correction_enabled_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let automatic_correction = rp.pop_bool();
        log::debug!(
            "Glue::Time::StaticService::SetStandardUserSystemClockAutomaticCorrectionEnabled: {}",
            automatic_correction
        );
        let rc = service
            .set_standard_user_system_clock_automatic_correction_enabled(automatic_correction);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(rc);
    }

    /// GetStandardUserSystemClockInitialYear (cmd 102) handler.
    fn get_standard_user_system_clock_initial_year_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        match service.get_standard_user_system_clock_initial_year() {
            Ok(year) => {
                log::debug!(
                    "Glue::Time::StaticService::GetStandardUserSystemClockInitialYear -> {}",
                    year
                );
                let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_i32(year);
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    /// GetStandardUserSystemClockAutomaticCorrectionUpdatedTime (cmd 201) handler.
    fn get_standard_user_system_clock_automatic_correction_updated_time_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        match service.get_standard_user_system_clock_automatic_correction_updated_time() {
            Ok(time_point) => {
                log::debug!(
                    "Glue::Time::StaticService::GetStandardUserSystemClockAutomaticCorrectionUpdatedTime -> {:?}",
                    time_point
                );
                let mut rb = ResponseBuilder::new(
                    ctx,
                    2 + (core::mem::size_of::<SteadyClockTimePoint>() / 4) as u32,
                    0,
                    0,
                );
                rb.push_result(RESULT_SUCCESS);
                rb.push_raw(&time_point);
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    /// CalculateMonotonicSystemClockBaseTimePoint (cmd 300) handler.
    fn calculate_monotonic_system_clock_base_time_point_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let context: SystemClockContext = rp.pop_raw();
        match service.calculate_monotonic_system_clock_base_time_point(&context) {
            Ok(time) => {
                let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_i64(time);
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    /// GetClockSnapshot (cmd 400) handler.
    fn get_clock_snapshot_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let type_val = rp.pop_u32();
        let type_ = match type_val {
            0 => TimeType::UserSystemClock,
            1 => TimeType::NetworkSystemClock,
            2 => TimeType::LocalSystemClock,
            _ => TimeType::UserSystemClock,
        };
        match service.get_clock_snapshot(type_) {
            Ok(snapshot) => {
                let snapshot_bytes: &[u8] = unsafe {
                    core::slice::from_raw_parts(
                        &snapshot as *const ClockSnapshot as *const u8,
                        core::mem::size_of::<ClockSnapshot>(),
                    )
                };
                ctx.write_buffer(snapshot_bytes, 0);
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(RESULT_SUCCESS);
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    /// GetClockSnapshotFromSystemClockContext (cmd 401) handler.
    fn get_clock_snapshot_from_system_clock_context_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let type_val = rp.pop_u32();
        let type_ = match type_val {
            0 => TimeType::UserSystemClock,
            1 => TimeType::NetworkSystemClock,
            2 => TimeType::LocalSystemClock,
            _ => TimeType::UserSystemClock,
        };

        let user_context: SystemClockContext = rp.pop_raw();
        let network_context: SystemClockContext = rp.pop_raw();

        match service.get_clock_snapshot_from_system_clock_context(
            type_,
            &user_context,
            &network_context,
        ) {
            Ok(snapshot) => {
                let snapshot_bytes: &[u8] = unsafe {
                    core::slice::from_raw_parts(
                        &snapshot as *const ClockSnapshot as *const u8,
                        core::mem::size_of::<ClockSnapshot>(),
                    )
                };
                ctx.write_buffer(snapshot_bytes, 0);
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(RESULT_SUCCESS);
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    /// CalculateStandardUserSystemClockDifferenceByUser (cmd 500) handler.
    fn calculate_standard_user_system_clock_difference_by_user_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);

        let buf_a = ctx.read_buffer(0);
        let buf_b = ctx.read_buffer(1);

        let a: ClockSnapshot = if buf_a.len() >= core::mem::size_of::<ClockSnapshot>() {
            unsafe { core::ptr::read(buf_a.as_ptr() as *const ClockSnapshot) }
        } else {
            ClockSnapshot::default()
        };
        let b: ClockSnapshot = if buf_b.len() >= core::mem::size_of::<ClockSnapshot>() {
            unsafe { core::ptr::read(buf_b.as_ptr() as *const ClockSnapshot) }
        } else {
            ClockSnapshot::default()
        };

        match service.calculate_standard_user_system_clock_difference_by_user(&a, &b) {
            Ok(diff) => {
                let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_i64(diff);
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    /// CalculateSpanBetween (cmd 501) handler.
    fn calculate_span_between_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);

        let buf_a = ctx.read_buffer(0);
        let buf_b = ctx.read_buffer(1);

        let a: ClockSnapshot = if buf_a.len() >= core::mem::size_of::<ClockSnapshot>() {
            unsafe { core::ptr::read(buf_a.as_ptr() as *const ClockSnapshot) }
        } else {
            ClockSnapshot::default()
        };
        let b: ClockSnapshot = if buf_b.len() >= core::mem::size_of::<ClockSnapshot>() {
            unsafe { core::ptr::read(buf_b.as_ptr() as *const ClockSnapshot) }
        } else {
            ClockSnapshot::default()
        };

        match service.calculate_span_between(&a, &b) {
            Ok(time) => {
                let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_i64(time);
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    // =========================================================================
    // Business logic methods (unchanged)
    // =========================================================================

    pub fn get_standard_user_system_clock(&self) -> ResultCode {
        log::debug!("Glue::Time::StaticService::GetStandardUserSystemClock called");
        RESULT_SUCCESS
    }

    pub fn get_standard_network_system_clock(&self) -> ResultCode {
        log::debug!("Glue::Time::StaticService::GetStandardNetworkSystemClock called");
        RESULT_SUCCESS
    }

    pub fn get_standard_steady_clock(&self) -> crate::hle::service::psc::time::steady_clock::SteadyClock {
        log::debug!("Glue::Time::StaticService::GetStandardSteadyClock called");
        self.wrapped_service.lock().unwrap().get_standard_steady_clock()
    }

    pub fn get_time_zone_service(&self) -> Result<TimeZoneService, ResultCode> {
        log::debug!("Glue::Time::StaticService::GetTimeZoneService called");
        let wrapped = self.wrapped_service.lock().unwrap().get_time_zone_service();
        Ok(TimeZoneService::with_wrapped(
            self.system,
            self.setup_info.can_write_timezone_device_location,
            wrapped,
            Arc::clone(&self.file_timestamp_worker),
            Arc::clone(&self.time_zone_binary),
        ))
    }

    pub fn get_standard_local_system_clock(&self) -> ResultCode {
        log::debug!("Glue::Time::StaticService::GetStandardLocalSystemClock called");
        RESULT_SUCCESS
    }

    pub fn get_ephemeral_network_system_clock(&self) -> ResultCode {
        log::debug!("Glue::Time::StaticService::GetEphemeralNetworkSystemClock called");
        RESULT_SUCCESS
    }

    pub fn set_standard_steady_clock_internal_offset(&self, offset_ns: i64) -> ResultCode {
        log::debug!(
            "Glue::Time::StaticService::SetStandardSteadyClockInternalOffset: offset_ns={offset_ns}"
        );
        use crate::hle::service::psc::time::errors::RESULT_PERMISSION_DENIED;
        if !self.setup_info.can_write_steady_clock {
            return RESULT_PERMISSION_DENIED;
        }
        let _offset_s = offset_ns / 1_000_000_000;
        RESULT_SUCCESS
    }

    pub fn get_standard_steady_clock_rtc_value(&self) -> Result<i64, ResultCode> {
        log::debug!("Glue::Time::StaticService::GetStandardSteadyClockRtcValue called");
        self.standard_steady_clock_resource
            .lock()
            .unwrap()
            .get_rtc_time_in_seconds()
    }

    pub fn is_standard_user_system_clock_automatic_correction_enabled(
        &self,
    ) -> Result<bool, ResultCode> {
        self.wrapped_service
            .lock()
            .unwrap()
            .is_standard_user_system_clock_automatic_correction_enabled()
    }

    pub fn set_standard_user_system_clock_automatic_correction_enabled(
        &self,
        automatic_correction: bool,
    ) -> ResultCode {
        self.wrapped_service
            .lock()
            .unwrap()
            .set_standard_user_system_clock_automatic_correction_enabled(automatic_correction)
    }

    /// GetStandardUserSystemClockInitialYear (cmd 102).
    ///
    /// Corresponds to `StaticService::GetStandardUserSystemClockInitialYear` in upstream.
    /// In upstream, delegates to m_set_sys->GetSettingsItemValueImpl<s32>(
    ///     "time", "standard_user_clock_initial_year").
    pub fn get_standard_user_system_clock_initial_year(&self) -> Result<i32, ResultCode> {
        log::debug!("Glue::Time::StaticService::GetStandardUserSystemClockInitialYear called");
        // Upstream delegates to m_set_sys->GetSettingsItemValueImpl<s32>(
        // "time", "standard_user_clock_initial_year"). The ISystemSettingsServer
        // (set:sys) service is not yet wired. The default value in upstream
        // settings is 2019.
        Ok(2019)
    }

    pub fn is_standard_network_system_clock_accuracy_sufficient(&self) -> Result<bool, ResultCode> {
        Ok(self
            .wrapped_service
            .lock()
            .unwrap()
            .is_standard_network_system_clock_accuracy_sufficient())
    }

    pub fn get_clock_snapshot(&self, type_: TimeType) -> Result<ClockSnapshot, ResultCode> {
        self.wrapped_service
            .lock()
            .unwrap()
            .get_clock_snapshot(type_)
    }

    pub fn get_clock_snapshot_from_system_clock_context(
        &self,
        type_: TimeType,
        user_context: &SystemClockContext,
        network_context: &SystemClockContext,
    ) -> Result<ClockSnapshot, ResultCode> {
        self.wrapped_service
            .lock()
            .unwrap()
            .get_clock_snapshot_from_system_clock_context(type_, user_context, network_context)
    }

    /// CalculateMonotonicSystemClockBaseTimePoint (cmd 300).
    ///
    /// Corresponds to `StaticService::CalculateMonotonicSystemClockBaseTimePoint` in upstream.
    /// Delegates to the wrapped PSC static service.
    pub fn calculate_monotonic_system_clock_base_time_point(
        &self,
        context: &SystemClockContext,
    ) -> Result<i64, ResultCode> {
        log::debug!("Glue::Time::StaticService::CalculateMonotonicSystemClockBaseTimePoint called");
        self.wrapped_service
            .lock()
            .unwrap()
            .calculate_monotonic_system_clock_base_time_point(context)
    }

    pub fn get_standard_user_system_clock_automatic_correction_updated_time(
        &self,
    ) -> Result<SteadyClockTimePoint, ResultCode> {
        self.wrapped_service
            .lock()
            .unwrap()
            .get_standard_user_system_clock_automatic_correction_updated_time()
    }

    pub fn calculate_standard_user_system_clock_difference_by_user(
        &self,
        a: &ClockSnapshot,
        b: &ClockSnapshot,
    ) -> Result<i64, ResultCode> {
        Ok(self
            .wrapped_service
            .lock()
            .unwrap()
            .calculate_standard_user_system_clock_difference_by_user(a, b))
    }

    pub fn calculate_span_between(
        &self,
        a: &ClockSnapshot,
        b: &ClockSnapshot,
    ) -> Result<i64, ResultCode> {
        self.wrapped_service
            .lock()
            .unwrap()
            .calculate_span_between(a, b)
    }
}

// =============================================================================
// ServiceFramework + SessionRequestHandler implementation
// =============================================================================

impl SessionRequestHandler for StaticService {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        &self.service_name
    }
}

impl ServiceFramework for StaticService {
    fn get_service_name(&self) -> &str {
        &self.service_name
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
    use crate::hle::service::psc::time::common::SteadyClockTimePoint;

    fn make_time_manager() -> Arc<Mutex<GlueTimeManager>> {
        let service_manager = Arc::new(Mutex::new(
            crate::hle::service::sm::sm::ServiceManager::new(),
        ));
        let result = service_manager.lock().unwrap().register_service(
            "time:m".to_string(),
            64,
            Box::new(|| {
                Arc::new(
                    crate::hle::service::psc::time::service_manager::TimeServiceManager::new(
                        crate::core::SystemRef::null(),
                        std::ptr::null(),
                        std::ptr::null_mut(),
                    ),
                )
            }),
        );
        assert!(result.is_success());
        Arc::new(Mutex::new(GlueTimeManager::new(
            service_manager,
            crate::core::SystemRef::null(),
        )))
    }

    fn user_setup() -> StaticServiceSetupInfo {
        StaticServiceSetupInfo {
            can_write_local_clock: false,
            can_write_user_clock: false,
            can_write_network_clock: false,
            can_write_timezone_device_location: false,
            can_write_steady_clock: false,
            can_write_uninitialized_clock: false,
        }
    }

    #[test]
    fn standard_steady_clock_getter_returns_the_wrapped_clock() {
        let service = StaticService::new(
            crate::core::SystemRef::null(), user_setup(), "time:u", make_time_manager(),
        );
        let clock = service.get_standard_steady_clock();
        let wrapped = service.wrapped_service.lock().unwrap().get_standard_steady_clock();
        assert_eq!(clock.service_name(), wrapped.service_name());
        // Same backend initialization and permission gates, not a success stub.
        assert_eq!(clock.get_test_offset(), wrapped.get_test_offset());
        assert_eq!(clock.set_test_offset(17), wrapped.set_test_offset(17));
        assert_eq!(clock.get_current_time_point().is_ok(), wrapped.get_current_time_point().is_ok());
    }

    #[test]
    fn get_time_zone_service_returns_glue_service_object() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let system = Box::new(crate::core::System::new_for_test());
                let system_ref = crate::core::SystemRef::from_ref(system.as_ref());
                let set_sys = Arc::new(crate::hle::service::set::system_settings_server::SystemSettingsService::new_for_test());
                assert!(system.service_manager().unwrap().lock().unwrap().register_service(
                    "set:sys".to_string(), 64, Box::new(move || set_sys.clone()),
                ).is_success());
                let service = StaticService::new(system_ref, user_setup(), "time:u", make_time_manager());
                let time_zone_service = service.get_time_zone_service().unwrap();
                assert_eq!(time_zone_service.service_name(), "ITimeZoneService");
            })
            .unwrap().join().unwrap();
    }

    fn admin_setup() -> StaticServiceSetupInfo {
        StaticServiceSetupInfo {
            can_write_local_clock: true,
            can_write_user_clock: true,
            can_write_network_clock: false,
            can_write_timezone_device_location: true,
            can_write_steady_clock: false,
            can_write_uninitialized_clock: false,
        }
    }

    #[test]
    fn delegated_correction_queries_follow_wrapped_psc_static_service() {
        // Use admin setup so we have can_write_user_clock permission
        let time_manager = make_time_manager();
        {
            let manager = time_manager.lock().unwrap();
            let mut time = manager.psc_time.lock().unwrap();
            time.standard_steady_clock.lock().unwrap()
                .initialize([0; 16], 0, 0, 0, false);
            time.standard_local_system_clock
                .initialize(&SystemClockContext::default(), 0);
            time.standard_network_system_clock
                .initialize(&SystemClockContext::default(), i64::MAX);
            time.standard_user_system_clock.set_initialized();
        }
        let service = StaticService::new(
            crate::core::SystemRef::null(),
            admin_setup(),
            "time:a",
            time_manager,
        );
        {
            let wrapped = service.wrapped_service.lock().unwrap();
            let rc = wrapped.set_standard_user_system_clock_automatic_correction_enabled(true);
            assert!(
                rc.is_success(),
                "Failed to enable automatic correction: {:?}",
                rc
            );
        }

        assert_eq!(
            service
                .is_standard_user_system_clock_automatic_correction_enabled()
                .unwrap(),
            true
        );
        assert_eq!(
            service
                .get_standard_user_system_clock_automatic_correction_updated_time()
                .unwrap(),
            SteadyClockTimePoint::default()
        );
    }

    #[test]
    fn shared_memory_handle_uses_wrapped_psc_time_owner() {
        let time_manager = make_time_manager();
        let service = StaticService::new(
            crate::core::SystemRef::null(),
            user_setup(),
            "time:u",
            Arc::clone(&time_manager),
        );

        let expected = {
            let manager = time_manager.lock().unwrap();
            let psc_time = manager.psc_time.lock().unwrap();
            psc_time.shared_memory.get_k_shared_memory_arc()
        };
        let actual = service
            .wrapped_service
            .lock()
            .unwrap()
            .get_shared_memory_arc()
            .expect("wrapped PSC static service should expose shared memory");

        assert!(Arc::ptr_eq(&expected, &actual));
    }

    #[test]
    fn retains_the_time_manager_resource_instances() {
        let time_manager = make_time_manager();
        let service = StaticService::new(
            crate::core::SystemRef::null(),
            user_setup(),
            "time:u",
            Arc::clone(&time_manager),
        );

        let manager = time_manager.lock().unwrap();
        assert!(Arc::ptr_eq(
            &manager.file_timestamp_worker,
            &service.file_timestamp_worker
        ));
        assert!(Arc::ptr_eq(
            &manager.steady_clock_resource,
            &service.standard_steady_clock_resource
        ));
        assert!(Arc::ptr_eq(
            &manager.time_zone_binary,
            &service.time_zone_binary
        ));
    }
}

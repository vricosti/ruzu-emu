// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/psc/time/static.h
//! Port of zuyu/src/core/hle/service/psc/time/static.cpp

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

use super::common::{
    get_span_between_time_points, ClockSnapshot, StaticServiceSetupInfo, SteadyClockTimePoint,
    SystemClockContext, TimeType,
};
use super::errors::{
    RESULT_CLOCK_MISMATCH, RESULT_CLOCK_UNINITIALIZED, RESULT_NOT_IMPLEMENTED,
    RESULT_PERMISSION_DENIED, RESULT_TIME_NOT_FOUND,
};
use super::manager::TimeManager;
use super::steady_clock::{SteadyClock, TimeManagerSteadyClockBackend};
use super::system_clock::{SystemClock, SystemClockKind, TimeManagerBackend};
use super::time_zone::TimeZone;
use super::time_zone_service::TimeZoneService;

/// IPC command IDs for StaticService.
///
/// Corresponds to the function table in upstream `static.cpp`.
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
    pub const CALCULATE_SPAN_BETWEEN: u32 = 501;
}

/// Default setup info for `time:su`.
pub const TIME_SU_SETUP_INFO: StaticServiceSetupInfo = StaticServiceSetupInfo {
    can_write_local_clock: false,
    can_write_user_clock: false,
    can_write_network_clock: false,
    can_write_timezone_device_location: false,
    can_write_steady_clock: false,
    can_write_uninitialized_clock: true,
};

/// Corresponds to the anonymous `GetTimeFromTimePointAndContext` helper in
/// upstream `static.cpp`.
pub fn get_time_from_time_point_and_context(
    time_point: &SteadyClockTimePoint,
    context: &SystemClockContext,
) -> Result<i64, ResultCode> {
    if !time_point.id_matches(&context.steady_time_point) {
        return Err(RESULT_CLOCK_MISMATCH);
    }
    Ok(context.offset + time_point.time_point)
}

/// `PSC::Time::StaticService`.
///
/// Corresponds to `PSC::Time::StaticService` in upstream static.h.
/// In upstream, this holds references to clock cores from TimeManager.
/// Here we maintain local state for the clock queries that don't
/// require sub-service creation, and create standalone sub-services
/// for the "Get" methods.
pub struct StaticService {
    pub setup_info: StaticServiceSetupInfo,
    automatic_correction_enabled: AtomicBool,
    automatic_correction_time_point: Mutex<SteadyClockTimePoint>,
    user_clock_initialized: bool,
    steady_clock_initialized: bool,
    network_accuracy_sufficient: bool,
    /// User system clock context (for GetClockSnapshot).
    user_context: SystemClockContext,
    /// Network system clock context (for GetClockSnapshot).
    network_context: SystemClockContext,
    /// Current steady clock time point (for GetClockSnapshot).
    steady_clock_time_point: SteadyClockTimePoint,
    /// TimeZone reference for calendar conversions in GetClockSnapshotImpl.
    /// Matches upstream `m_time_zone` (reference to `m_time->m_time_zone`).
    time_zone: TimeZone,
    /// Shared PSC::Time::TimeManager owner when this StaticService is created from
    /// the real time bootstrap path.
    shared_time: Option<Arc<Mutex<TimeManager>>>,
    /// Cached handle returned by GetSharedMemoryNativeHandle.
    shared_memory_handle: Mutex<Option<u32>>,
    /// Boot instant for CalculateMonotonicSystemClockBaseTimePoint.
    /// Matches upstream `m_system.CoreTiming().GetClockTicks()` converted to time.
    boot_instant: std::time::Instant,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl StaticService {
    pub fn new(setup_info: StaticServiceSetupInfo) -> Self {
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
            (commands::CALCULATE_SPAN_BETWEEN, Some(StaticService::calculate_span_between_handler), "CalculateSpanBetween"),
        ]);
        Self {
            setup_info,
            automatic_correction_enabled: AtomicBool::new(false),
            automatic_correction_time_point: Mutex::new(SteadyClockTimePoint::default()),
            user_clock_initialized: false,
            steady_clock_initialized: false,
            network_accuracy_sufficient: false,
            user_context: SystemClockContext::default(),
            network_context: SystemClockContext::default(),
            steady_clock_time_point: SteadyClockTimePoint::default(),
            time_zone: TimeZone::new(),
            shared_time: None,
            shared_memory_handle: Mutex::new(None),
            boot_instant: std::time::Instant::now(),
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    pub fn with_time_manager(
        setup_info: StaticServiceSetupInfo,
        shared_time: Arc<Mutex<TimeManager>>,
    ) -> Self {
        let mut service = Self::new(setup_info);
        service.shared_time = Some(shared_time);
        service
    }

    pub fn set_user_clock_initialized(&mut self, initialized: bool) {
        self.user_clock_initialized = initialized;
    }

    pub fn set_steady_clock_initialized(&mut self, initialized: bool) {
        self.steady_clock_initialized = initialized;
    }

    /// Set the user system clock context (for snapshot queries).
    pub fn set_user_context(&mut self, context: &SystemClockContext) {
        self.user_context = *context;
    }

    /// Set the network system clock context (for snapshot queries).
    pub fn set_network_context(&mut self, context: &SystemClockContext) {
        self.network_context = *context;
    }

    /// Set the current steady clock time point.
    pub fn set_steady_clock_time_point(&mut self, time_point: &SteadyClockTimePoint) {
        self.steady_clock_time_point = *time_point;
    }

    // =========================================================================
    // Commands 0-5: Sub-service creation
    //
    // In upstream, these create new IPC service objects referencing the clock
    // cores from TimeManager. Here we create standalone instances with the
    // appropriate permission flags from setup_info.
    // =========================================================================

    /// GetStandardUserSystemClock (cmd 0).
    ///
    /// Corresponds to `StaticService::GetStandardUserSystemClock` in upstream.
    /// Creates a SystemClock sub-service for the user system clock.
    pub fn get_standard_user_system_clock(&self) -> SystemClock {
        log::debug!("PSC::Time::StaticService::GetStandardUserSystemClock called");
        if let Some(shared_time) = &self.shared_time {
            return SystemClock::with_backend(
                self.setup_info.can_write_user_clock,
                self.setup_info.can_write_uninitialized_clock,
                Arc::new(TimeManagerBackend::new(
                    Arc::clone(shared_time),
                    SystemClockKind::User,
                )),
            );
        }
        SystemClock::new(
            self.setup_info.can_write_user_clock,
            self.setup_info.can_write_uninitialized_clock,
        )
    }

    /// GetStandardNetworkSystemClock (cmd 1).
    ///
    /// Corresponds to `StaticService::GetStandardNetworkSystemClock` in upstream.
    /// Creates a SystemClock sub-service for the network system clock.
    pub fn get_standard_network_system_clock(&self) -> SystemClock {
        log::debug!("PSC::Time::StaticService::GetStandardNetworkSystemClock called");
        if let Some(shared_time) = &self.shared_time {
            return SystemClock::with_backend(
                self.setup_info.can_write_network_clock,
                self.setup_info.can_write_uninitialized_clock,
                Arc::new(TimeManagerBackend::new(
                    Arc::clone(shared_time),
                    SystemClockKind::Network,
                )),
            );
        }
        SystemClock::new(
            self.setup_info.can_write_network_clock,
            self.setup_info.can_write_uninitialized_clock,
        )
    }

    /// GetStandardSteadyClock (cmd 2).
    ///
    /// Corresponds to `StaticService::GetStandardSteadyClock` in upstream.
    /// Creates a SteadyClock sub-service.
    pub fn get_standard_steady_clock(&self) -> SteadyClock {
        log::debug!("PSC::Time::StaticService::GetStandardSteadyClock called");
        if let Some(shared_time) = &self.shared_time {
            return SteadyClock::with_backend(
                self.setup_info.can_write_steady_clock,
                self.setup_info.can_write_uninitialized_clock,
                Arc::new(TimeManagerSteadyClockBackend::new(Arc::clone(shared_time))),
            );
        }
        SteadyClock::new(
            self.setup_info.can_write_steady_clock,
            self.setup_info.can_write_uninitialized_clock,
        )
    }

    /// GetTimeZoneService (cmd 3).
    ///
    /// Corresponds to `StaticService::GetTimeZoneService` in upstream.
    /// Creates a TimeZoneService sub-service.
    pub fn get_time_zone_service(&self) -> TimeZoneService {
        log::debug!("PSC::Time::StaticService::GetTimeZoneService called");
        if let Some(shared_time) = &self.shared_time {
            return TimeZoneService::with_time_manager(
                self.setup_info.can_write_timezone_device_location,
                Arc::clone(shared_time),
            );
        }
        TimeZoneService::new(self.setup_info.can_write_timezone_device_location)
    }

    /// GetStandardLocalSystemClock (cmd 4).
    ///
    /// Corresponds to `StaticService::GetStandardLocalSystemClock` in upstream.
    /// Creates a SystemClock sub-service for the local system clock.
    pub fn get_standard_local_system_clock(&self) -> SystemClock {
        log::debug!("PSC::Time::StaticService::GetStandardLocalSystemClock called");
        if let Some(shared_time) = &self.shared_time {
            return SystemClock::with_backend(
                self.setup_info.can_write_local_clock,
                self.setup_info.can_write_uninitialized_clock,
                Arc::new(TimeManagerBackend::new(
                    Arc::clone(shared_time),
                    SystemClockKind::Local,
                )),
            );
        }
        SystemClock::new(
            self.setup_info.can_write_local_clock,
            self.setup_info.can_write_uninitialized_clock,
        )
    }

    /// GetEphemeralNetworkSystemClock (cmd 5).
    ///
    /// Corresponds to `StaticService::GetEphemeralNetworkSystemClock` in upstream.
    /// Creates a SystemClock sub-service for the ephemeral network clock.
    pub fn get_ephemeral_network_system_clock(&self) -> SystemClock {
        log::debug!("PSC::Time::StaticService::GetEphemeralNetworkSystemClock called");
        if let Some(shared_time) = &self.shared_time {
            return SystemClock::with_backend(
                self.setup_info.can_write_network_clock,
                self.setup_info.can_write_uninitialized_clock,
                Arc::new(TimeManagerBackend::new(
                    Arc::clone(shared_time),
                    SystemClockKind::EphemeralNetwork,
                )),
            );
        }
        SystemClock::new(
            self.setup_info.can_write_network_clock,
            self.setup_info.can_write_uninitialized_clock,
        )
    }

    // =========================================================================
    // Command 20: Shared memory
    // =========================================================================

    /// GetSharedMemoryNativeHandle (cmd 20).
    ///
    /// Corresponds to `StaticService::GetSharedMemoryNativeHandle` in upstream.
    /// Returns the kernel shared memory handle for lock-free time reads.
    ///
    /// Upstream returns `&m_shared_memory.GetKSharedMemory()` through a CMIF
    /// `OutCopyHandle`. Rust materializes the equivalent guest handle at the IPC
    /// boundary because `ResponseBuilder` stores raw handle values.
    pub fn get_shared_memory_native_handle(&self, ctx: &mut HLERequestContext) -> Option<u32> {
        log::debug!("PSC::Time::StaticService::GetSharedMemoryNativeHandle called");

        let mut cached = self.shared_memory_handle.lock().unwrap();
        if let Some(handle) = *cached {
            return Some(handle);
        }

        let thread = ctx.get_thread()?;
        let thread_guard = thread.lock().unwrap();
        let parent = thread_guard.parent.as_ref()?.upgrade()?;
        let mut process = parent.lock().unwrap();

        static NEXT_SHMEM_ID: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0x3100_0000);
        let object_id = NEXT_SHMEM_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let shared_memory = self.get_shared_memory_arc()?;

        process.register_shared_memory_object(object_id, shared_memory);
        let handle = process.handle_table.add(object_id).ok()?;
        *cached = Some(handle);
        Some(handle)
    }

    pub fn get_shared_memory_arc(
        &self,
    ) -> Option<Arc<crate::hle::kernel::k_shared_memory::KSharedMemory>> {
        let shared_time = self.shared_time.as_ref()?;
        let time = shared_time.lock().unwrap();
        Some(time.shared_memory.get_k_shared_memory_arc())
    }

    // =========================================================================
    // Commands 50-51: Steady clock operations
    // =========================================================================

    pub fn set_standard_steady_clock_internal_offset(&self, offset_ns: i64) -> ResultCode {
        log::debug!(
            "StaticService::SetStandardSteadyClockInternalOffset: offset_ns={offset_ns}. Not implemented!"
        );
        if !self.setup_info.can_write_steady_clock {
            return RESULT_PERMISSION_DENIED;
        }
        RESULT_NOT_IMPLEMENTED
    }

    pub fn get_standard_steady_clock_rtc_value(&self) -> Result<i64, ResultCode> {
        log::debug!("StaticService::GetStandardSteadyClockRtcValue: Not implemented!");
        Err(RESULT_NOT_IMPLEMENTED)
    }

    // =========================================================================
    // Commands 100-102: User system clock
    // =========================================================================

    pub fn is_standard_user_system_clock_automatic_correction_enabled(
        &self,
    ) -> Result<bool, ResultCode> {
        if let Some(shared_time) = &self.shared_time {
            let time = shared_time.lock().unwrap();
            if !time.standard_user_system_clock.is_initialized() {
                return Err(RESULT_CLOCK_UNINITIALIZED);
            }
            return Ok(time.standard_user_system_clock.get_automatic_correction());
        }

        if !self.user_clock_initialized {
            return Err(RESULT_CLOCK_UNINITIALIZED);
        }
        Ok(self.automatic_correction_enabled.load(Ordering::Relaxed))
    }

    pub fn set_standard_user_system_clock_automatic_correction_enabled(
        &self,
        automatic_correction: bool,
    ) -> ResultCode {
        if let Some(shared_time) = &self.shared_time {
            let mut time = shared_time.lock().unwrap();
            if !time.standard_user_system_clock.is_initialized()
                || !time.standard_steady_clock.state.is_initialized()
            {
                return RESULT_CLOCK_UNINITIALIZED;
            }
            if !self.setup_info.can_write_user_clock {
                return RESULT_PERMISSION_DENIED;
            }

            let local = std::ptr::addr_of_mut!(time.standard_local_system_clock);
            let network = std::ptr::addr_of!(time.standard_network_system_clock);
            let rc = unsafe {
                time.standard_user_system_clock.set_automatic_correction(
                    automatic_correction,
                    &mut *local,
                    &*network,
                )
            };
            if rc != RESULT_SUCCESS {
                return rc;
            }

            time.shared_memory
                .set_automatic_correction(automatic_correction);

            let time_point = match super::clocks::steady_clock_core::get_current_time_point(
                &time.standard_steady_clock,
            ) {
                Ok(time_point) => time_point,
                Err(rc) => return rc,
            };
            time.standard_user_system_clock
                .set_time_point_and_signal(&time_point);
            time.standard_user_system_clock.get_event().signal();
            return RESULT_SUCCESS;
        }

        if !self.user_clock_initialized || !self.steady_clock_initialized {
            return RESULT_CLOCK_UNINITIALIZED;
        }
        if !self.setup_info.can_write_user_clock {
            return RESULT_PERMISSION_DENIED;
        }
        self.automatic_correction_enabled
            .store(automatic_correction, Ordering::Relaxed);
        // Upstream calls m_shared_memory.SetAutomaticCorrection(automatic_correction)
        // and then gets the current steady clock time point to call
        // m_user_system_clock.SetTimePointAndSignal(time_point) followed by
        // m_user_system_clock.GetEvent().Signal().
        //
        // The SharedMemory and clock core references are held by
        // Glue::Time::StaticService (which wraps this PSC service). The Glue
        // layer is responsible for calling shared_memory.set_automatic_correction()
        // and updating the time point via the TimeManager. Here we only update
        // the local automatic_correction_time_point with the current steady
        // clock time point since that is the data this service owns.
        *self.automatic_correction_time_point.lock().unwrap() = self.steady_clock_time_point;
        RESULT_SUCCESS
    }

    /// GetStandardUserSystemClockInitialYear (cmd 102).
    ///
    /// Corresponds to `StaticService::GetStandardUserSystemClockInitialYear` in upstream.
    /// Upstream returns ResultNotImplemented.
    pub fn get_standard_user_system_clock_initial_year(&self) -> Result<i32, ResultCode> {
        log::debug!("StaticService::GetStandardUserSystemClockInitialYear: Not implemented!");
        Err(RESULT_NOT_IMPLEMENTED)
    }

    // =========================================================================
    // Commands 200-201: Network accuracy
    // =========================================================================

    pub fn is_standard_network_system_clock_accuracy_sufficient(&self) -> bool {
        if let Some(shared_time) = &self.shared_time {
            let time = shared_time.lock().unwrap();
            return time.standard_network_system_clock.is_accuracy_sufficient();
        }
        self.network_accuracy_sufficient
    }

    pub fn get_standard_user_system_clock_automatic_correction_updated_time(
        &self,
    ) -> Result<SteadyClockTimePoint, ResultCode> {
        if let Some(shared_time) = &self.shared_time {
            let time = shared_time.lock().unwrap();
            if !time.standard_user_system_clock.is_initialized() {
                return Err(RESULT_CLOCK_UNINITIALIZED);
            }
            return Ok(time.standard_user_system_clock.get_time_point());
        }

        if !self.user_clock_initialized {
            return Err(RESULT_CLOCK_UNINITIALIZED);
        }
        Ok(*self.automatic_correction_time_point.lock().unwrap())
    }

    // =========================================================================
    // Command 300: Monotonic base time point
    // =========================================================================

    /// CalculateMonotonicSystemClockBaseTimePoint (cmd 300).
    ///
    /// Corresponds to `StaticService::CalculateMonotonicSystemClockBaseTimePoint` in upstream.
    /// Calculates: (context.offset + time_point.time_point) - (current_time_ns / 1e9)
    ///
    /// Requires steady clock initialization and matching clock source IDs.
    pub fn calculate_monotonic_system_clock_base_time_point(
        &self,
        context: &SystemClockContext,
    ) -> Result<i64, ResultCode> {
        if let Some(shared_time) = &self.shared_time {
            let time = shared_time.lock().unwrap();
            if !time.standard_steady_clock.state.is_initialized() {
                return Err(RESULT_CLOCK_UNINITIALIZED);
            }

            let time_point = super::clocks::steady_clock_core::get_current_time_point(
                &time.standard_steady_clock,
            )?;
            if !time_point.id_matches(&context.steady_time_point) {
                return Err(RESULT_CLOCK_MISMATCH);
            }

            let current_time_s = time.standard_steady_clock.get_current_uptime_ns() / 1_000_000_000;
            return Ok((context.offset + time_point.time_point) - current_time_s);
        }

        if !self.steady_clock_initialized {
            return Err(RESULT_CLOCK_UNINITIALIZED);
        }

        let time_point = self.steady_clock_time_point;

        if !time_point.id_matches(&context.steady_time_point) {
            return Err(RESULT_CLOCK_MISMATCH);
        }

        // Upstream:
        //   auto ticks = m_system.CoreTiming().GetClockTicks();
        //   auto current_time_ns = ConvertToTimeSpan(ticks).count();
        //   *out_time = (context.offset + time_point.time_point) - (current_time_ns / 1e9);
        //
        // We use elapsed wall time since boot as the CoreTiming approximation.
        let elapsed_ns = self.boot_instant.elapsed().as_nanos() as i64;
        let current_time_s = elapsed_ns / 1_000_000_000;
        let out_time = (context.offset + time_point.time_point) - current_time_s;
        log::debug!(
            "StaticService::CalculateMonotonicSystemClockBaseTimePoint: context_offset={}, elapsed_s={}, out_time={}",
            context.offset, current_time_s, out_time
        );
        Ok(out_time)
    }

    // =========================================================================
    // Commands 400-401: Clock snapshots
    // =========================================================================

    /// GetClockSnapshot (cmd 400).
    ///
    /// Corresponds to `StaticService::GetClockSnapshot` in upstream.
    /// Gets clock snapshot from the current user and network contexts.
    pub fn get_clock_snapshot(&self, type_: TimeType) -> Result<ClockSnapshot, ResultCode> {
        log::debug!("StaticService::GetClockSnapshot: type={:?}", type_);
        if let Some(shared_time) = &self.shared_time {
            let mut time = shared_time.lock().unwrap();
            let local = std::ptr::addr_of_mut!(time.standard_local_system_clock);
            let network = std::ptr::addr_of!(time.standard_network_system_clock);
            let user_context = unsafe {
                time.standard_user_system_clock
                    .get_context(&mut *local, &*network)?
            };
            let network_context = time.standard_network_system_clock.clock.get_context()?;
            return Self::get_clock_snapshot_impl_live(
                &mut time,
                &user_context,
                &network_context,
                type_,
            );
        }
        self.get_clock_snapshot_impl(&self.user_context, &self.network_context, type_)
    }

    /// GetClockSnapshotFromSystemClockContext (cmd 401).
    ///
    /// Corresponds to `StaticService::GetClockSnapshotFromSystemClockContext` in upstream.
    /// Gets clock snapshot from the provided user and network contexts.
    pub fn get_clock_snapshot_from_system_clock_context(
        &self,
        type_: TimeType,
        user_context: &SystemClockContext,
        network_context: &SystemClockContext,
    ) -> Result<ClockSnapshot, ResultCode> {
        log::debug!(
            "StaticService::GetClockSnapshotFromSystemClockContext: type={:?}",
            type_
        );
        if let Some(shared_time) = &self.shared_time {
            let mut time = shared_time.lock().unwrap();
            return Self::get_clock_snapshot_impl_live(
                &mut time,
                user_context,
                network_context,
                type_,
            );
        }
        self.get_clock_snapshot_impl(user_context, network_context, type_)
    }

    // =========================================================================
    // Commands 500-501: Calculations
    // =========================================================================

    pub fn calculate_standard_user_system_clock_difference_by_user(
        &self,
        a: &ClockSnapshot,
        b: &ClockSnapshot,
    ) -> i64 {
        let diff_s = b.user_context.offset - a.user_context.offset;

        if a.user_context == b.user_context
            || !a
                .user_context
                .steady_time_point
                .id_matches(&b.user_context.steady_time_point)
        {
            return 0;
        }

        if !a.is_automatic_correction_enabled || !b.is_automatic_correction_enabled {
            return diff_s.saturating_mul(1_000_000_000);
        }

        if a.network_context
            .steady_time_point
            .id_matches(&a.steady_clock_time_point)
            || b.network_context
                .steady_time_point
                .id_matches(&b.steady_clock_time_point)
        {
            return 0;
        }

        diff_s.saturating_mul(1_000_000_000)
    }

    pub fn calculate_span_between(
        &self,
        a: &ClockSnapshot,
        b: &ClockSnapshot,
    ) -> Result<i64, ResultCode> {
        let time_s =
            get_span_between_time_points(&a.steady_clock_time_point, &b.steady_clock_time_point);

        let time_s = match time_s {
            Some(t) => t,
            None => {
                if a.network_time == 0 || b.network_time == 0 {
                    return Err(RESULT_TIME_NOT_FOUND);
                }
                b.network_time - a.network_time
            }
        };

        Ok(time_s.saturating_mul(1_000_000_000))
    }

    // =========================================================================
    // IPC handler callbacks (ServiceFramework pattern)
    // =========================================================================

    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const StaticService) }
    }

    /// Helper to create a sub-service and push it as a domain object or move handle.
    fn push_sub_service(ctx: &mut HLERequestContext, sub_service: Arc<dyn SessionRequestHandler>) {
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(sub_service);
    }

    /// GetStandardUserSystemClock (cmd 0) handler.
    fn get_standard_user_system_clock_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let sub = service.get_standard_user_system_clock();
        Self::push_sub_service(ctx, Arc::new(sub));
    }

    /// GetStandardNetworkSystemClock (cmd 1) handler.
    fn get_standard_network_system_clock_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let sub = service.get_standard_network_system_clock();
        Self::push_sub_service(ctx, Arc::new(sub));
    }

    /// GetStandardSteadyClock (cmd 2) handler.
    fn get_standard_steady_clock_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let sub = service.get_standard_steady_clock();
        Self::push_sub_service(ctx, Arc::new(sub));
    }

    /// GetTimeZoneService (cmd 3) handler.
    fn get_time_zone_service_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let sub = service.get_time_zone_service();
        Self::push_sub_service(ctx, Arc::new(sub));
    }

    /// GetStandardLocalSystemClock (cmd 4) handler.
    fn get_standard_local_system_clock_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let sub = service.get_standard_local_system_clock();
        Self::push_sub_service(ctx, Arc::new(sub));
    }

    /// GetEphemeralNetworkSystemClock (cmd 5) handler.
    fn get_ephemeral_network_system_clock_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let sub = service.get_ephemeral_network_system_clock();
        Self::push_sub_service(ctx, Arc::new(sub));
    }

    /// GetSharedMemoryNativeHandle (cmd 20) handler.
    fn get_shared_memory_native_handle_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        match service.get_shared_memory_native_handle(ctx) {
            Some(handle) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_copy_objects(handle);
            }
            None => {
                log::error!(
                    "PSC::Time::StaticService::GetSharedMemoryNativeHandle failed to create handle"
                );
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(RESULT_SUCCESS);
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

    /// IsStandardUserSystemClockAutomaticCorrectionEnabled (cmd 100) handler.
    fn is_standard_user_system_clock_automatic_correction_enabled_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        match service.is_standard_user_system_clock_automatic_correction_enabled() {
            Ok(enabled) => {
                log::debug!(
                    "PSC::Time::StaticService::IsStandardUserSystemClockAutomaticCorrectionEnabled -> {}",
                    enabled
                );
                let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u32(if enabled { 1 } else { 0 });
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
            "PSC::Time::StaticService::SetStandardUserSystemClockAutomaticCorrectionEnabled: {}",
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

    /// IsStandardNetworkSystemClockAccuracySufficient (cmd 200) handler.
    fn is_standard_network_system_clock_accuracy_sufficient_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let sufficient = service.is_standard_network_system_clock_accuracy_sufficient();
        log::debug!(
            "PSC::Time::StaticService::IsStandardNetworkSystemClockAccuracySufficient -> {}",
            sufficient
        );
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(if sufficient { 1 } else { 0 });
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
                    "PSC::Time::StaticService::GetStandardUserSystemClockAutomaticCorrectionUpdatedTime -> {:?}",
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
    ///
    /// Upstream reads TimeType from input, calls GetClockSnapshot, and writes
    /// the ClockSnapshot to the output buffer (type B/C).
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
    ///
    /// Upstream reads TimeType, then reads user_context and network_context
    /// from the input buffer (type A/X), writes ClockSnapshot to output buffer.
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

        // Read user_context and network_context from inline parameters.
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
    ///
    /// Upstream reads two ClockSnapshots from input buffers and returns the
    /// difference in nanoseconds.
    fn calculate_standard_user_system_clock_difference_by_user_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);

        // Read two ClockSnapshots from input buffers (type A/X).
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

        let diff = service.calculate_standard_user_system_clock_difference_by_user(&a, &b);
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i64(diff);
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
    // Private helpers
    // =========================================================================

    /// GetClockSnapshotImpl (private).
    ///
    /// Corresponds to `StaticService::GetClockSnapshotImpl` in upstream.
    /// Fills a ClockSnapshot from the provided contexts.
    fn get_clock_snapshot_impl(
        &self,
        user_context: &SystemClockContext,
        network_context: &SystemClockContext,
        type_: TimeType,
    ) -> Result<ClockSnapshot, ResultCode> {
        let mut snapshot = ClockSnapshot::default();

        snapshot.user_context = *user_context;
        snapshot.network_context = *network_context;
        snapshot.steady_clock_time_point = self.steady_clock_time_point;
        snapshot.is_automatic_correction_enabled =
            self.automatic_correction_enabled.load(Ordering::Relaxed);

        // Get location name from timezone.
        // Matches upstream: m_time_zone.GetLocationName(out_snapshot->location_name)
        snapshot.location_name = self.time_zone.get_location_name().unwrap_or([0u8; 0x24]);

        // Compute user_time from time_point and user_context.
        // Matches upstream: GetTimeFromTimePointAndContext(&out_snapshot->user_time, ...)
        snapshot.user_time = get_time_from_time_point_and_context(
            &snapshot.steady_clock_time_point,
            &snapshot.user_context,
        )?;

        // Compute user calendar time via TimeZone.
        // Matches upstream: m_time_zone.ToCalendarTimeWithMyRule(user_calendar_time, ..., user_time)
        let (cal, cal_info) = self
            .time_zone
            .to_calendar_time_with_my_rule(snapshot.user_time)?;
        snapshot.user_calendar_time = cal;
        snapshot.user_calendar_additional_time = cal_info;

        // Compute network_time, defaulting to 0 on mismatch (matching upstream).
        match get_time_from_time_point_and_context(
            &snapshot.steady_clock_time_point,
            &snapshot.network_context,
        ) {
            Ok(time) => snapshot.network_time = time,
            Err(_) => snapshot.network_time = 0,
        }

        // Compute network calendar time via TimeZone.
        // Matches upstream: m_time_zone.ToCalendarTimeWithMyRule(network_calendar_time, ..., network_time)
        let (cal, cal_info) = self
            .time_zone
            .to_calendar_time_with_my_rule(snapshot.network_time)?;
        snapshot.network_calendar_time = cal;
        snapshot.network_calendar_additional_time = cal_info;

        snapshot.time_type = type_ as u8;
        snapshot.unk_ce = 0;

        Ok(snapshot)
    }

    fn get_clock_snapshot_impl_live(
        time: &mut TimeManager,
        user_context: &SystemClockContext,
        network_context: &SystemClockContext,
        type_: TimeType,
    ) -> Result<ClockSnapshot, ResultCode> {
        let mut snapshot = ClockSnapshot::default();

        snapshot.user_context = *user_context;
        snapshot.network_context = *network_context;
        snapshot.steady_clock_time_point =
            super::clocks::steady_clock_core::get_current_time_point(&time.standard_steady_clock)?;
        snapshot.is_automatic_correction_enabled =
            time.standard_user_system_clock.get_automatic_correction();
        snapshot.location_name = time.time_zone.get_location_name().unwrap_or([0u8; 0x24]);

        snapshot.user_time = get_time_from_time_point_and_context(
            &snapshot.steady_clock_time_point,
            &snapshot.user_context,
        )?;
        let (cal, cal_info) = time
            .time_zone
            .to_calendar_time_with_my_rule(snapshot.user_time)?;
        snapshot.user_calendar_time = cal;
        snapshot.user_calendar_additional_time = cal_info;

        match get_time_from_time_point_and_context(
            &snapshot.steady_clock_time_point,
            &snapshot.network_context,
        ) {
            Ok(time) => snapshot.network_time = time,
            Err(_) => snapshot.network_time = 0,
        }
        let (cal, cal_info) = time
            .time_zone
            .to_calendar_time_with_my_rule(snapshot.network_time)?;
        snapshot.network_calendar_time = cal;
        snapshot.network_calendar_additional_time = cal_info;
        snapshot.time_type = type_ as u8;
        snapshot.unk_ce = 0;

        Ok(snapshot)
    }
}

impl SessionRequestHandler for StaticService {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "time:su"
    }
}

impl ServiceFramework for StaticService {
    fn get_service_name(&self) -> &str {
        "time:su"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

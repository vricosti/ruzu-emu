// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/glue/time/manager.h
//! Port of zuyu/src/core/hle/service/glue/time/manager.cpp

use std::sync::{Arc, Mutex};

use super::file_timestamp_worker::FileTimestampWorker;
use super::standard_steady_clock_resource::StandardSteadyClockResource;
use super::time_zone_binary::TimeZoneBinary;
use super::worker::TimeWorker;

use crate::hle::service::psc::time::common::{
    CalendarTime, StaticServiceSetupInfo, SteadyClockTimePoint, SystemClockContext,
};
use crate::hle::service::psc::time::r#static::StaticService as PscStaticService;
use crate::hle::service::psc::time::service_manager::TimeServiceManager;
use crate::hle::service::set::system_settings_server::SystemSettingsService;
use crate::hle::service::sm::sm::ServiceManager;

/// TimeManager corresponds to upstream `Glue::Time::TimeManager`.
pub struct TimeManager {
    system: crate::core::SystemRef,
    pub worker: TimeWorker,
    pub file_timestamp_worker: Arc<Mutex<FileTimestampWorker>>,
    pub steady_clock_resource: Arc<Mutex<StandardSteadyClockResource>>,
    pub time_zone_binary: Arc<Mutex<TimeZoneBinary>>,
    pub psc_time: Arc<Mutex<crate::hle::service::psc::time::manager::TimeManager>>,
    pub time_sm: Arc<PscStaticService>,
}

impl TimeManager {
    /// Matches upstream `TimeManager::TimeManager(Core::System& system)`.
    pub fn new(
        service_manager: Arc<Mutex<ServiceManager>>,
        system: crate::core::SystemRef,
    ) -> Self {
        let time_m_handler =
            ServiceManager::get_service_blocking(&service_manager, system, "time:m");
        let time_m = time_m_handler
            .as_any()
            .downcast_ref::<TimeServiceManager>()
            .expect("time:m is not a PSC::Time::ServiceManager");
        let psc_time = time_m.shared_time();
        let core_timing = if system.is_null() {
            Arc::new(crate::core_timing::CoreTiming::new())
        } else {
            system.get().core_timing()
        };
        let file_timestamp_worker = Arc::new(Mutex::new(FileTimestampWorker::new()));
        let steady_clock_resource = Arc::new(Mutex::new(StandardSteadyClockResource::new()));
        let time_zone_binary = Arc::new(Mutex::new(TimeZoneBinary::new(system)));

        Self {
            system,
            worker: TimeWorker::new(
                system,
                Arc::clone(&time_m_handler),
                core_timing,
                Arc::clone(&steady_clock_resource),
                Arc::clone(&file_timestamp_worker),
            ),
            file_timestamp_worker,
            steady_clock_resource,
            time_zone_binary,
            psc_time,
            time_sm: time_m.get_static_service_as_service_manager(),
        }
    }

    pub fn make_static_service(&self, setup_info: StaticServiceSetupInfo) -> PscStaticService {
        PscStaticService::with_time_manager(setup_info, Arc::clone(&self.psc_time))
    }

    pub fn initialize(&mut self, service_manager: &Arc<Mutex<ServiceManager>>) {
        log::info!("Glue::Time::TimeManager: starting initialization");

        let set_sys_handler =
            ServiceManager::get_service_blocking(service_manager, self.system, "set:sys");
        let set_sys = set_sys_handler
            .as_any()
            .downcast_ref::<SystemSettingsService>()
            .expect("set:sys is not an ISystemSettingsServer");

        let time_m_handler =
            ServiceManager::get_service_blocking(service_manager, self.system, "time:m");
        let time_m = time_m_handler
            .as_any()
            .downcast_ref::<TimeServiceManager>()
            .expect("time:m is not a PSC::Time::ServiceManager");

        let res = self.time_zone_binary.lock().unwrap().mount();
        if res.is_error() {
            log::error!("TimeManager: TimeZoneBinary::Mount failed");
        }

        self.worker
            .initialize(Arc::clone(&self.time_sm), Arc::clone(&set_sys_handler));

        {
            let mut file_timestamp_worker = self.file_timestamp_worker.lock().unwrap();
            file_timestamp_worker.system_clock =
                Some(Arc::new(self.time_sm.get_standard_user_system_clock()));
            file_timestamp_worker.time_zone = Some(Arc::new(self.time_sm.get_time_zone_service()));
        }

        self.setup_standard_steady_clock_core(set_sys, time_m);

        let user_clock_context = {
            let inner = set_sys.inner.lock().unwrap();
            decode_system_clock_context(&inner.get_user_system_clock_context())
        };

        let mut epoch_time = get_epoch_time_from_initial_year(set_sys);
        if user_clock_context == SystemClockContext::default() {
            if let Ok(rtc_time) = self
                .steady_clock_resource
                .lock()
                .unwrap()
                .get_rtc_time_in_seconds()
            {
                epoch_time = rtc_time;
            }
        }
        let _ = time_m.setup_standard_local_system_clock_core(&user_clock_context, epoch_time);

        let (network_clock_context, network_accuracy_ns) = {
            let inner = set_sys.inner.lock().unwrap();
            let network_clock_context =
                decode_system_clock_context(&inner.get_network_system_clock_context());
            let accuracy_minutes = inner
                .get_settings_item_value_i32(
                    "time",
                    "standard_network_clock_sufficient_accuracy_minutes",
                )
                .unwrap_or(43_200);
            (
                network_clock_context,
                i64::from(accuracy_minutes) * 60 * 1_000_000_000,
            )
        };
        let _ = time_m
            .setup_standard_network_system_clock_core(network_clock_context, network_accuracy_ns);

        let (automatic_correction_enabled, automatic_correction_time_point) = {
            let inner = set_sys.inner.lock().unwrap();
            (
                inner.is_user_system_clock_automatic_correction_enabled(),
                decode_steady_clock_time_point(
                    &inner.get_user_system_clock_automatic_correction_updated_time(),
                ),
            )
        };
        let _ = time_m.setup_standard_user_system_clock_core(
            automatic_correction_enabled,
            automatic_correction_time_point,
        );

        let _ = time_m.setup_ephemeral_network_system_clock_core();

        self.setup_time_zone_service_core(set_sys, time_m);

        let _ = self
            .steady_clock_resource
            .lock()
            .unwrap()
            .get_rtc_time_in_seconds();

        self.worker.start_thread();
        self.file_timestamp_worker.lock().unwrap().initialized = true;

        log::info!("Glue::Time::TimeManager: initialization complete");
    }

    fn setup_standard_steady_clock_core(
        &mut self,
        set_sys: &SystemSettingsService,
        time_m: &TimeServiceManager,
    ) {
        let (external_clock_source_id, external_internal_offset_s, test_offset_minutes) = {
            let inner = set_sys.inner.lock().unwrap();
            (
                inner.get_external_steady_clock_source_id(),
                inner.get_external_steady_clock_internal_offset(),
                inner
                    .get_settings_item_value_i32(
                        "time",
                        "standard_steady_clock_test_offset_minutes",
                    )
                    .unwrap_or(0),
            )
        };

        let external_internal_offset_ns = external_internal_offset_s * 1_000_000_000;
        let standard_steady_clock_test_offset_ns =
            i64::from(test_offset_minutes) * 60 * 1_000_000_000;

        let mut steady_clock_resource = self.steady_clock_resource.lock().unwrap();
        let reset_detected = steady_clock_resource.get_reset_detected();
        let mut candidate_source_id = external_clock_source_id;
        if reset_detected {
            candidate_source_id = [0u8; 16];
        }

        let mut clock_source_id = [0u8; 16];
        steady_clock_resource.initialize(Some(&mut clock_source_id), &candidate_source_id);

        if clock_source_id != external_clock_source_id {
            set_sys
                .inner
                .lock()
                .unwrap()
                .set_external_steady_clock_source_id(clock_source_id);
        }

        let _ = time_m.setup_standard_steady_clock_core(
            reset_detected,
            clock_source_id,
            steady_clock_resource.get_time(),
            external_internal_offset_ns,
            standard_steady_clock_test_offset_ns,
        );
    }

    fn setup_time_zone_service_core(
        &mut self,
        set_sys: &SystemSettingsService,
        time_m: &TimeServiceManager,
    ) {
        let raw_name = set_sys
            .inner
            .lock()
            .unwrap()
            .get_device_time_zone_location_name();
        let mut time_zone_binary = self.time_zone_binary.lock().unwrap();
        let name = get_time_zone_string(&time_zone_binary, raw_name);

        if name != raw_name {
            {
                let mut inner = set_sys.inner.lock().unwrap();
                inner.set_device_time_zone_location_name(name);

                let context = self
                    .psc_time
                    .lock()
                    .unwrap()
                    .standard_local_system_clock
                    .clock
                    .get_context()
                    .unwrap_or_default();
                inner.set_device_time_zone_location_updated_time(encode_steady_clock_time_point(
                    &context.steady_time_point,
                ));
            }
        }

        let time_point = {
            let inner = set_sys.inner.lock().unwrap();
            decode_steady_clock_time_point(&inner.get_device_time_zone_location_updated_time())
        };

        let location_count = time_zone_binary.get_time_zone_count();
        let rule_version = time_zone_binary
            .get_time_zone_version()
            .unwrap_or([0u8; 0x10]);
        let rule_buffer = time_zone_binary
            .get_time_zone_rule(&name)
            .unwrap_or_default();
        if rule_buffer.is_empty() {
            log::error!("TimeManager: failed to read timezone rule");
        }

        let _ = time_m.setup_time_zone_service_core(
            &name,
            &rule_version,
            location_count,
            &time_point,
            &rule_buffer,
        );
    }
}

fn get_epoch_time_from_initial_year(set_sys: &SystemSettingsService) -> i64 {
    let year = set_sys
        .inner
        .lock()
        .unwrap()
        .get_settings_item_value_i32("time", "standard_user_clock_initial_year")
        .unwrap_or(2000);

    calendar_time_to_epoch(CalendarTime {
        year: year as i16,
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        second: 0,
    })
}

fn get_time_zone_string(
    time_zone_binary: &crate::hle::service::glue::time::time_zone_binary::TimeZoneBinary,
    _in_name: [u8; 0x24],
) -> [u8; 0x24] {
    let time_zone_index = *common::settings::values().time_zone_index.get_value() as usize;
    let configured_zone = if time_zone_index == 0 {
        common::time_zone::find_system_time_zone()
    } else {
        common::time_zone::get_time_zone_strings()
            .get(time_zone_index)
            .copied()
            .map(str::to_string)
            .unwrap_or_else(common::time_zone::get_default_time_zone)
    };

    let mut configured_name = [0u8; 0x24];
    let configured_bytes = configured_zone.as_bytes();
    let copy_len = configured_bytes
        .len()
        .min(configured_name.len().saturating_sub(1));
    configured_name[..copy_len].copy_from_slice(&configured_bytes[..copy_len]);

    if !time_zone_binary.is_valid(&configured_name) {
        let fallback_zone = common::time_zone::find_system_time_zone();
        configured_name = [0u8; 0x24];
        let fallback_bytes = fallback_zone.as_bytes();
        let copy_len = fallback_bytes
            .len()
            .min(configured_name.len().saturating_sub(1));
        configured_name[..copy_len].copy_from_slice(&fallback_bytes[..copy_len]);
    }
    configured_name
}

fn decode_system_clock_context(bytes: &[u8; 0x20]) -> SystemClockContext {
    unsafe { std::ptr::read_unaligned(bytes.as_ptr() as *const SystemClockContext) }
}

fn decode_steady_clock_time_point(bytes: &[u8; 0x18]) -> SteadyClockTimePoint {
    unsafe { std::ptr::read_unaligned(bytes.as_ptr() as *const SteadyClockTimePoint) }
}

fn encode_steady_clock_time_point(time_point: &SteadyClockTimePoint) -> [u8; 0x18] {
    let mut out = [0u8; 0x18];
    unsafe {
        std::ptr::copy_nonoverlapping(
            time_point as *const SteadyClockTimePoint as *const u8,
            out.as_mut_ptr(),
            core::mem::size_of::<SteadyClockTimePoint>(),
        );
    }
    out
}

fn calendar_time_to_epoch(calendar: CalendarTime) -> i64 {
    const MONTH_START_DAY_OF_YEAR: [i32; 12] =
        [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];

    let month_s16 = calendar.month as i16;
    let month = (((month_s16 * 43) & !i16::MAX) + ((month_s16 * 43) >> 9)) as i8;
    let mut month_index = calendar.month - 12 * month;
    if month_index == 0 {
        month_index = 12;
    }
    let year = (month as i32 + calendar.year as i32) - i32::from(month_index == 0);
    let v8 = if year >= 0 { year } else { year + 3 };

    let mut days_since_epoch =
        calendar.day as i64 + MONTH_START_DAY_OF_YEAR[month_index as usize - 1] as i64;
    days_since_epoch +=
        (year as i64 * 365) + (v8 / 4) as i64 - (year / 100) as i64 + (year / 400) as i64 - 365;

    let is_leap = |y: i32| -> bool { (y % 4 == 0) && ((y % 100 != 0) || (y % 400 == 0)) };
    if month_index <= 2 && is_leap(year) {
        days_since_epoch -= 1;
    }

    (((24 * days_since_epoch + calendar.hour as i64) * 60 + calendar.minute as i64) * 60
        + calendar.second as i64)
        - 62_135_683_200
}

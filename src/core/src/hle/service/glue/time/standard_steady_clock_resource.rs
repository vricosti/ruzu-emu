// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/glue/time/standard_steady_clock_resource.h
//! Port of zuyu/src/core/hle/service/glue/time/standard_steady_clock_resource.cpp
//!
//! StandardSteadyClockResource: manages RTC time and boot time for the steady clock.

use crate::core::SystemRef;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::psc::time::common::{convert_to_time_span_ns, ClockSourceId};
use std::sync::Mutex;

/// Constants matching upstream.
///
/// Max77620PmicSession and Max77620RtcSession in upstream.
#[allow(dead_code)]
const MAX77620_PMIC_SESSION: u32 = 0x3A000001;
#[allow(dead_code)]
const MAX77620_RTC_SESSION: u32 = 0x3B000001;

/// Get the current wall-clock time in seconds since epoch.
///
/// Corresponds to `GetTimeInSeconds` in upstream standard_steady_clock_resource.cpp.
fn get_time_in_seconds() -> Result<i64, ResultCode> {
    let time_s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let settings = common::settings::values();
    Ok(if *settings.custom_rtc_enabled.get_value() {
        time_s.wrapping_add(*settings.custom_rtc_offset.get_value())
    } else {
        time_s
    })
}

/// StandardSteadyClockResource manages the RTC-derived boot time.
///
/// Corresponds to `StandardSteadyClockResource` in upstream.
pub struct StandardSteadyClockResource {
    system: SystemRef,
    mutex: Mutex<()>,
    clock_source_id: ClockSourceId,
    time: i64,
    set_time_result: ResultCode,
    rtc_reset: bool,
}

impl StandardSteadyClockResource {
    pub fn new(system: SystemRef) -> Self {
        assert!(
            cfg!(test) || !system.is_null(),
            "RTC resource requires a live System"
        );
        Self {
            system,
            mutex: Mutex::new(()),
            clock_source_id: [0u8; 16],
            time: 0,
            set_time_result: RESULT_SUCCESS,
            rtc_reset: false,
        }
    }

    fn clock_ticks(&self) -> u64 {
        #[cfg(test)]
        if self.system.is_null() {
            return 0;
        }
        self.system.get().core_timing().get_clock_ticks()
    }

    fn sleep_for_retry(&self) {
        #[cfg(test)]
        if self.system.is_null() {
            return;
        }
        crate::hle::kernel::svc::svc_thread::sleep_thread(self.system.get(), 1_000_000);
    }

    /// Initialize the resource, attempting to read the RTC.
    ///
    /// Corresponds to `StandardSteadyClockResource::Initialize` in upstream.
    pub fn initialize(
        &mut self,
        out_source_id: Option<&mut ClockSourceId>,
        external_source_id: &ClockSourceId,
    ) {
        const NUM_TRIES: usize = 20;

        let mut succeeded = false;
        let mut last_result = RESULT_SUCCESS;

        for _ in 0..NUM_TRIES {
            last_result = self.set_current_time();
            if last_result.is_success() {
                succeeded = true;
                break;
            }
            self.sleep_for_retry();
        }

        if succeeded {
            self.set_time_result = RESULT_SUCCESS;
            let empty_id: ClockSourceId = [0u8; 16];
            if *external_source_id != empty_id {
                self.clock_source_id = *external_source_id;
            } else {
                // Generate a random UUID
                self.clock_source_id = rand_clock_source_id();
            }
        } else {
            self.set_time_result = last_result;
            // Use a negative boot-time offset
            self.time = convert_to_time_span_ns(self.clock_ticks() as i64).wrapping_neg();
            self.clock_source_id = rand_clock_source_id();
        }

        if let Some(out) = out_source_id {
            *out = self.clock_source_id;
        }
    }

    /// Get the current boot-time offset.
    ///
    /// Corresponds to `StandardSteadyClockResource::GetTime` in upstream.
    pub fn get_time(&self) -> i64 {
        self.time
    }

    /// Check if an RTC reset was detected.
    ///
    /// Corresponds to `StandardSteadyClockResource::GetResetDetected` in upstream.
    pub fn get_reset_detected(&mut self) -> bool {
        // Upstream calls Rtc::GetRtcResetDetected(Max77620RtcSession).
        // If detected, it calls SetSys::SetExternalSteadyClockSourceId
        // with an invalid ID and Rtc::ClearRtcResetDetected. Since we
        // don't have RTC hardware access, we always report no reset
        // (matching upstream's effective behavior on non-Switch hardware).
        self.rtc_reset = false;
        self.rtc_reset
    }

    /// Set the current boot time from RTC.
    ///
    /// Corresponds to `StandardSteadyClockResource::SetCurrentTime` in upstream.
    pub fn set_current_time(&mut self) -> ResultCode {
        let start_tick = self.clock_ticks();
        let rtc_time_s = match get_time_in_seconds() {
            Ok(t) => t,
            Err(e) => return e,
        };

        let end_tick = self.clock_ticks();
        let boot_time = match boot_time_from_rtc(rtc_time_s, start_tick, end_tick) {
            Ok(time) => time,
            Err(error) => return error,
        };

        let _lock = self.mutex.lock().unwrap();
        self.time = boot_time;
        RESULT_SUCCESS
    }

    /// Get the RTC time in seconds.
    ///
    /// Corresponds to `StandardSteadyClockResource::GetRtcTimeInSeconds` in upstream.
    pub fn get_rtc_time_in_seconds(&self) -> Result<i64, ResultCode> {
        get_time_in_seconds()
    }

    /// Update the boot time (called periodically).
    ///
    /// Corresponds to `StandardSteadyClockResource::UpdateTime` in upstream.
    pub fn update_time(&mut self) {
        const NUM_TRIES: usize = 3;

        for _ in 0..NUM_TRIES {
            let res = self.set_current_time();
            if res.is_success() {
                break;
            }
            self.sleep_for_retry();
        }
    }
}

/// Generate a random clock source ID (UUID).
fn rand_clock_source_id() -> ClockSourceId {
    common::uuid::UUID::make_random().uuid
}

// Mechanical extraction of SetCurrentTime's arithmetic for boundary tests.
fn boot_time_from_rtc(seconds: i64, start_tick: u64, end_tick: u64) -> Result<i64, ResultCode> {
    if convert_to_time_span_ns(end_tick.wrapping_sub(start_tick) as i64) >= 101_000_000 {
        return Err(crate::hle::service::psc::time::errors::RESULT_RTC_TIMEOUT);
    }
    Ok(seconds
        .wrapping_mul(1_000_000_000)
        .wrapping_sub(convert_to_time_span_ns(end_tick as i64)))
}

#[cfg(test)]
mod settings_tests {
    use super::*;

    #[test]
    fn boot_time_subtracts_elapsed_counter_ticks() {
        let ticks = common::wall_clock::CNTFRQ * 7;
        assert_eq!(
            boot_time_from_rtc(100, ticks, ticks).unwrap(),
            93_000_000_000
        );
    }

    #[test]
    fn rtc_timeout_includes_exactly_101_milliseconds() {
        let boundary = common::wall_clock::CNTFRQ * 101 / 1000;
        assert!(boot_time_from_rtc(100, 0, boundary - 1).is_ok());
        assert_eq!(
            boot_time_from_rtc(100, 0, boundary),
            Err(crate::hle::service::psc::time::errors::RESULT_RTC_TIMEOUT)
        );
        assert!(boot_time_from_rtc(100, 0, boundary + 1).is_err());
    }

    #[test]
    fn initialization_preserves_external_clock_identity() {
        let mut resource = StandardSteadyClockResource::new(SystemRef::null());
        let external = [0x42; 16];
        let mut output = [0; 16];
        resource.initialize(Some(&mut output), &external);
        assert_eq!(output, external);
    }

    #[test]
    fn rtc_applies_signed_offset_only_when_enabled() {
        struct Restore(bool, i64);
        impl Drop for Restore {
            fn drop(&mut self) {
                let mut settings = common::settings::values_mut();
                settings.custom_rtc_enabled.set_value(self.0);
                settings.custom_rtc_offset.set_value(self.1);
            }
        }
        let _restore = {
            let settings = common::settings::values();
            Restore(
                *settings.custom_rtc_enabled.get_value(),
                *settings.custom_rtc_offset.get_value(),
            )
        };
        for (enabled, offset) in [(false, 86_400), (true, 86_400), (true, -86_400)] {
            {
                let mut settings = common::settings::values_mut();
                settings.custom_rtc_enabled.set_value(enabled);
                settings.custom_rtc_offset.set_value(offset);
            }
            let before = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            let actual = StandardSteadyClockResource::new(SystemRef::null())
                .get_rtc_time_in_seconds()
                .unwrap();
            let after = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            let expected_offset = if enabled { offset } else { 0 };
            assert!((before + expected_offset..=after + expected_offset).contains(&actual));
        }
    }
}

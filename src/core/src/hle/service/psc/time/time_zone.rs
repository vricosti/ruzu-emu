// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/psc/time/time_zone.h/.cpp
//!
//! TimeZone: manages timezone rules and conversions.
//! TZif binary parsing is in the `tzif` sibling module.

use std::sync::Mutex;

use super::common::{
    CalendarAdditionalInfo, CalendarTime, LocationName, RuleVersion, SteadyClockTimePoint,
};
use super::errors::{
    RESULT_CLOCK_UNINITIALIZED, RESULT_OVERFLOW, RESULT_TIME_ZONE_NOT_FOUND,
    RESULT_TIME_ZONE_OUT_OF_RANGE, RESULT_TIME_ZONE_PARSE_FAILED,
};
pub use super::tzif::{
    detzcode, detzcode64, localsub, mktime_tzname, parse_posix_tz, CalendarTimeInternal, TtInfo,
    TzRule, TM_YEAR_BASE,
};
use super::tzif::{TZ_MAX_CHARS, TZ_MAX_TIMES, TZ_MAX_TYPES};
use crate::hle::result::{ResultCode, RESULT_SUCCESS};

/// Corresponds to the anonymous `ValidateRule` helper in upstream
/// `time_zone.cpp`.
fn validate_rule(rule: &TzRule) -> Result<(), ResultCode> {
    if rule.typecnt > TZ_MAX_TYPES as i32
        || rule.timecnt > TZ_MAX_TIMES as i32
        || rule.charcnt > TZ_MAX_CHARS as i32
    {
        return Err(RESULT_TIME_ZONE_OUT_OF_RANGE);
    }

    for i in 0..rule.timecnt.max(0) as usize {
        if i >= rule.types.len() || i >= rule.ats.len() || i32::from(rule.types[i]) >= rule.typecnt
        {
            return Err(RESULT_TIME_ZONE_OUT_OF_RANGE);
        }
    }

    for i in 0..rule.typecnt.max(0) as usize {
        if i >= rule.ttis.len() || rule.ttis[i].tt_desigidx >= rule.chars.len() as i32 {
            return Err(RESULT_TIME_ZONE_OUT_OF_RANGE);
        }
    }

    Ok(())
}

/// Corresponds to the anonymous `GetTimeZoneTime` helper in upstream
/// `time_zone.cpp`.
fn get_time_zone_time(rule: &TzRule, time: i64, index: i32, index_offset: i32) -> Option<i64> {
    let expected_index = index.checked_add(index_offset)?;
    let index = usize::try_from(index).ok()?;
    let expected = usize::try_from(expected_index).ok()?;
    let current_type = usize::from(*rule.types.get(index)?);
    let expected_type = usize::from(*rule.types.get(expected)?);
    let current_offset = rule.ttis.get(current_type)?.tt_utoff as i64;
    let expected_offset = rule.ttis.get(expected_type)?.tt_utoff as i64;
    let time_to_find = time
        .wrapping_add(current_offset)
        .wrapping_sub(expected_offset);

    let mut found_index = 0i32;
    if rule.timecnt > 1 && rule.ats[0] <= time_to_find {
        let mut low = 1usize;
        let mut high = rule.timecnt as usize;
        while low < high {
            let mid = (low + high) / 2;
            if rule.ats[mid] <= time_to_find {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        found_index = (low - 1) as i32;
    }

    (found_index == expected_index).then_some(time_to_find)
}

/// TimeZone manages timezone location, rules, and time conversions.
pub struct TimeZone {
    initialized: bool,
    mutex: Mutex<()>,
    location: LocationName,
    my_rule: TzRule,
    steady_clock_time_point: SteadyClockTimePoint,
    total_location_name_count: u32,
    rule_version: RuleVersion,
}

impl TimeZone {
    pub fn new() -> Self {
        Self {
            initialized: false,
            mutex: Mutex::new(()),
            location: [0u8; 0x24],
            my_rule: TzRule::default(),
            steady_clock_time_point: SteadyClockTimePoint::default(),
            total_location_name_count: 0,
            rule_version: [0u8; 0x10],
        }
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub fn set_initialized(&mut self) {
        self.initialized = true;
    }

    pub fn set_time_point(&mut self, time_point: &SteadyClockTimePoint) {
        let _lock = self.mutex.lock().unwrap();
        self.steady_clock_time_point = *time_point;
    }

    pub fn set_total_location_name_count(&mut self, count: u32) {
        let _lock = self.mutex.lock().unwrap();
        self.total_location_name_count = count;
    }

    pub fn set_rule_version(&mut self, rule_version: &RuleVersion) {
        let _lock = self.mutex.lock().unwrap();
        self.rule_version = *rule_version;
    }

    pub fn get_location_name(&self) -> Result<LocationName, ResultCode> {
        let _lock = self.mutex.lock().unwrap();
        if !self.initialized {
            return Err(RESULT_CLOCK_UNINITIALIZED);
        }
        Ok(self.location)
    }

    pub fn get_total_location_count(&self) -> Result<u32, ResultCode> {
        let _lock = self.mutex.lock().unwrap();
        if !self.initialized {
            return Err(RESULT_CLOCK_UNINITIALIZED);
        }
        Ok(self.total_location_name_count)
    }

    pub fn get_rule_version(&self) -> Result<RuleVersion, ResultCode> {
        let _lock = self.mutex.lock().unwrap();
        if !self.initialized {
            return Err(RESULT_CLOCK_UNINITIALIZED);
        }
        Ok(self.rule_version)
    }

    pub fn get_time_point(&self) -> Result<SteadyClockTimePoint, ResultCode> {
        let _lock = self.mutex.lock().unwrap();
        if !self.initialized {
            return Err(RESULT_CLOCK_UNINITIALIZED);
        }
        Ok(self.steady_clock_time_point)
    }

    /// Convert a POSIX time to calendar time using the given rule.
    ///
    /// Matches upstream `localtime_rz` -> `localsub` -> `timesub`.
    pub fn to_calendar_time(
        &self,
        time: i64,
        rule: &TzRule,
    ) -> Result<(CalendarTime, CalendarAdditionalInfo), ResultCode> {
        let _lock = self.mutex.lock().unwrap();
        self.to_calendar_time_impl(time, rule)
    }

    fn to_calendar_time_impl(
        &self,
        time: i64,
        rule: &TzRule,
    ) -> Result<(CalendarTime, CalendarAdditionalInfo), ResultCode> {
        validate_rule(rule)?;

        let internal = localsub(rule, time).ok_or(RESULT_OVERFLOW)?;

        let calendar = CalendarTime {
            year: (internal.tm_year + TM_YEAR_BASE as i32) as i16,
            month: (internal.tm_mon + 1) as i8, // tm_mon is 0-based, CalendarTime month is 1-based
            day: internal.tm_mday as i8,
            hour: internal.tm_hour as i8,
            minute: internal.tm_min as i8,
            second: internal.tm_sec as i8,
        };

        let mut name = [0u8; 8];
        let abbr_len = internal
            .tm_zone
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(internal.tm_zone.len());
        let copy_len = abbr_len.min(name.len() - 1);
        name[..copy_len].copy_from_slice(&internal.tm_zone[..copy_len]);

        let additional = CalendarAdditionalInfo {
            day_of_week: internal.tm_wday,
            day_of_year: internal.tm_yday,
            name,
            is_dst: internal.tm_isdst,
            ut_offset: internal.tm_utoff,
        };

        Ok((calendar, additional))
    }

    /// Convert using the device's own rule.
    pub fn to_calendar_time_with_my_rule(
        &self,
        time: i64,
    ) -> Result<(CalendarTime, CalendarAdditionalInfo), ResultCode> {
        if !self.initialized {
            return Err(RESULT_CLOCK_UNINITIALIZED);
        }
        let _lock = self.mutex.lock().unwrap();
        self.to_calendar_time_impl(time, &self.my_rule)
    }

    /// Parse a timezone binary for the given location name.
    ///
    /// Calls `TzRule::parse` to parse the TZif binary into a proper rule
    /// with transition times, type info, and abbreviations (matching upstream
    /// `ParseTimeZoneBinary` -> `tzloadbody`).
    pub fn parse_binary(&mut self, name: &LocationName, binary: &[u8]) -> ResultCode {
        let _lock = self.mutex.lock().unwrap();
        match TzRule::parse(binary) {
            Some(rule) => {
                self.my_rule = rule;
                self.location = *name;
                RESULT_SUCCESS
            }
            None => RESULT_TIME_ZONE_PARSE_FAILED,
        }
    }

    /// Parse a timezone binary into an output rule.
    pub fn parse_binary_into(&self, rule: &mut TzRule, binary: &[u8]) -> ResultCode {
        let _lock = self.mutex.lock().unwrap();
        match TzRule::parse(binary) {
            Some(parsed) => {
                *rule = parsed;
                RESULT_SUCCESS
            }
            None => RESULT_TIME_ZONE_PARSE_FAILED,
        }
    }

    /// Convert calendar time to POSIX time using the given rule.
    ///
    /// Matches upstream `mktime_tzname` -> `time1`.
    pub fn to_posix_time(
        &self,
        out_times: &mut [i64],
        calendar: &CalendarTime,
        rule: &TzRule,
    ) -> Result<u32, ResultCode> {
        let _lock = self.mutex.lock().unwrap();
        let count = match self.to_posix_time_impl(out_times, calendar, rule, -1) {
            Err(rc) if rc == RESULT_TIME_ZONE_NOT_FOUND => 0,
            result => result?,
        };
        if count == 2 && out_times[0] > out_times[1] {
            out_times.swap(0, 1);
        }
        Ok(count)
    }

    fn to_posix_time_impl(
        &self,
        out_times: &mut [i64],
        calendar: &CalendarTime,
        rule: &TzRule,
        is_dst: i32,
    ) -> Result<u32, ResultCode> {
        validate_rule(rule)?;
        if out_times.is_empty() {
            return Ok(0);
        }

        let local_calendar = CalendarTime {
            year: calendar.year,
            month: calendar.month.wrapping_sub(1),
            day: calendar.day,
            hour: calendar.hour,
            minute: calendar.minute,
            second: calendar.second,
        };
        let mut internal = CalendarTimeInternal {
            tm_sec: calendar.second as i32,
            tm_min: calendar.minute as i32,
            tm_hour: calendar.hour as i32,
            tm_mday: calendar.day as i32,
            tm_mon: calendar.month as i32 - 1,
            tm_year: calendar.year as i32 - TM_YEAR_BASE as i32,
            tm_wday: 0,
            tm_yday: 0,
            tm_isdst: is_dst,
            tm_zone: [0u8; 16],
            tm_utoff: 0,
            time_index: 0,
        };

        let mut time = 0i64;
        match mktime_tzname(&mut time, rule, &mut internal) {
            0 => {}
            1 => return Err(RESULT_OVERFLOW),
            2 => return Err(RESULT_TIME_ZONE_NOT_FOUND),
            _ => unreachable!("mktime_tzname returned an invalid status"),
        }

        if internal.tm_sec != i32::from(local_calendar.second)
            || internal.tm_min != i32::from(local_calendar.minute)
            || internal.tm_hour != i32::from(local_calendar.hour)
            || internal.tm_mday != i32::from(local_calendar.day)
            || internal.tm_mon != i32::from(local_calendar.month)
            || internal.tm_year != i32::from(local_calendar.year) - TM_YEAR_BASE as i32
        {
            return Err(RESULT_TIME_ZONE_NOT_FOUND);
        }

        out_times[0] = time;
        if out_times.len() < 2 {
            return Ok(1);
        }

        if internal.time_index > 0 {
            if let Some(time2) = get_time_zone_time(rule, time, internal.time_index, -1) {
                out_times[1] = time2;
                return Ok(2);
            }
        }

        if internal.time_index + 1 < rule.timecnt {
            if let Some(time2) = get_time_zone_time(rule, time, internal.time_index, 1) {
                out_times[1] = time2;
                return Ok(2);
            }
        }

        Ok(1)
    }

    /// Convert calendar time to POSIX time using the device's own rule.
    pub fn to_posix_time_with_my_rule(
        &self,
        out_times: &mut [i64],
        calendar: &CalendarTime,
    ) -> Result<u32, ResultCode> {
        let _lock = self.mutex.lock().unwrap();
        let count = match self.to_posix_time_impl(out_times, calendar, &self.my_rule, -1) {
            Err(rc) if rc == RESULT_TIME_ZONE_NOT_FOUND => 0,
            result => result?,
        };
        if count == 2 && out_times[0] > out_times[1] {
            out_times.swap(0, 1);
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc_rule() -> TzRule {
        parse_posix_tz(b"UTC0").expect("fixed UTC rule")
    }

    fn ambiguous_rule() -> TzRule {
        let mut rule = TzRule::default();
        rule.timecnt = 2;
        rule.typecnt = 2;
        rule.charcnt = 8;
        rule.ats[0] = 0;
        rule.types[0] = 0;
        rule.ats[1] = 7_200;
        rule.types[1] = 1;
        rule.ttis[0] = TtInfo {
            tt_utoff: 3_600,
            tt_isdst: 1,
            tt_desigidx: 0,
            ..TtInfo::default()
        };
        rule.ttis[1] = TtInfo {
            tt_utoff: 0,
            tt_isdst: 0,
            tt_desigidx: 4,
            ..TtInfo::default()
        };
        rule.chars[..8].copy_from_slice(b"DST\0STD\0");
        rule.default_type = 1;
        rule
    }

    #[test]
    fn test_default_rule_matches_cpp_value_initialization() {
        let rule = TzRule::default();
        assert_eq!(rule.timecnt, 0);
        assert_eq!(rule.typecnt, 0);
        assert_eq!(rule.ttis[0].tt_utoff, 0);
        assert_eq!(rule.ttis[0].tt_isdst, 0);
        assert!(rule.as_bytes().iter().all(|byte| *byte == 0));
    }

    #[test]
    fn test_utc_epoch_conversion() {
        let tz = TimeZone::new();
        let rule = utc_rule();
        let (cal, info) = tz.to_calendar_time(0, &rule).unwrap();
        assert_eq!(cal.year, 1970);
        assert_eq!(cal.month, 1);
        assert_eq!(cal.day, 1);
        assert_eq!(cal.hour, 0);
        assert_eq!(cal.minute, 0);
        assert_eq!(cal.second, 0);
        assert_eq!(info.day_of_week, 4); // Thursday
        assert_eq!(info.day_of_year, 0);
        assert_eq!(info.ut_offset, 0);
        assert_eq!(info.is_dst, 0);
    }

    #[test]
    fn test_utc_known_date() {
        // 2023-06-15 13:40:00 UTC = 1686836400
        let tz = TimeZone::new();
        let rule = utc_rule();
        let (cal, info) = tz.to_calendar_time(1686836400, &rule).unwrap();
        assert_eq!(cal.year, 2023);
        assert_eq!(cal.month, 6);
        assert_eq!(cal.day, 15);
        assert_eq!(cal.hour, 13);
        assert_eq!(cal.minute, 40);
        assert_eq!(cal.second, 0);
        assert_eq!(info.day_of_week, 4); // Thursday
        assert_eq!(info.ut_offset, 0);
    }

    #[test]
    fn test_utc_roundtrip() {
        let tz = TimeZone::new();
        let rule = utc_rule();

        // Forward conversion
        let (cal, _) = tz.to_calendar_time(1686836400, &rule).unwrap();

        // Reverse conversion
        let mut out = [0i64; 1];
        let count = tz.to_posix_time(&mut out, &cal, &rule).unwrap();
        assert_eq!(count, 1);
        assert_eq!(out[0], 1686836400);
    }

    #[test]
    fn reverse_conversion_returns_both_ordered_ambiguous_times() {
        let time_zone = TimeZone::new();
        let calendar = CalendarTime {
            year: 1970,
            month: 1,
            day: 1,
            hour: 2,
            minute: 30,
            second: 0,
        };
        let mut out_times = [0i64; 2];

        let count = time_zone
            .to_posix_time(&mut out_times, &calendar, &ambiguous_rule())
            .unwrap();

        assert_eq!(count, 2);
        assert_eq!(out_times, [5_400, 9_000]);
    }

    #[test]
    fn reverse_conversion_maps_normalized_calendar_to_zero_results() {
        let time_zone = TimeZone::new();
        let calendar = CalendarTime {
            year: 1970,
            month: 13,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
        };
        let mut out_times = [i64::MIN; 2];

        let count = time_zone
            .to_posix_time(&mut out_times, &calendar, &utc_rule())
            .unwrap();

        assert_eq!(count, 0);
    }

    #[test]
    fn test_detzcode() {
        // Big-endian 0x00000000 = 0
        assert_eq!(detzcode(&[0, 0, 0, 0]), 0);
        // Big-endian 0x00000001 = 1
        assert_eq!(detzcode(&[0, 0, 0, 1]), 1);
        // Big-endian 0xFFFFFFFF = -1
        assert_eq!(detzcode(&[0xFF, 0xFF, 0xFF, 0xFF]), -1);
        // Big-endian 0x80000000 = i32::MIN
        assert_eq!(detzcode(&[0x80, 0, 0, 0]), i32::MIN);
    }

    #[test]
    fn test_detzcode64() {
        assert_eq!(detzcode64(&[0, 0, 0, 0, 0, 0, 0, 0]), 0);
        assert_eq!(detzcode64(&[0, 0, 0, 0, 0, 0, 0, 1]), 1);
        assert_eq!(
            detzcode64(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]),
            -1
        );
    }

    #[test]
    fn test_tzif_magic_validation() {
        // Invalid magic
        assert!(TzRule::parse(b"XXXX").is_none());
        // Too short
        assert!(TzRule::parse(b"TZif").is_none());
    }

    #[test]
    fn initialization_gates_state_and_parse_failure_preserves_outputs() {
        let mut time_zone = TimeZone::new();
        assert_eq!(
            time_zone.get_location_name(),
            Err(RESULT_CLOCK_UNINITIALIZED)
        );
        assert_eq!(
            time_zone.get_total_location_count(),
            Err(RESULT_CLOCK_UNINITIALIZED)
        );
        assert_eq!(
            time_zone.get_rule_version(),
            Err(RESULT_CLOCK_UNINITIALIZED)
        );
        assert_eq!(time_zone.get_time_point(), Err(RESULT_CLOCK_UNINITIALIZED));
        assert_eq!(
            time_zone.to_calendar_time_with_my_rule(0).unwrap_err(),
            RESULT_CLOCK_UNINITIALIZED
        );

        time_zone.set_initialized();
        let original_name = time_zone.get_location_name().unwrap();
        let mut replacement_name = [0u8; 0x24];
        replacement_name[..7].copy_from_slice(b"Etc/GMT");
        assert_eq!(
            time_zone.parse_binary(&replacement_name, b"invalid tzif"),
            RESULT_TIME_ZONE_PARSE_FAILED
        );
        assert_eq!(time_zone.get_location_name().unwrap(), original_name);

        let mut output_rule = TzRule::default();
        output_rule.typecnt = 7;
        assert_eq!(
            time_zone.parse_binary_into(&mut output_rule, b"invalid tzif"),
            RESULT_TIME_ZONE_PARSE_FAILED
        );
        assert_eq!(output_rule.typecnt, 7);
    }

    #[test]
    fn conversion_rejects_out_of_range_rule_counts() {
        let time_zone = TimeZone::new();
        let mut rule = TzRule::default();
        rule.typecnt = 129;
        assert_eq!(
            time_zone.to_calendar_time(0, &rule).unwrap_err(),
            RESULT_TIME_ZONE_OUT_OF_RANGE
        );

        let mut out_times = [0i64; 2];
        assert_eq!(
            time_zone.to_posix_time(&mut out_times, &CalendarTime::default(), &rule),
            Err(RESULT_TIME_ZONE_OUT_OF_RANGE)
        );
    }

    #[test]
    fn test_parse_posix_tz_simple() {
        // Simple fixed offset
        let rule = parse_posix_tz(b"UTC0").unwrap();
        assert_eq!(rule.typecnt, 1);
        assert_eq!(rule.ttis[0].tt_utoff, 0);
        assert_eq!(rule.ttis[0].tt_isdst, 0);
    }

    #[test]
    fn test_parse_posix_tz_with_dst() {
        // US Eastern
        let rule = parse_posix_tz(b"EST5EDT,M3.2.0,M11.1.0").unwrap();
        assert_eq!(rule.typecnt, 2);
        assert_eq!(rule.ttis[0].tt_utoff, -5 * 3600); // EST = UTC-5
        assert_eq!(rule.ttis[0].tt_isdst, 0);
        assert_eq!(rule.ttis[1].tt_utoff, -4 * 3600); // EDT = UTC-4
        assert_ne!(rule.ttis[1].tt_isdst, 0);
    }
}

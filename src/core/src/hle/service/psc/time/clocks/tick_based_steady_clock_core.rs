// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/psc/time/clocks/tick_based_steady_clock_core.h/.cpp
//!
//! TickBasedSteadyClockCore: a steady clock driven by CoreTiming ticks.

use super::steady_clock_core::{SteadyClockCoreImpl, SteadyClockCoreState};
use crate::hle::result::ResultCode;
use crate::hle::service::psc::time::common::{ClockSourceId, SteadyClockTimePoint};
use crate::hle::service::psc::time::errors::RESULT_NOT_IMPLEMENTED;

/// TickBasedSteadyClockCore uses CoreTiming ticks to derive the current time.
///
/// In the full system, `m_system.CoreTiming().GetClockTicks()` provides the tick source.
/// Here we store a function pointer / callback for the tick source, or use a default.
pub struct TickBasedSteadyClockCore {
    pub state: SteadyClockCoreState,
    clock_source_id: ClockSourceId,
    /// Callback to get clock ticks (nanoseconds). In upstream, this is
    /// `ConvertToTimeSpan(m_system.CoreTiming().GetClockTicks())`.
    /// Default returns 0 until wired to CoreTiming.
    get_ticks_ns: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl TickBasedSteadyClockCore {
    pub fn new(get_ticks_ns: Box<dyn Fn() -> i64 + Send + Sync>) -> Self {
        let clock_source_id = common::uuid::UUID::make_random().uuid;
        Self {
            state: SteadyClockCoreState::new(),
            clock_source_id,
            get_ticks_ns,
        }
    }

    /// Create with a default (zero) tick source. Useful for testing.
    pub fn new_default() -> Self {
        Self::new(Box::new(|| 0))
    }
}

impl SteadyClockCoreImpl for TickBasedSteadyClockCore {
    fn get_current_time_point_impl(&self) -> Result<SteadyClockTimePoint, ResultCode> {
        let ticks_ns = (self.get_ticks_ns)();
        // Convert nanoseconds to seconds (integer division, matching upstream
        // duration_cast<seconds>).
        let current_time_s = ticks_ns / 1_000_000_000;
        Ok(SteadyClockTimePoint {
            time_point: current_time_s,
            clock_source_id: self.clock_source_id,
        })
    }

    fn get_current_raw_time_point_impl(&self) -> i64 {
        // Upstream: gets time point in seconds, then converts back to nanoseconds
        match self.get_current_time_point_impl() {
            Ok(tp) => tp.time_point * 1_000_000_000,
            Err(_) => {
                log::error!("TickBasedSteadyClockCore: Failed to GetCurrentTimePoint!");
                0
            }
        }
    }

    fn get_test_offset_impl(&self) -> i64 {
        0
    }

    fn set_test_offset_impl(&mut self, _offset: i64) {
        // no-op, matching upstream
    }

    fn get_internal_offset_impl(&self) -> i64 {
        0
    }

    fn set_internal_offset_impl(&mut self, _offset: i64) {
        // no-op, matching upstream
    }

    fn get_rtc_value_impl(&self) -> Result<i64, ResultCode> {
        Err(RESULT_NOT_IMPLEMENTED)
    }

    fn get_setup_result_value_impl(&self) -> ResultCode {
        crate::hle::result::RESULT_SUCCESS
    }
}

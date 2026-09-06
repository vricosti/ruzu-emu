// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/psc/time/common.h and common.cpp

pub type ClockSourceId = [u8; 16]; // Common::UUID

/// Port of upstream `ConvertToTimeSpan`.
///
/// Converts CoreTiming CNTPCT ticks (19.2 MHz) into nanoseconds while preserving
/// the upstream saturation behavior for extreme signed values.
pub fn convert_to_time_span_ns(ticks: i64) -> i64 {
    const ONE_SECOND_NS: i64 = 1_000_000_000;
    const CNTFRQ: i64 = common::wall_clock::CNTFRQ as i64;
    const MAX: i64 = CNTFRQ * (i64::MAX / ONE_SECOND_NS);

    if ticks > MAX {
        return i64::MAX;
    }
    if ticks < -MAX {
        return i64::MIN;
    }

    let a = ticks / CNTFRQ * ONE_SECOND_NS;
    let b = ((ticks % CNTFRQ) * ONE_SECOND_NS) / CNTFRQ;
    a + b
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TimeType {
    UserSystemClock = 0,
    NetworkSystemClock = 1,
    LocalSystemClock = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct SteadyClockTimePoint {
    pub time_point: i64,
    pub clock_source_id: ClockSourceId,
}
const _: () = assert!(core::mem::size_of::<SteadyClockTimePoint>() == 0x18);

impl SteadyClockTimePoint {
    pub fn id_matches(&self, other: &SteadyClockTimePoint) -> bool {
        self.clock_source_id == other.clock_source_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct SystemClockContext {
    pub offset: i64,
    pub steady_time_point: SteadyClockTimePoint,
}
const _: () = assert!(core::mem::size_of::<SystemClockContext>() == 0x20);

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct CalendarTime {
    pub year: i16,
    pub month: i8,
    pub day: i8,
    pub hour: i8,
    pub minute: i8,
    pub second: i8,
}
const _: () = assert!(core::mem::size_of::<CalendarTime>() == 0x8);

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct CalendarAdditionalInfo {
    pub day_of_week: i32,
    pub day_of_year: i32,
    pub name: [u8; 8],
    pub is_dst: i32,
    pub ut_offset: i32,
}
const _: () = assert!(core::mem::size_of::<CalendarAdditionalInfo>() == 0x18);

pub type LocationName = [u8; 0x24];
const _: () = assert!(core::mem::size_of::<LocationName>() == 0x24);

pub type RuleVersion = [u8; 0x10];
const _: () = assert!(core::mem::size_of::<RuleVersion>() == 0x10);

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ClockSnapshot {
    pub user_context: SystemClockContext,
    pub network_context: SystemClockContext,
    pub user_time: i64,
    pub network_time: i64,
    pub user_calendar_time: CalendarTime,
    pub network_calendar_time: CalendarTime,
    pub user_calendar_additional_time: CalendarAdditionalInfo,
    pub network_calendar_additional_time: CalendarAdditionalInfo,
    pub steady_clock_time_point: SteadyClockTimePoint,
    pub location_name: LocationName,
    pub is_automatic_correction_enabled: bool,
    pub time_type: u8, // TimeType
    pub unk_ce: u16,
}
const _: () = assert!(core::mem::size_of::<ClockSnapshot>() == 0xD0);

impl Default for ClockSnapshot {
    fn default() -> Self {
        // SAFETY: ClockSnapshot is repr(C) with all-zero-valid fields.
        unsafe { core::mem::zeroed() }
    }
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct ContinuousAdjustmentTimePoint {
    pub rtc_offset: i64,
    pub diff_scale: i64,
    pub shift_amount: i64,
    pub lower: i64,
    pub upper: i64,
    pub clock_source_id: ClockSourceId,
}
const _: () = assert!(core::mem::size_of::<ContinuousAdjustmentTimePoint>() == 0x38);

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct AlarmInfo {
    pub alert_time: i64,
    pub priority: u32,
    pub _padding: u32,
}
const _: () = assert!(core::mem::size_of::<AlarmInfo>() == 0x10);

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct StaticServiceSetupInfo {
    pub can_write_local_clock: bool,
    pub can_write_user_clock: bool,
    pub can_write_network_clock: bool,
    pub can_write_timezone_device_location: bool,
    pub can_write_steady_clock: bool,
    pub can_write_uninitialized_clock: bool,
}
const _: () = assert!(core::mem::size_of::<StaticServiceSetupInfo>() == 0x6);

/// OperationEvent wraps a kernel event for context change notifications.
///
/// Corresponds to `OperationEvent` in upstream common.h.
/// In upstream, this is an intrusive list node (IntrusiveListBaseNode<OperationEvent>)
/// that holds a KEvent. ContextWriters maintain a list of these and signal all
/// of them when a context changes.
///
/// Here we use an ID-based approach: each OperationEvent gets a unique ID so it
/// can be tracked in the ContextWriter's list and removed if needed.
#[derive(Clone)]
pub struct OperationEvent {
    id: u64,
    /// Kernel event for signaling context changes.
    /// Corresponds to `Kernel::KEvent* m_event` in upstream.
    event: u32,
    ctx: std::sync::Arc<crate::hle::service::kernel_helpers::ServiceContext>,
}

static NEXT_OPERATION_EVENT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl OperationEvent {
    /// Create a new OperationEvent.
    ///
    /// Corresponds to `OperationEvent::OperationEvent(Core::System&)` in upstream.
    pub fn new() -> Self {
        let id = NEXT_OPERATION_EVENT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut ctx = crate::hle::service::kernel_helpers::ServiceContext::new("Time:OperationEvent".into());
        let event = ctx.create_event("Time:OperationEvent:Event".into());
        Self {
            id,
            event,
            ctx: std::sync::Arc::new(ctx),
        }
    }

    /// Get this event's unique ID (for list tracking).
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Signal this event's underlying kernel event.
    ///
    /// Corresponds to `m_event->Signal()` in upstream.
    pub fn signal(&self) {
        self.get_event().signal();
    }

    pub fn get_event(&self) -> std::sync::Arc<crate::hle::service::os::event::Event> {
        self.ctx.get_event(self.event).expect("operation event must exist")
    }
}

/// Get the span between two time points in seconds.
pub fn get_span_between_time_points(
    a: &SteadyClockTimePoint,
    b: &SteadyClockTimePoint,
) -> Option<i64> {
    if !a.id_matches(b) {
        return None;
    }
    if a.time_point >= 0 && b.time_point < a.time_point.wrapping_add(i64::MIN) {
        return None; // overflow
    }
    if a.time_point < 0 && b.time_point > a.time_point.wrapping_add(i64::MAX) {
        return None; // overflow
    }
    Some(b.time_point - a.time_point)
}

#[cfg(test)]
mod tests {
    use super::convert_to_time_span_ns;

    #[test]
    fn convert_to_time_span_matches_cntpct_frequency() {
        assert_eq!(convert_to_time_span_ns(0), 0);
        assert_eq!(
            convert_to_time_span_ns(common::wall_clock::CNTFRQ as i64),
            1_000_000_000
        );
        assert_eq!(
            convert_to_time_span_ns(-(common::wall_clock::CNTFRQ as i64)),
            -1_000_000_000
        );
    }
}

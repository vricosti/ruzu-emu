// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/psc/time/alarms.h/.cpp
//!
//! Alarm, Alarms, IAlarmService, ISteadyClockAlarm:
//! Timer-based alarm system that triggers power state requests when
//! the steady clock reaches a configured alert time.

use std::sync::{Arc, Mutex};

use super::errors::RESULT_CLOCK_UNINITIALIZED;
use super::power_state_request_manager::PowerStateRequestManager;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::os::event::Event;

/// Alarm type determines priority.
///
/// Corresponds to `AlarmType` enum in upstream alarms.h.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AlarmType {
    WakeupAlarm = 0,
    BackgroundTaskAlarm = 1,
}

/// IPC command IDs for IAlarmService.
///
/// Corresponds to the function table in upstream IAlarmService constructor.
pub mod alarm_service_commands {
    pub const CREATE_WAKEUP_ALARM: u32 = 0;
    pub const CREATE_BACKGROUND_TASK_ALARM: u32 = 1;
}

/// IPC command IDs for ISteadyClockAlarm.
///
/// Corresponds to the function table in upstream ISteadyClockAlarm constructor.
pub mod steady_clock_alarm_commands {
    pub const GET_ALARM_EVENT: u32 = 0;
    pub const ENABLE: u32 = 1;
    pub const DISABLE: u32 = 2;
    pub const IS_ENABLED: u32 = 3;
}

/// An individual alarm entry.
///
/// Corresponds to `Alarm` struct in upstream alarms.h.
/// In upstream, this is an intrusive list node. Here we store alarms
/// in a Vec inside the `Alarms` manager.
pub struct Alarm {
    /// Priority: WakeupAlarm=1, BackgroundTaskAlarm=0.
    priority: u32,
    /// Alert time in nanoseconds (from steady clock raw time).
    alert_time: i64,
    /// Whether this alarm is currently registered in the alarm list.
    linked: bool,
    /// Alarm type.
    alarm_type: AlarmType,
    /// Kernel event for signaling when this alarm fires.
    /// Corresponds to `Kernel::KEvent* m_event` in upstream.
    event: Arc<Event>,
    // Upstream holds an `nn::psc::sf::IPmStateLock` for Lock(). That
    // interface depends on the PSC power management module which is not
    // yet ported. Lock() is a no-op in upstream as well (the lock
    // service reference is never initialized).
}

impl Alarm {
    /// Create a new Alarm.
    ///
    /// Corresponds to `Alarm::Alarm(System&, ServiceContext&, AlarmType)` in upstream.
    pub fn new(alarm_type: AlarmType) -> Self {
        let priority = match alarm_type {
            AlarmType::WakeupAlarm => 1,
            AlarmType::BackgroundTaskAlarm => 0,
        };
        Self {
            priority,
            alert_time: 0,
            linked: false,
            alarm_type,
            event: Arc::new(Event::new()),
        }
    }

    pub fn get_alert_time(&self) -> i64 {
        self.alert_time
    }

    pub fn set_alert_time(&mut self, time: i64) {
        self.alert_time = time;
    }

    pub fn get_priority(&self) -> u32 {
        self.priority
    }

    pub fn is_linked(&self) -> bool {
        self.linked
    }

    /// Signal this alarm's event.
    ///
    /// Corresponds to `Alarm::Signal()` in upstream.
    pub fn signal(&self) {
        self.event.signal();
        log::debug!("Alarm::Signal (type={:?})", self.alarm_type);
    }

    /// Lock this alarm (for power state management).
    ///
    /// Corresponds to `Alarm::Lock()` in upstream.
    /// Upstream checks `if (m_lock_service) { m_lock_service->Lock(); }` but
    /// the lock service reference is never initialized, so this is effectively
    /// a no-op in upstream as well.
    pub fn lock(&self) -> ResultCode {
        RESULT_SUCCESS
    }
}

/// Alarm data stored in the sorted list.
struct AlarmEntry {
    alert_time: i64,
    priority: u32,
    alarm_id: usize,
}

/// Alarms manages a sorted list of alarms and checks/signals them
/// based on the steady clock.
///
/// Corresponds to `Alarms` class in upstream alarms.h.
pub struct Alarms {
    inner: Mutex<AlarmsInner>,
    /// Kernel event signaled when the closest alarm changes.
    /// Corresponds to `Kernel::KEvent* m_event` in upstream.
    event: u32,
    ctx: crate::hle::service::kernel_helpers::ServiceContext,
}

struct AlarmsInner {
    /// Sorted alarm entries (by alert_time, then priority).
    entries: Vec<AlarmEntry>,
    /// Next alarm ID for tracking.
    next_id: usize,
    /// Whether the steady clock is initialized.
    steady_clock_initialized: bool,
    /// Callback to get raw time from the steady clock in nanoseconds.
    get_raw_time: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl Alarms {
    /// Create a new Alarms manager.
    ///
    /// Corresponds to `Alarms::Alarms(System&, StandardSteadyClockCore&,
    ///   PowerStateRequestManager&)` in upstream.
    ///
    /// `get_raw_time` provides the steady clock's raw time in nanoseconds.
    pub fn new(get_raw_time: Box<dyn Fn() -> i64 + Send + Sync>) -> Self {
        let mut ctx = crate::hle::service::kernel_helpers::ServiceContext::new("Psc:Alarms".into());
        let event = ctx.create_event("Psc:Alarms:Event".into());
        Self {
            inner: Mutex::new(AlarmsInner {
                entries: Vec::new(),
                next_id: 0,
                steady_clock_initialized: false,
                get_raw_time,
            }),
            event,
            ctx,
        }
    }

    /// Mark the steady clock as initialized.
    pub fn set_steady_clock_initialized(&self, initialized: bool) {
        let mut inner = self.inner.lock().unwrap();
        inner.steady_clock_initialized = initialized;
    }

    /// Enable an alarm at the given time offset from now.
    ///
    /// The time is aligned up to 1-second granularity in nanoseconds.
    /// Corresponds to `Alarms::Enable` in upstream.
    pub fn enable(&self, alarm: &mut Alarm, time: i64) -> ResultCode {
        let mut inner = self.inner.lock().unwrap();
        if !inner.steady_clock_initialized {
            return RESULT_CLOCK_UNINITIALIZED;
        }

        if alarm.is_linked() {
            // Already registered — remove first
            inner
                .entries
                .retain(|e| e.alarm_id != 0 || e.alert_time != alarm.alert_time);
            alarm.linked = false;
        }

        let raw_time = (inner.get_raw_time)();
        let one_second_ns: i64 = 1_000_000_000;
        // AlignUp to 1-second boundary
        let time_ns = time + raw_time;
        let aligned_time = ((time_ns + one_second_ns - 1) / one_second_ns) * one_second_ns;
        alarm.set_alert_time(aligned_time);

        let alarm_id = inner.next_id;
        inner.next_id += 1;

        // Insert sorted by alert_time, then priority
        let entry = AlarmEntry {
            alert_time: aligned_time,
            priority: alarm.get_priority(),
            alarm_id,
        };
        let pos = inner
            .entries
            .iter()
            .position(|e| {
                aligned_time < e.alert_time
                    || (aligned_time == e.alert_time && alarm.get_priority() < e.priority)
            })
            .unwrap_or(inner.entries.len());
        inner.entries.insert(pos, entry);
        alarm.linked = true;

        // Signal the closest-alarm-updated event
        self.get_event().signal();

        RESULT_SUCCESS
    }

    /// Disable an alarm.
    ///
    /// Corresponds to `Alarms::Disable` in upstream.
    pub fn disable(&self, alarm: &mut Alarm) {
        let mut inner = self.inner.lock().unwrap();
        if !alarm.is_linked() {
            return;
        }

        inner.entries.retain(|e| {
            e.alert_time != alarm.get_alert_time() || e.priority != alarm.get_priority()
        });
        alarm.linked = false;

        // Upstream calls UpdateClosestAndSignal here, which updates the
        // m_closest_alarm pointer and signals the event if any alarms remain.
        // We signal the event unconditionally (matching upstream behavior
        // when the list is non-empty; when empty, the signal is harmless).
        self.get_event().signal();
    }

    /// Check all alarms against the current steady clock time and signal
    /// any that have triggered.
    ///
    /// Corresponds to `Alarms::CheckAndSignal` in upstream.
    pub fn check_and_signal(&self, power_state_manager: &PowerStateRequestManager) {
        let mut inner = self.inner.lock().unwrap();
        if inner.entries.is_empty() {
            return;
        }

        let raw_time = (inner.get_raw_time)();
        let mut alarm_signalled = false;
        let mut triggered_priorities = Vec::new();

        // Collect triggered alarms
        inner.entries.retain(|entry| {
            if raw_time >= entry.alert_time {
                triggered_priorities.push(entry.priority);
                alarm_signalled = true;
                false // remove from list
            } else {
                true // keep
            }
        });

        if !alarm_signalled {
            return;
        }

        for priority in &triggered_priorities {
            // In upstream, each alarm's Signal() and Lock() are called.
            // The individual alarm events are signaled via Alarm::signal() when
            // alarms are stored directly; here we signal via the power state manager.
            power_state_manager.update_pending_power_state_request_priority(*priority);
        }

        power_state_manager.signal_power_state_request_availability();

        // Upstream calls UpdateClosestAndSignal, which updates
        // m_closest_alarm and signals the event if alarms remain.
        self.get_event().signal();
    }

    /// Get the closest (soonest) alarm.
    ///
    /// Returns `Some((alert_time, priority))` if there is an alarm, `None` otherwise.
    /// Corresponds to `Alarms::GetClosestAlarm` in upstream.
    pub fn get_closest_alarm(&self) -> Option<(i64, u32)> {
        let inner = self.inner.lock().unwrap();
        inner.entries.first().map(|e| (e.alert_time, e.priority))
    }

    /// Get the raw steady clock time in nanoseconds.
    ///
    /// Corresponds to `Alarms::GetRawTime` in upstream.
    pub fn get_raw_time(&self) -> i64 {
        let inner = self.inner.lock().unwrap();
        (inner.get_raw_time)()
    }

    pub fn get_event(&self) -> Arc<Event> {
        self.ctx.get_event(self.event).expect("closest alarm event must exist")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alarm_priority_by_type() {
        let wakeup = Alarm::new(AlarmType::WakeupAlarm);
        assert_eq!(wakeup.get_priority(), 1);

        let background = Alarm::new(AlarmType::BackgroundTaskAlarm);
        assert_eq!(background.get_priority(), 0);
    }

    #[test]
    fn enable_and_disable_alarm() {
        let alarms = Alarms::new(Box::new(|| 1_000_000_000)); // 1 second in ns
        alarms.set_steady_clock_initialized(true);

        let mut alarm = Alarm::new(AlarmType::WakeupAlarm);
        assert!(!alarm.is_linked());

        let rc = alarms.enable(&mut alarm, 5_000_000_000);
        assert!(rc.is_success());
        assert!(alarm.is_linked());

        alarms.disable(&mut alarm);
        assert!(!alarm.is_linked());
    }

    #[test]
    fn enable_fails_without_initialization() {
        let alarms = Alarms::new(Box::new(|| 0));
        let mut alarm = Alarm::new(AlarmType::WakeupAlarm);

        let rc = alarms.enable(&mut alarm, 1_000_000_000);
        assert!(rc.is_error());
    }

    #[test]
    fn check_and_signal_triggers_past_alarms() {
        use std::sync::atomic::{AtomicI64, Ordering};
        use std::sync::Arc;

        let time = Arc::new(AtomicI64::new(0));
        let time_clone = Arc::clone(&time);
        let alarms = Alarms::new(Box::new(move || time_clone.load(Ordering::Relaxed)));
        alarms.set_steady_clock_initialized(true);

        let power_mgr = PowerStateRequestManager::new();

        // Enable alarm at time offset 2 seconds
        let mut alarm = Alarm::new(AlarmType::WakeupAlarm);
        time.store(1_000_000_000, Ordering::Relaxed);
        let rc = alarms.enable(&mut alarm, 2_000_000_000);
        assert!(rc.is_success());

        // Check before alarm time — nothing should trigger
        time.store(2_000_000_000, Ordering::Relaxed);
        alarms.check_and_signal(&power_mgr);
        let (had, _) = power_mgr.get_and_clear_power_state_request();
        assert!(!had);

        // Advance past alarm time — should trigger
        time.store(4_000_000_000, Ordering::Relaxed);
        alarms.check_and_signal(&power_mgr);
        let (had, priority) = power_mgr.get_and_clear_power_state_request();
        assert!(had);
        assert_eq!(priority, 1); // WakeupAlarm priority
    }

    #[test]
    fn closest_alarm_is_earliest() {
        let alarms = Alarms::new(Box::new(|| 0));
        alarms.set_steady_clock_initialized(true);

        let mut alarm1 = Alarm::new(AlarmType::WakeupAlarm);
        let mut alarm2 = Alarm::new(AlarmType::BackgroundTaskAlarm);

        alarms.enable(&mut alarm1, 10_000_000_000);
        alarms.enable(&mut alarm2, 5_000_000_000);

        let closest = alarms.get_closest_alarm();
        assert!(closest.is_some());
        let (time, _) = closest.unwrap();
        // alarm2 should be first (earlier time)
        assert!(time <= alarm1.get_alert_time());
    }
}

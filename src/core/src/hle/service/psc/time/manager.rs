// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/psc/time/manager.h
//!
//! The TimeManager owns all clock cores, timezone, shared memory, power state
//! management, alarms, and context writers.

use std::sync::{Arc, Mutex};

use super::alarms::Alarms;
use super::clocks::context_writers::{
    EphemeralNetworkSystemClockContextWriter, LocalSystemClockContextWriter,
    NetworkSystemClockContextWriter,
};
use super::clocks::ephemeral_network_system_clock_core::EphemeralNetworkSystemClockCore;
use super::clocks::standard_local_system_clock_core::StandardLocalSystemClockCore;
use super::clocks::standard_network_system_clock_core::StandardNetworkSystemClockCore;
use super::clocks::standard_steady_clock_core::StandardSteadyClockCore;
use super::clocks::standard_user_system_clock_core::StandardUserSystemClockCore;
use super::clocks::tick_based_steady_clock_core::TickBasedSteadyClockCore;
use super::power_state_request_manager::PowerStateRequestManager;
use super::shared_memory::SharedMemory;
use super::time_zone::TimeZone;

/// TimeManager owns all clock cores, timezone state, shared memory, power state
/// management, alarms, and context writers.
///
/// Corresponds to `PSC::Time::TimeManager` in upstream manager.h.
pub struct TimeManager {
    pub standard_steady_clock: Arc<Mutex<StandardSteadyClockCore>>,
    pub tick_based_steady_clock: Arc<TickBasedSteadyClockCore>,
    pub standard_local_system_clock: StandardLocalSystemClockCore,
    pub standard_network_system_clock: StandardNetworkSystemClockCore,
    pub standard_user_system_clock: StandardUserSystemClockCore,
    pub ephemeral_network_clock: EphemeralNetworkSystemClockCore,
    pub time_zone: TimeZone,
    pub shared_memory: SharedMemory,
    pub power_state_request_manager: PowerStateRequestManager,
    pub alarms: Alarms,
    pub local_system_clock_context_writer: LocalSystemClockContextWriter,
    pub network_system_clock_context_writer: NetworkSystemClockContextWriter,
    pub ephemeral_network_clock_context_writer: EphemeralNetworkSystemClockContextWriter,
}

impl TimeManager {
    /// Create a new TimeManager with default clocks.
    ///
    /// Corresponds to `TimeManager::TimeManager(Core::System&)` in upstream.
    /// The `get_ticks_ns` callback provides CoreTiming ticks in nanoseconds.
    pub fn new(get_ticks_ns: Box<dyn Fn() -> i64 + Send + Sync>) -> Self {
        Self::new_with_shared_memory(get_ticks_ns, None, None)
    }

    pub fn new_with_shared_memory(
        get_ticks_ns: Box<dyn Fn() -> i64 + Send + Sync>,
        device_memory: Option<&crate::device_memory::DeviceMemory>,
        memory_manager: Option<&mut crate::hle::kernel::k_memory_manager::KMemoryManager>,
    ) -> Self {
        let get_ticks_ns = Arc::new(get_ticks_ns);
        // Preserve upstream references to the actual steady clock, including its
        // RTC base, offsets and monotonic clamp. Arc keeps the referenced owner
        // stable when TimeManager moves; callbacks never relock TimeManager.
        let make_time_point_cb = |clock: Arc<Mutex<StandardSteadyClockCore>>| {
            Box::new(move || {
                super::clocks::steady_clock_core::get_current_time_point(&*clock.lock().unwrap())
            })
                as Box<
                    dyn Fn() -> Result<
                            crate::hle::service::psc::time::common::SteadyClockTimePoint,
                            crate::hle::result::ResultCode,
                        > + Send
                        + Sync,
                >
        };

        // StandardSteadyClockCore
        let standard_steady_clock = Arc::new(Mutex::new(StandardSteadyClockCore::new({
            let ticks = Arc::clone(&get_ticks_ns);
            Box::new(move || ticks())
        })));

        // TickBasedSteadyClockCore
        let tick_based_steady_clock = Arc::new(TickBasedSteadyClockCore::new({
            let ticks = Arc::clone(&get_ticks_ns);
            Box::new(move || ticks())
        }));

        // System clock cores — each gets a time point callback from the tick source
        let standard_local_system_clock = StandardLocalSystemClockCore::new(make_time_point_cb(
            Arc::clone(&standard_steady_clock),
        ));
        let standard_network_system_clock = StandardNetworkSystemClockCore::new(
            make_time_point_cb(Arc::clone(&standard_steady_clock)),
        );
        let standard_user_system_clock = StandardUserSystemClockCore::new();
        let ephemeral_network_clock = EphemeralNetworkSystemClockCore::new({
            let clock = Arc::clone(&tick_based_steady_clock);
            Box::new(move || super::clocks::steady_clock_core::get_current_time_point(&*clock))
        });

        // TimeZone
        let time_zone = TimeZone::new();

        // SharedMemory
        let shared_memory = match (device_memory, memory_manager) {
            (Some(device_memory), Some(memory_manager)) => {
                SharedMemory::new(device_memory, memory_manager)
            }
            _ => SharedMemory::new_for_test(),
        };

        // PowerStateRequestManager
        let power_state_request_manager = PowerStateRequestManager::new();

        // Alarms — gets a raw time callback from the steady clock tick source
        let alarms = Alarms::new({
            let clock = Arc::clone(&standard_steady_clock);
            Box::new(move || {
                super::clocks::steady_clock_core::get_raw_time(&*clock.lock().unwrap())
            })
        });

        // Context writers
        // In upstream, these hold references to shared_memory and system_clock.
        // Here we use callback-based design; callbacks will be wired during
        // initialization when the TimeManager is set up.
        let local_system_clock_context_writer = LocalSystemClockContextWriter::new();
        let network_system_clock_context_writer = NetworkSystemClockContextWriter::new();
        let ephemeral_network_clock_context_writer =
            EphemeralNetworkSystemClockContextWriter::new();

        Self {
            standard_steady_clock,
            tick_based_steady_clock,
            standard_local_system_clock,
            standard_network_system_clock,
            standard_user_system_clock,
            ephemeral_network_clock,
            time_zone,
            shared_memory,
            power_state_request_manager,
            alarms,
            local_system_clock_context_writer,
            network_system_clock_context_writer,
            ephemeral_network_clock_context_writer,
        }
    }

    /// Create with default (zero) tick source. Useful for testing.
    pub fn new_default() -> Self {
        Self::new(Box::new(|| 0))
    }
}

#[cfg(test)]
mod tests {
    use super::TimeManager;

    #[test]
    fn system_clocks_follow_live_standard_clock_and_ephemeral_uses_tick_clock() {
        use super::super::clocks::steady_clock_core::{
            get_current_time_point, SteadyClockCoreImpl,
        };
        use std::sync::{
            atomic::{AtomicI64, Ordering},
            Arc,
        };
        let ticks = Arc::new(AtomicI64::new(5_000_000_000));
        let manager = TimeManager::new(Box::new({
            let ticks = Arc::clone(&ticks);
            move || ticks.load(Ordering::Relaxed)
        }));
        let id = [0x24; 16];
        manager.standard_steady_clock.lock().unwrap().initialize(
            id,
            100_000_000_000,
            2_000_000_000,
            3_000_000_000,
            false,
        );
        for elapsed in [5, 8, 6] {
            ticks.store(elapsed * 1_000_000_000, Ordering::Relaxed);
            let expected =
                get_current_time_point(&*manager.standard_steady_clock.lock().unwrap()).unwrap();
            assert_eq!(
                manager
                    .standard_local_system_clock
                    .clock
                    .get_current_time_point()
                    .unwrap(),
                expected
            );
            assert_eq!(
                manager
                    .standard_network_system_clock
                    .clock
                    .get_current_time_point()
                    .unwrap(),
                expected
            );
            assert_eq!(
                manager.alarms.get_raw_time(),
                expected.time_point * 1_000_000_000
            );
            let ephemeral = manager
                .ephemeral_network_clock
                .clock
                .get_current_time_point()
                .unwrap();
            assert_eq!(
                ephemeral,
                get_current_time_point(&*manager.tick_based_steady_clock).unwrap()
            );
            assert_eq!(ephemeral.time_point, elapsed);
            assert_ne!(ephemeral.clock_source_id, id);
        }
        {
            let mut clock = manager.standard_steady_clock.lock().unwrap();
            clock.set_rtc_offset(200_000_000_000);
            clock.set_internal_offset_impl(4_000_000_000);
            clock.set_test_offset_impl(5_000_000_000);
        }
        assert_eq!(
            manager
                .standard_local_system_clock
                .clock
                .get_current_time_point()
                .unwrap()
                .time_point,
            215
        );
    }

    #[test]
    fn local_clock_callback_uses_updated_shared_clock_source_id() {
        let manager = TimeManager::new(Box::new(|| 5_000_000_000));
        let new_id = [0x5Au8; 16];
        manager
            .standard_steady_clock
            .lock()
            .unwrap()
            .initialize(new_id, 0, 0, 0, false);

        let time_point = manager
            .standard_local_system_clock
            .clock
            .get_current_time_point()
            .unwrap();
        assert_eq!(time_point.clock_source_id, new_id);
        assert_eq!(time_point.time_point, 5);
    }
}

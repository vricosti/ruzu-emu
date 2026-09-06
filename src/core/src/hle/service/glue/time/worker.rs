// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/glue/time/worker.h
//! Port of zuyu/src/core/hle/service/glue/time/worker.cpp

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use parking_lot::Mutex as ParkingMutex;

use super::alarm_worker::AlarmWorker;
use super::file_timestamp_worker::FileTimestampWorker;
use super::pm_state_change_handler::PmStateChangeHandler;
use super::standard_steady_clock_resource::StandardSteadyClockResource;
use crate::core::SystemRef;
use crate::core_timing::{CoreTiming, EventType as TimingEventType, UnscheduleEventType};
use crate::hle::service::hle_ipc::SessionRequestHandlerPtr;
use crate::hle::service::kernel_helpers::ServiceContext;
use crate::hle::service::os::event::Event;
use crate::hle::service::os::multi_wait::MultiWait;
use crate::hle::service::os::multi_wait_holder::MultiWaitHolder;
use crate::hle::service::psc::time::common::{SteadyClockTimePoint, SystemClockContext};
use crate::hle::service::psc::time::r#static::StaticService as PscStaticService;
use crate::hle::service::psc::time::service_manager::TimeServiceManager;
use crate::hle::service::psc::time::system_clock::SystemClock;
use crate::hle::service::set::system_settings_server::SystemSettingsService;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
enum WorkerEventType {
    Exit = 0,
    PowerStateChange = 1,
    SignalAlarms = 2,
    UpdateLocalSystemClock = 3,
    UpdateNetworkSystemClock = 4,
    UpdateEphemeralSystemClock = 5,
    UpdateSteadyClock = 6,
    UpdateFileTimestamp = 7,
    AutoCorrect = 8,
}

impl WorkerEventType {
    fn from_user_data(value: usize) -> Self {
        match value {
            0 => Self::Exit,
            1 => Self::PowerStateChange,
            2 => Self::SignalAlarms,
            3 => Self::UpdateLocalSystemClock,
            4 => Self::UpdateNetworkSystemClock,
            5 => Self::UpdateEphemeralSystemClock,
            6 => Self::UpdateSteadyClock,
            7 => Self::UpdateFileTimestamp,
            8 => Self::AutoCorrect,
            _ => unreachable!("unknown TimeWorker event index {value}"),
        }
    }
}

/// TimeWorker runs the glue time-service event thread.
///
/// Shared Rust owners replace C++ references only where the background thread
/// must retain the manager-owned allocation.
pub struct TimeWorker {
    system: SystemRef,
    service_context: ServiceContext,
    set_sys: Option<SessionRequestHandlerPtr>,
    thread: Option<JoinHandle<()>>,
    exit_event_handle: u32,
    exit_event: Arc<Event>,
    time_manager: SessionRequestHandlerPtr,
    time_sm: Option<Arc<PscStaticService>>,
    network_clock: Option<Arc<SystemClock>>,
    local_clock: Option<Arc<SystemClock>>,
    ephemeral_clock: Option<Arc<SystemClock>>,
    steady_clock_resource: Arc<Mutex<StandardSteadyClockResource>>,
    file_timestamp_worker: Arc<Mutex<FileTimestampWorker>>,
    local_clock_event: Option<Arc<Event>>,
    network_clock_event: Option<Arc<Event>>,
    ephemeral_clock_event: Option<Arc<Event>>,
    standard_user_auto_correct_clock_event: Option<Arc<Event>>,
    timer_steady_clock_handle: u32,
    timer_steady_clock: Arc<Event>,
    timer_steady_clock_timing_event: Arc<ParkingMutex<TimingEventType>>,
    timer_file_system_handle: u32,
    timer_file_system: Arc<Event>,
    timer_file_system_timing_event: Arc<ParkingMutex<TimingEventType>>,
    alarm_worker: Arc<Mutex<AlarmWorker>>,
    pm_state_change_handler: Arc<Mutex<PmStateChangeHandler>>,
    ig_report_network_clock_context_set: Arc<AtomicBool>,
    report_network_clock_context: Arc<Mutex<SystemClockContext>>,
    ig_report_ephemeral_clock_context_set: Arc<AtomicBool>,
    report_ephemeral_clock_context: Arc<Mutex<SystemClockContext>>,
    core_timing: Arc<CoreTiming>,
    stop_requested: Arc<AtomicBool>,
}

impl TimeWorker {
    /// Matches `TimeWorker::TimeWorker` in Eden.
    pub fn new(
        system: SystemRef,
        time_manager: SessionRequestHandlerPtr,
        core_timing: Arc<CoreTiming>,
        steady_clock_resource: Arc<Mutex<StandardSteadyClockResource>>,
        file_timestamp_worker: Arc<Mutex<FileTimestampWorker>>,
    ) -> Self {
        let mut service_context = ServiceContext::new("Glue:TimeWorker".to_string());
        let exit_event_handle = service_context.create_event("Glue:TimeWorker:Event".to_string());
        let exit_event = service_context
            .get_event(exit_event_handle)
            .expect("TimeWorker exit event must exist");
        let timer_steady_clock_handle =
            service_context.create_event("Glue:TimeWorker:SteadyClockTimerEvent".to_string());
        let timer_steady_clock = service_context
            .get_event(timer_steady_clock_handle)
            .expect("TimeWorker steady-clock timer event must exist");
        let timer_file_system_handle =
            service_context.create_event("Glue:TimeWorker:FileTimeTimerEvent".to_string());
        let timer_file_system = service_context
            .get_event(timer_file_system_handle)
            .expect("TimeWorker filesystem timer event must exist");

        let timer_steady_clock_timing_event = {
            let event = Arc::clone(&timer_steady_clock);
            Arc::new(ParkingMutex::new(TimingEventType::new(
                Box::new(move |_late_ns, _time| {
                    event.signal();
                    None
                }),
                "Time::SteadyClockEvent".to_string(),
            )))
        };
        let timer_file_system_timing_event = {
            let event = Arc::clone(&timer_file_system);
            Arc::new(ParkingMutex::new(TimingEventType::new(
                Box::new(move |_late_ns, _time| {
                    event.signal();
                    None
                }),
                "Time::SteadyClockEvent".to_string(),
            )))
        };
        let alarm_worker = Arc::new(Mutex::new(AlarmWorker::new(
            Arc::clone(&time_manager),
            Arc::clone(&core_timing),
        )));

        Self {
            system,
            service_context,
            set_sys: None,
            thread: None,
            exit_event_handle,
            exit_event,
            time_manager,
            time_sm: None,
            network_clock: None,
            local_clock: None,
            ephemeral_clock: None,
            steady_clock_resource,
            file_timestamp_worker,
            local_clock_event: None,
            network_clock_event: None,
            ephemeral_clock_event: None,
            standard_user_auto_correct_clock_event: None,
            timer_steady_clock_handle,
            timer_steady_clock,
            timer_steady_clock_timing_event,
            timer_file_system_handle,
            timer_file_system,
            timer_file_system_timing_event,
            alarm_worker,
            pm_state_change_handler: Arc::new(Mutex::new(PmStateChangeHandler::new())),
            ig_report_network_clock_context_set: Arc::new(AtomicBool::new(false)),
            report_network_clock_context: Arc::new(Mutex::new(SystemClockContext::default())),
            ig_report_ephemeral_clock_context_set: Arc::new(AtomicBool::new(false)),
            report_ephemeral_clock_context: Arc::new(Mutex::new(SystemClockContext::default())),
            core_timing,
            stop_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Matches `TimeWorker::Initialize` in Eden.
    pub fn initialize(
        &mut self,
        time_sm: Arc<PscStaticService>,
        set_sys: SessionRequestHandlerPtr,
    ) {
        self.set_sys = Some(Arc::clone(&set_sys));
        self.time_sm = Some(Arc::clone(&time_sm));
        self.alarm_worker.lock().unwrap().initialize();

        let set_sys_service = system_settings_service(&set_sys);
        let (steady_clock_interval_m, fs_notify_time_s) = {
            let inner = set_sys_service.inner.lock().unwrap();
            (
                inner
                    .get_settings_item_value_i32(
                        "time",
                        "standard_steady_clock_rtc_update_interval_minutes",
                    )
                    .expect("standard steady-clock update interval must exist"),
                inner
                    .get_settings_item_value_i32("time", "notify_time_to_fs_interval_seconds")
                    .expect("filesystem timestamp update interval must exist"),
            )
        };
        let steady_clock_interval_ns = i64::from(steady_clock_interval_m) * 60 * 1_000_000_000;
        let fs_notify_time_ns = i64::from(fs_notify_time_s) * 1_000_000_000;
        self.core_timing.schedule_looping_event(
            Duration::ZERO,
            duration_from_positive_ns(steady_clock_interval_ns),
            &self.timer_steady_clock_timing_event,
            false,
        );
        self.core_timing.schedule_looping_event(
            Duration::ZERO,
            duration_from_positive_ns(fs_notify_time_ns),
            &self.timer_file_system_timing_event,
            false,
        );

        self.local_clock = Some(Arc::new(time_sm.get_standard_local_system_clock()));
        self.network_clock = Some(Arc::new(time_sm.get_standard_network_system_clock()));
        self.ephemeral_clock = Some(Arc::new(time_sm.get_ephemeral_network_system_clock()));

        let time_manager = time_service_manager(&self.time_manager);
        assert_eq!(
            time_manager.get_standard_local_clock_operation_event(&mut self.local_clock_event),
            crate::hle::result::RESULT_SUCCESS
        );
        assert_eq!(
            time_manager.get_standard_network_clock_operation_event_for_service_manager(
                &mut self.network_clock_event,
            ),
            crate::hle::result::RESULT_SUCCESS
        );
        assert_eq!(
            time_manager.get_ephemeral_network_clock_operation_event_for_service_manager(
                &mut self.ephemeral_clock_event,
            ),
            crate::hle::result::RESULT_SUCCESS
        );
        assert_eq!(
            time_manager.get_standard_user_system_clock_automatic_correction_updated_event(
                &mut self.standard_user_auto_correct_clock_event,
            ),
            crate::hle::result::RESULT_SUCCESS
        );
    }

    /// Matches `TimeWorker::StartThread` in Eden.
    pub fn start_thread(&mut self) {
        assert!(self.thread.is_none(), "TimeWorker thread already started");
        self.stop_requested.store(false, Ordering::Release);

        let system = self.system;
        let exit_event = Arc::clone(&self.exit_event);
        let alarm_worker = Arc::clone(&self.alarm_worker);
        let pm_state_change_handler = Arc::clone(&self.pm_state_change_handler);
        let time_manager = Arc::clone(&self.time_manager);
        let time_sm = Arc::clone(
            self.time_sm
                .as_ref()
                .expect("TimeWorker must be initialized before StartThread"),
        );
        let set_sys = Arc::clone(
            self.set_sys
                .as_ref()
                .expect("TimeWorker must be initialized before StartThread"),
        );
        let local_clock = Arc::clone(self.local_clock.as_ref().unwrap());
        let network_clock = Arc::clone(self.network_clock.as_ref().unwrap());
        let ephemeral_clock = Arc::clone(self.ephemeral_clock.as_ref().unwrap());
        let local_clock_event = Arc::clone(self.local_clock_event.as_ref().unwrap());
        let network_clock_event = Arc::clone(self.network_clock_event.as_ref().unwrap());
        let ephemeral_clock_event = Arc::clone(self.ephemeral_clock_event.as_ref().unwrap());
        let auto_correct_event = Arc::clone(
            self.standard_user_auto_correct_clock_event
                .as_ref()
                .unwrap(),
        );
        let timer_steady_clock = Arc::clone(&self.timer_steady_clock);
        let timer_file_system = Arc::clone(&self.timer_file_system);
        let steady_clock_resource = Arc::clone(&self.steady_clock_resource);
        let file_timestamp_worker = Arc::clone(&self.file_timestamp_worker);
        let network_report_set = Arc::clone(&self.ig_report_network_clock_context_set);
        let network_report_context = Arc::clone(&self.report_network_clock_context);
        let ephemeral_report_set = Arc::clone(&self.ig_report_ephemeral_clock_context_set);
        let ephemeral_report_context = Arc::clone(&self.report_ephemeral_clock_context);
        let stop_requested = Arc::clone(&self.stop_requested);

        self.thread = Some(
            std::thread::Builder::new()
                .name("TimeWorker".to_string())
                .spawn(move || {
                    common::thread::set_current_thread_name("TimeWorker");
                    common::thread::set_current_thread_priority(
                        common::thread::ThreadPriority::Low,
                    );

                    while !stop_requested.load(Ordering::Acquire) {
                        let alarm_event = alarm_worker.lock().unwrap().get_event();
                        let priority = pm_state_change_handler.lock().unwrap().priority;
                        let mut event_sources = vec![
                            (WorkerEventType::Exit, Arc::clone(&exit_event)),
                            (WorkerEventType::PowerStateChange, alarm_event),
                        ];
                        if priority == 0 {
                            let alarm_timer_event =
                                alarm_worker.lock().unwrap().get_timer_event();
                            event_sources.extend([
                                (WorkerEventType::SignalAlarms, alarm_timer_event),
                                (
                                    WorkerEventType::UpdateLocalSystemClock,
                                    Arc::clone(&local_clock_event),
                                ),
                                (
                                    WorkerEventType::UpdateNetworkSystemClock,
                                    Arc::clone(&network_clock_event),
                                ),
                                (
                                    WorkerEventType::UpdateEphemeralSystemClock,
                                    Arc::clone(&ephemeral_clock_event),
                                ),
                                (
                                    WorkerEventType::UpdateSteadyClock,
                                    Arc::clone(&timer_steady_clock),
                                ),
                                (
                                    WorkerEventType::UpdateFileTimestamp,
                                    Arc::clone(&timer_file_system),
                                ),
                                (WorkerEventType::AutoCorrect, Arc::clone(&auto_correct_event)),
                            ]);
                        }

                        let mut holders: Vec<Box<MultiWaitHolder>> = event_sources
                            .into_iter()
                            .map(|(event_type, event)| {
                                let mut holder = Box::new(MultiWaitHolder::from_event(event));
                                holder.set_user_data(event_type as usize);
                                holder
                            })
                            .collect();
                        let mut multi_wait = MultiWait::new();
                        for holder in &mut holders {
                            multi_wait.link_holder(&mut **holder);
                        }

                        let signaled = if !system.is_null() {
                            system
                                .get()
                                .kernel()
                                .and_then(|kernel| multi_wait.wait_any(kernel))
                        } else {
                            #[cfg(test)]
                            { multi_wait.wait_any_local() }
                            #[cfg(not(test))]
                            { panic!("TimeWorker requires a live System") }
                        };
                        let Some(signaled) = signaled else {
                            continue;
                        };
                        let event_type = WorkerEventType::from_user_data(unsafe {
                            (*signaled).get_user_data()
                        });

                        match event_type {
                            WorkerEventType::Exit => return,
                            WorkerEventType::PowerStateChange => {
                                let alarm_worker = alarm_worker.lock().unwrap();
                                alarm_worker.get_event().clear();
                                if pm_state_change_handler.lock().unwrap().priority <= 1 {
                                    alarm_worker.on_power_state_changed();
                                }
                            }
                            WorkerEventType::SignalAlarms => {
                                alarm_worker.lock().unwrap().get_timer_event().clear();
                                assert_eq!(
                                    time_service_manager(&time_manager).check_and_signal_alarms(),
                                    crate::hle::result::RESULT_SUCCESS
                                );
                            }
                            WorkerEventType::UpdateLocalSystemClock => {
                                local_clock_event.clear();
                                let context = local_clock
                                    .get_system_clock_context()
                                    .expect("local system clock context must be available");
                                system_settings_service(&set_sys)
                                    .inner
                                    .lock()
                                    .unwrap()
                                    .set_user_system_clock_context(encode_system_clock_context(
                                        &context,
                                    ));
                                file_timestamp_worker
                                    .lock()
                                    .unwrap()
                                    .set_filesystem_posix_time();
                            }
                            WorkerEventType::UpdateNetworkSystemClock => {
                                network_clock_event.clear();
                                let context = network_clock
                                    .get_system_clock_context()
                                    .expect("network system clock context must be available");
                                system_settings_service(&set_sys)
                                    .inner
                                    .lock()
                                    .unwrap()
                                    .set_network_system_clock_context(encode_system_clock_context(
                                        &context,
                                    ));
                                if network_clock.get_current_time().is_err() {
                                    continue;
                                }

                                let was_set = network_report_set.load(Ordering::Acquire);
                                let _offset_before = if was_set {
                                    network_report_context.lock().unwrap().offset
                                } else {
                                    0
                                };
                                *network_report_context.lock().unwrap() = context;
                                if !was_set {
                                    network_report_set.store(true, Ordering::Release);
                                }
                                file_timestamp_worker
                                    .lock()
                                    .unwrap()
                                    .set_filesystem_posix_time();
                            }
                            WorkerEventType::UpdateEphemeralSystemClock => {
                                ephemeral_clock_event.clear();
                                let Ok(context) = ephemeral_clock.get_system_clock_context() else {
                                    continue;
                                };
                                if ephemeral_clock.get_current_time().is_err() {
                                    continue;
                                }

                                let was_set = ephemeral_report_set.load(Ordering::Acquire);
                                let _offset_before = if was_set {
                                    ephemeral_report_context.lock().unwrap().offset
                                } else {
                                    0
                                };
                                *ephemeral_report_context.lock().unwrap() = context;
                                if !was_set {
                                    ephemeral_report_set.store(true, Ordering::Release);
                                }
                            }
                            WorkerEventType::UpdateSteadyClock => {
                                timer_steady_clock.clear();
                                let base_time = {
                                    let mut resource = steady_clock_resource.lock().unwrap();
                                    resource.update_time();
                                    resource.get_time()
                                };
                                assert_eq!(
                                    time_service_manager(&time_manager)
                                        .set_standard_steady_clock_base_time(base_time),
                                    crate::hle::result::RESULT_SUCCESS
                                );
                            }
                            WorkerEventType::UpdateFileTimestamp => {
                                timer_file_system.clear();
                                file_timestamp_worker
                                    .lock()
                                    .unwrap()
                                    .set_filesystem_posix_time();
                            }
                            WorkerEventType::AutoCorrect => {
                                auto_correct_event.clear();
                                let automatic_correction = time_sm
                                    .is_standard_user_system_clock_automatic_correction_enabled()
                                    .expect("automatic-correction state must be available");
                                let time_point = time_sm
                                    .get_standard_user_system_clock_automatic_correction_updated_time()
                                    .expect("automatic-correction update time must be available");
                                let set_sys_service = system_settings_service(&set_sys);
                                let mut inner = set_sys_service.inner.lock().unwrap();
                                inner.set_user_system_clock_automatic_correction_enabled(
                                    automatic_correction,
                                );
                                inner.set_user_system_clock_automatic_correction_updated_time(
                                    encode_steady_clock_time_point(&time_point),
                                );
                            }
                        }
                    }
                })
                .expect("failed to start TimeWorker thread"),
        );
    }
}

impl Drop for TimeWorker {
    fn drop(&mut self) {
        if let Some(event) = &self.local_clock_event {
            event.signal();
        }
        if let Some(event) = &self.network_clock_event {
            event.signal();
        }
        if let Some(event) = &self.ephemeral_clock_event {
            event.signal();
        }
        if self.thread.is_some() {
            std::thread::sleep(Duration::from_millis(16));
        }

        self.stop_requested.store(true, Ordering::Release);
        self.exit_event.signal();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }

        self.service_context.close_event(self.exit_event_handle);
        self.core_timing.unschedule_event(
            &self.timer_steady_clock_timing_event,
            UnscheduleEventType::NoWait,
        );
        self.service_context
            .close_event(self.timer_steady_clock_handle);
        self.core_timing.unschedule_event(
            &self.timer_file_system_timing_event,
            UnscheduleEventType::NoWait,
        );
        self.service_context
            .close_event(self.timer_file_system_handle);
    }
}

fn time_service_manager(handler: &SessionRequestHandlerPtr) -> &TimeServiceManager {
    handler
        .as_any()
        .downcast_ref::<TimeServiceManager>()
        .expect("time:m is not a PSC::Time::ServiceManager")
}

fn system_settings_service(handler: &SessionRequestHandlerPtr) -> &SystemSettingsService {
    handler
        .as_any()
        .downcast_ref::<SystemSettingsService>()
        .expect("set:sys is not an ISystemSettingsServer")
}

fn duration_from_positive_ns(nanoseconds: i64) -> Duration {
    Duration::from_nanos(
        u64::try_from(nanoseconds).expect("TimeWorker interval must be non-negative"),
    )
}

fn encode_system_clock_context(context: &SystemClockContext) -> [u8; 0x20] {
    let mut out = [0u8; 0x20];
    unsafe {
        std::ptr::copy_nonoverlapping(
            context as *const SystemClockContext as *const u8,
            out.as_mut_ptr(),
            core::mem::size_of::<SystemClockContext>(),
        );
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::result::RESULT_SUCCESS;

    fn make_time_manager() -> Arc<TimeServiceManager> {
        Arc::new(TimeServiceManager::new(
            SystemRef::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
        ))
    }

    #[test]
    fn retains_manager_owned_resource_allocations() {
        let time_manager = make_time_manager();
        let time_manager: SessionRequestHandlerPtr = time_manager;
        let steady_clock_resource = Arc::new(Mutex::new(StandardSteadyClockResource::new()));
        let file_timestamp_worker = Arc::new(Mutex::new(FileTimestampWorker::new()));
        let worker = TimeWorker::new(
            SystemRef::null(),
            time_manager,
            Arc::new(CoreTiming::new()),
            Arc::clone(&steady_clock_resource),
            Arc::clone(&file_timestamp_worker),
        );

        assert!(Arc::ptr_eq(
            &steady_clock_resource,
            &worker.steady_clock_resource
        ));
        assert!(Arc::ptr_eq(
            &file_timestamp_worker,
            &worker.file_timestamp_worker
        ));
    }

    #[test]
    fn dispatches_local_clock_update_and_stops_cleanly() {
        std::thread::Builder::new()
            .name("TimeWorkerDispatchTest".to_string())
            .stack_size(16 * 1024 * 1024)
            .spawn(run_local_clock_dispatch_test)
            .unwrap()
            .join()
            .unwrap();
    }

    fn run_local_clock_dispatch_test() {
        let time_manager = make_time_manager();
        let clock_source_id = [0x42; 16];
        let context = SystemClockContext {
            offset: 0x1234,
            steady_time_point: SteadyClockTimePoint {
                time_point: 0x5678,
                clock_source_id,
            },
        };
        assert_eq!(
            time_manager.setup_standard_steady_clock_core(false, clock_source_id, 0, 0, 0,),
            RESULT_SUCCESS
        );
        assert_eq!(
            time_manager.setup_standard_local_system_clock_core(&context, 0),
            RESULT_SUCCESS
        );
        assert_eq!(
            time_manager.setup_standard_network_system_clock_core(context, i64::MAX),
            RESULT_SUCCESS
        );
        assert_eq!(
            time_manager.setup_standard_user_system_clock_core(
                false,
                SteadyClockTimePoint {
                    time_point: 0,
                    clock_source_id,
                },
            ),
            RESULT_SUCCESS
        );
        assert_eq!(
            time_manager.setup_ephemeral_network_system_clock_core(),
            RESULT_SUCCESS
        );

        let time_sm = time_manager.get_static_service_as_service_manager();
        let expected_context = time_sm
            .get_standard_local_system_clock()
            .get_system_clock_context()
            .unwrap();
        let set_sys = Arc::new(SystemSettingsService::new());
        let set_sys_handler: SessionRequestHandlerPtr = set_sys.clone();
        let time_manager_handler: SessionRequestHandlerPtr = time_manager;
        let mut worker = TimeWorker::new(
            SystemRef::null(),
            time_manager_handler,
            Arc::new(CoreTiming::new()),
            Arc::new(Mutex::new(StandardSteadyClockResource::new())),
            Arc::new(Mutex::new(FileTimestampWorker::new())),
        );
        worker.initialize(time_sm, set_sys_handler);
        let local_clock_event = Arc::clone(worker.local_clock_event.as_ref().unwrap());
        worker.start_thread();
        local_clock_event.signal();

        let expected = encode_system_clock_context(&expected_context);
        let mut observed = false;
        for _ in 0..100 {
            if set_sys
                .inner
                .lock()
                .unwrap()
                .get_user_system_clock_context()
                == expected
            {
                observed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            observed,
            "TimeWorker did not dispatch the local-clock event"
        );
        // Glue's guest service stack may be abandoned during shutdown. The
        // kernel's post-fiber owner must still destroy the worker and join its
        // host thread, even while weak factory references remain alive.
        let kernel = crate::hle::kernel::kernel::KernelCore::new();
        let owner = Arc::new(Mutex::new(worker));
        let factory_reference = Arc::downgrade(&owner);
        kernel.retain_service_lifetime_owner(owner);
        assert!(factory_reference.upgrade().is_some());
        kernel.finalize_services_after_cpu_shutdown();
        assert!(factory_reference.upgrade().is_none());
    }
}

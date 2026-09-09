// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/vi/conductor.h
//! Port of zuyu/src/core/hle/service/vi/conductor.cpp
//!
//! The Conductor manages vsync timing and display composition.
//! It maintains a VsyncManager per display and drives the composition loop.
//!
//! In multicore mode, the CoreTiming callback signals a thread event and a
//! dedicated VsyncThread calls process_vsync(). In single-core mode, the
//! callback directly calls process_vsync().

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use common::settings;

use crate::core::SystemRef;
use crate::core_timing;
use crate::hle::service::nvnflinger::surface_flinger::SurfaceFlinger;
use crate::hle::service::os::event::Event;

use super::vsync_manager::VsyncManager;

/// Frame interval in nanoseconds (60 FPS).
pub const FRAME_NS: i64 = 1_000_000_000 / 60;

fn should_trace_vsync_debug() -> bool {
    std::env::var_os("RUZU_TRACE_VSYNC").is_some()
}

static VSYNC_CALLBACK_COUNT: AtomicU64 = AtomicU64::new(0);
static VSYNC_SIGNAL_ALREADY_SET_COUNT: AtomicU64 = AtomicU64::new(0);
static VSYNC_THREAD_WAKE_COUNT: AtomicU64 = AtomicU64::new(0);
static VSYNC_PROCESS_COUNT: AtomicU64 = AtomicU64::new(0);
static VSYNC_PROCESS_TOTAL_US: AtomicU64 = AtomicU64::new(0);
static VSYNC_PROCESS_MAX_US: AtomicU64 = AtomicU64::new(0);

fn vsync_profile_enabled() -> bool {
    std::env::var_os("RUZU_PROFILE_VSYNC").is_some()
}

fn update_max(target: &AtomicU64, value: u64) {
    let mut current = target.load(Ordering::Relaxed);
    while value > current {
        match target.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(next) => current = next,
        }
    }
}

/// Thread-safe event primitive matching upstream `Common::Event`.
/// Uses a condvar + bool: Set() signals, Wait() blocks until signaled then auto-resets.
struct ThreadEvent {
    mutex: Mutex<bool>,
    condvar: Condvar,
}

impl ThreadEvent {
    fn new() -> Self {
        Self {
            mutex: Mutex::new(false),
            condvar: Condvar::new(),
        }
    }

    fn set(&self) {
        let mut signaled = self.mutex.lock().unwrap();
        if vsync_profile_enabled() && *signaled {
            VSYNC_SIGNAL_ALREADY_SET_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        if !*signaled {
            *signaled = true;
            self.condvar.notify_one();
        }
    }

    fn wait(&self) {
        let mut signaled = self.mutex.lock().unwrap();
        while !*signaled {
            signaled = self.condvar.wait(signaled).unwrap();
        }
        *signaled = false;
    }
}

/// Shared state between the CoreTiming callback and the Conductor,
/// so the callback can compute GetNextTicks without locking the full Conductor.
struct SharedTickState {
    swap_interval: std::sync::atomic::AtomicI32,
    // compose_speed_scale stored as bits for atomic access
    compose_speed_scale_bits: std::sync::atomic::AtomicU32,
}

impl SharedTickState {
    fn new() -> Self {
        Self {
            swap_interval: std::sync::atomic::AtomicI32::new(1),
            compose_speed_scale_bits: std::sync::atomic::AtomicU32::new(1.0f32.to_bits()),
        }
    }

    fn update(&self, swap_interval: i32, compose_speed_scale: f32) {
        self.swap_interval.store(swap_interval, Ordering::Relaxed);
        self.compose_speed_scale_bits
            .store(compose_speed_scale.to_bits(), Ordering::Relaxed);
    }

    fn get_next_ticks(&self, system: SystemRef) -> i64 {
        let compose_speed_scale =
            f32::from_bits(self.compose_speed_scale_bits.load(Ordering::Relaxed));
        let swap_interval = self.swap_interval.load(Ordering::Relaxed);
        let (use_multi_core, use_speed_limit) = speed_settings();
        compute_next_ticks(
            swap_interval,
            compose_speed_scale,
            use_multi_core,
            use_speed_limit,
            settings::speed_limit(),
            system.get().get_nvdec_active(),
            *settings::values().use_video_framerate.get_value(),
        )
    }
}

fn speed_settings() -> (bool, bool) {
    let values = settings::values();
    (
        *values.use_multi_core.get_value(),
        *values.use_speed_limit.get_value(),
    )
}

fn compute_next_ticks(
    swap_interval: i32,
    compose_speed_scale: f32,
    use_multi_core: bool,
    use_speed_limit: bool,
    speed_limit: u16,
    nvdec_active: bool,
    use_video_framerate: bool,
) -> i64 {
    let mut speed_scale = 1.0f32;
    if use_multi_core {
        if use_speed_limit {
            speed_scale = 100.0f32 / speed_limit as f32;
        } else {
            speed_scale = 0.01f32;
        }
    }

    speed_scale /= compose_speed_scale;

    if nvdec_active && use_video_framerate {
        speed_scale = 1.0f32;
    }

    let effective_fps = 60.0f32 / swap_interval as f32;
    (speed_scale * (1_000_000_000.0f32 / effective_fps)) as i64
}

/// Conductor drives display composition and vsync signaling.
///
/// Upstream creates a CoreTiming event "ScreenComposition" that fires every
/// 16.67ms. In multicore mode, the callback signals a thread event and a
/// dedicated VsyncThread calls ProcessVsync(). In single-core mode, the
/// callback directly calls ProcessVsync().
pub struct Conductor {
    vsync_managers: HashMap<u64, VsyncManager>,
    swap_interval: i32,
    compose_speed_scale: f32,
    system: SystemRef,
    /// Surface flinger reference for composition.
    /// Upstream: Conductor holds Container& and calls Container::ComposeOnDisplay
    /// which delegates to SurfaceFlinger::ComposeDisplay. We skip the Container
    /// indirection to avoid circular references.
    surface_flinger: Arc<SurfaceFlinger>,
    event: Option<Arc<parking_lot::Mutex<core_timing::EventType>>>,
    /// Shared tick state for the CoreTiming callback to compute GetNextTicks.
    tick_state: Arc<SharedTickState>,
    /// VsyncThread handle (multicore mode only).
    vsync_thread: Option<std::thread::JoinHandle<()>>,
    /// Signal for waking the VsyncThread (multicore mode only).
    vsync_signal: Option<Arc<ThreadEvent>>,
    /// Stop flag for the VsyncThread (multicore mode only).
    stop_requested: Option<Arc<AtomicBool>>,
}

impl Conductor {
    pub fn new(
        system: SystemRef,
        display_ids: &[u64],
        surface_flinger: Arc<SurfaceFlinger>,
    ) -> Self {
        let mut vsync_managers = HashMap::new();
        for &id in display_ids {
            vsync_managers.insert(id, VsyncManager::new());
        }

        Self {
            vsync_managers,
            swap_interval: 1,
            compose_speed_scale: 1.0,
            system,
            surface_flinger,
            event: None,
            tick_state: Arc::new(SharedTickState::new()),
            vsync_thread: None,
            vsync_signal: None,
            stop_requested: None,
        }
    }

    /// Start the vsync timer. Must be called after the Conductor is placed
    /// inside an `Arc<Mutex<>>` so the CoreTiming callback can reference it.
    ///
    /// Upstream does this in the constructor, but we split it because the
    /// CoreTiming callback needs a weak reference back to the Conductor.
    pub fn start(conductor: &Arc<Mutex<Self>>) {
        let is_multicore = conductor.lock().unwrap().system.get().is_multicore();

        if is_multicore {
            // Multicore mode: CoreTiming callback signals the thread event,
            // a dedicated VsyncThread wakes and calls process_vsync().
            let signal = Arc::new(ThreadEvent::new());
            let stop = Arc::new(AtomicBool::new(false));

            // Create the CoreTiming event that just signals the thread event.
            let signal_for_callback = Arc::clone(&signal);
            let tick_state = Arc::clone(&conductor.lock().unwrap().tick_state);
            let system_for_callback = conductor.lock().unwrap().system;
            let event = core_timing::create_event(
                "ScreenComposition".to_string(),
                Box::new(move |_time, _ns_late| {
                    static SC_COUNT: std::sync::atomic::AtomicU64 =
                        std::sync::atomic::AtomicU64::new(0);
                    let sc = SC_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if should_trace_vsync_debug() && sc < 3 {
                        log::info!("[SC_CB] #{} signal_ptr={:p}", sc, &*signal_for_callback);
                    }
                    if vsync_profile_enabled() {
                        VSYNC_CALLBACK_COUNT.fetch_add(1, Ordering::Relaxed);
                    }
                    signal_for_callback.set();
                    let next_ns = tick_state.get_next_ticks(system_for_callback);
                    Some(Duration::from_nanos(next_ns as u64))
                }),
            );

            let system = conductor.lock().unwrap().system;
            let core_timing = system.get().core_timing();
            let frame_dur = Duration::from_nanos(FRAME_NS as u64);
            core_timing.schedule_looping_event(frame_dur, frame_dur, &event, false);

            // Spawn the VsyncThread.
            let signal_for_thread = Arc::clone(&signal);
            let stop_for_thread = Arc::clone(&stop);
            let conductor_weak = Arc::downgrade(conductor);
            let system_for_thread = system;
            let thread = std::thread::Builder::new()
                .name("VSyncThread".to_string())
                .spawn(move || {
                    Self::vsync_thread(
                        signal_for_thread,
                        stop_for_thread,
                        conductor_weak,
                        system_for_thread,
                    );
                })
                .expect("Failed to spawn VSyncThread");

            let mut cond = conductor.lock().unwrap();
            cond.event = Some(event);
            cond.vsync_thread = Some(thread);
            cond.vsync_signal = Some(signal);
            cond.stop_requested = Some(stop);
            log::info!(
                "Conductor: vsync timer started at {}ns interval (multicore, VsyncThread)",
                FRAME_NS
            );
        } else {
            // Single-core mode: CoreTiming callback directly calls process_vsync().
            let weak = Arc::downgrade(conductor);
            let tick_state = Arc::clone(&conductor.lock().unwrap().tick_state);
            let system_for_callback = conductor.lock().unwrap().system;
            let event = core_timing::create_event(
                "ScreenComposition".to_string(),
                Box::new(move |_time, _ns_late| {
                    if let Some(conductor) = weak.upgrade() {
                        conductor.lock().unwrap().process_vsync();
                    }
                    let next_ns = tick_state.get_next_ticks(system_for_callback);
                    Some(Duration::from_nanos(next_ns as u64))
                }),
            );

            let system = conductor.lock().unwrap().system;
            let core_timing = system.get().core_timing();
            let frame_dur = Duration::from_nanos(FRAME_NS as u64);
            core_timing.schedule_looping_event(frame_dur, frame_dur, &event, false);

            conductor.lock().unwrap().event = Some(event);
            log::info!(
                "Conductor: vsync timer started at {}ns interval (single-core)",
                FRAME_NS
            );
        }
    }

    /// VsyncThread entry point. Waits on the signal and calls process_vsync().
    /// Port of upstream `Conductor::VsyncThread(std::stop_token token)`.
    fn vsync_thread(
        signal: Arc<ThreadEvent>,
        stop: Arc<AtomicBool>,
        conductor: std::sync::Weak<Mutex<Self>>,
        system: SystemRef,
    ) {
        static VT_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        if should_trace_vsync_debug() {
            log::info!("[VSYNC_THREAD] started signal_ptr={:p}", &*signal);
        }
        while !stop.load(Ordering::Relaxed) {
            signal.wait();

            let vt = VT_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if vsync_profile_enabled() {
                VSYNC_THREAD_WAKE_COUNT.fetch_add(1, Ordering::Relaxed);
            }
            if should_trace_vsync_debug() && (vt < 10 || vt % 60 == 0) {
                log::info!("[VSYNC_THREAD] woke #{}", vt);
            }

            if system.get().is_shutting_down() {
                if should_trace_vsync_debug() {
                    log::info!("[VSYNC_THREAD] shutting down");
                }
                return;
            }

            if let Some(conductor) = conductor.upgrade() {
                conductor.lock().unwrap().process_vsync();
            } else {
                log::error!("[VSYNC_THREAD] conductor DROPPED at wake #{}, exiting!", vt);
                return;
            }
        }
        if should_trace_vsync_debug() {
            log::info!("[VSYNC_THREAD] stop requested");
        }
    }

    pub fn link_vsync_event(&mut self, display_id: u64, event: Arc<Event>) {
        if let Some(manager) = self.vsync_managers.get_mut(&display_id) {
            let count = manager.event_count() + 1;
            manager.link_vsync_event(event);
            log::info!(
                "Conductor::link_vsync_event: display_id={} linked (total={})",
                display_id,
                count
            );
        } else {
            log::error!(
                "Conductor::link_vsync_event: display_id={} NOT FOUND! Available: {:?}",
                display_id,
                self.vsync_managers.keys().collect::<Vec<_>>()
            );
        }
    }

    pub fn unlink_vsync_event(&mut self, display_id: u64, event: &Arc<Event>) {
        if let Some(manager) = self.vsync_managers.get_mut(&display_id) {
            manager.unlink_vsync_event(event);
        }
    }

    /// Process a vsync tick: compose each display, then signal vsync events.
    /// Port of upstream `Conductor::ProcessVsync`.
    fn process_vsync(&mut self) {
        let start = if vsync_profile_enabled() {
            Some(std::time::Instant::now())
        } else {
            None
        };
        static VSYNC_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let vc = VSYNC_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if should_trace_vsync_debug() && (vc % 300 == 0 || vc % 60 == 0) {
            let total_events: usize = self.vsync_managers.values().map(|m| m.event_count()).sum();
            log::info!("[VSYNC] tick#{} total_linked_events={}", vc, total_events);
        }
        for (&display_id, manager) in self.vsync_managers.iter() {
            // Upstream: m_container.ComposeOnDisplay(&m_swap_interval, &m_compose_speed_scale, display_id);
            self.surface_flinger.compose_display(
                &mut self.swap_interval,
                &mut self.compose_speed_scale,
                display_id,
            );
            manager.signal_vsync();
        }
        // Update shared tick state so the CoreTiming callback uses current values.
        self.tick_state
            .update(self.swap_interval, self.compose_speed_scale);
        if let Some(start) = start {
            let elapsed_us = start.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
            VSYNC_PROCESS_COUNT.fetch_add(1, Ordering::Relaxed);
            VSYNC_PROCESS_TOTAL_US.fetch_add(elapsed_us, Ordering::Relaxed);
            update_max(&VSYNC_PROCESS_MAX_US, elapsed_us);
        }
    }

    /// Calculate the next tick interval in nanoseconds.
    /// Port of upstream `Conductor::GetNextTicks`.
    pub fn get_next_ticks(&self) -> i64 {
        let (use_multi_core, use_speed_limit) = speed_settings();
        compute_next_ticks(
            self.swap_interval,
            self.compose_speed_scale,
            use_multi_core,
            use_speed_limit,
            settings::speed_limit(),
            self.system.get().get_nvdec_active(),
            *settings::values().use_video_framerate.get_value(),
        )
    }
}

pub fn dump_vsync_profile() {
    if !vsync_profile_enabled() {
        return;
    }
    let callbacks = VSYNC_CALLBACK_COUNT.load(Ordering::Relaxed);
    let already_set = VSYNC_SIGNAL_ALREADY_SET_COUNT.load(Ordering::Relaxed);
    let wakes = VSYNC_THREAD_WAKE_COUNT.load(Ordering::Relaxed);
    let process = VSYNC_PROCESS_COUNT.load(Ordering::Relaxed);
    let total_us = VSYNC_PROCESS_TOTAL_US.load(Ordering::Relaxed);
    let max_us = VSYNC_PROCESS_MAX_US.load(Ordering::Relaxed);
    let avg_us = if process != 0 { total_us / process } else { 0 };
    eprintln!(
        "[VSYNC_PROFILE] callbacks={} already_set={} thread_wakes={} process={} total_us={} avg_us={} max_us={}",
        callbacks, already_set, wakes, process, total_us, avg_us, max_us
    );
}

impl Drop for Conductor {
    fn drop(&mut self) {
        // Unschedule the CoreTiming event.
        if let Some(ref event) = self.event {
            let core_timing = self.system.get().core_timing();
            core_timing.unschedule_event(event, core_timing::UnscheduleEventType::NoWait);
        }

        // Stop the VsyncThread if running (multicore mode).
        if let Some(ref stop) = self.stop_requested {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(ref signal) = self.vsync_signal {
            signal.set(); // Wake the thread so it sees the stop flag.
        }
        if let Some(thread) = self.vsync_thread.take() {
            let _ = thread.join();
        }

        log::info!("Conductor: vsync timer stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::compute_next_ticks;

    // GetNextTicks uses f32 arithmetic, unlike the integer FrameNs used when
    // initially scheduling the event. At 60 Hz its result rounds up by 1 ns.
    const NEXT_TICKS_60HZ: i64 = 16_666_667;

    #[test]
    fn get_next_ticks_matches_60hz_at_default_multicore_speed_limit() {
        let ticks = compute_next_ticks(1, 1.0, true, true, 100, false, false);
        assert_eq!(ticks, NEXT_TICKS_60HZ);
    }

    #[test]
    fn get_next_ticks_uses_unlocked_multicore_rate_when_speed_limit_disabled() {
        let ticks = compute_next_ticks(1, 1.0, true, false, 100, false, false);
        assert_eq!(ticks, 166_666);
    }

    #[test]
    fn get_next_ticks_overrides_compose_scale_for_nvdec_video_rate() {
        let ticks = compute_next_ticks(2, 0.05, true, true, 100, true, true);
        assert_eq!(ticks, 33_333_334);
    }

    #[test]
    fn video_rate_override_requires_both_decoder_activity_and_setting() {
        for (active, enabled) in [(false, false), (false, true), (true, false)] {
            assert_eq!(
                compute_next_ticks(1, 1.0, true, true, 200, active, enabled),
                8_333_333,
            );
        }
        assert_eq!(
            compute_next_ticks(1, 1.0, true, true, 200, true, true),
            NEXT_TICKS_60HZ
        );
        // The video override wins over unlocked speed and composition scaling.
        assert_eq!(
            compute_next_ticks(1, 0.5, true, false, 200, true, true),
            NEXT_TICKS_60HZ
        );
    }

    #[test]
    fn single_core_leaves_speed_limiting_to_cpu_but_keeps_composition_scale() {
        for limited in [false, true] {
            assert_eq!(
                compute_next_ticks(1, 1.0, false, limited, 200, false, false),
                NEXT_TICKS_60HZ
            );
            assert_eq!(
                compute_next_ticks(1, 2.0, false, limited, 200, false, false),
                8_333_333
            );
        }
    }
}

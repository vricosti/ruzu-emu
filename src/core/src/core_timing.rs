//! Port of zuyu/src/core/core_timing.h and zuyu/src/core/core_timing.cpp
//! Status: COMPLET
//! Derniere synchro: 2026-03-05
//!
//! The timing system. Manages timed events by scheduling callbacks at specific
//! CPU tick counts. Uses a priority queue (BinaryHeap) of events, and can
//! schedule/unschedule events and advance time. This is CRITICAL for emulation.

use common::thread::Event;
use common::wall_clock::{self, WallClock};
use parking_lot::Mutex;
use std::cmp::Ordering as CmpOrdering;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

/// Maximum slice length for single-core timing.
const MAX_SLICE_LENGTH: i64 = 10000;

const CPU_CLOCK_BASE_MHZ: u32 = 1020;
const CPU_CLOCK_BOOST_MHZ: u32 = 1734;
const CPU_CLOCK_OVERCLOCK_MHZ: u32 = 2040;

fn cpu_clock_target_mhz(clock: common::settings_enums::CpuClock) -> u32 {
    use common::settings_enums::CpuClock;
    match clock {
        CpuClock::Boost => CPU_CLOCK_BOOST_MHZ,
        CpuClock::Overclock => CPU_CLOCK_OVERCLOCK_MHZ,
        CpuClock::Normal => CPU_CLOCK_BASE_MHZ,
    }
}

fn scale_clock_ticks(raw_ticks: u64, target_mhz: u32, sync_core_speed: bool, speed: u16) -> u64 {
    let mut ticks = if target_mhz != CPU_CLOCK_BASE_MHZ {
        raw_ticks.wrapping_mul(target_mhz as u64) / CPU_CLOCK_BASE_MHZ as u64
    } else {
        raw_ticks
    };
    if sync_core_speed {
        ticks = (ticks as f64 / (speed as f64 * 0.01)) as u64;
    }
    ticks
}

/// Eden `GetNextTickCount`: replace the JIT-reported slice with the configured
/// fixed tick count only while the paired custom-ticks setting is enabled.
fn get_next_tick_count(use_custom_ticks: bool, configured_ticks: u32, next_ticks: u64) -> u64 {
    if use_custom_ticks {
        configured_ticks as u64
    } else {
        next_ticks
    }
}

/// Guest CNTPCT ticks per millisecond (19.2 MHz counter).
const CNTPCT_TICKS_PER_MS: u64 = wall_clock::CNTFRQ / 1000;

/// NX2 session extension, not present upstream: adaptive pacing of the guest
/// counter while the session presents far below real time.
///
/// Mario Kart World measures its own frame time with CNTPCT. Once a frame
/// appears to overrun its budget several times over, the title's late-frame
/// handling drops individual objects from the main view on arbitrary frames
/// (their shadow-pass draws stay), which shows as scenery flickering. A
/// console never reaches that regime; an emulated session presenting at
/// 15-20 FPS is permanently in it. Eden's `sync_core_speed` divides CNTPCT by
/// the configured speed limit for the same class of problem, but that factor
/// is static. This paces the counter by the measured present interval instead:
/// below `ENGAGE_MS` per frame the counter advances so that one presented
/// frame appears to last `target_ms`; at or above real time it is untouched.
///
/// The mapping is piecewise linear and rebased at every rate change, so the
/// paced counter stays continuous and monotonic and never runs ahead of the
/// raw one. Kernel timers and sleeps keep running on the host clock, exactly
/// as with `sync_core_speed`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ClockPacing {
    anchor_raw: u64,
    anchor_paced: u64,
    /// Paced ticks per raw tick, in 1/65536 units; `RATE_ONE` is real time.
    rate: u32,
    last_present_raw: Option<u64>,
    /// Smoothed raw ticks between two presents; 0 until the first interval.
    smoothed_frame_ticks: u64,
    engaged: bool,
}

impl ClockPacing {
    const RATE_ONE: u32 = 1 << 16;
    /// Slowest pacing: one sixteenth of real time.
    const RATE_MIN: u32 = Self::RATE_ONE / 16;
    /// Pacing starts above this smoothed present interval (below ~28 FPS)...
    const ENGAGE_MS: u64 = 36;
    /// ...and stops once the session is back under it (30 FPS and faster).
    const RELEASE_MS: u64 = 33;
    /// Present intervals longer than this are pauses or loads, not frames.
    const IGNORE_MS: u64 = 500;
    /// Apparent duration of one presented frame while pacing.
    pub(crate) const DEFAULT_TARGET_MS: u64 = 25;

    pub(crate) const fn new() -> Self {
        Self {
            anchor_raw: 0,
            anchor_paced: 0,
            rate: Self::RATE_ONE,
            last_present_raw: None,
            smoothed_frame_ticks: 0,
            engaged: false,
        }
    }

    /// Paced counter value for a raw counter value.
    pub(crate) fn paced(&self, raw: u64) -> u64 {
        let elapsed = raw.saturating_sub(self.anchor_raw);
        self.anchor_paced
            .wrapping_add(((elapsed as u128 * self.rate as u128) >> 16) as u64)
    }

    fn set_rate(&mut self, raw: u64, rate: u32) {
        if rate == self.rate {
            return;
        }
        self.anchor_paced = self.paced(raw);
        self.anchor_raw = raw.max(self.anchor_raw);
        self.rate = rate;
    }

    /// Account one presented frame at raw counter value `raw`.
    pub(crate) fn frame_presented(&mut self, raw: u64, target_ms: u64) {
        let Some(last) = self.last_present_raw.replace(raw) else {
            return;
        };
        let interval = raw.saturating_sub(last);
        if interval == 0 || interval > Self::IGNORE_MS * CNTPCT_TICKS_PER_MS {
            return;
        }
        self.smoothed_frame_ticks = if self.smoothed_frame_ticks == 0 {
            interval
        } else {
            (self.smoothed_frame_ticks * 3 + interval) / 4
        };
        let smoothed = self.smoothed_frame_ticks;
        if smoothed > Self::ENGAGE_MS * CNTPCT_TICKS_PER_MS {
            self.engaged = true;
        } else if smoothed < Self::RELEASE_MS * CNTPCT_TICKS_PER_MS {
            self.engaged = false;
        }
        let rate = if self.engaged {
            let target = target_ms * CNTPCT_TICKS_PER_MS;
            ((target as u128 * Self::RATE_ONE as u128) / smoothed as u128)
                .clamp(Self::RATE_MIN as u128, Self::RATE_ONE as u128) as u32
        } else {
            Self::RATE_ONE
        };
        self.set_rate(raw, rate);
    }

    /// Return to real time, keeping the counter continuous.
    pub(crate) fn release(&mut self, raw: u64) {
        self.set_rate(raw, Self::RATE_ONE);
        self.last_present_raw = None;
        self.smoothed_frame_ticks = 0;
        self.engaged = false;
    }
}

/// `RUZU_NX2_CLOCK_PACING=0` disables the NX2 counter pacing;
/// `RUZU_NX2_CLOCK_PACING_MS=<8..=33>` overrides the apparent frame duration.
fn nx2_clock_pacing_target_ms() -> Option<u64> {
    static TARGET: OnceLock<Option<u64>> = OnceLock::new();
    *TARGET.get_or_init(|| {
        if std::env::var_os("RUZU_NX2_CLOCK_PACING")
            .is_some_and(|value| value.to_string_lossy().starts_with('0'))
        {
            return None;
        }
        Some(
            std::env::var("RUZU_NX2_CLOCK_PACING_MS")
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
                .filter(|ms| (8..=33).contains(ms))
                .unwrap_or(ClockPacing::DEFAULT_TARGET_MS),
        )
    })
}

/// A callback that may be scheduled for a particular core timing event.
/// Returns an optional reschedule time in nanoseconds.
pub type TimedCallback = Box<dyn Fn(i64, Duration) -> Option<Duration> + Send + Sync>;

fn trace_ct_fire_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_CT_FIRE").is_some())
}

/// Contains the characteristics of a particular event.
pub struct EventType {
    /// The event's callback function.
    ///
    /// Stored behind an `Arc` (it is immutable after construction) so the
    /// timing loop can clone the handle and **release the `EventType` Mutex
    /// before invoking the callback**. This is what lets a callback such as
    /// `KHardwareTimer::do_task` acquire the scheduler lock without the
    /// `event_type`-lock <-> scheduler-lock inversion (a guest holding the
    /// scheduler lock that schedules/unschedules a timer event would otherwise
    /// deadlock against the timing thread holding the `EventType` Mutex across
    /// the callback).
    pub callback: Arc<TimedCallback>,
    /// The name of the event (for debugging).
    pub name: String,
    /// A monotonic sequence number, incremented when this event is changed externally.
    pub sequence_number: usize,
}

impl EventType {
    pub fn new(callback: TimedCallback, name: String) -> Self {
        Self {
            callback: Arc::new(callback),
            name,
            sequence_number: 0,
        }
    }
}

/// Creates a core timing event with the given name and callback.
pub fn create_event(name: String, callback: TimedCallback) -> Arc<Mutex<EventType>> {
    Arc::new(Mutex::new(EventType::new(callback, name)))
}

/// Whether to wait for in-progress events when unscheduling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnscheduleEventType {
    Wait,
    NoWait,
}

/// A scheduled event in the timing queue.
struct TimingEvent {
    /// Time in nanoseconds when this event fires.
    time: i64,
    /// FIFO ordering for events at the same time.
    fifo_order: u64,
    /// Weak reference to the event type.
    event_type: Weak<Mutex<EventType>>,
    /// If non-zero, this event auto-reschedules with this interval.
    reschedule_time: i64,
}

impl PartialEq for TimingEvent {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time && self.fifo_order == other.fifo_order
    }
}

impl Eq for TimingEvent {}

// BinaryHeap is a max-heap; we want the earliest event first,
// so we reverse the ordering (smallest time = highest priority).
impl PartialOrd for TimingEvent {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for TimingEvent {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        // Reverse ordering: earlier time = higher priority
        other
            .time
            .cmp(&self.time)
            .then_with(|| other.fifo_order.cmp(&self.fifo_order))
    }
}

/// The core timing system.
///
/// This is the system to schedule events into the emulated machine's future.
/// Time is measured in nanoseconds (globally) and CPU ticks (for single-core mode).
struct CoreTimingState {
    global_timer: i64,
    event_queue: BinaryHeap<TimingEvent>,
    event_fifo_id: u64,
    pause_end_time: i64,
    cpu_ticks: u64,
    downcount: i64,
}

pub struct CoreTiming {
    clock: Box<dyn WallClock>,

    state: Mutex<CoreTimingState>,

    event: Event,
    pause_event: Event,
    basic_lock: Mutex<()>,
    advance_lock: Mutex<()>,
    timer_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    paused: AtomicBool,
    paused_set: AtomicBool,
    wait_set: AtomicBool,
    shutting_down: AtomicBool,
    has_started: AtomicBool,
    on_thread_init: Mutex<Option<Box<dyn Fn() + Send>>>,

    is_multicore: AtomicBool,

    /// NX2 session counter pacing (see `ClockPacing`); real time otherwise.
    clock_pacing: Mutex<ClockPacing>,
}

impl CoreTiming {
    pub fn new() -> Self {
        Self {
            clock: wall_clock::create_optimal_clock(),
            state: Mutex::new(CoreTimingState {
                global_timer: 0,
                event_queue: BinaryHeap::new(),
                event_fifo_id: 0,
                pause_end_time: 0,
                cpu_ticks: 0,
                downcount: MAX_SLICE_LENGTH,
            }),
            event: Event::new(),
            pause_event: Event::new(),
            basic_lock: Mutex::new(()),
            advance_lock: Mutex::new(()),
            timer_thread: Mutex::new(None),
            paused: AtomicBool::new(false),
            paused_set: AtomicBool::new(false),
            wait_set: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            has_started: AtomicBool::new(false),
            on_thread_init: Mutex::new(None),
            is_multicore: AtomicBool::new(false),
            clock_pacing: Mutex::new(ClockPacing::new()),
        }
    }

    /// One frame was presented by an NX2 session. Updates the counter pacing
    /// from the interval since the previous present; inert for other sessions,
    /// in single-core mode and with `RUZU_NX2_CLOCK_PACING=0`.
    pub fn notify_frame_presented(&self) {
        if !self.is_multicore.load(Ordering::SeqCst) {
            return;
        }
        let raw = self.clock.get_cntpct() as u64;
        let target = nx2_clock_pacing_target_ms()
            .filter(|_| common::settings::values().use_nx2_session_policies);
        let mut pacing = self.clock_pacing.lock();
        // Diagnostic (RUZU_NX2_CLOCK_PACING_LOG=1): real and apparent duration
        // of every presented frame, in microseconds.
        static LOG: OnceLock<bool> = OnceLock::new();
        if *LOG.get_or_init(|| std::env::var_os("RUZU_NX2_CLOCK_PACING_LOG").is_some()) {
            if let Some(last) = pacing.last_present_raw {
                let to_us = |ticks: u64| ticks * 1000 / CNTPCT_TICKS_PER_MS;
                log::warn!(
                    "clock pacing: frame real {} us apparent {} us rate {}/65536",
                    to_us(raw.saturating_sub(last)),
                    to_us(pacing.paced(raw).saturating_sub(pacing.paced(last))),
                    pacing.rate
                );
            }
        }
        match target {
            Some(target_ms) => pacing.frame_presented(raw, target_ms),
            None => pacing.release(raw),
        }
    }

    /// Sets if emulation is multicore or single core. Must be set before Initialize.
    pub fn set_multicore(&self, is_multicore: bool) {
        self.is_multicore.store(is_multicore, Ordering::SeqCst);
    }

    /// CoreTiming begins at the boundary of timing slice -1. An initial call to Advance() is
    /// required to end slice -1 and start slice 0 before the first cycle of code is executed.
    pub fn initialize<F: Fn() + Send + 'static>(self: &Arc<Self>, on_thread_init: F) {
        self.reset();
        *self.on_thread_init.lock() = Some(Box::new(on_thread_init));
        {
            let mut state = self.state.lock();
            state.event_fifo_id = 0;
            state.cpu_ticks = 0;
        }
        self.shutting_down.store(false, Ordering::SeqCst);

        if !self.is_multicore.load(Ordering::SeqCst) {
            return;
        }

        // Upstream passes `std::ref(*this)` to the timing thread; the thread
        // does not own CoreTiming. Keeping a strong Arc here creates a cycle:
        // Drop is responsible for stopping and joining the thread, but Drop
        // cannot run while that same thread owns the final Arc.
        //
        // The raw pointer remains valid until Drop::reset joins this thread.
        let ct_ptr = Arc::as_ptr(self) as usize;

        let handle = std::thread::Builder::new()
            .name("CoreTiming".to_string())
            .spawn(move || {
                let ct = unsafe { &*(ct_ptr as *const CoreTiming) };
                // Upstream: ThreadEntry calls on_thread_init() then ThreadLoop().
                if let Some(ref init_fn) = *ct.on_thread_init.lock() {
                    init_fn();
                }

                // ThreadLoop — matches upstream CoreTiming::ThreadLoop().
                // Rust still uses interior locks for mutable state, but the
                // timing thread is now owned and spawned by initialize().
                ct.has_started.store(true, Ordering::SeqCst);
                // Ensure the timer thread starts unpaused.
                // sync_pause(false) from System::run() may not have been called yet.
                ct.paused.store(false, Ordering::SeqCst);
                while !ct.shutting_down.load(Ordering::SeqCst) {
                    while !ct.paused.load(Ordering::SeqCst) {
                        ct.paused_set.store(false, Ordering::SeqCst);

                        let next_time = ct.advance();
                        if let Some(next_ns) = next_time {
                            let now_ns = ct.get_global_time_ns().as_nanos() as i64;
                            let wait_time = next_ns - now_ns;
                            if trace_ct_fire_enabled() {
                                log::info!(
                                    "CT_LOOP next_ns={} now_ns={} wait_ns={}",
                                    next_ns,
                                    now_ns,
                                    wait_time
                                );
                            }
                            if wait_time > 0 {
                                // Match upstream ThreadLoop: wait interruptibly so newly
                                // scheduled earlier events wake the timer thread
                                // immediately.
                                ct.event
                                    .wait_for(std::time::Duration::from_nanos(wait_time as u64));
                            }
                        } else {
                            if trace_ct_fire_enabled() {
                                log::info!("CT_LOOP queue_empty, parking");
                            }
                            ct.wait_set.store(true, Ordering::SeqCst);
                            ct.event.wait();
                        }
                        ct.wait_set.store(false, Ordering::SeqCst);
                    }

                    ct.paused_set.store(true, Ordering::SeqCst);
                    ct.pause_event.wait();
                }
            })
            .expect("Failed to spawn CoreTiming timer thread");

        *self.timer_thread.lock() = Some(handle);
        log::info!("CoreTiming: timer thread started");
    }

    /// Clear all pending events. This should ONLY be done on exit.
    pub fn clear_pending_events(&self) {
        let _adv = self.advance_lock.lock();
        let _basic = self.basic_lock.lock();
        self.state.lock().event_queue.clear();
        self.event.set();
    }

    /// Pauses/Unpauses the execution of the timer thread.
    pub fn pause(&self, is_paused: bool) {
        self.paused.store(is_paused, Ordering::SeqCst);
        self.pause_event.set();

        if !is_paused {
            self.state.lock().pause_end_time = self.get_global_time_ns().as_nanos() as i64;
        }
    }

    /// Pauses/Unpauses the execution of the timer thread and waits until paused.
    pub fn sync_pause(&self, is_paused: bool) {
        if is_paused == self.paused.load(Ordering::SeqCst)
            && self.paused_set.load(Ordering::SeqCst) == is_paused
        {
            return;
        }

        self.pause(is_paused);

        if self.timer_thread.lock().is_some() {
            if !is_paused {
                self.pause_event.set();
            }
            self.event.set();
            // Spin-wait for the timer thread to acknowledge the pause state
            while self.paused_set.load(Ordering::SeqCst) != is_paused {
                std::hint::spin_loop();
            }
        }

        if !is_paused {
            self.state.lock().pause_end_time = self.get_global_time_ns().as_nanos() as i64;
        }
    }

    /// Checks if core timing is running.
    pub fn is_running(&self) -> bool {
        !self.paused_set.load(Ordering::SeqCst)
    }

    /// Checks if the timer thread has started.
    pub fn has_started(&self) -> bool {
        self.has_started.load(Ordering::SeqCst)
    }

    /// Checks if there are any pending time events.
    pub fn has_pending_events(&self) -> bool {
        let _lock = self.basic_lock.lock();
        !(self.wait_set.load(Ordering::SeqCst) && self.state.lock().event_queue.is_empty())
    }

    /// Schedules an event in core timing.
    pub fn schedule_event(
        &self,
        ns_into_future: Duration,
        event_type: &Arc<Mutex<EventType>>,
        absolute_time: bool,
    ) {
        let _lock = self.basic_lock.lock();
        let next_time = if absolute_time {
            ns_into_future
        } else {
            self.get_global_time_ns() + ns_into_future
        };

        let mut state = self.state.lock();
        let fifo_order = state.event_fifo_id;
        state.event_queue.push(TimingEvent {
            time: next_time.as_nanos() as i64,
            fifo_order,
            event_type: Arc::downgrade(event_type),
            reschedule_time: 0,
        });
        state.event_fifo_id += 1;
        drop(_lock);
        self.event.set();
    }

    /// Schedules an event which will automatically re-schedule itself with the given time,
    /// until unscheduled.
    pub fn schedule_looping_event(
        &self,
        start_time: Duration,
        resched_time: Duration,
        event_type: &Arc<Mutex<EventType>>,
        absolute_time: bool,
    ) {
        let _lock = self.basic_lock.lock();
        let next_time = if absolute_time {
            start_time
        } else {
            self.get_global_time_ns() + start_time
        };

        let mut state = self.state.lock();
        let fifo_order = state.event_fifo_id;
        state.event_queue.push(TimingEvent {
            time: next_time.as_nanos() as i64,
            fifo_order,
            event_type: Arc::downgrade(event_type),
            reschedule_time: resched_time.as_nanos() as i64,
        });
        state.event_fifo_id += 1;
        drop(_lock);
        self.event.set();
    }

    /// Unschedules all instances of the given event type.
    pub fn unschedule_event(
        &self,
        event_type: &Arc<Mutex<EventType>>,
        unsched_type: UnscheduleEventType,
    ) {
        {
            let _lock = self.basic_lock.lock();

            let event_type_ptr = Arc::as_ptr(event_type);
            // Rebuild the queue without the matching events
            let mut state = self.state.lock();
            let old_queue = std::mem::take(&mut state.event_queue);
            for evt in old_queue.into_iter() {
                if let Some(strong) = evt.event_type.upgrade() {
                    if Arc::as_ptr(&strong) != event_type_ptr {
                        state.event_queue.push(evt);
                    }
                }
                // Drop events with dead weak refs too
            }

            event_type.lock().sequence_number += 1;
        }

        // Force any in-progress events to finish
        if unsched_type == UnscheduleEventType::Wait {
            let _lock = self.advance_lock.lock();
        }
    }

    /// Adds ticks to the CPU tick counter and decrements downcount.
    pub fn add_ticks(&self, ticks_to_add: u64) {
        let (use_custom_ticks, configured_ticks) = {
            let values = common::settings::values();
            (
                *values.use_custom_cpu_ticks.get_value(),
                *values.cpu_ticks.get_value(),
            )
        };
        let ticks = get_next_tick_count(use_custom_ticks, configured_ticks, ticks_to_add);
        let mut state = self.state.lock();
        state.cpu_ticks += ticks;
        state.downcount -= ticks as i64;
    }

    /// Resets the tick downcount to the max slice length.
    pub fn reset_ticks(&self) {
        self.state.lock().downcount = MAX_SLICE_LENGTH;
    }

    /// Adds idle ticks.
    pub fn idle(&self) {
        self.add_ticks(1000);
    }

    /// Returns the current downcount.
    pub fn get_downcount(&self) -> i64 {
        self.state.lock().downcount
    }

    /// Returns the current CNTPCT tick value.
    pub fn get_clock_ticks(&self) -> u64 {
        let raw_ticks = if self.is_multicore.load(Ordering::SeqCst) {
            // Identity unless an NX2 session is presenting below real time.
            self.clock_pacing.lock().paced(self.clock.get_cntpct() as u64)
        } else {
            wall_clock::cpu_tick_to_cntpct(self.state.lock().cpu_ticks)
        };
        let (target_mhz, sync_core_speed) = {
            let values = common::settings::values();
            (
                cpu_clock_target_mhz(*values.cpu_clock.get_value()),
                *values.sync_core_speed.get_value(),
            )
        };
        let speed = if sync_core_speed {
            common::settings::speed_limit()
        } else {
            100
        };
        scale_clock_ticks(raw_ticks, target_mhz, sync_core_speed, speed)
    }

    /// Returns the current GPU tick value.
    pub fn get_gpu_ticks(&self) -> u64 {
        if self.is_multicore.load(Ordering::SeqCst) {
            self.clock.get_gpu_tick() as u64
        } else {
            wall_clock::cpu_tick_to_gpu_tick(self.state.lock().cpu_ticks)
        }
    }

    /// Returns current time in nanoseconds.
    pub fn get_global_time_ns(&self) -> Duration {
        if self.is_multicore.load(Ordering::SeqCst) {
            self.clock.get_time_ns()
        } else {
            Duration::from_nanos(wall_clock::cpu_tick_to_ns(self.state.lock().cpu_ticks))
        }
    }

    /// Returns current time in microseconds.
    pub fn get_global_time_us(&self) -> Duration {
        if self.is_multicore.load(Ordering::SeqCst) {
            self.clock.get_time_us()
        } else {
            Duration::from_micros(wall_clock::cpu_tick_to_us(self.state.lock().cpu_ticks))
        }
    }

    /// Checks for events manually and returns time in nanoseconds for next event.
    /// This is the core advance function that fires all due events.
    ///
    /// The C++ version holds both advance_lock and basic_lock, unlocking basic_lock
    /// around callbacks. In Rust, since we have &mut self (exclusive access),
    /// we use the advance_lock for external synchronization and handle the
    /// basic_lock at a coarser granularity.
    pub fn advance(&self) -> Option<i64> {
        let _adv_lock = self.advance_lock.lock();
        self.state.lock().global_timer = self.get_global_time_ns().as_nanos() as i64;

        loop {
            // Check under basic_lock if there's a due event
            let maybe_evt = {
                let _basic = self.basic_lock.lock();
                let mut state = self.state.lock();
                match state.event_queue.peek() {
                    Some(evt) if evt.time <= state.global_timer => state.event_queue.pop(),
                    _ => None,
                }
            };

            let evt = match maybe_evt {
                Some(e) => e,
                None => break,
            };

            let upgraded = evt.event_type.upgrade();
            if upgraded.is_none() && trace_ct_fire_enabled() {
                log::info!(
                    "CT_DROP_WEAK time={} reschedule={}",
                    evt.time,
                    evt.reschedule_time
                );
            }
            if let Some(event_type_arc) = upgraded {
                let evt_time = evt.time;
                let evt_sequence_num = event_type_arc.lock().sequence_number;

                // Fire the callback without holding basic_lock
                let ns_late = self.get_global_time_ns().as_nanos() as i64 - evt_time;
                if trace_ct_fire_enabled() {
                    let name = event_type_arc.lock().name.clone();
                    log::info!(
                        "CT_FIRE name={} time={} reschedule={}",
                        name,
                        evt_time,
                        evt.reschedule_time
                    );
                }
                // Clone the callback handle and RELEASE the EventType Mutex
                // before invoking it, so callbacks that take the scheduler lock
                // (e.g. KHardwareTimer::do_task) cannot deadlock against guest
                // threads scheduling/unscheduling timer events under that lock.
                let callback = {
                    let et = event_type_arc.lock();
                    Arc::clone(&et.callback)
                };
                let callback_result =
                    callback(evt_time, Duration::from_nanos(ns_late.max(0) as u64));

                if evt.reschedule_time != 0 {
                    // Looping event: reschedule under basic_lock
                    let _basic = self.basic_lock.lock();
                    let mut state = self.state.lock();

                    // Check if the event was externally modified
                    if evt_sequence_num != event_type_arc.lock().sequence_number {
                        state.global_timer = self.get_global_time_ns().as_nanos() as i64;
                        continue;
                    }

                    let next_schedule_time = match callback_result {
                        Some(dur) => dur.as_nanos() as i64,
                        None => evt.reschedule_time,
                    };

                    // Upstream reschedules relative to the event's scheduled
                    // time, then only clamps events that were scheduled into a
                    // pause window. This preserves catch-up behavior for late
                    // looping events (Core::Timing::CoreTiming::Advance).
                    let mut next_time = evt_time + next_schedule_time;
                    if evt_time < state.pause_end_time {
                        next_time = state.pause_end_time + next_schedule_time;
                    }

                    let fifo_order = state.event_fifo_id;
                    state.event_queue.push(TimingEvent {
                        time: next_time,
                        fifo_order,
                        event_type: evt.event_type.clone(),
                        reschedule_time: next_schedule_time,
                    });
                    state.event_fifo_id += 1;
                }
            }

            self.state.lock().global_timer = self.get_global_time_ns().as_nanos() as i64;
        }

        let _basic = self.basic_lock.lock();
        self.state.lock().event_queue.peek().map(|evt| evt.time)
    }

    /// Resets the timing system. Called on shutdown.
    pub(crate) fn reset(&self) {
        self.paused.store(true, Ordering::SeqCst);
        self.shutting_down.store(true, Ordering::SeqCst);
        self.pause_event.set();
        self.event.set();
        if let Some(thread) = self.timer_thread.lock().take() {
            let _ = thread.join();
        }
        self.has_started.store(false, Ordering::SeqCst);
    }
}

impl Default for CoreTiming {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CoreTiming {
    fn drop(&mut self) {
        self.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn custom_cpu_ticks_replace_each_reported_slice() {
        assert_eq!(get_next_tick_count(false, 16_000, 731), 731);
        assert_eq!(get_next_tick_count(true, 16_000, 731), 16_000);
    }

    #[test]
    fn cpu_clock_and_speed_scaling_match_eden_targets() {
        use common::settings_enums::CpuClock;

        assert_eq!(cpu_clock_target_mhz(CpuClock::Normal), 1020);
        assert_eq!(cpu_clock_target_mhz(CpuClock::Boost), 1734);
        assert_eq!(cpu_clock_target_mhz(CpuClock::Overclock), 2040);
        assert_eq!(scale_clock_ticks(1020, 1734, false, 100), 1734);
        assert_eq!(scale_clock_ticks(1020, 1020, true, 50), 2040);
        assert_eq!(scale_clock_ticks(1020, 1020, true, 200), 510);
    }

    #[test]
    fn clock_pacing_is_identity_at_real_time_and_until_the_second_present() {
        let mut pacing = ClockPacing::new();
        assert_eq!(pacing.paced(123_456), 123_456);
        let frame = 16 * CNTPCT_TICKS_PER_MS;
        for index in 0..20u64 {
            pacing.frame_presented(index * frame, ClockPacing::DEFAULT_TARGET_MS);
            assert_eq!(pacing.paced(index * frame + 7), index * frame + 7);
        }
        // 30 FPS is inside the hysteresis band and never engages either.
        let mut pacing = ClockPacing::new();
        let frame = 33 * CNTPCT_TICKS_PER_MS;
        for index in 0..20u64 {
            pacing.frame_presented(index * frame, ClockPacing::DEFAULT_TARGET_MS);
        }
        assert_eq!(pacing.paced(20 * frame), 20 * frame);
    }

    #[test]
    fn clock_pacing_makes_a_slow_frame_appear_to_last_the_target() {
        let mut pacing = ClockPacing::new();
        let frame = 60 * CNTPCT_TICKS_PER_MS;
        let mut previous = 0;
        for index in 0..40u64 {
            let raw = index * frame;
            let before = pacing.paced(raw);
            pacing.frame_presented(raw, 25);
            // Rebasing never moves the counter, and it never runs backwards.
            assert_eq!(pacing.paced(raw), before);
            assert!(before >= previous);
            previous = before;
        }
        let apparent = pacing.paced(40 * frame) - pacing.paced(39 * frame);
        let target = 25 * CNTPCT_TICKS_PER_MS;
        assert!(apparent.abs_diff(target) <= target / 100, "apparent {apparent} target {target}");
        // Never ahead of the raw counter.
        assert!(pacing.paced(40 * frame) <= 40 * frame);
    }

    #[test]
    fn clock_pacing_clamps_releases_and_ignores_pauses() {
        // A 2 s present interval is a load or a pause, not a frame.
        let mut pacing = ClockPacing::new();
        pacing.frame_presented(0, 25);
        pacing.frame_presented(2000 * CNTPCT_TICKS_PER_MS, 25);
        assert_eq!(pacing, {
            let mut expected = ClockPacing::new();
            expected.last_present_raw = Some(2000 * CNTPCT_TICKS_PER_MS);
            expected
        });

        // 400 ms frames would need 1/16 of real time; the rate is clamped there.
        let mut pacing = ClockPacing::new();
        let frame = 480 * CNTPCT_TICKS_PER_MS;
        for index in 0..30u64 {
            pacing.frame_presented(index * frame, 25);
        }
        assert_eq!(pacing.rate, ClockPacing::RATE_MIN);

        // Back at full speed the pacing releases and the counter keeps its
        // offset: it continues at real time from where it was.
        let start = 30 * frame;
        let fast = 16 * CNTPCT_TICKS_PER_MS;
        for index in 0..40u64 {
            pacing.frame_presented(start + index * fast, 25);
        }
        assert_eq!(pacing.rate, ClockPacing::RATE_ONE);
        let now = start + 40 * fast;
        assert_eq!(pacing.paced(now + 1000) - pacing.paced(now), 1000);
        assert!(pacing.paced(now) < now);

        let mut released = pacing;
        released.release(now);
        assert_eq!(released.paced(now), pacing.paced(now));
        assert_eq!(released.last_present_raw, None);
    }

    #[test]
    fn advance_looping_event_reschedules_from_event_time() {
        let core_timing = CoreTiming::new();
        let count = Arc::new(AtomicU64::new(0));
        let count_for_cb = count.clone();
        let event = create_event(
            "looping".to_string(),
            Box::new(move |_time, _late| {
                count_for_cb.fetch_add(1, Ordering::SeqCst);
                None
            }),
        );

        core_timing.schedule_looping_event(
            Duration::from_nanos(0),
            Duration::from_nanos(5),
            &event,
            false,
        );

        let first_time = core_timing.state.lock().event_queue.peek().unwrap().time;
        core_timing.advance();
        let second_time = core_timing.state.lock().event_queue.peek().unwrap().time;

        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(second_time, first_time + 5);
    }

    #[test]
    fn timer_thread_wakes_for_earlier_new_event() {
        let core_timing = Arc::new(CoreTiming::new());
        core_timing.set_multicore(true);
        core_timing.initialize(|| {});

        let late_fired = Arc::new(AtomicBool::new(false));
        let late_fired_cb = late_fired.clone();
        let late_event = create_event(
            "late".to_string(),
            Box::new(move |_time, _late| {
                late_fired_cb.store(true, Ordering::SeqCst);
                None
            }),
        );

        let early_fired_at = Arc::new(AtomicU64::new(0));
        let early_fired_at_cb = early_fired_at.clone();
        let start = Instant::now();
        let early_event = create_event(
            "early".to_string(),
            Box::new(move |_time, _late| {
                early_fired_at_cb.store(start.elapsed().as_millis() as u64, Ordering::SeqCst);
                None
            }),
        );

        core_timing.schedule_event(Duration::from_millis(250), &late_event, false);
        std::thread::sleep(Duration::from_millis(20));
        core_timing.schedule_event(Duration::from_millis(20), &early_event, false);

        let deadline = Instant::now() + Duration::from_millis(200);
        while early_fired_at.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }

        let early_ms = early_fired_at.load(Ordering::SeqCst);
        assert!(early_ms > 0, "earlier event never fired");
        assert!(
            early_ms < 150,
            "earlier event fired too late: {}ms",
            early_ms
        );
        assert!(
            !late_fired.load(Ordering::SeqCst),
            "later event should not have fired first"
        );
    }

    #[test]
    fn timer_thread_does_not_own_core_timing() {
        let core_timing = Arc::new(CoreTiming::new());
        core_timing.set_multicore(true);
        core_timing.initialize(|| {});
        let weak = Arc::downgrade(&core_timing);

        drop(core_timing);

        assert!(
            weak.upgrade().is_none(),
            "the timing thread must not keep CoreTiming alive"
        );
    }
}

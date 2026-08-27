//! Port of zuyu/src/common/thread.h and zuyu/src/common/thread.cpp
//! Status: COMPLET
//! Derniere synchro: 2026-03-05

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// Thread priority levels, matching the C++ enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ThreadPriority {
    Low = 0,
    Normal = 1,
    High = 2,
    VeryHigh = 3,
    Critical = 4,
}

/// Set the current thread's scheduling priority.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn lowest_allowed_nice() -> i32 {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NICE, &mut limit) } != 0 {
        return 0;
    }
    if limit.rlim_cur >= 40 {
        -20
    } else {
        20 - limit.rlim_cur as i32
    }
}

/// Preferred CPU placement for worker threads.
///
/// Port of upstream `Common::ThreadPlacement`. On non-Android hosts the
/// placement helpers are no-ops, while non-default placement still lowers the
/// worker priority exactly as `StatefulThreadWorker` does upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ThreadPlacement {
    Default = 0,
    Background = 1,
    Efficiency = 2,
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn nice_value_for_priority(priority: ThreadPriority) -> i32 {
    const NICE_AUDIO: i32 = -16;
    const NICE_URGENT_DISPLAY: i32 = -8;
    const NICE_DISPLAY: i32 = -4;
    const NICE_DEFAULT: i32 = 0;
    const NICE_BACKGROUND: i32 = 10;

    let wanted = match priority {
        ThreadPriority::Low => NICE_BACKGROUND,
        ThreadPriority::Normal => NICE_DEFAULT,
        ThreadPriority::High => NICE_DISPLAY,
        ThreadPriority::VeryHigh => NICE_URGENT_DISPLAY,
        ThreadPriority::Critical => NICE_AUDIO,
    };
    wanted.max(NICE_DEFAULT.min(lowest_allowed_nice()))
}

/// Port of upstream `Common::SetCurrentThreadPriority`.
pub fn set_current_thread_priority(new_priority: ThreadPriority) {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let nice_value = nice_value_for_priority(new_priority);
        if unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, nice_value) } != 0 {
            log::debug!(
                "Could not set thread nice value to {nice_value}: {}",
                std::io::Error::last_os_error()
            );
        }
    }

    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
            THREAD_PRIORITY_BELOW_NORMAL, THREAD_PRIORITY_HIGHEST, THREAD_PRIORITY_NORMAL,
            THREAD_PRIORITY_TIME_CRITICAL,
        };
        let windows_priority = match new_priority {
            ThreadPriority::Low => THREAD_PRIORITY_BELOW_NORMAL,
            ThreadPriority::Normal => THREAD_PRIORITY_NORMAL,
            ThreadPriority::High => THREAD_PRIORITY_ABOVE_NORMAL,
            ThreadPriority::VeryHigh => THREAD_PRIORITY_HIGHEST,
            ThreadPriority::Critical => THREAD_PRIORITY_TIME_CRITICAL,
        };
        SetThreadPriority(GetCurrentThread(), windows_priority);
    }

    #[cfg(all(
        unix,
        not(any(target_os = "linux", target_os = "android", target_os = "haiku"))
    ))]
    unsafe {
        let max_priority = libc::sched_get_priority_max(libc::SCHED_OTHER);
        let min_priority = libc::sched_get_priority_min(libc::SCHED_OTHER);
        if max_priority > min_priority {
            let level = (new_priority as u32).min(4);
            let mut params: libc::sched_param = std::mem::zeroed();
            params.sched_priority =
                min_priority + ((max_priority - min_priority) * level as i32) / 4;
            libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_OTHER, &params);
        }
    }

    #[cfg(not(any(windows, unix, target_os = "linux", target_os = "android")))]
    {
        let _ = new_priority;
    }
}

/// Set the current thread's name (visible in debuggers).
/// On Linux, truncates to 15 characters as required by pthread_setname_np.
pub fn set_current_thread_name(name: &str) {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;

        // Linux limits thread names to 15 characters
        let truncated: String = name.chars().take(15).collect();
        if let Ok(c_name) = CString::new(truncated) {
            unsafe {
                libc::pthread_setname_np(libc::pthread_self(), c_name.as_ptr());
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        if let Ok(c_name) = CString::new(name) {
            unsafe {
                libc::pthread_setname_np(c_name.as_ptr());
            }
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = name;
    }
}

/// Port of upstream `Common::SetCurrentThreadToPerformanceCores`.
///
/// Eden only changes affinity in its Android implementation; Android JNI and
/// ADPF integration are excluded from this Rust port, so other hosts take the
/// same no-op branch.
pub fn set_current_thread_to_performance_cores() {}

/// Port of upstream `Common::SetCurrentThreadToEfficiencyCores`.
///
/// Eden only changes affinity in its Android implementation. Android JNI and
/// ADPF integration are excluded from this Rust port, so supported desktop
/// hosts take the same no-op branch.
pub fn set_current_thread_to_efficiency_cores() {}

/// Port of upstream `Common::SetCurrentThreadToBackgroundWork`.
pub fn set_current_thread_to_background_work() {}

/// Port of upstream `Common::SetCurrentThreadToAllCores`.
pub fn set_current_thread_to_all_cores() {}

/// An event that can be set and waited on, matching the C++ Common::Event.
pub struct Event {
    mutex: Mutex<()>,
    condvar: Condvar,
    is_set: AtomicBool,
}

impl Event {
    pub fn new() -> Self {
        Self {
            mutex: Mutex::new(()),
            condvar: Condvar::new(),
            is_set: AtomicBool::new(false),
        }
    }

    pub fn is_set_peek(&self) -> bool {
        self.is_set.load(Ordering::SeqCst)
    }

    pub fn set(&self) {
        let _lk = self.mutex.lock().unwrap();
        self.is_set.store(true, Ordering::SeqCst);
        self.condvar.notify_one();
    }

    pub fn wait(&self) {
        let lk = self.mutex.lock().unwrap();
        let _lk = self
            .condvar
            .wait_while(lk, |_| !self.is_set.load(Ordering::SeqCst))
            .unwrap();
        self.is_set.store(false, Ordering::SeqCst);
    }

    pub fn wait_for(&self, duration: Duration) -> bool {
        let lk = self.mutex.lock().unwrap();
        let result = self
            .condvar
            .wait_timeout_while(lk, duration, |_| !self.is_set.load(Ordering::SeqCst));
        match result {
            Ok((_guard, timeout_result)) => {
                if timeout_result.timed_out() {
                    return false;
                }
                self.is_set.store(false, Ordering::SeqCst);
                true
            }
            Err(_) => false,
        }
    }

    pub fn reset(&self) {
        let _lk = self.mutex.lock().unwrap();
        self.is_set.store(false, Ordering::SeqCst);
    }

    pub fn is_set(&self) -> bool {
        self.is_set.load(Ordering::SeqCst)
    }
}

impl Default for Event {
    fn default() -> Self {
        Self::new()
    }
}

/// A barrier that blocks until all `count` threads have called `sync()`.
pub struct Barrier {
    mutex: Mutex<BarrierState>,
    condvar: Condvar,
    count: usize,
}

struct BarrierState {
    waiting: usize,
    generation: usize,
}

impl Barrier {
    pub fn new(count: usize) -> Self {
        Self {
            mutex: Mutex::new(BarrierState {
                waiting: 0,
                generation: 0,
            }),
            condvar: Condvar::new(),
            count,
        }
    }

    /// Blocks until all `count` threads have called sync().
    /// Returns true for all threads when the barrier is reached.
    pub fn sync(&self) -> bool {
        let mut state = self.mutex.lock().unwrap();
        let current_generation = state.generation;

        state.waiting += 1;
        if state.waiting == self.count {
            state.generation += 1;
            state.waiting = 0;
            self.condvar.notify_all();
            true
        } else {
            while current_generation == state.generation {
                state = self.condvar.wait(state).unwrap();
            }
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_set_wait() {
        let event = Event::new();
        event.set();
        // Should return immediately since it's already set
        event.wait();
        assert!(!event.is_set());
    }

    #[test]
    fn test_event_wait_for_timeout() {
        let event = Event::new();
        let result = event.wait_for(Duration::from_millis(10));
        assert!(!result);
    }

    #[test]
    fn test_set_thread_name() {
        set_current_thread_name("test_thread");
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn nice_values_match_upstream_and_respect_rlimit() {
        let lowest = lowest_allowed_nice();
        let expected = |wanted: i32| wanted.max(0.min(lowest));
        assert_eq!(nice_value_for_priority(ThreadPriority::Low), expected(10));
        assert_eq!(nice_value_for_priority(ThreadPriority::Normal), expected(0));
        assert_eq!(nice_value_for_priority(ThreadPriority::High), expected(-4));
        assert_eq!(
            nice_value_for_priority(ThreadPriority::VeryHigh),
            expected(-8)
        );
        assert_eq!(
            nice_value_for_priority(ThreadPriority::Critical),
            expected(-16)
        );
    }
}

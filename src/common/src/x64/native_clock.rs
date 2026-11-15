//! Port of zuyu/src/common/x64/native_clock.h and native_clock.cpp
//!
//! RDTSC-based high-precision clock for x86_64 platforms.

#[cfg(target_arch = "x86_64")]
use std::time::Duration;
#[cfg(target_arch = "x86_64")]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(target_arch = "x86_64")]
use crate::uint128::{get_fixed_point64_factor, multiply_high};
#[cfg(target_arch = "x86_64")]
use crate::wall_clock::{WallClock, CNTFRQ, GPU_TICK_FREQ};
#[cfg(target_arch = "x86_64")]
use crate::x64::rdtsc::fenced_rdtsc;

/// Ratio denominators matching C++ std::nano, std::micro, std::milli
const NS_RATIO_DEN: u64 = 1_000_000_000;
const US_RATIO_DEN: u64 = 1_000_000;
const MS_RATIO_DEN: u64 = 1_000;

/// RDTSC-based native clock for x86_64.
///
/// Uses fixed-point multiplication to convert TSC ticks to various time units
/// without expensive division at query time.
#[cfg(target_arch = "x86_64")]
pub struct NativeClock {
    rdtsc_frequency: u64,
    ns_rdtsc_factor: u64,
    us_rdtsc_factor: u64,
    ms_rdtsc_factor: u64,
    cntpct_rdtsc_factor: AtomicU64,
    guest_cntfrq: AtomicU64,
    gputick_rdtsc_factor: u64,
}

#[cfg(target_arch = "x86_64")]
impl NativeClock {
    /// Create a new NativeClock with the given RDTSC frequency.
    pub fn new(rdtsc_frequency: u64) -> Self {
        Self {
            rdtsc_frequency,
            ns_rdtsc_factor: get_fixed_point64_factor(NS_RATIO_DEN, rdtsc_frequency),
            us_rdtsc_factor: get_fixed_point64_factor(US_RATIO_DEN, rdtsc_frequency),
            ms_rdtsc_factor: get_fixed_point64_factor(MS_RATIO_DEN, rdtsc_frequency),
            cntpct_rdtsc_factor: AtomicU64::new(get_fixed_point64_factor(CNTFRQ, rdtsc_frequency)),
            guest_cntfrq: AtomicU64::new(CNTFRQ),
            gputick_rdtsc_factor: get_fixed_point64_factor(GPU_TICK_FREQ, rdtsc_frequency),
        }
    }

    /// Returns the RDTSC frequency used by this clock.
    pub fn rdtsc_frequency(&self) -> u64 {
        self.rdtsc_frequency
    }
}

#[cfg(target_arch = "x86_64")]
impl WallClock for NativeClock {
    fn set_guest_cntfrq(&self, frequency: u64) {
        self.guest_cntfrq.store(frequency, Ordering::Relaxed);
        self.cntpct_rdtsc_factor.store(
            get_fixed_point64_factor(frequency, self.rdtsc_frequency), Ordering::Relaxed);
    }

    fn get_guest_cntfrq(&self) -> u64 {
        self.guest_cntfrq.load(Ordering::Relaxed)
    }

    fn get_time_ns(&self) -> Duration {
        let ns = multiply_high(self.get_uptime() as u64, self.ns_rdtsc_factor);
        Duration::from_nanos(ns)
    }

    fn get_time_us(&self) -> Duration {
        let us = multiply_high(self.get_uptime() as u64, self.us_rdtsc_factor);
        Duration::from_micros(us)
    }

    fn get_time_ms(&self) -> Duration {
        let ms = multiply_high(self.get_uptime() as u64, self.ms_rdtsc_factor);
        Duration::from_millis(ms)
    }

    fn get_cntpct(&self) -> i64 {
        multiply_high(self.get_uptime() as u64, self.cntpct_rdtsc_factor.load(Ordering::Relaxed)) as i64
    }

    fn get_gpu_tick(&self) -> i64 {
        multiply_high(self.get_uptime() as u64, self.gputick_rdtsc_factor) as i64
    }

    fn get_uptime(&self) -> i64 {
        fenced_rdtsc() as i64
    }

    fn is_native(&self) -> bool {
        true
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;
    use crate::x64::rdtsc::estimate_rdtsc_frequency;

    #[test]
    fn test_native_clock_is_native() {
        let freq = estimate_rdtsc_frequency();
        let clock = NativeClock::new(freq);
        assert!(clock.is_native());
    }

    #[test]
    fn test_native_clock_uptime_increases() {
        let freq = estimate_rdtsc_frequency();
        let clock = NativeClock::new(freq);
        let t1 = clock.get_uptime();
        let t2 = clock.get_uptime();
        assert!(t2 >= t1);
    }
}

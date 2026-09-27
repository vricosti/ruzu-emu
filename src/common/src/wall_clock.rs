//! Port of zuyu/src/common/wall_clock.h and zuyu/src/common/wall_clock.cpp
//! Status: COMPLET
//! Derniere synchro: 2026-03-05

use std::time::{Duration, Instant};

/// CNTPCT_EL0 Frequency = 19.2 MHz
pub const CNTFRQ: u64 = 19_200_000;
/// GM20B GPU Tick Frequency = 614.4 MHz
pub const GPU_TICK_FREQ: u64 = 614_400_000;
/// T210/4 A57 CPU Tick Frequency = 1020.0 MHz
pub const CPU_TICK_FREQ: u64 = 1_020_000_000;

// Ratio constants for conversions (matching the C++ std::ratio types)
// NsToCNTPCT: CNTFRQ / 1_000_000_000
// NsToGPUTick: GPUTickFreq / 1_000_000_000
// CPUTickToNs: 1_000_000_000 / CPUTickFreq
// CPUTickToUs: 1_000_000 / CPUTickFreq
// CPUTickToCNTPCT: CNTFRQ / CPUTickFreq
// CPUTickToGPUTick: GPUTickFreq / CPUTickFreq

const NS_PER_SEC: u64 = 1_000_000_000;
const US_PER_SEC: u64 = 1_000_000;

/// Trait for wall clock implementations, matching the C++ WallClock abstract class.
pub trait WallClock: Send + Sync {
    /// Returns the time in nanoseconds since the construction of this clock.
    fn get_time_ns(&self) -> Duration;

    /// Returns the time in microseconds since the construction of this clock.
    fn get_time_us(&self) -> Duration;

    /// Returns the time in milliseconds since the construction of this clock.
    fn get_time_ms(&self) -> Duration;

    /// Returns the guest CNTPCT ticks since the construction of this clock.
    fn get_cntpct(&self) -> i64;

    /// Returns the guest GPU ticks since the construction of this clock.
    fn get_gpu_tick(&self) -> i64;

    /// Returns the raw host timer ticks since an indeterminate epoch.
    fn get_uptime(&self) -> i64;

    /// Returns whether the clock directly uses the host's hardware clock.
    fn is_native(&self) -> bool;
}

// Static conversion functions matching the C++ static methods

/// Convert nanoseconds to CNTPCT ticks.
///
/// Upstream uses `std::ratio<CNTFRQ, std::nano::den>` which simplifies at
/// compile time; use u128 intermediate here to avoid u64 overflow for large
/// nanosecond inputs (19.2M * 1e9 overflows u64 near ~15 minutes of uptime).
#[inline]
pub fn ns_to_cntpct(ns: u64) -> u64 {
    ((ns as u128) * CNTFRQ as u128 / NS_PER_SEC as u128) as u64
}

/// Convert nanoseconds to GPU ticks.
#[inline]
pub fn ns_to_gpu_tick(ns: u64) -> u64 {
    ((ns as u128) * GPU_TICK_FREQ as u128 / NS_PER_SEC as u128) as u64
}

/// Convert CPU ticks to nanoseconds.
#[inline]
pub fn cpu_tick_to_ns(cpu_tick: u64) -> u64 {
    ((cpu_tick as u128) * NS_PER_SEC as u128 / CPU_TICK_FREQ as u128) as u64
}

/// Convert CPU ticks to microseconds.
#[inline]
pub fn cpu_tick_to_us(cpu_tick: u64) -> u64 {
    ((cpu_tick as u128) * US_PER_SEC as u128 / CPU_TICK_FREQ as u128) as u64
}

/// Convert CPU ticks to CNTPCT ticks.
#[inline]
pub fn cpu_tick_to_cntpct(cpu_tick: u64) -> u64 {
    ((cpu_tick as u128) * CNTFRQ as u128 / CPU_TICK_FREQ as u128) as u64
}

/// Convert CPU ticks to GPU ticks.
#[inline]
pub fn cpu_tick_to_gpu_tick(cpu_tick: u64) -> u64 {
    ((cpu_tick as u128) * GPU_TICK_FREQ as u128 / CPU_TICK_FREQ as u128) as u64
}

/// Standard wall clock implementation using std::time.
/// This is the fallback when no native high-precision clock is available.
pub struct StandardWallClock {
    _start: Instant,
}

impl StandardWallClock {
    pub fn new() -> Self {
        Self {
            _start: Instant::now(),
        }
    }
}

impl Default for StandardWallClock {
    fn default() -> Self {
        Self::new()
    }
}

impl WallClock for StandardWallClock {
    fn get_time_ns(&self) -> Duration {
        Duration::from_nanos(self.get_uptime().max(0) as u64)
    }

    fn get_time_us(&self) -> Duration {
        let ns = self.get_time_ns();
        Duration::from_micros(ns.as_micros() as u64)
    }

    fn get_time_ms(&self) -> Duration {
        let ns = self.get_time_ns();
        Duration::from_millis(ns.as_millis() as u64)
    }

    fn get_cntpct(&self) -> i64 {
        // Upstream uses `std::ratio<CNTFRQ, std::nano::den>` which simplifies
        // the fraction at compile time (19.2M/1e9 -> 12/625), avoiding u64
        // overflow on large uptime values. Use u128 intermediate to match.
        let uptime = self.get_uptime() as u64;
        ((uptime as u128 * CNTFRQ as u128) / NS_PER_SEC as u128) as i64
    }

    fn get_gpu_tick(&self) -> i64 {
        let uptime = self.get_uptime() as u64;
        ((uptime as u128 * GPU_TICK_FREQ as u128) / NS_PER_SEC as u128) as i64
    }

    fn get_uptime(&self) -> i64 {
        // Match upstream: zuyu's StandardWallClock::GetUptime returns
        // `std::chrono::steady_clock::now().time_since_epoch().count()` in
        // nanoseconds. On Linux `steady_clock` is CLOCK_MONOTONIC, which
        // measures ns since an implementation-defined epoch (typically host
        // boot on Linux). Rust's `std::time::Instant` is CLOCK_MONOTONIC
        // under the hood but hides the absolute epoch behind a relative
        // `elapsed()` API. Reading `CLOCK_MONOTONIC` directly preserves the
        // upstream epoch so guest-visible CNTPCT (derived from `get_uptime`)
        // has matching magnitude to zuyu's.
        #[cfg(unix)]
        {
            let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
            (ts.tv_sec as i64) * 1_000_000_000 + (ts.tv_nsec as i64)
        }
        #[cfg(not(unix))]
        {
            self._start.elapsed().as_nanos() as i64
        }
    }

    fn is_native(&self) -> bool {
        false
    }
}

/// Creates the optimal clock for the current platform.
///
/// Mirrors upstream `Common::CreateOptimalClock`:
/// - On x86_64, use RDTSC-backed `NativeClock` iff the TSC is invariant and
///   runs above 1 GHz. Q0.64 cannot represent the exact ratio 1 at 1 GHz.
///   Otherwise fall back to
///   `StandardWallClock` (std::time-based).
/// - On aarch64, use CNTVCT_EL0-backed `NativeClock`.
/// - Otherwise, `StandardWallClock`.
pub fn create_optimal_clock() -> Box<dyn WallClock> {
    #[cfg(target_arch = "x86_64")]
    {
        let caps = crate::x64::cpu_detect::get_cpu_caps();
        return create_x64_clock(caps.invariant_tsc, caps.tsc_frequency);
    }
    #[cfg(target_arch = "aarch64")]
    {
        return Box::new(crate::arm64::native_clock::NativeClock::new());
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        Box::new(StandardWallClock::new())
    }
}

/// Mechanical extraction of Eden's g_wall_clock x64 selection for boundary tests.
#[cfg(target_arch = "x86_64")]
fn create_x64_clock(invariant: bool, frequency: u64) -> Box<dyn WallClock> {
    if invariant && frequency > NS_PER_SEC {
        Box::new(crate::x64::native_clock::NativeClock::new(frequency))
    } else {
        Box::new(StandardWallClock::new())
    }
}

/// Creates a standard wall clock (non-native).
pub fn create_standard_wall_clock() -> Box<dyn WallClock> {
    Box::new(StandardWallClock::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn native_selection_excludes_the_unrepresentable_one_ghz_ratio() {
        for frequency in [0, NS_PER_SEC - 1, NS_PER_SEC] {
            assert!(!create_x64_clock(true, frequency).is_native());
        }
        for frequency in [NS_PER_SEC + 1, 2_500_000_000, 4_000_000_000] {
            assert!(create_x64_clock(true, frequency).is_native());
            assert!(!create_x64_clock(false, frequency).is_native());
            assert_ne!(crate::uint128::get_fixed_point64_factor(NS_PER_SEC, frequency), 0);
        }
        assert_eq!(crate::uint128::get_fixed_point64_factor(NS_PER_SEC, NS_PER_SEC), 0);
    }

    #[test]
    fn test_standard_wall_clock() {
        let clock = StandardWallClock::new();
        assert!(!clock.is_native());

        let ns = clock.get_time_ns();
        assert!(ns.as_nanos() > 0);

        let uptime1 = clock.get_uptime();
        std::thread::sleep(Duration::from_millis(10));
        let uptime2 = clock.get_uptime();
        assert!(uptime2 > uptime1);
    }

    #[test]
    fn test_conversion_functions() {
        assert_eq!(ns_to_cntpct(1_000_000_000), CNTFRQ);
        assert_eq!(cpu_tick_to_ns(CPU_TICK_FREQ), NS_PER_SEC);
    }
}

//! Port of zuyu/src/common/arm64/native_clock.h and native_clock.cpp
//!
//! ARM64 native clock using CNTVCT_EL0 (virtual counter) for high-precision timing.

#[cfg(target_arch = "aarch64")]
use std::time::Duration;
#[cfg(target_arch = "aarch64")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(target_arch = "aarch64")]
use portable_atomic::AtomicU128;

#[cfg(target_arch = "aarch64")]
use crate::wall_clock::{WallClock, CNTFRQ, GPU_TICK_FREQ};

/// Ratio denominators matching C++ std::nano, std::micro, std::milli
#[cfg(target_arch = "aarch64")]
const NS_RATIO_DEN: u64 = 1_000_000_000;
#[cfg(target_arch = "aarch64")]
const US_RATIO_DEN: u64 = 1_000_000;
#[cfg(target_arch = "aarch64")]
const MS_RATIO_DEN: u64 = 1_000;

/// 128-bit factor type for ARM64 fixed-point math.
/// On ARM64, the counter frequency is typically much lower than x86 TSC,
/// so we need 128-bit precision for the fixed-point factors.
#[cfg(target_arch = "aarch64")]
type FactorType = u128;

/// Compute a 128-bit fixed-point factor: `(num << 64) / den`.
#[cfg(target_arch = "aarch64")]
#[inline]
fn get_fixed_point_factor(num: u64, den: u64) -> FactorType {
    ((num as u128) << 64) / (den as u128)
}

/// Multiply a u64 value by a 128-bit factor and return the high 64 bits.
#[cfg(target_arch = "aarch64")]
#[inline]
fn multiply_high_128(m: u64, factor: FactorType) -> u64 {
    (((m as u128) * factor) >> 64) as u64
}

/// ARM64 native clock using the CNTVCT_EL0 counter.
///
/// Uses 128-bit fixed-point multiplication to convert counter ticks to
/// various time units without expensive division at query time.
#[cfg(target_arch = "aarch64")]
pub struct NativeClock {
    ns_cntfrq_factor: FactorType,
    us_cntfrq_factor: FactorType,
    ms_cntfrq_factor: FactorType,
    guest_cntfrq_factor: AtomicU128,
    guest_cntfrq: AtomicU64,
    host_cntfrq: u64,
    gputick_cntfrq_factor: FactorType,
}

#[cfg(target_arch = "aarch64")]
impl NativeClock {
    /// Create a new NativeClock, reading the host CNTFRQ to compute conversion factors.
    pub fn new() -> Self {
        let host_cntfrq = (Self::get_host_cntfrq() as u64).max(1);
        Self {
            ns_cntfrq_factor: get_fixed_point_factor(NS_RATIO_DEN, host_cntfrq),
            us_cntfrq_factor: get_fixed_point_factor(US_RATIO_DEN, host_cntfrq),
            ms_cntfrq_factor: get_fixed_point_factor(MS_RATIO_DEN, host_cntfrq),
            guest_cntfrq_factor: AtomicU128::new(get_fixed_point_factor(CNTFRQ, host_cntfrq)),
            guest_cntfrq: AtomicU64::new(CNTFRQ),
            host_cntfrq,
            gputick_cntfrq_factor: get_fixed_point_factor(GPU_TICK_FREQ, host_cntfrq),
        }
    }

    /// Returns the guest CNTFRQ conversion factor (for external use).
    pub fn get_guest_cntfrq_factor(&self) -> u128 {
        self.guest_cntfrq_factor.load(Ordering::Relaxed)
    }

    /// Read the host counter frequency (CNTFRQ_EL0).
    ///
    /// On certain Android devices with known broken CNTFRQ_EL0 values,
    /// returns a hardcoded correct frequency for the specific board.
    pub fn get_host_cntfrq() -> i64 {
        let cntfrq_el0: u64;

        // On Android, some Exynos SoCs report incorrect CNTFRQ_EL0 values.
        // The C++ code checks ro.product.board system property. In Rust we
        // handle this via Android system property reading when on Android.
        #[cfg(target_os = "android")]
        {
            // Try to detect known problematic boards
            if let Some(freq) = get_android_board_frequency() {
                return freq as i64;
            }
        }

        // Read CNTFRQ_EL0 from the hardware.
        unsafe {
            std::arch::asm!(
                "mrs {cntfrq}, cntfrq_el0",
                cntfrq = out(reg) cntfrq_el0,
            );
        }

        cntfrq_el0 as i64
    }
}

/// On Android, check for known Exynos boards with incorrect CNTFRQ_EL0.
#[cfg(all(target_arch = "aarch64", target_os = "android"))]
fn get_android_board_frequency() -> Option<u64> {
    // Read Android system property ro.product.board
    // This would require the android-properties crate or JNI in a real implementation.
    // For now, we attempt to read from /sys/devices/system/cpu/cpu0/cpufreq or
    // use the __system_property_get NDK function.

    // Placeholder: In a full implementation, this would use
    // libc::__system_property_get to read "ro.product.board"
    let board = read_android_property("ro.product.board")?;

    match board.as_str() {
        "s5e9925" => Some(25_600_000),    // Exynos 2200
        "exynos2100" => Some(26_000_000), // Exynos 2100
        "exynos9810" => Some(26_000_000), // Exynos 9810
        "s5e8825" => Some(26_000_000),    // Exynos 1280
        _ => None,
    }
}

/// Read an Android system property by name.
#[cfg(all(target_arch = "aarch64", target_os = "android"))]
fn read_android_property(name: &str) -> Option<String> {
    use std::ffi::CString;

    let c_name = CString::new(name).ok()?;
    let mut buffer = [0u8; 92]; // PROP_VALUE_MAX

    // Safety: __system_property_get is a well-defined NDK function.
    let len = unsafe {
        // This function is available in the Android NDK
        extern "C" {
            fn __system_property_get(
                name: *const libc::c_char,
                value: *mut libc::c_char,
            ) -> libc::c_int;
        }
        __system_property_get(c_name.as_ptr(), buffer.as_mut_ptr() as *mut libc::c_char)
    };

    if len > 0 {
        Some(String::from_utf8_lossy(&buffer[..len as usize]).to_string())
    } else {
        None
    }
}

#[cfg(target_arch = "aarch64")]
impl Default for NativeClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_arch = "aarch64")]
impl WallClock for NativeClock {
    fn set_guest_cntfrq(&self, frequency: u64) {
        self.guest_cntfrq.store(frequency, Ordering::Relaxed);
        self.guest_cntfrq_factor.store(
            get_fixed_point_factor(frequency, self.host_cntfrq), Ordering::Relaxed);
    }

    fn get_guest_cntfrq(&self) -> u64 {
        self.guest_cntfrq.load(Ordering::Relaxed)
    }

    fn get_time_ns(&self) -> Duration {
        let ns = multiply_high_128(self.get_uptime() as u64, self.ns_cntfrq_factor);
        Duration::from_nanos(ns)
    }

    fn get_time_us(&self) -> Duration {
        let us = multiply_high_128(self.get_uptime() as u64, self.us_cntfrq_factor);
        Duration::from_micros(us)
    }

    fn get_time_ms(&self) -> Duration {
        let ms = multiply_high_128(self.get_uptime() as u64, self.ms_cntfrq_factor);
        Duration::from_millis(ms)
    }

    fn get_cntpct(&self) -> i64 {
        multiply_high_128(self.get_uptime() as u64, self.get_guest_cntfrq_factor()) as i64
    }

    fn get_gpu_tick(&self) -> i64 {
        multiply_high_128(self.get_uptime() as u64, self.gputick_cntfrq_factor) as i64
    }

    fn get_uptime(&self) -> i64 {
        let cntvct_el0: i64;
        // Read the virtual counter with data synchronization barriers
        // to ensure ordering, matching the upstream `dsb ish` fencing.
        unsafe {
            std::arch::asm!(
                "dsb ish",
                "mrs {cntvct}, cntvct_el0",
                "dsb ish",
                cntvct = out(reg) cntvct_el0,
            );
        }
        cntvct_el0
    }

    fn is_native(&self) -> bool {
        true
    }
}

#[cfg(all(test, target_arch = "aarch64"))]
mod tests {
    use super::*;

    #[test]
    fn test_native_clock_is_native() {
        let clock = NativeClock::new();
        assert!(clock.is_native());
    }

    #[test]
    fn test_native_clock_uptime_increases() {
        let clock = NativeClock::new();
        let t1 = clock.get_uptime();
        let t2 = clock.get_uptime();
        assert!(t2 >= t1);
    }

    #[test]
    fn test_get_host_cntfrq() {
        let freq = NativeClock::get_host_cntfrq();
        assert!(freq > 0, "Host CNTFRQ should be positive");
    }
}

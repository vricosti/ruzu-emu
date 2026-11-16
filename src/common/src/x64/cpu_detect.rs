//! Port of zuyu/src/common/x64/cpu_detect.h and cpu_detect.cpp
//!
//! CPU capability detection via CPUID for x86_64 platforms.

#[cfg(target_arch = "x86_64")]
use std::sync::OnceLock;

#[cfg(target_arch = "x86_64")]
use crate::x64::rdtsc;

/// CPU manufacturer enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Manufacturer {
    Unknown = 0,
    Intel = 1,
    Amd = 2,
    Hygon = 3,
}

impl Manufacturer {
    /// Parse manufacturer from CPUID brand string.
    pub fn parse(brand_string: &str) -> Manufacturer {
        match brand_string {
            "GenuineIntel" => Manufacturer::Intel,
            "AuthenticAMD" => Manufacturer::Amd,
            "HygonGenuine" => Manufacturer::Hygon,
            _ => Manufacturer::Unknown,
        }
    }
}

/// x86/x64 CPU capabilities that may be detected by this module.
#[derive(Debug, Clone)]
pub struct CpuCaps {
    pub manufacturer: Manufacturer,
    /// 12-byte manufacturer brand string (e.g. "GenuineIntel") + null terminator.
    pub brand_string: [u8; 13],
    /// Extended CPU model string (up to 48 bytes).
    pub cpu_string: [u8; 48],

    pub base_frequency: u32,
    pub max_frequency: u32,
    pub bus_frequency: u32,

    pub tsc_crystal_ratio_denominator: u32,
    pub tsc_crystal_ratio_numerator: u32,
    pub crystal_frequency: u32,
    /// TSC frequency derived from the above three values.
    pub tsc_frequency: u64,

    // SSE family
    pub sse: bool,
    pub sse2: bool,
    pub sse3: bool,
    pub ssse3: bool,
    pub sse4_1: bool,
    pub sse4_2: bool,

    // AVX family
    pub avx: bool,
    pub avx_vnni: bool,
    pub avx2: bool,
    pub avx512f: bool,
    pub avx512dq: bool,
    pub avx512cd: bool,
    pub avx512bw: bool,
    pub avx512vl: bool,
    pub avx512vbmi: bool,
    pub avx512bitalg: bool,

    // Miscellaneous
    pub aes: bool,
    pub bmi1: bool,
    pub bmi2: bool,
    pub f16c: bool,
    pub fma: bool,
    pub fma4: bool,
    pub gfni: bool,
    pub invariant_tsc: bool,
    pub lzcnt: bool,
    pub monitorx: bool,
    pub movbe: bool,
    pub pclmulqdq: bool,
    pub popcnt: bool,
    pub sha: bool,
    pub waitpkg: bool,
}

impl Default for CpuCaps {
    fn default() -> Self {
        Self {
            manufacturer: Manufacturer::Unknown,
            brand_string: [0u8; 13],
            cpu_string: [0u8; 48],
            base_frequency: 0,
            max_frequency: 0,
            bus_frequency: 0,
            tsc_crystal_ratio_denominator: 0,
            tsc_crystal_ratio_numerator: 0,
            crystal_frequency: 0,
            tsc_frequency: 0,
            sse: false,
            sse2: false,
            sse3: false,
            ssse3: false,
            sse4_1: false,
            sse4_2: false,
            avx: false,
            avx_vnni: false,
            avx2: false,
            avx512f: false,
            avx512dq: false,
            avx512cd: false,
            avx512bw: false,
            avx512vl: false,
            avx512vbmi: false,
            avx512bitalg: false,
            aes: false,
            bmi1: false,
            bmi2: false,
            f16c: false,
            fma: false,
            fma4: false,
            gfni: false,
            invariant_tsc: false,
            lzcnt: false,
            monitorx: false,
            movbe: false,
            pclmulqdq: false,
            popcnt: false,
            sha: false,
            waitpkg: false,
        }
    }
}

/// Helper: test if bit `n` is set in `value`.
#[inline]
fn bit(value: i32, n: u32) -> bool {
    (value >> n) & 1 != 0
}

/// Executes CPUID with `function_id` and `subfunction_id`, returns [eax, ebx, ecx, edx].
#[cfg(target_arch = "x86_64")]
#[inline]
fn cpuid_ex(function_id: u32, subfunction_id: u32) -> [i32; 4] {
    // __cpuid_count is safe on x86_64 (always supported).
    let result = unsafe { std::arch::x86_64::__cpuid_count(function_id, subfunction_id) };
    [
        result.eax as i32,
        result.ebx as i32,
        result.ecx as i32,
        result.edx as i32,
    ]
}

/// Executes CPUID with `function_id` (subfunction 0).
#[cfg(target_arch = "x86_64")]
#[inline]
fn cpuid(function_id: u32) -> [i32; 4] {
    cpuid_ex(function_id, 0)
}

/// Read XCR (Extended Control Register) value.
#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn xgetbv(index: u32) -> u64 {
    // Safety: caller must ensure XSAVE is supported.
    let eax: u32;
    let edx: u32;
    std::arch::asm!(
        "xgetbv",
        in("ecx") index,
        out("eax") eax,
        out("edx") edx,
    );
    ((edx as u64) << 32) | (eax as u64)
}

const XCR_XFEATURE_ENABLED_MASK: u32 = 0;

/// Detect CPU capabilities via CPUID.
#[cfg(target_arch = "x86_64")]
fn detect() -> CpuCaps {
    let mut caps = CpuCaps::default();

    // Detect CPU's CPUID capabilities and grab manufacturer string
    let cpu_id = cpuid(0x0000_0000);
    let max_std_fn = cpu_id[0] as u32; // EAX

    // Brand string is stored across EBX, EDX, ECX (in that order)
    caps.brand_string = [0u8; 13];
    caps.brand_string[0..4].copy_from_slice(&(cpu_id[1] as u32).to_le_bytes()); // EBX
    caps.brand_string[4..8].copy_from_slice(&(cpu_id[3] as u32).to_le_bytes()); // EDX
    caps.brand_string[8..12].copy_from_slice(&(cpu_id[2] as u32).to_le_bytes()); // ECX
    caps.brand_string[12] = 0;

    let brand_str = std::str::from_utf8(&caps.brand_string[..12])
        .unwrap_or("")
        .trim_end_matches('\0');
    caps.manufacturer = Manufacturer::parse(brand_str);

    // Set reasonable default cpu string even if brand string not available
    let copy_len = caps.brand_string.len().min(caps.cpu_string.len());
    caps.cpu_string[..copy_len].copy_from_slice(&caps.brand_string[..copy_len]);

    let cpu_id_ext = cpuid(0x8000_0000);
    let max_ex_fn = cpu_id_ext[0] as u32;

    // Detect family and other miscellaneous features
    if max_std_fn >= 1 {
        let cpu_id = cpuid(0x0000_0001);

        caps.sse = bit(cpu_id[3], 25);
        caps.sse2 = bit(cpu_id[3], 26);
        caps.sse3 = bit(cpu_id[2], 0);
        caps.pclmulqdq = bit(cpu_id[2], 1);
        caps.ssse3 = bit(cpu_id[2], 9);
        caps.sse4_1 = bit(cpu_id[2], 19);
        caps.sse4_2 = bit(cpu_id[2], 20);
        caps.movbe = bit(cpu_id[2], 22);
        caps.popcnt = bit(cpu_id[2], 23);
        caps.aes = bit(cpu_id[2], 25);
        caps.f16c = bit(cpu_id[2], 29);

        // AVX support requires 3 separate checks:
        //  - Is the AVX bit set in CPUID?
        //  - Is the XSAVE bit set in CPUID?
        //  - XGETBV result has the XCR bit set.
        if bit(cpu_id[2], 28) && bit(cpu_id[2], 27) {
            // Safety: XSAVE bit is set so xgetbv is available.
            if (unsafe { xgetbv(XCR_XFEATURE_ENABLED_MASK) } & 0x6) == 0x6 {
                caps.avx = true;
                if bit(cpu_id[2], 12) {
                    caps.fma = true;
                }
            }
        }

        if max_std_fn >= 7 {
            let cpu_id = cpuid_ex(0x0000_0007, 0x0000_0000);
            // Can't enable AVX{2,512} unless the XSAVE/XGETBV checks above passed
            if caps.avx {
                caps.avx2 = bit(cpu_id[1], 5);
                caps.avx512f = bit(cpu_id[1], 16);
                caps.avx512dq = bit(cpu_id[1], 17);
                caps.avx512cd = bit(cpu_id[1], 28);
                caps.avx512bw = bit(cpu_id[1], 30);
                caps.avx512vl = bit(cpu_id[1], 31);
                caps.avx512vbmi = bit(cpu_id[2], 1);
                caps.avx512bitalg = bit(cpu_id[2], 12);
            }

            caps.bmi1 = bit(cpu_id[1], 3);
            caps.bmi2 = bit(cpu_id[1], 8);
            caps.sha = bit(cpu_id[1], 29);

            caps.waitpkg = bit(cpu_id[2], 5);
            caps.gfni = bit(cpu_id[2], 8);

            let cpu_id = cpuid_ex(0x0000_0007, 0x0000_0001);
            caps.avx_vnni = caps.avx && bit(cpu_id[0], 4);
        }
    }

    if max_ex_fn >= 0x8000_0004 {
        // Extract CPU model string
        let cpu_id = cpuid(0x8000_0002);
        copy_cpuid_to_slice(&mut caps.cpu_string[0..16], &cpu_id);
        let cpu_id = cpuid(0x8000_0003);
        copy_cpuid_to_slice(&mut caps.cpu_string[16..32], &cpu_id);
        let cpu_id = cpuid(0x8000_0004);
        copy_cpuid_to_slice(&mut caps.cpu_string[32..48], &cpu_id);
    }

    if max_ex_fn >= 0x8000_0001 {
        // Check for more features
        let cpu_id = cpuid(0x8000_0001);
        caps.lzcnt = bit(cpu_id[2], 5);
        caps.fma4 = bit(cpu_id[2], 16);
        caps.monitorx = bit(cpu_id[2], 29);
    }

    if max_ex_fn >= 0x8000_0007 {
        let cpu_id = cpuid(0x8000_0007);
        caps.invariant_tsc = bit(cpu_id[3], 8);
    }

    if max_std_fn >= 0x15 {
        let cpu_id = cpuid(0x15);
        caps.tsc_crystal_ratio_denominator = cpu_id[0] as u32;
        caps.tsc_crystal_ratio_numerator = cpu_id[1] as u32;
        caps.crystal_frequency = cpu_id[2] as u32;
        // Some CPU models might not return a crystal frequency.
        // The CPU model can be detected to use the values from turbostat
        // but it's easier to just estimate the TSC tick rate for these cases.
        if caps.tsc_crystal_ratio_denominator != 0 {
            caps.tsc_frequency = (caps.crystal_frequency as u64)
                * (caps.tsc_crystal_ratio_numerator as u64)
                / (caps.tsc_crystal_ratio_denominator as u64);
        } else {
            caps.tsc_frequency = rdtsc::estimate_rdtsc_frequency();
        }
    }

    if max_std_fn >= 0x16 {
        let cpu_id = cpuid(0x16);
        caps.base_frequency = cpu_id[0] as u32;
        caps.max_frequency = cpu_id[1] as u32;
        caps.bus_frequency = cpu_id[2] as u32;
    }

    caps
}

/// Copy 4 CPUID registers (16 bytes) into a byte slice.
#[cfg(target_arch = "x86_64")]
fn copy_cpuid_to_slice(dest: &mut [u8], regs: &[i32; 4]) {
    for (i, &reg) in regs.iter().enumerate() {
        let bytes = (reg as u32).to_le_bytes();
        dest[i * 4..i * 4 + 4].copy_from_slice(&bytes);
    }
}

/// Gets the supported capabilities of the host CPU.
/// The result is cached after the first call.
#[cfg(target_arch = "x86_64")]
pub fn get_cpu_caps() -> &'static CpuCaps {
    static CAPS: OnceLock<CpuCaps> = OnceLock::new();
    CAPS.get_or_init(detect)
}

/// Detects CPU core count.
///
/// On Linux, reads SMT status to distinguish physical cores from logical threads.
/// Returns `None` if detection fails.
pub fn get_processor_count() -> Option<usize> {
    #[cfg(target_os = "linux")]
    {
        // Upstream `std::thread::hardware_concurrency()`: online processors, not affinity.
        let thread_count = Some(crate::thread::hardware_concurrency()).filter(|&n| n != 0)?;
        // Read SMT status from sysfs
        let smt_state = std::fs::read_to_string("/sys/devices/system/cpu/smt/active").ok();
        match smt_state.as_deref().map(|s| s.trim()) {
            Some("0") => Some(thread_count),
            Some("1") => Some(thread_count / 2),
            _ => None,
        }
    }

    #[cfg(target_os = "windows")]
    {
        // On Windows, use available_parallelism as a reasonable fallback.
        // The C++ code uses GetLogicalProcessorInformation to count physical cores;
        // the Rust standard library doesn't expose that directly.
        std::thread::available_parallelism().map(|n| n.get()).ok()
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

/// Get the CPU model string as a Rust string.
#[cfg(target_arch = "x86_64")]
pub fn get_cpu_string() -> String {
    let caps = get_cpu_caps();
    let end = caps
        .cpu_string
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(caps.cpu_string.len());
    String::from_utf8_lossy(&caps.cpu_string[..end])
        .trim()
        .to_string()
}

/// Get the manufacturer brand string as a Rust string.
#[cfg(target_arch = "x86_64")]
pub fn get_brand_string() -> String {
    let caps = get_cpu_caps();
    let end = caps
        .brand_string
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(caps.brand_string.len());
    String::from_utf8_lossy(&caps.brand_string[..end])
        .trim()
        .to_string()
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;

    #[test]
    fn test_detect_cpu_caps() {
        let caps = get_cpu_caps();
        // On any x86_64 system running this test, SSE2 should be present.
        assert!(caps.sse2, "SSE2 should be supported on x86_64");
        assert_ne!(
            caps.manufacturer,
            Manufacturer::Unknown,
            "Manufacturer should be detected"
        );
    }

    #[test]
    fn test_manufacturer_parse() {
        assert_eq!(Manufacturer::parse("GenuineIntel"), Manufacturer::Intel);
        assert_eq!(Manufacturer::parse("AuthenticAMD"), Manufacturer::Amd);
        assert_eq!(Manufacturer::parse("HygonGenuine"), Manufacturer::Hygon);
        assert_eq!(Manufacturer::parse("SomethingElse"), Manufacturer::Unknown);
    }

    #[test]
    fn test_get_cpu_string() {
        let s = get_cpu_string();
        assert!(!s.is_empty(), "CPU string should not be empty");
    }
}

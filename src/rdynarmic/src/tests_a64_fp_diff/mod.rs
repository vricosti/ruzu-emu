//! Differential tests for floating-point IR opcodes whose x64 emitters were
//! ported from Eden's Dynarmic (`backend/x64/emit_x64_floating_point.cpp`,
//! `emit_x64_vector_floating_point.cpp`) in place of rdynarmic's former
//! host-function fallbacks.
//!
//! Each test executes one A64 instruction through the full JIT (frontend,
//! optimisation passes, x64 backend) and compares the destination register
//! and the cumulative FPSR bits with an oracle. The oracle is the former
//! fallback, kept verbatim in this file, executed under the same MXCSR the
//! guest code runs under (as the fallback was in production), plus the ARM
//! exception model where the fallback did not report exceptions.
//!
//! Every test runs the targeted special values and a pseudo-random stream
//! (`RDYNARMIC_FP_DIFF_ITERS` vectors per host-feature set, default 20 000;
//! the delivered validation used 1 000 000) under all 16 combinations of
//! FPCR.{DN, FZ, RMode}, once with the host's features and once with the
//! AVX family masked out so the SSE paths execute too.

#![cfg(target_arch = "x86_64")]

mod fp_from_fixed;
mod fp_min_max;
mod fp_vector_compare;
mod fp_vector_min_max;
mod fp_vector_three_op;

use crate::backend::x64::block_of_code::test_host_feature_mask;
use crate::backend::x64::host_feature::HostFeature;
use crate::interface::a64::config::{
    Exception as A64Exception, UserCallbacks as A64UserCallbacks, UserConfig as A64UserConfig,
    Vector,
};
use crate::interface::halt_reason::HaltReason;
use crate::interface::optimization_flags::OptimizationFlag;
use crate::A64Jit;

const CODE_BASE: u64 = 0x1000;

// ---------------------------------------------------------------------
// Execution harness
// ---------------------------------------------------------------------

struct Env {
    code: Vec<u32>,
    halt_reason: Option<usize>,
}

impl A64UserCallbacks for Env {
    fn memory_read_code(&self, vaddr: u64) -> Option<u32> {
        let index = vaddr.wrapping_sub(CODE_BASE) / 4;
        Some(*self.code.get(index as usize).unwrap_or(&0xD400_0001))
    }
    fn memory_read_8(&self, _vaddr: u64) -> u8 {
        0
    }
    fn memory_read_16(&self, _vaddr: u64) -> u16 {
        0
    }
    fn memory_read_32(&self, vaddr: u64) -> u32 {
        self.memory_read_code(vaddr).unwrap_or(0)
    }
    fn memory_read_64(&self, _vaddr: u64) -> u64 {
        0
    }
    fn memory_read_128(&self, _vaddr: u64) -> Vector {
        [0, 0]
    }
    fn memory_write_8(&mut self, _vaddr: u64, _value: u8) {}
    fn memory_write_16(&mut self, _vaddr: u64, _value: u16) {}
    fn memory_write_32(&mut self, _vaddr: u64, _value: u32) {}
    fn memory_write_64(&mut self, _vaddr: u64, _value: u64) {}
    fn memory_write_128(&mut self, _vaddr: u64, _value: Vector) {}
    fn call_svc(&mut self, _swi: u32) {
        let ptr = self.halt_reason.expect("halt reason pointer registered");
        unsafe {
            (*(ptr as *const std::sync::atomic::AtomicU32))
                .fetch_or(HaltReason::SVC.bits(), std::sync::atomic::Ordering::SeqCst);
        }
    }
    fn set_halt_reason_ptr(&mut self, ptr: *const u32) {
        self.halt_reason = Some(ptr as usize);
    }
    fn exception_raised(&mut self, pc: u64, exception: A64Exception) {
        panic!("unexpected exception {exception:?} at 0x{pc:x}");
    }
    fn add_ticks(&mut self, _ticks: u64) {}
    fn get_ticks_remaining(&self) -> u64 {
        1000
    }
    fn get_cntpct(&self) -> u64 {
        0
    }
}

pub(super) struct Harness {
    jit: A64Jit,
}

impl Harness {
    /// `code` is executed from `CODE_BASE` and must end with `SVC #0`.
    /// The JIT is built with `features` as the detected host features
    /// and with the optimisation flags ruzu uses for CPU accuracy "Auto"
    /// (Eden `arm_dynarmic_64.cpp`): safe optimisations + Unsafe_UnfuseFMA
    /// + Unsafe_IgnoreGlobalMonitor.
    pub(super) fn new(code: &[u32], features: HostFeature) -> Self {
        let mut config = A64UserConfig::new(Box::new(Env {
            code: code.to_vec(),
            halt_reason: None,
        }));
        config.enable_cycle_counting = false;
        config.code_cache_size = 64 * 1024 * 1024;
        config.unsafe_optimizations = true;
        config.optimizations = OptimizationFlag::ALL_SAFE_OPTIMIZATIONS
            | OptimizationFlag::UNSAFE_UNFUSE_FMA
            | OptimizationFlag::UNSAFE_IGNORE_GLOBAL_MONITOR;
        test_host_feature_mask::set(features);
        let jit = A64Jit::new(config).unwrap();
        test_host_feature_mask::set(HostFeature::all());
        Self { jit }
    }

    /// Runs the code with V1/V2/V3 = `inputs` and returns (V0, FPSR).
    pub(super) fn run(&mut self, fpcr: u32, v0: Vector, inputs: &[Vector]) -> (Vector, u32) {
        self.jit.set_pc(CODE_BASE);
        self.jit.set_fpcr(fpcr);
        self.jit.set_fpsr(0);
        self.jit.set_vector_parts(0, v0[0], v0[1]);
        for (i, value) in inputs.iter().enumerate() {
            self.jit.set_vector_parts(i + 1, value[0], value[1]);
        }
        let halt = self.jit.run();
        assert!(halt.contains(HaltReason::SVC), "halt={halt:?}");
        let (lo, hi) = self.jit.get_vector_parts(0);
        ([lo, hi], self.jit.get_fpsr())
    }
}

// ---------------------------------------------------------------------
// Oracle support: run the legacy fallback under the guest MXCSR
// ---------------------------------------------------------------------

/// Guest MXCSR for an FPCR value, as `A64JitState::set_fpcr` computes it.
pub(super) fn guest_mxcsr(fpcr: u32) -> u32 {
    const RMODE: [u32; 4] = [0x0000, 0x4000, 0x2000, 0x6000];
    let mut mxcsr = 0x1f80 | RMODE[((fpcr >> 22) & 3) as usize];
    if fpcr & (1 << 24) != 0 {
        mxcsr |= 0x8000 | 0x0040;
    }
    mxcsr
}

/// FPSR cumulative bits produced by MXCSR exception flags, as
/// `A64JitState::get_fpsr` maps them.
pub(super) fn mxcsr_flags_to_fpsr(mxcsr: u32) -> u32 {
    (mxcsr & 0b1) | ((mxcsr & 0b11_1100) >> 1)
}

/// Runs `f` with MXCSR = guest MXCSR of `fpcr` (exception flags clear)
/// and returns its result and the FPSR bits its host arithmetic raised.
#[allow(deprecated)]
pub(super) fn under_guest_mxcsr<R>(fpcr: u32, f: impl FnOnce() -> R) -> (R, u32) {
    use std::arch::x86_64::{_mm_getcsr, _mm_setcsr};
    unsafe {
        let saved = _mm_getcsr();
        _mm_setcsr(guest_mxcsr(fpcr));
        let result = std::hint::black_box(f());
        let after = _mm_getcsr();
        _mm_setcsr(saved);
        (result, mxcsr_flags_to_fpsr(after))
    }
}

// ---------------------------------------------------------------------
// Input generation
// ---------------------------------------------------------------------

pub(super) const SPECIAL32: &[u32] = &[
    0x0000_0000,
    0x8000_0000, // +-0
    0x0000_0001,
    0x8000_0001, // smallest denormals
    0x007f_ffff,
    0x807f_ffff, // largest denormals
    0x0040_0000,
    0x8020_0000, // mid denormals
    0x0080_0000,
    0x8080_0000, // smallest normals
    0x0080_0001,
    0x3f80_0000,
    0xbf80_0000, // 1, -1
    0x3fc0_0000,
    0xbfc0_0000, // 1.5
    0x3f00_0000,
    0x3f40_0000,
    0x4020_0000,
    0xc020_0000, // 0.5, 0.75, 2.5
    0x4b00_0000,
    0x4b00_0001,
    0x4b7f_ffff,
    0xcb00_0001, // 2^23 boundary
    0x4f00_0000,
    0xcf00_0000,
    0x5f00_0000, // 2^31, -2^31, 2^63
    0x7f7f_ffff,
    0xff7f_ffff, // max normals
    0x7f80_0000,
    0xff80_0000, // infinities
    0x7fc0_0000,
    0xffc0_0000,
    0x7fc1_2345,
    0xffff_ffff,
    0x7fff_ffff, // qNaNs
    0x7f80_0001,
    0xff80_0001,
    0x7fa0_0000,
    0x7f81_2345,
    0xffbf_ffff, // sNaNs
];

pub(super) const SPECIAL64: &[u64] = &[
    0x0000_0000_0000_0000,
    0x8000_0000_0000_0000,
    0x0000_0000_0000_0001,
    0x8000_0000_0000_0001,
    0x000f_ffff_ffff_ffff,
    0x800f_ffff_ffff_ffff,
    0x0010_0000_0000_0000,
    0x8010_0000_0000_0000,
    0x3ff0_0000_0000_0000,
    0xbff0_0000_0000_0000,
    0x3ff8_0000_0000_0000,
    0xbff8_0000_0000_0000,
    0x3fe0_0000_0000_0000,
    0x4004_0000_0000_0000,
    0x4330_0000_0000_0000,
    0x4330_0000_0000_0001,
    0xc330_0000_0000_0001,
    0x43e0_0000_0000_0000,
    0x7fef_ffff_ffff_ffff,
    0xffef_ffff_ffff_ffff,
    0x7ff0_0000_0000_0000,
    0xfff0_0000_0000_0000,
    0x7ff8_0000_0000_0000,
    0xfff8_0000_0000_0000,
    0x7ff8_0000_1234_5678,
    0xffff_ffff_ffff_ffff,
    0x7ff0_0000_0000_0001,
    0xfff0_0000_0000_0001,
    0x7ff4_0000_0000_0000,
    0x7ff0_dead_beef_0000,
];

pub(super) struct Rng(u64);

impl Rng {
    pub(super) fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    pub(super) fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    /// A 32-bit lane: a special value, a value near one, or random bits.
    pub(super) fn lane32(&mut self) -> u32 {
        let r = self.next();
        match r % 8 {
            0 | 1 => SPECIAL32[(r >> 8) as usize % SPECIAL32.len()],
            2 => {
                // small exponent range: frequent equal/close values
                let exp = 120 + ((r >> 8) % 16) as u32;
                ((r >> 32) as u32 & 0x8000_0000) | (exp << 23) | ((r >> 40) as u32 & 0x7)
            }
            3 => (r >> 16) as u32 & 0x807f_ffff, // denormals
            _ => (r >> 16) as u32,
        }
    }
    pub(super) fn lane64(&mut self) -> u64 {
        let r = self.next();
        match r % 8 {
            0 | 1 => SPECIAL64[(r >> 8) as usize % SPECIAL64.len()],
            2 => {
                let exp = 1016 + (r >> 8) % 16;
                (self.next() & 0x8000_0000_0000_0000) | (exp << 52) | ((r >> 40) & 0x7)
            }
            3 => self.next() & 0x800f_ffff_ffff_ffff,
            _ => self.next(),
        }
    }
    pub(super) fn vector(&mut self, fsize: usize) -> Vector {
        match fsize {
            32 => [
                self.lane32() as u64 | (self.lane32() as u64) << 32,
                self.lane32() as u64 | (self.lane32() as u64) << 32,
            ],
            64 => [self.lane64(), self.lane64()],
            _ => unreachable!(),
        }
    }
}

/// FPCR values covering DN x FZ x RMode.
pub(super) fn all_fpcrs() -> Vec<u32> {
    let mut out = Vec::new();
    for dn in 0..2u32 {
        for fz in 0..2u32 {
            for rmode in 0..4u32 {
                out.push((dn << 25) | (fz << 24) | (rmode << 22));
            }
        }
    }
    out
}

pub(super) fn iterations() -> usize {
    std::env::var("RDYNARMIC_FP_DIFF_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000)
}

/// Host feature sets under which each test is run: the detected host,
/// and the same host with AVX and later masked out (SSE4.2 paths).
pub(super) fn feature_sets() -> Vec<(&'static str, HostFeature)> {
    let avx_family = HostFeature::AVX
        | HostFeature::AVX2
        | HostFeature::AVX512F
        | HostFeature::AVX512CD
        | HostFeature::AVX512VL
        | HostFeature::AVX512BW
        | HostFeature::AVX512DQ
        | HostFeature::AVX512BITALG
        | HostFeature::AVX512VBMI
        | HostFeature::FMA
        | HostFeature::F16C
        | HostFeature::GFNI;
    vec![
        ("host", HostFeature::all()),
        ("sse", HostFeature::all() - avx_family),
    ]
}

/// FPSR bits compared: IOC, DZC, OFC, UFC, IXC (IDC is not produced by
/// the upstream x64 emitters, which do not map MXCSR.DE).
pub(super) const FPSR_COMPARED: u32 = 0b1_1111;

pub(super) type Oracle<'a> = &'a dyn Fn(u32, &[Vector]) -> (Vector, u32);

/// Runs `insn; svc #0` over the special-value cross product (lane-wise
/// pairing) and the random stream, comparing with `oracle`.
pub(super) fn diff_test(
    name: &str,
    insn: u32,
    fsize: usize,
    nargs: usize,
    compare_fpsr: bool,
    oracle: Oracle<'_>,
) {
    let iters = iterations();
    let code = [insn, 0xD400_0001];
    let mut checked = 0usize;
    for (features_name, features) in feature_sets() {
        let mut harness = Harness::new(&code, features);
        let mut rng = Rng::new(0x9e37_79b9_7f4a_7c15 ^ insn as u64);
        let check = |fpcr: u32, inputs: &[Vector], harness: &mut Harness| {
            let v0 = [0xdead_beef_dead_beef, 0x0123_4567_89ab_cdef];
            let (got, got_fpsr) = harness.run(fpcr, v0, inputs);
            let (want, want_fpsr) = oracle(fpcr, inputs);
            let fpsr_ok =
                !compare_fpsr || (got_fpsr & FPSR_COMPARED) == (want_fpsr & FPSR_COMPARED);
            assert!(
                got == want && fpsr_ok,
                "{name} [{features_name}] fpcr={fpcr:#010x} inputs={:x?}: got {:x?} fpsr={got_fpsr:#x}, want {:x?} fpsr={want_fpsr:#x}",
                inputs,
                got,
                want
            );
        };

        // Special values: every ordered pair placed in every lane.
        let specials: Vec<u64> = match fsize {
            32 => SPECIAL32.iter().map(|&v| v as u64).collect(),
            64 => SPECIAL64.to_vec(),
            _ => unreachable!(),
        };
        let lanes = if name.starts_with("FPVector") {
            128 / fsize
        } else {
            1
        };
        let splat = |v: u64, i: usize| -> Vector {
            // value v in lane i, a rotating neighbour elsewhere
            let mut out = [0u64; 2];
            for lane in 0..lanes {
                let value = if lane == i {
                    v
                } else {
                    specials[(lane * 7 + i) % specials.len()]
                };
                match fsize {
                    32 => out[lane / 2] |= value << (32 * (lane % 2)),
                    _ => out[lane] = value,
                }
            }
            out
        };
        for &fpcr in &all_fpcrs() {
            for (ia, &a) in specials.iter().enumerate() {
                for &b in specials.iter() {
                    let lane = ia % lanes;
                    let mut inputs = vec![splat(a, lane), splat(b, lane)];
                    if nargs == 3 {
                        inputs.push(splat(b ^ a, lane));
                    }
                    inputs.truncate(nargs);
                    check(fpcr, &inputs, &mut harness);
                    checked += 1;
                }
            }
        }

        // Random stream.
        let fpcrs = all_fpcrs();
        for i in 0..iters {
            let fpcr = fpcrs[i % fpcrs.len()];
            let inputs: Vec<Vector> = (0..nargs).map(|_| rng.vector(fsize)).collect();
            check(fpcr, &inputs, &mut harness);
            checked += 1;
        }
    }
    eprintln!("{name}: {checked} vectors checked");
}

// ---------------------------------------------------------------------
// ARM exception model helpers
// ---------------------------------------------------------------------

pub(super) fn lanes(v: &Vector, fsize: usize) -> Vec<u64> {
    match fsize {
        32 => vec![
            v[0] & 0xffff_ffff,
            v[0] >> 32,
            v[1] & 0xffff_ffff,
            v[1] >> 32,
        ],
        64 => vec![v[0], v[1]],
        _ => unreachable!(),
    }
}

pub(super) fn is_nan(bits: u64, fsize: usize) -> bool {
    match fsize {
        32 => bits & 0x7fff_ffff > 0x7f80_0000,
        _ => bits & 0x7fff_ffff_ffff_ffff > 0x7ff0_0000_0000_0000,
    }
}

pub(super) fn is_snan(bits: u64, fsize: usize) -> bool {
    is_nan(bits, fsize)
        && match fsize {
            32 => bits & 0x0040_0000 == 0,
            _ => bits & 0x0008_0000_0000_0000 == 0,
        }
}

pub(super) fn vec_to_bytes(v: &Vector) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&v[0].to_le_bytes());
    out[8..].copy_from_slice(&v[1].to_le_bytes());
    out
}

pub(super) fn bytes_to_vec(b: &[u8; 16]) -> Vector {
    [
        u64::from_le_bytes(b[..8].try_into().unwrap()),
        u64::from_le_bytes(b[8..].try_into().unwrap()),
    ]
}

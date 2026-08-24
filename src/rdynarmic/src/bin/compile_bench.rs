//! Deterministic JIT compile microbenchmark for rdynarmic.
//!
//! Compiles a fixed set of ARM32 code patterns repeatedly (clearing the block
//! cache between iterations to force re-compilation) and reports mean / median
//! optimization deltas <10% become measurable.
//!
//! Run with:
//!   cargo run --release --bin compile_bench
//!   cargo run --release --bin compile_bench -- --iters 2000 --warmup 200
//!
//! Output format is stable and machine-readable so we can diff before/after
//! optimization attempts.

use std::env;
use std::time::Instant;

use rdynarmic::interface::a32::config::{
    Exception as A32Exception, UserCallbacks as A32UserCallbacks, UserConfig as A32UserConfig,
};
use rdynarmic::interface::optimization_flags::OptimizationFlag;
use rdynarmic::jit::A32Jit;

const BASE_PC: u32 = 0x0001_0000;
const SP_INIT: u32 = 0x0010_0000;

// ---------------------------------------------------------------------------
// Minimal UserCallbacks for the compile path
// ---------------------------------------------------------------------------
//
// During get_or_compile_block, the JIT calls memory_read_code and
// is_read_only_memory. None of the runtime callbacks (memory_read_*,
// memory_write_*, exclusive_*, call_supervisor) are invoked because we never
// execute the compiled code.

struct BenchEnv {
    code: Vec<u32>,
    base: u32,
    ticks_left: u64,
}

impl BenchEnv {
    fn new(code: Vec<u32>, base: u32) -> Self {
        Self {
            code,
            base,
            ticks_left: 1_000_000,
        }
    }
}

impl A32UserCallbacks for BenchEnv {
    fn memory_read_code(&self, v: u32) -> Option<u32> {
        if v < self.base {
            return Some(0xEAFF_FFFE); // b .
        }
        let off = ((v - self.base) >> 2) as usize;
        if off < self.code.len() {
            Some(self.code[off])
        } else {
            Some(0xEAFF_FFFE)
        }
    }
    fn memory_read_8(&self, _: u32) -> u8 {
        0
    }
    fn memory_read_16(&self, _: u32) -> u16 {
        0
    }
    fn memory_read_32(&self, _: u32) -> u32 {
        0
    }
    fn memory_read_64(&self, _: u32) -> u64 {
        0
    }
    fn memory_write_8(&mut self, _: u32, _: u8) {}
    fn memory_write_16(&mut self, _: u32, _: u16) {}
    fn memory_write_32(&mut self, _: u32, _: u32) {}
    fn memory_write_64(&mut self, _: u32, _: u64) {}
    fn memory_write_exclusive_8(&mut self, _: u32, _: u8, _: u8) -> bool {
        true
    }
    fn memory_write_exclusive_16(&mut self, _: u32, _: u16, _: u16) -> bool {
        true
    }
    fn memory_write_exclusive_32(&mut self, _: u32, _: u32, _: u32) -> bool {
        true
    }
    fn memory_write_exclusive_64(&mut self, _: u32, _: u64, _: u64) -> bool {
        true
    }
    fn call_svc(&mut self, _: u32) {}
    fn exception_raised(&mut self, _: u32, _: A32Exception) {}
    fn add_ticks(&mut self, ticks: u64) {
        self.ticks_left = self.ticks_left.saturating_sub(ticks);
    }
    fn get_ticks_remaining(&self) -> u64 {
        self.ticks_left
    }
}

// ---------------------------------------------------------------------------
// ARM32 instruction encodings used by patterns
// ---------------------------------------------------------------------------

const BX_LR: u32 = 0xE12F_FF1E; // bx lr — block terminator

#[inline]
const fn arm_add_imm(rd: u32, rn: u32, imm: u32) -> u32 {
    // ADD Rd, Rn, #imm  — cond=AL, I=1, opcode=0100
    0xE280_0000 | (rn << 16) | (rd << 12) | (imm & 0xFF)
}

#[inline]
const fn arm_sub_imm(rd: u32, rn: u32, imm: u32) -> u32 {
    // SUB Rd, Rn, #imm
    0xE240_0000 | (rn << 16) | (rd << 12) | (imm & 0xFF)
}

#[inline]
const fn arm_add_reg(rd: u32, rn: u32, rm: u32) -> u32 {
    // ADD Rd, Rn, Rm
    0xE080_0000 | (rn << 16) | (rd << 12) | rm
}

#[inline]
const fn arm_sub_reg(rd: u32, rn: u32, rm: u32) -> u32 {
    0xE040_0000 | (rn << 16) | (rd << 12) | rm
}

#[inline]
const fn arm_mov_reg(rd: u32, rm: u32) -> u32 {
    // MOV Rd, Rm
    0xE1A0_0000 | (rd << 12) | rm
}

#[inline]
const fn arm_ldr_imm(rt: u32, rn: u32, imm: u32) -> u32 {
    // LDR Rt, [Rn, #imm]  — pre-indexed, U=1
    0xE590_0000 | (rn << 16) | (rt << 12) | (imm & 0xFFF)
}

#[inline]
const fn arm_str_imm(rt: u32, rn: u32, imm: u32) -> u32 {
    // STR Rt, [Rn, #imm]
    0xE580_0000 | (rn << 16) | (rt << 12) | (imm & 0xFFF)
}

// ---------------------------------------------------------------------------
// Pattern builders
// ---------------------------------------------------------------------------

/// micro: 4 instructions, mostly emit overhead measurement.
fn pat_micro() -> Vec<u32> {
    vec![
        arm_add_imm(0, 0, 1),
        arm_sub_imm(1, 1, 1),
        arm_mov_reg(2, 3),
        BX_LR,
    ]
}

/// arith_block: 16 ALU operations + terminator. Stresses Add32/Sub32 hot path.
fn pat_arith_block() -> Vec<u32> {
    let mut v = Vec::with_capacity(17);
    for i in 0..8 {
        v.push(arm_add_reg(0, 0, (1 + (i & 3)) as u32));
        v.push(arm_sub_reg(0, 0, (1 + ((i + 1) & 3)) as u32));
    }
    v.push(BX_LR);
    v
}

/// mem_load: 8 LDRs from R12 + 8 ADDs. Stresses A32ReadMemory32.
fn pat_mem_load() -> Vec<u32> {
    let mut v = Vec::with_capacity(17);
    for i in 0..8 {
        v.push(arm_ldr_imm(i as u32 & 7, 12, (i * 4) as u32));
        v.push(arm_add_imm(0, 0, 1));
    }
    v.push(BX_LR);
    v
}

/// mem_store: 8 STRs to R12 + 8 SUBs. Stresses A32WriteMemory32.
fn pat_mem_store() -> Vec<u32> {
    let mut v = Vec::with_capacity(17);
    for i in 0..8 {
        v.push(arm_str_imm(i as u32 & 7, 12, (i * 4) as u32));
        v.push(arm_sub_imm(0, 0, 1));
    }
    v.push(BX_LR);
    v
}

/// mixed: 8 LDR + 8 STR + 8 ADD + 8 SUB. Realistic mix of memory + ALU.
fn pat_mixed() -> Vec<u32> {
    let mut v = Vec::with_capacity(33);
    for i in 0..8 {
        v.push(arm_ldr_imm(i as u32 & 7, 12, (i * 4) as u32));
    }
    for i in 0..8 {
        v.push(arm_str_imm(i as u32 & 7, 12, ((i + 8) * 4) as u32));
    }
    for i in 0..8 {
        v.push(arm_add_reg(0, 0, (1 + (i & 3)) as u32));
    }
    for i in 0..8 {
        v.push(arm_sub_reg(0, 0, (1 + (i & 3)) as u32));
    }
    v.push(BX_LR);
    v
}

/// large: 256 ALU instructions. Tests emit cost on a big block.
fn pat_large(n: usize) -> Vec<u32> {
    let mut v = Vec::with_capacity(n + 1);
    for i in 0..n {
        v.push(arm_add_reg(0, 0, (1 + (i & 3)) as u32));
    }
    v.push(BX_LR);
    v
}

// ---------------------------------------------------------------------------
// JIT setup
// ---------------------------------------------------------------------------

fn make_jit(code: Vec<u32>, optimizations: OptimizationFlag) -> A32Jit {
    let env = BenchEnv::new(code, BASE_PC);
    let mut config = A32UserConfig::new(Box::new(env));
    config.enable_cycle_counting = false;
    config.code_cache_size = 32 * 1024 * 1024;
    config.optimizations = optimizations;
    let mut jit = A32Jit::new(config).expect("A32Jit::new failed");
    for i in 0..15 {
        jit.set_register(i, 0);
    }
    jit.set_register(13, SP_INIT);
    jit.set_pc(BASE_PC);
    jit.set_cpsr(0x0000_0010); // USR mode, ARM state
    jit
}

// ---------------------------------------------------------------------------
// Statistics
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Stats {
    samples: Vec<u64>,
}

impl Stats {
    fn percentile(&mut self, p: f64) -> u64 {
        if self.samples.is_empty() {
            return 0;
        }
        self.samples.sort_unstable();
        let idx = ((self.samples.len() as f64 - 1.0) * p).round() as usize;
        self.samples[idx.min(self.samples.len() - 1)]
    }
    fn mean(&self) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        self.samples.iter().sum::<u64>() as f64 / self.samples.len() as f64
    }
    fn min(&self) -> u64 {
        *self.samples.iter().min().unwrap_or(&0)
    }
    fn max(&self) -> u64 {
        *self.samples.iter().max().unwrap_or(&0)
    }
}

fn report(name: &str, insts: usize, mut stats: Stats) {
    let n = stats.samples.len();
    let mean = stats.mean();
    let min = stats.min();
    let max = stats.max();
    let p50 = stats.percentile(0.50);
    let p95 = stats.percentile(0.95);
    let p99 = stats.percentile(0.99);
    println!(
        "[BENCH] pattern={:<14} insts={:>4} iters={:>5} \
         min={:>6.2}us p50={:>6.2}us mean={:>6.2}us \
         p95={:>6.2}us p99={:>6.2}us max={:>6.2}us per_inst={:>5.0}ns",
        name,
        insts,
        n,
        min as f64 / 1000.0,
        p50 as f64 / 1000.0,
        mean / 1000.0,
        p95 as f64 / 1000.0,
        p99 as f64 / 1000.0,
        max as f64 / 1000.0,
        mean / insts as f64
    );
}

// ---------------------------------------------------------------------------
// Bench runner
// ---------------------------------------------------------------------------

fn run_pattern(name: &str, code: Vec<u32>, iters: usize, warmup: usize, opts: OptimizationFlag) {
    let insts = code.len();
    let mut jit = make_jit(code, opts);

    // Warmup: compile + clear N times so allocator / page cache settle.
    for _ in 0..warmup {
        let _ = jit.compile_block_only();
        jit.clear_cache();
    }

    let mut stats = Stats {
        samples: Vec::with_capacity(iters),
    };

    for _ in 0..iters {
        jit.clear_cache();
        let t0 = Instant::now();
        let _ = jit.compile_block_only();
        let ns = t0.elapsed().as_nanos() as u64;
        stats.samples.push(ns);
    }

    report(name, insts, stats);
}

fn parse_args() -> (usize, usize, String) {
    let mut iters = 1000;
    let mut warmup = 100;
    let mut filter = String::new();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--iters" => {
                iters = args.next().and_then(|s| s.parse().ok()).unwrap_or(1000);
            }
            "--warmup" => {
                warmup = args.next().and_then(|s| s.parse().ok()).unwrap_or(100);
            }
            "--filter" => {
                filter = args.next().unwrap_or_default();
            }
            "--help" | "-h" => {
                eprintln!("Usage: compile_bench [--iters N] [--warmup N] [--filter SUBSTR]");
                std::process::exit(0);
            }
            other => {
                eprintln!("Unknown arg: {other}");
                std::process::exit(1);
            }
        }
    }
    (iters, warmup, filter)
}

fn main() {
    // Avoid stealing CPU from the JIT loop with profiling output mid-run.
    let (iters, warmup, filter) = parse_args();
    let opts = OptimizationFlag::ALL_SAFE_OPTIMIZATIONS;

    println!(
        "[BENCH] rdynarmic compile microbenchmark — iters={} warmup={} opts={:#x}",
        iters,
        warmup,
        opts.bits()
    );
    println!(
        "[BENCH] each iter: clear_cache() then compile_block_only() at PC=0x{:08X}",
        BASE_PC
    );

    let patterns: Vec<(&str, Vec<u32>)> = vec![
        ("micro", pat_micro()),
        ("arith_block", pat_arith_block()),
        ("mem_load", pat_mem_load()),
        ("mem_store", pat_mem_store()),
        ("mixed", pat_mixed()),
        ("large_64", pat_large(64)),
        ("large_256", pat_large(256)),
    ];

    for (name, code) in patterns {
        if !filter.is_empty() && !name.contains(&filter) {
            continue;
        }
        run_pattern(name, code, iters, warmup, opts);
    }

    // If RDYNARMIC_PROFILE_OPCODES is set AND the `profile_opcodes` Cargo
    // feature is enabled at build time, dump the per-opcode breakdown.
    // Default release builds don't carry the profiling infrastructure at
    // all — rebuild with `--features profile_opcodes` to use this.
    #[cfg(feature = "profile_opcodes")]
    if env::var_os("RDYNARMIC_PROFILE_OPCODES").is_some() {
        rdynarmic::backend::x64::opcode_profile::dump_top(30);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_environment_implements_a32_callbacks_directly() {
        fn assert_a32_callbacks<T: A32UserCallbacks>() {}

        assert_a32_callbacks::<BenchEnv>();
    }
}

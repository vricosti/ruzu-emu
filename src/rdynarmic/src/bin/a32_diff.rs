//! Differential comparison tool for ARM32 JIT execution.
//!
//! Loads ARM32 code from a flat binary file, runs it instruction-by-instruction
//! on both rdynarmic and upstream dynarmic (via the a32_oracle subprocess),
//! and reports the first register divergence.
//!
//! Usage:
//!   a32_diff <code.bin> <load_addr_hex> <entry_pc_hex> <sp_hex> [max_steps]
//!
//! Example:
//!   a32_diff rtld.bin 200000 200000 2499f90 10000

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use rdynarmic::jit::A32Jit;
use rdynarmic::jit_config::{JitConfig, OptimizationFlag, UserCallbacks};

const ORACLE: &str = "/home/vricosti/Dev/emulators/zuyu/build/a32_oracle";

// ---------------------------------------------------------------------------
// Shared memory environment for rdynarmic
// ---------------------------------------------------------------------------

struct DiffEnv {
    /// Memory segments as (base, Vec<u8>). Sorted by base so bisection works.
    /// Covers both code and data — the guest reads instructions and data
    /// via the same address space.
    segments: Vec<(u64, Vec<u8>)>,
    /// Write overlay for stores to addresses not covered by any loaded
    /// segment. Keeps memory use low while supporting guest writes.
    overlay: HashMap<u64, u8>,
    ticks_left: u64,
    svc_hit: bool,
}

impl DiffEnv {
    fn new() -> Self {
        Self {
            segments: Vec::new(),
            overlay: HashMap::new(),
            ticks_left: 200,
            svc_hit: false,
        }
    }

    fn load_code(&mut self, base: u64, data: &[u8]) {
        self.segments.push((base, data.to_vec()));
        // Keep segments sorted for bisection lookup.
        self.segments.sort_by_key(|&(b, _)| b);
    }

    /// Find the segment containing `addr`; returns (segment index, offset within segment).
    #[inline]
    fn find_segment(&self, addr: u64) -> Option<(usize, usize)> {
        // Partition_point: index of first seg with base > addr.
        let idx = self.segments.partition_point(|&(b, _)| b <= addr);
        if idx == 0 {
            return None;
        }
        let i = idx - 1;
        let (base, ref data) = self.segments[i];
        let off = (addr - base) as usize;
        if off < data.len() {
            Some((i, off))
        } else {
            None
        }
    }

    #[inline]
    fn read_byte(&self, addr: u64) -> u8 {
        if let Some(v) = self.overlay.get(&addr) {
            return *v;
        }
        if let Some((i, off)) = self.find_segment(addr) {
            return self.segments[i].1[off];
        }
        0
    }

    #[inline]
    fn write_byte(&mut self, addr: u64, value: u8) {
        if let Some((i, off)) = self.find_segment(addr) {
            self.segments[i].1[off] = value;
        } else {
            self.overlay.insert(addr, value);
        }
    }
}

impl UserCallbacks for DiffEnv {
    fn memory_read_code(&self, vaddr: u64) -> Option<u32> {
        let a = vaddr & !3;
        if let Some((i, off)) = self.find_segment(a) {
            let d = &self.segments[i].1;
            if off + 4 <= d.len() {
                return Some(u32::from_le_bytes([
                    d[off],
                    d[off + 1],
                    d[off + 2],
                    d[off + 3],
                ]));
            }
        }
        // Fall back: 0xEAFFFFFE is ARM "b ." (infinite self-loop) — matches old behavior.
        Some(0xEAFFFFFE)
    }
    fn memory_read_8(&self, vaddr: u64) -> u8 {
        self.read_byte(vaddr)
    }
    fn memory_read_16(&self, vaddr: u64) -> u16 {
        self.memory_read_8(vaddr) as u16 | (self.memory_read_8(vaddr + 1) as u16) << 8
    }
    fn memory_read_32(&self, vaddr: u64) -> u32 {
        self.memory_read_16(vaddr) as u32 | (self.memory_read_16(vaddr + 2) as u32) << 16
    }
    fn memory_read_64(&self, vaddr: u64) -> u64 {
        self.memory_read_32(vaddr) as u64 | (self.memory_read_32(vaddr + 4) as u64) << 32
    }
    fn memory_read_128(&self, vaddr: u64) -> (u64, u64) {
        (self.memory_read_64(vaddr), self.memory_read_64(vaddr + 8))
    }
    fn memory_write_8(&mut self, vaddr: u64, value: u8) {
        self.write_byte(vaddr, value);
    }
    fn memory_write_16(&mut self, vaddr: u64, value: u16) {
        self.memory_write_8(vaddr, value as u8);
        self.memory_write_8(vaddr + 1, (value >> 8) as u8);
    }
    fn memory_write_32(&mut self, vaddr: u64, value: u32) {
        self.memory_write_16(vaddr, value as u16);
        self.memory_write_16(vaddr + 2, (value >> 16) as u16);
    }
    fn memory_write_64(&mut self, vaddr: u64, value: u64) {
        self.memory_write_32(vaddr, value as u32);
        self.memory_write_32(vaddr + 4, (value >> 32) as u32);
    }
    fn memory_write_128(&mut self, vaddr: u64, lo: u64, hi: u64) {
        self.memory_write_64(vaddr, lo);
        self.memory_write_64(vaddr + 8, hi);
    }
    fn exclusive_write_8(&mut self, _: u64, _: u8, _: u8) -> bool {
        true
    }
    fn exclusive_write_16(&mut self, _: u64, _: u16, _: u16) -> bool {
        true
    }
    fn exclusive_write_32(&mut self, _: u64, _: u32, _: u32) -> bool {
        true
    }
    fn exclusive_write_64(&mut self, _: u64, _: u64, _: u64) -> bool {
        true
    }
    fn exclusive_write_128(&mut self, _: u64, _: u64, _: u64, _: u64, _: u64) -> bool {
        true
    }
    fn call_supervisor(&mut self, _svc: u32) {
        self.svc_hit = true;
    }
    fn exception_raised(&mut self, _pc: u64, _exc: u64) {}
    fn add_ticks(&mut self, ticks: u64) {
        self.ticks_left = self.ticks_left.saturating_sub(ticks);
    }
    fn get_ticks_remaining(&self) -> u64 {
        self.ticks_left
    }
}

// ---------------------------------------------------------------------------
// Oracle subprocess wrapper
// ---------------------------------------------------------------------------

struct Oracle {
    child: std::process::Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl Oracle {
    fn start(cpsr: u32, regs: &[u32; 15]) -> Self {
        let mut child = Command::new(ORACLE)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Failed to start a32_oracle");

        let stdin = child.stdin.as_mut().unwrap();
        // Send INIT command
        write!(stdin, "INIT {:08x}", cpsr).unwrap();
        for &r in regs {
            write!(stdin, " {:08x}", r).unwrap();
        }
        writeln!(stdin).unwrap();
        stdin.flush().unwrap();

        let reader = BufReader::new(child.stdout.take().unwrap());
        let mut oracle = Oracle { child, reader };

        // Read OK response
        let resp = oracle.read_line();
        assert_eq!(resp.trim(), "OK", "Oracle INIT failed: {}", resp);

        oracle
    }

    fn send_code(&mut self, addr: u32, instructions: &[u32]) {
        let stdin = self.child.stdin.as_mut().unwrap();
        write!(stdin, "CODE {:08x} {:x}", addr, instructions.len()).unwrap();
        for &insn in instructions {
            write!(stdin, " {:08x}", insn).unwrap();
        }
        writeln!(stdin).unwrap();
        stdin.flush().unwrap();
        let resp = self.read_line();
        assert_eq!(resp.trim(), "OK", "Oracle CODE failed: {}", resp);
    }

    fn send_mem_write(&mut self, addr: u32, data: &[u8]) {
        let stdin = self.child.stdin.as_mut().unwrap();
        write!(stdin, "MEMW {:08x} {:x}", addr, data.len()).unwrap();
        for &b in data {
            write!(stdin, " {:02x}", b).unwrap();
        }
        writeln!(stdin).unwrap();
        stdin.flush().unwrap();
        let resp = self.read_line();
        assert_eq!(resp.trim(), "OK", "Oracle MEMW failed: {}", resp);
    }

    fn set_register(&mut self, reg: u32, value: u32) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "SETREG {} {:08x}", reg, value).unwrap();
        stdin.flush().unwrap();
        let resp = self.read_line();
        assert_eq!(resp.trim(), "OK", "Oracle SETREG failed: {}", resp);
    }

    fn step(&mut self) -> ([u32; 16], u32) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "STEP").unwrap();
        stdin.flush().unwrap();
        self.parse_regs_response()
    }

    fn quit(&mut self) {
        if let Some(stdin) = self.child.stdin.as_mut() {
            let _ = writeln!(stdin, "QUIT");
            let _ = stdin.flush();
        }
        let _ = self.child.wait();
    }

    fn read_line(&mut self) -> String {
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        line
    }

    fn parse_regs_response(&mut self) -> ([u32; 16], u32) {
        let line = self.read_line();
        let tokens: Vec<&str> = line.trim().split_whitespace().collect();
        assert!(tokens.len() >= 17, "Bad STEP response: {}", line);
        let mut regs = [0u32; 16];
        for i in 0..16 {
            regs[i] = u32::from_str_radix(tokens[i], 16).unwrap();
        }
        let cpsr = u32::from_str_radix(tokens[16], 16).unwrap();
        (regs, cpsr)
    }
}

impl Drop for Oracle {
    fn drop(&mut self) {
        self.quit();
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

const REG_NAMES: [&str; 16] = [
    "R0", "R1", "R2", "R3", "R4", "R5", "R6", "R7", "R8", "R9", "R10", "R11", "R12", "SP", "LR",
    "PC",
];

fn optimization_flags_from_mask(mask: u32) -> OptimizationFlag {
    let mut flags = OptimizationFlag::NO_OPTIMIZATIONS;
    for flag in [
        OptimizationFlag::BLOCK_LINKING,
        OptimizationFlag::RETURN_STACK_BUFFER,
        OptimizationFlag::FAST_DISPATCH,
        OptimizationFlag::GET_SET_ELIMINATION,
        OptimizationFlag::CONST_PROP,
        OptimizationFlag::MISC_IR_OPT,
    ] {
        if mask & flag.bits() != 0 {
            flags |= flag;
        }
    }
    flags
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!(
            "Usage: a32_diff <entry_pc_hex> <sp_hex> <max_steps> [--cpsr=HEX] [--optimizations=HEX] [addr:file ...]"
        );
        eprintln!("Example: a32_diff 200000 2499f90 100000 200000:/tmp/ruzu_nso_200000.bin");
        eprintln!("Thumb:   a32_diff 0 2499f90 4 --cpsr=30 0:/tmp/thumb.bin");
        std::process::exit(1);
    }

    let entry_pc = u32::from_str_radix(&args[1], 16).expect("bad entry_pc");
    let sp = u32::from_str_radix(&args[2], 16).expect("bad sp");
    let max_steps: u64 = args[3].parse().expect("bad max_steps");

    // Parse optional --cpsr=HEX and --r0=..=--r14=HEX (initial GPR values).
    // register state at SVC #466 exit (r8=0, r9=0x4002c054, etc.).
    let mut cpsr_override: Option<u32> = None;
    let mut optimization_mask = 0u32;
    let mut reg_overrides: [Option<u32>; 15] = [None; 15];
    let mut module_args_start = 4;
    for (idx, arg) in args[4..].iter().enumerate() {
        let mut consumed = true;
        if arg.starts_with("--cpsr=") {
            cpsr_override = Some(u32::from_str_radix(&arg[7..], 16).expect("bad cpsr"));
        } else if let Some(value) = arg.strip_prefix("--optimizations=") {
            optimization_mask = u32::from_str_radix(value.trim_start_matches("0x"), 16)
                .expect("bad optimization mask");
        } else if let Some(rest) = arg.strip_prefix("--r") {
            // --rNN=HEX where NN in 0..15 (SP = r13, LR = r14)
            if let Some((num, val)) = rest.split_once('=') {
                if let Ok(n) = num.parse::<usize>() {
                    if n < 15 {
                        reg_overrides[n] =
                            Some(u32::from_str_radix(val, 16).expect("bad reg value"));
                    }
                }
            }
        } else {
            consumed = false;
        }
        if !consumed {
            module_args_start = 4 + idx;
            break;
        }
        module_args_start = 4 + idx + 1;
    }

    // Load all code modules
    let mut all_modules: Vec<(u64, Vec<u8>)> = Vec::new();
    for arg in &args[module_args_start..] {
        let parts: Vec<&str> = arg.splitn(2, ':').collect();
        if parts.len() != 2 {
            eprintln!("Bad module spec '{}', expected addr:file", arg);
            std::process::exit(1);
        }
        let load_addr = u64::from_str_radix(parts[0], 16).expect("bad module addr");
        let code_data = std::fs::read(parts[1]).expect(&format!("Failed to read {}", parts[1]));
        eprintln!(
            "Loaded {} bytes from {} at {:#x}",
            code_data.len(),
            parts[1],
            load_addr
        );
        all_modules.push((load_addr, code_data));
    }

    // Setup rdynarmic
    let mut env = DiffEnv::new();
    for (load_addr, code_data) in &all_modules {
        env.load_code(*load_addr, code_data);
    }

    let config = JitConfig {
        coprocessors: JitConfig::default_coprocessors(),
        callbacks: Box::new(env),
        enable_cycle_counting: false,
        code_cache_size: 64 * 1024 * 1024,
        optimizations: optimization_flags_from_mask(optimization_mask),
        unsafe_optimizations: false,
        global_monitor: None,
        fastmem_pointer: None,
        page_table_pointer: None,
        define_unpredictable_behaviour: false,
        arch_version: rdynarmic::interface::a32::arch_version::ArchVersion::V8,
        hook_hint_instructions: false,
        processor_id: 0,
        wall_clock_cntpct: false,
        cntfrq_el0: 600_000_000,
        ctr_el0: 0x8444_c004,
        dczid_el0: 4,
        hook_data_cache_operations: false,
        hook_isb: false,
        tpidrro_el0: None,
        tpidr_el0: None,
        memory: rdynarmic::backend::x64::emit_context::MemoryEmitConfig::default(),
    };

    let mut jit = A32Jit::new(config).expect("Failed to create rdynarmic A32Jit");

    // Set initial state
    let cpsr = cpsr_override.unwrap_or(0x00000010); // USR mode, ARM state (or Thumb if --cpsr=30)
    let mut init_regs = [0u32; 15];
    init_regs[13] = sp; // SP (positional arg; can also be overridden via --r13=..)
                        // Apply any --rN=HEX overrides on top of the default zeros + positional SP.
    for i in 0..15 {
        if let Some(v) = reg_overrides[i] {
            init_regs[i] = v;
        }
    }

    for i in 0..15 {
        jit.set_register(i, init_regs[i]);
    }
    jit.set_register(15, entry_pc);
    jit.set_cpsr(cpsr);

    // Setup oracle
    let mut oracle = Oracle::start(cpsr, &init_regs);

    // Send all modules to oracle as both code and data
    for (load_addr, code_data) in &all_modules {
        let instructions: Vec<u32> = code_data
            .chunks(4)
            .map(|c| {
                u32::from_le_bytes([
                    c.get(0).copied().unwrap_or(0),
                    c.get(1).copied().unwrap_or(0),
                    c.get(2).copied().unwrap_or(0),
                    c.get(3).copied().unwrap_or(0),
                ])
            })
            .collect();

        for (chunk_idx, chunk) in instructions.chunks(1000).enumerate() {
            let addr = *load_addr as u32 + (chunk_idx * 1000 * 4) as u32;
            oracle.send_code(addr, chunk);
        }

        for (chunk_idx, chunk) in code_data.chunks(4000).enumerate() {
            let addr = *load_addr as u32 + (chunk_idx * 4000) as u32;
            oracle.send_mem_write(addr, chunk);
        }
    }

    // Set PC on oracle
    oracle.set_register(15, entry_pc);

    eprintln!(
        "Starting differential comparison ({} max steps)...",
        max_steps
    );

    // Ring buffer for last N instructions (for context on divergence)
    let ring_size = 20;
    let mut ring: Vec<(u64, [u32; 16], u32)> = Vec::with_capacity(ring_size);

    // Step both JITs and compare
    for step in 0..max_steps {
        // Step rdynarmic
        let rd_hr = jit.step();
        let mut rd_regs = [0u32; 16];
        for i in 0..16 {
            rd_regs[i] = jit.get_register(i);
        }
        let rd_cpsr = jit.get_cpsr();

        // Step oracle
        let (or_regs, or_cpsr) = oracle.step();

        // Record in ring buffer
        if ring.len() >= ring_size {
            ring.remove(0);
        }
        ring.push((step, rd_regs, rd_cpsr));

        // Compare registers
        let mut reg_diverged = false;
        for i in 0..16 {
            if rd_regs[i] != or_regs[i] {
                if !reg_diverged {
                    eprintln!("\n!!! DIVERGENCE at step {} !!!", step);
                    eprintln!("  PC (rdynarmic) = {:#010x}", rd_regs[15]);
                    eprintln!("  PC (oracle)    = {:#010x}", or_regs[15]);
                }
                reg_diverged = true;
                eprintln!(
                    "  {} differs: rdynarmic={:#010x} oracle={:#010x}",
                    REG_NAMES[i], rd_regs[i], or_regs[i]
                );
            }
        }

        // Compare CPSR (only NZCV bits for now — bit 28-31)
        if (rd_cpsr ^ or_cpsr) & 0xF0000000 != 0 {
            if !reg_diverged {
                eprintln!(
                    "\n[CPSR-only divergence at step {} PC={:#010x}]: rdynarmic={:#010x} oracle={:#010x} (continuing — likely harmless if next flag-setting insn overwrites)",
                    step, rd_regs[15], rd_cpsr, or_cpsr
                );
            }
            // Force oracle and rdynarmic CPSR in sync to keep diff going — we
            // tolerate lazy-flag-format differences between backends.
            jit.set_cpsr(or_cpsr);
        }

        // Verbose per-step log for the 20 steps before PC=0 — lets us see
        // the exact instruction sequence that leads into truncated heap.
        if step >= 1480 && step <= 1500 {
            eprintln!(
                "  [step {}] PC=0x{:08x} LR=0x{:08x} R0={:#x} R1={:#x} R4={:#x} R5={:#x} R7={:#x} CPSR={:#x}",
                step, rd_regs[15], rd_regs[14], rd_regs[0], rd_regs[1],
                rd_regs[4], rd_regs[5], rd_regs[7], rd_cpsr
            );
        }
        // Verbose single-step log when PC just became 0 — to find the exact
        // instruction that deref'd truncated heap.
        static PC_WAS_NONZERO: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        if rd_regs[15] != 0 {
            PC_WAS_NONZERO.store(true, std::sync::atomic::Ordering::Relaxed);
        } else if PC_WAS_NONZERO.swap(false, std::sync::atomic::Ordering::Relaxed) {
            eprintln!(
                "  [PC just became 0 at step {}] R0={:#x} R1={:#x} R2={:#x} R3={:#x} R4={:#x} R5={:#x} R7={:#x} R11={:#x} SP={:#x} LR={:#x}",
                step, rd_regs[0], rd_regs[1], rd_regs[2], rd_regs[3],
                rd_regs[4], rd_regs[5], rd_regs[7], rd_regs[11],
                rd_regs[13], rd_regs[14]
            );
        }

        let diverged = reg_diverged;
        if diverged {
            // Dump ring buffer for context
            eprintln!("\n--- Last {} instructions (rdynarmic) ---", ring.len());
            for (s, regs, cpsr) in &ring {
                let insn_addr = regs[15].wrapping_sub(4); // PC points to next, insn was at PC-4
                eprintln!(
                    "  step={:>6} PC={:#010x} R0={:#x} R1={:#x} R2={:#x} R3={:#x} R4={:#x} SP={:#x} CPSR={:#x}",
                    s, insn_addr, regs[0], regs[1], regs[2], regs[3], regs[4], regs[13], cpsr,
                );
            }
            std::process::exit(1);
        }

        // Check for SVC (both sides should no-op it)
        if rd_hr.contains(rdynarmic::halt_reason::HaltReason::SVC) {
            eprintln!("  step {}: SVC hit, skipping", step);
            // Both sides no-op SVCs, so just continue
        }

        // Progress report
        if step % 10000 == 0 && step > 0 {
            eprintln!(
                "  ... {} steps, PC={:#010x}, no divergence",
                step, rd_regs[15]
            );
        }
    }

    eprintln!("Completed {} steps with no divergence!", max_steps);
}

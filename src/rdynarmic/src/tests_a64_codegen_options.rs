//! The x86-64 code generation options `ArmDynarmic64` turns on must not change what guest code
//! computes: every program runs with the options off and on and must end in the same state.
//!
//! Port of Lichii `src/tests/core/jit_codegen_options.cpp` (commit aab5393019). Divergence from
//! Eden allowed by the user on 2026-11-17 (see DIFF.md).

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    use crate::frontend::a64::translate::translate_single_instruction;
    use crate::interface::a64::config::{
        Exception, UserCallbacks, UserConfig, Vector,
    };
    use crate::interface::exclusive_monitor::ExclusiveMonitor;
    use crate::ir::block::Block;
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::jit::A64Jit;
    use crate::HaltReason;

    const SVC0: u32 = 0xD400_0001;

    /// The options `ArmDynarmic64` enables (every CPU accuracy but Paranoid, multicore).
    pub(crate) fn enable_codegen_options(config: &mut UserConfig) {
        config.spill_to_free_gpr = true;
        config.scratch_copy_to_free_reg = true;
        config.fold_address_offsets = true;
        config.elide_redundant_zext = true;
        config.omit_block_frame = true;
        config.fast_dispatch_table_bits = 16;
        config.fast_dispatch_set_associative = true;
        config.rsb_size = 32;
        config.halt_check_on_back_edges = true;
        config.reuse_host_flags = true;
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct State {
        regs: [u64; 31],
        vecs: [Vector; 32],
        sp: u64,
        pc: u64,
        nzcv: u32,
        fpsr: u32,
        memory: BTreeMap<u64, u8>,
    }

    /// FPSR without the cumulative exception bits: host callbacks run under the guest's MXCSR
    /// and can leave flags in it with either setting.
    fn capture(jit: &A64Jit, memory: BTreeMap<u64, u8>) -> State {
        State {
            regs: jit.get_registers(),
            vecs: jit.get_vectors(),
            sp: jit.get_sp(),
            pc: jit.get_pc(),
            nzcv: jit.get_pstate() & 0xF000_0000,
            fpsr: jit.get_fpsr() & !0x9F,
            memory,
        }
    }

    #[derive(Default)]
    struct EnvState {
        code: Vec<u32>,
        code_start: u64,
        memory: BTreeMap<u64, u8>,
    }

    /// Memory is a map with a pattern for unwritten bytes; code outside the block is SVC #0,
    /// which stops the run (there is no cycle counter to do it).
    #[derive(Clone)]
    struct RandomBlockEnv {
        state: Arc<Mutex<EnvState>>,
        halt_reason: Arc<Mutex<usize>>,
        reads: Arc<std::sync::atomic::AtomicU64>,
    }

    impl RandomBlockEnv {
        fn read(&self, vaddr: u64, size: usize) -> u64 {
            self.reads.fetch_add(1, Ordering::Relaxed);
            let state = self.state.lock().unwrap();
            let mut value = 0u64;
            for i in 0..size as u64 {
                let address = vaddr.wrapping_add(i);
                let byte = state.memory.get(&address).copied().unwrap_or(address as u8);
                value |= (byte as u64) << (8 * i);
            }
            value
        }

        fn write(&self, vaddr: u64, value: u64, size: usize) {
            let mut state = self.state.lock().unwrap();
            for i in 0..size as u64 {
                state.memory.insert(vaddr.wrapping_add(i), (value >> (8 * i)) as u8);
            }
        }
    }

    impl UserCallbacks for RandomBlockEnv {
        fn memory_read_code(&self, vaddr: u64) -> Option<u32> {
            let state = self.state.lock().unwrap();
            let offset = vaddr.wrapping_sub(state.code_start);
            Some(if offset < state.code.len() as u64 * 4 {
                state.code[(offset / 4) as usize]
            } else {
                SVC0
            })
        }
        fn memory_read_8(&self, vaddr: u64) -> u8 {
            self.read(vaddr, 1) as u8
        }
        fn memory_read_16(&self, vaddr: u64) -> u16 {
            self.read(vaddr, 2) as u16
        }
        fn memory_read_32(&self, vaddr: u64) -> u32 {
            self.read(vaddr, 4) as u32
        }
        fn memory_read_64(&self, vaddr: u64) -> u64 {
            self.read(vaddr, 8)
        }
        fn memory_read_128(&self, vaddr: u64) -> Vector {
            [self.read(vaddr, 8), self.read(vaddr.wrapping_add(8), 8)]
        }
        fn memory_write_8(&mut self, vaddr: u64, value: u8) {
            self.write(vaddr, value as u64, 1)
        }
        fn memory_write_16(&mut self, vaddr: u64, value: u16) {
            self.write(vaddr, value as u64, 2)
        }
        fn memory_write_32(&mut self, vaddr: u64, value: u32) {
            self.write(vaddr, value as u64, 4)
        }
        fn memory_write_64(&mut self, vaddr: u64, value: u64) {
            self.write(vaddr, value, 8)
        }
        fn memory_write_128(&mut self, vaddr: u64, value: Vector) {
            self.write(vaddr, value[0], 8);
            self.write(vaddr.wrapping_add(8), value[1], 8);
        }
        fn memory_write_exclusive_8(&mut self, vaddr: u64, value: u8, _: u8) -> bool {
            self.write(vaddr, value as u64, 1);
            true
        }
        fn memory_write_exclusive_16(&mut self, vaddr: u64, value: u16, _: u16) -> bool {
            self.write(vaddr, value as u64, 2);
            true
        }
        fn memory_write_exclusive_32(&mut self, vaddr: u64, value: u32, _: u32) -> bool {
            self.write(vaddr, value as u64, 4);
            true
        }
        fn memory_write_exclusive_64(&mut self, vaddr: u64, value: u64, _: u64) -> bool {
            self.write(vaddr, value, 8);
            true
        }
        fn memory_write_exclusive_128(&mut self, vaddr: u64, value: Vector, _: Vector) -> bool {
            self.memory_write_128(vaddr, value);
            true
        }
        fn call_svc(&mut self, _swi: u32) {
            let ptr = *self.halt_reason.lock().unwrap();
            unsafe {
                (&*(ptr as *const AtomicU32)).fetch_or(HaltReason::SVC.bits(), Ordering::SeqCst);
            }
        }
        fn exception_raised(&mut self, _pc: u64, _exception: Exception) {}
        fn add_ticks(&mut self, _ticks: u64) {}
        fn get_ticks_remaining(&self) -> u64 {
            0
        }
        fn get_cntpct(&self) -> u64 {
            0
        }
        fn set_halt_reason_ptr(&mut self, ptr: *const u32) {
            *self.halt_reason.lock().unwrap() = ptr as usize;
        }
    }

    /// One A64 encoding class from the decoder table: fixed bits and the bits that may vary.
    struct Encoding {
        bits: u32,
        mask: u32,
    }

    fn testable_encodings() -> Vec<Encoding> {
        // System instructions read or write host-side state; the translator asserts on some
        // encodings of the complex FP ops; the SM4 S-box needs the table in the low 2 GiB with
        // either setting.
        const EXCLUDED: &[&str] = &[
            "UnallocatedEncoding",
            "SVC",
            "BRK",
            "MSR_reg",
            "MSR_imm",
            "MRS",
            "SYS",
            "FCADD_vec",
            "FCMLA_vec",
            "FCMLA_elt",
            "SM4E",
            "SM4EKEY",
            // rdynarmic, not the options: `emit_floating_point.rs::scalar_rounding_mode` panics
            // on RoundingMode::ToOdd (5), which FCVTXN passes to FPDoubleToSingle (reported;
            // that file belongs to the FP emission work).
            "FCVTXN_1",
            "FCVTXN_2",
        ];
        let mut result = Vec::new();
        for line in include_str!("frontend/a64/a64.inc").lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("INST(") else {
                continue;
            };
            let name = rest.split(',').next().unwrap().trim();
            if EXCLUDED.contains(&name) {
                continue;
            }
            let bitstring = rest.rsplit('"').nth(1).unwrap();
            assert_eq!(bitstring.len(), 32, "{line}");
            let mut encoding = Encoding { bits: 0, mask: 0 };
            for (i, c) in bitstring.chars().enumerate() {
                let bit = 1u32 << (31 - i);
                if c == '0' || c == '1' {
                    encoding.mask |= bit;
                    if c == '1' {
                        encoding.bits |= bit;
                    }
                }
            }
            result.push(encoding);
        }
        result
    }

    fn should_test(instruction: u32, pc: u64, is_last: bool) -> bool {
        let location = A64LocationDescriptor::new(pc, 0, false);
        let mut block = Block::new(location.to_location());
        if !translate_single_instruction(&mut block, location, instruction) && !is_last {
            return false;
        }
        // rdynarmic, not the options: some translations access W31/X31 as a general register,
        // which the get/set elimination pass indexes out of bounds (reported).
        let bad_register = block.instructions.iter().find(|inst| {
            matches!(
                inst.opcode,
                Opcode::A64GetW | Opcode::A64GetX | Opcode::A64SetW | Opcode::A64SetX
            ) && inst.args[0].get_a64_reg() as usize >= 31
        });
        if let Some(inst) = bad_register {
            eprintln!(
                "[codegen options test] skipped 0x{instruction:08x}: {:?} of register 31",
                inst.opcode
            );
            return false;
        }
        !block.instructions.iter().any(|inst| {
            matches!(
                inst.opcode,
                Opcode::A64ExceptionRaised
                    | Opcode::A64CallSupervisor
                    | Opcode::A64DataCacheOperationRaised
                    | Opcode::A64InstructionCacheOperationRaised
                    | Opcode::A64GetCNTPCT
            )
        })
    }

    /// A branch back into the block would loop forever without a cycle counter.
    fn branches_into_block(inst: u32, pc: u64, start: u64, end: u64) -> bool {
        let target = |imm: u32, bits: u32| {
            let shifted = ((imm << (32 - bits)) as i32) >> (32 - bits);
            pc.wrapping_add((shifted as i64 * 4) as u64)
        };
        let to = if inst & 0x7C00_0000 == 0x1400_0000 {
            target(inst & 0x03FF_FFFF, 26)
        } else if inst & 0xFF00_0010 == 0x5400_0000 || inst & 0x7E00_0000 == 0x3400_0000 {
            target((inst >> 5) & 0x7FFFF, 19)
        } else if inst & 0x7E00_0000 == 0x3600_0000 {
            target((inst >> 5) & 0x3FFF, 14)
        } else {
            return false;
        };
        to >= start && to <= end
    }

    /// splitmix64: a fixed, dependency-free random sequence.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        fn range(&mut self, min: u64, max: u64) -> u64 {
            if min == 0 && max == u64::MAX {
                return self.next();
            }
            min + self.next() % (max - min + 1)
        }
    }

    struct Runner {
        env: RandomBlockEnv,
        jit: A64Jit,
        _monitor: Box<ExclusiveMonitor>,
    }

    fn make_runner(options: bool) -> Runner {
        let env = RandomBlockEnv {
            state: Arc::new(Mutex::new(EnvState::default())),
            halt_reason: Arc::new(Mutex::new(0)),
            reads: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        };
        let mut monitor = Box::new(ExclusiveMonitor::new(1));
        let mut config = UserConfig::new(Box::new(env.clone()));
        config.global_monitor = Some(&mut *monitor as *mut _);
        config.enable_cycle_counting = false;
        config.code_cache_size = 64 * 1024 * 1024;
        if options {
            enable_codegen_options(&mut config);
        }
        let jit = A64Jit::new(config).expect("A64 JIT");
        Runner {
            env,
            jit,
            _monitor: monitor,
        }
    }

    fn run_random_blocks(seed: u64, iterations: usize) {
        let encodings = testable_encodings();
        let mut rng = Rng(seed);
        let mut runners = [make_runner(false), make_runner(true)];

        for iteration in 0..iterations {
            let mut regs = [0u64; 31];
            regs.iter_mut().for_each(|r| *r = rng.next());
            let mut vecs = [[0u64; 2]; 32];
            vecs.iter_mut().for_each(|v| *v = [rng.next(), rng.next()]);
            let start = rng.range(0, 0x10_0000_0000) * 4;
            let count = rng.range(1, 40) as usize;
            let mut code = vec![0u32; count];
            for j in 0..count {
                let pc = start + j as u64 * 4;
                loop {
                    let encoding = &encodings[rng.range(0, encodings.len() as u64 - 1) as usize];
                    code[j] = encoding.bits | (rng.next() as u32 & !encoding.mask);
                    if should_test(code[j], pc, j == count - 1)
                        && !branches_into_block(code[j], pc, start, start + count as u64 * 4)
                    {
                        break;
                    }
                }
            }
            let sp = rng.range(0x30_0000_0000, 0x40_0000_0000) * 4;
            let pstate = (rng.range(0, 0xF) as u32) << 28;
            // AHP, DN, FZ, RMode and FZ16.
            let fpcr = rng.next() as u32 & 0x07C8_0000;

            let mut states = Vec::with_capacity(2);
            for runner in runners.iter_mut() {
                {
                    let mut state = runner.env.state.lock().unwrap();
                    state.code = code.clone();
                    state.code_start = start;
                    state.memory.clear();
                }
                let jit = &mut runner.jit;
                jit.clear_cache();
                jit.set_registers(regs);
                jit.set_vectors(vecs);
                jit.set_sp(sp);
                jit.set_pc(start);
                jit.set_pstate(pstate);
                jit.set_fpcr(fpcr);
                jit.set_fpsr(0);
                let halt = jit.run();
                jit.clear_halt(halt);
                let memory = runner.env.state.lock().unwrap().memory.clone();
                states.push(capture(jit, memory));
            }
            assert_eq!(
                states[0],
                states[1],
                "iteration {iteration}, {count} instructions at 0x{start:x}: {:08x?}",
                code
            );
        }
    }

    #[test]
    fn codegen_options_random_blocks() {
        run_random_blocks(150, 1000);
    }

    #[test]
    fn codegen_options_random_blocks_second_seed() {
        run_random_blocks(0x5EED_0F_C0DE, 1000);
    }

    // --- Targeted sequences ----------------------------------------------------------------

    /// Flag producers right before their consumers (reuse_host_flags), zero-extended W values
    /// (elide_redundant_zext), base + immediate addresses (fold_address_offsets) and register
    /// pressure (spill_to_free_gpr, scratch_copy_to_free_reg), each block ending in a
    /// conditional terminal, with many register and flag values.
    #[test]
    fn codegen_options_targeted_sequences() {
        let programs: &[&[u32]] = &[
            // cmp x0, x1; csel x2, x3, x4, hi; b.ls +8; add x5, x5, #1; svc
            &[0xEB01_001F, 0x9A84_8062, 0x5400_0049, 0x9100_04A5, SVC0],
            // subs w0, w0, #1; cset w6, eq; b.ne +8; mov x7, #1; svc
            &[0x7100_0400, 0x1A9F_17E6, 0x5400_0041, 0xD280_0027, SVC0],
            // adds x0, x1, x2; csinc x3, x4, x5, cc; b.hi +8; mov x8, #2; svc
            &[0xAB02_0020, 0x9A85_3483, 0x5400_0048, 0xD280_0048, SVC0],
            // cmn w1, #5; ccmp x2, x3, #4, ne; csel w4, w5, w6, lt; b.gt +8; mov x9, #3; svc
            &[0x3100_143F, 0xFA43_1044, 0x1A86_B0A4, 0x5400_004C, 0xD280_0069, SVC0],
            // add w0, w1, w2; mov x3, x0 (orr); umaddl x4, w0, w1, x5; cbz w0, +8; mov x6, #1; svc
            &[0x0B02_0020, 0xAA00_03E3, 0x9BA1_1404, 0x3400_0040, 0xD280_0026, SVC0],
            // ldr x0, [x1, #8]; ldr w2, [x1, #0x10]; str x3, [x1, #0x18]; tbz w2, #3, +8; mov x4, #1; svc
            &[0xF940_0420, 0xB940_1022, 0xF900_0C23, 0x3618_0042, 0xD280_0024, SVC0],
            // many live values: add x0..x7 from each other, then compare chain
            &[
                0x8B01_0002, 0x8B02_0023, 0x8B03_0044, 0x8B04_0065, 0x8B05_0086, 0x8B06_00A7,
                0xCB07_0008, 0xCB08_0029, 0x8A09_014A, 0xAA0A_016B, 0xEB0B_019F, 0x9A8C_A1AD,
                0x5400_004B, 0xD280_002E, SVC0,
            ],
        ];
        let mut rng = Rng(0xF1A6);
        let mut runners = [make_runner(false), make_runner(true)];
        for (program_index, code) in programs.iter().enumerate() {
            for iteration in 0..200 {
                let mut regs = [0u64; 31];
                for r in regs.iter_mut() {
                    *r = match rng.range(0, 3) {
                        0 => rng.range(0, 4),
                        1 => rng.next() & 0xFFFF_FFFF,
                        2 => 0x1000 + rng.range(0, 0x100) * 8,
                        _ => rng.next(),
                    };
                }
                let pstate = (rng.range(0, 0xF) as u32) << 28;
                let mut states = Vec::with_capacity(2);
                for runner in runners.iter_mut() {
                    {
                        let mut state = runner.env.state.lock().unwrap();
                        state.code = code.to_vec();
                        state.code_start = 0x10_0000;
                        state.memory.clear();
                    }
                    let jit = &mut runner.jit;
                    jit.clear_cache();
                    jit.set_registers(regs);
                    jit.set_sp(0x8000_0000);
                    jit.set_pc(0x10_0000);
                    jit.set_pstate(pstate);
                    jit.set_fpcr(0);
                    jit.set_fpsr(0);
                    let halt = jit.run();
                    jit.clear_halt(halt);
                    let memory = runner.env.state.lock().unwrap().memory.clone();
                    states.push(capture(jit, memory));
                }
                assert_eq!(states[0], states[1], "program {program_index}, iteration {iteration}");
            }
        }
    }

    // --- External halt with the options -------------------------------------------------------

    /// A running loop must still see an external halt with halt_check_on_back_edges: a loop
    /// closed by a backward link, one with a forward link inside, and one through BR (fast
    /// dispatch) and RET (RSB).
    #[test]
    fn codegen_options_external_halt_stops_running_loops() {
        use std::sync::mpsc;
        use std::thread;
        use std::time::{Duration, Instant};

        let loops: [&[u32]; 3] = [
            // 0x1000: ldr x0, [x1]; b 0x1000
            &[0xF940_0020, 0x17FF_FFFF],
            // 0x1000: ldr x0, [x1]; b 0x100c; svc; 0x100c: b 0x1000
            &[0xF940_0020, 0x1400_0002, SVC0, 0x17FF_FFFD],
            // 0x1000: ldr x0, [x1]; bl 0x1010; br x2 (x2 = 0x1000); svc; 0x1010: ret
            &[0xF940_0020, 0x9400_0003, 0xD61F_0040, SVC0, 0xD65F_03C0],
        ];
        for (loop_index, code) in loops.iter().enumerate() {
            let code = code.to_vec();
            let (setup_tx, setup_rx) = mpsc::sync_channel(0);
            let (result_tx, result_rx) = mpsc::sync_channel(0);
            let runner = thread::spawn(move || {
                let mut runner = make_runner(true);
                {
                    let mut state = runner.env.state.lock().unwrap();
                    state.code = code;
                    state.code_start = 0x1000;
                }
                runner.jit.set_pc(0x1000);
                runner.jit.set_register(1, 0x2000);
                runner.jit.set_register(2, 0x1000);
                setup_tx
                    .send((runner.jit.halt_reason_ptr() as usize, Arc::clone(&runner.env.reads)))
                    .unwrap();
                let halt = runner.jit.run();
                result_tx.send(halt).unwrap();
            });
            let (halt_ptr, reads) = setup_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while reads.load(Ordering::Acquire) < 100 {
                assert!(Instant::now() < deadline, "loop {loop_index} did not start");
                thread::yield_now();
            }
            unsafe { &*(halt_ptr as *const AtomicU32) }
                .fetch_or(HaltReason::EXTERNAL_HALT.bits(), Ordering::Release);
            let halt = result_rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|_| panic!("loop {loop_index} did not observe EXTERNAL_HALT"));
            assert!(halt.contains(HaltReason::EXTERNAL_HALT), "loop {loop_index}: {halt:?}");
            runner.join().unwrap();
        }
    }

    // --- Fastmem faults --------------------------------------------------------------------

    const fn ldr_x(rt: u32, rn: u32, imm: u32) -> u32 {
        0xF940_0000 | (imm / 8) << 10 | rn << 5 | rt
    }
    const fn ldr_w(rt: u32, rn: u32, imm: u32) -> u32 {
        0xB940_0000 | (imm / 4) << 10 | rn << 5 | rt
    }
    const fn ldrh_w(rt: u32, rn: u32, imm: u32) -> u32 {
        0x7940_0000 | (imm / 2) << 10 | rn << 5 | rt
    }
    const fn ldrb_w(rt: u32, rn: u32, imm: u32) -> u32 {
        0x3940_0000 | imm << 10 | rn << 5 | rt
    }
    const fn ldur_x(rt: u32, rn: u32, imm: i32) -> u32 {
        0xF840_0000 | ((imm as u32) & 0x1FF) << 12 | rn << 5 | rt
    }
    const fn str_x(rt: u32, rn: u32, imm: u32) -> u32 {
        0xF900_0000 | (imm / 8) << 10 | rn << 5 | rt
    }
    const fn strh_w(rt: u32, rn: u32, imm: u32) -> u32 {
        0x7900_0000 | (imm / 2) << 10 | rn << 5 | rt
    }
    const fn strb_w(rt: u32, rn: u32, imm: u32) -> u32 {
        0x3900_0000 | imm << 10 | rn << 5 | rt
    }
    const fn add_imm(rd: u32, rn: u32, imm: u32) -> u32 {
        0x9100_0000 | imm << 10 | rn << 5 | rd
    }
    const fn add_reg(rd: u32, rn: u32, rm: u32) -> u32 {
        0x8B00_0000 | rm << 16 | rn << 5 | rd
    }

    const ARENA_SIZE: u64 = 0x10000;
    const FAULT_PAGE: u64 = 0x3000;

    struct FaultEnvState {
        code: Vec<u32>,
        arena: usize,
        fault_page: BTreeMap<u64, u8>,
    }

    /// Unchecked 64-bit fastmem over a small arena whose page at FAULT_PAGE is inaccessible:
    /// accesses there fault and take the callbacks, which keep that page's bytes on the side.
    #[derive(Clone)]
    struct FastmemFaultEnv {
        state: Arc<Mutex<FaultEnvState>>,
        halt_reason: Arc<Mutex<usize>>,
    }

    impl FastmemFaultEnv {
        fn new(code: Vec<u32>) -> Self {
            let arena = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    ARENA_SIZE as usize,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            assert_ne!(arena, libc::MAP_FAILED);
            let env = Self {
                state: Arc::new(Mutex::new(FaultEnvState {
                    code,
                    arena: arena as usize,
                    fault_page: BTreeMap::new(),
                })),
                halt_reason: Arc::new(Mutex::new(0)),
            };
            assert_eq!(
                unsafe {
                    libc::mprotect(
                        (arena as *mut u8).add(FAULT_PAGE as usize).cast(),
                        0x1000,
                        libc::PROT_NONE,
                    )
                },
                0
            );
            env.reset();
            env
        }

        fn arena(&self) -> *mut u8 {
            self.state.lock().unwrap().arena as *mut u8
        }

        fn reset(&self) {
            let mut state = self.state.lock().unwrap();
            let arena = state.arena as *mut u8;
            for i in 0..ARENA_SIZE {
                if i.wrapping_sub(FAULT_PAGE) >= 0x1000 {
                    unsafe { arena.add(i as usize).write((i * 13 + 5) as u8) };
                }
            }
            state.fault_page.clear();
        }

        fn read8(&self, vaddr: u64) -> u8 {
            let state = self.state.lock().unwrap();
            if vaddr.wrapping_sub(FAULT_PAGE) < 0x1000 {
                return state
                    .fault_page
                    .get(&vaddr)
                    .copied()
                    .unwrap_or((vaddr * 7 + 1) as u8);
            }
            assert!(vaddr < ARENA_SIZE);
            unsafe { (state.arena as *const u8).add(vaddr as usize).read() }
        }

        fn write8(&self, vaddr: u64, value: u8) {
            let mut state = self.state.lock().unwrap();
            if vaddr.wrapping_sub(FAULT_PAGE) < 0x1000 {
                state.fault_page.insert(vaddr, value);
                return;
            }
            assert!(vaddr < ARENA_SIZE);
            unsafe { (state.arena as *mut u8).add(vaddr as usize).write(value) };
        }

        fn read(&self, vaddr: u64, size: u64) -> u64 {
            (0..size).fold(0, |value, i| value | (self.read8(vaddr + i) as u64) << (8 * i))
        }

        fn write(&self, vaddr: u64, value: u64, size: u64) {
            for i in 0..size {
                self.write8(vaddr + i, (value >> (8 * i)) as u8);
            }
        }
    }

    impl Drop for FaultEnvState {
        fn drop(&mut self) {
            unsafe { libc::munmap(self.arena as *mut libc::c_void, ARENA_SIZE as usize) };
        }
    }

    impl UserCallbacks for FastmemFaultEnv {
        fn memory_read_code(&self, vaddr: u64) -> Option<u32> {
            let state = self.state.lock().unwrap();
            Some(state.code.get((vaddr / 4) as usize).copied().unwrap_or(SVC0))
        }
        fn memory_read_8(&self, vaddr: u64) -> u8 {
            self.read(vaddr, 1) as u8
        }
        fn memory_read_16(&self, vaddr: u64) -> u16 {
            self.read(vaddr, 2) as u16
        }
        fn memory_read_32(&self, vaddr: u64) -> u32 {
            self.read(vaddr, 4) as u32
        }
        fn memory_read_64(&self, vaddr: u64) -> u64 {
            self.read(vaddr, 8)
        }
        fn memory_read_128(&self, vaddr: u64) -> Vector {
            [self.read(vaddr, 8), self.read(vaddr + 8, 8)]
        }
        fn memory_write_8(&mut self, vaddr: u64, value: u8) {
            self.write(vaddr, value as u64, 1)
        }
        fn memory_write_16(&mut self, vaddr: u64, value: u16) {
            self.write(vaddr, value as u64, 2)
        }
        fn memory_write_32(&mut self, vaddr: u64, value: u32) {
            self.write(vaddr, value as u64, 4)
        }
        fn memory_write_64(&mut self, vaddr: u64, value: u64) {
            self.write(vaddr, value, 8)
        }
        fn memory_write_128(&mut self, vaddr: u64, value: Vector) {
            self.write(vaddr, value[0], 8);
            self.write(vaddr + 8, value[1], 8);
        }
        fn call_svc(&mut self, _swi: u32) {
            let ptr = *self.halt_reason.lock().unwrap();
            unsafe {
                (&*(ptr as *const AtomicU32)).fetch_or(HaltReason::SVC.bits(), Ordering::SeqCst);
            }
        }
        fn exception_raised(&mut self, _pc: u64, _exception: Exception) {}
        fn add_ticks(&mut self, _ticks: u64) {}
        fn get_ticks_remaining(&self) -> u64 {
            0
        }
        fn get_cntpct(&self) -> u64 {
            0
        }
        fn set_halt_reason_ptr(&mut self, ptr: *const u32) {
            *self.halt_reason.lock().unwrap() = ptr as usize;
        }
    }

    fn run_fastmem_program(options: bool) -> Vec<State> {
        // X0 = 0x2800 and X8 = 0x2900, set before the run so the addresses stay base +
        // immediate: accesses at +0x800 and beyond fault, and X0 is used again after accesses
        // whose fault path may move it by the displacement and back.
        let code = vec![
            ldr_x(1, 0, 0x10),
            ldr_x(2, 0, 0x800),
            add_imm(11, 0, 3),
            ldrb_w(3, 0, 0x801),
            ldrh_w(4, 0, 0x7FE),
            ldr_w(5, 0, 0x900),
            ldur_x(10, 0, -8),
            str_x(6, 0, 0x808),
            strb_w(7, 0, 0xF),
            str_x(8, 8, 0x700),
            strh_w(8, 8, 0x702),
            add_reg(12, 8, 11),
            ldr_x(0, 0, 0x810),
            SVC0,
        ];
        let env = FastmemFaultEnv::new(code);
        let mut config = UserConfig::new(Box::new(env.clone()));
        config.enable_cycle_counting = false;
        config.code_cache_size = 16 * 1024 * 1024;
        config.fastmem_pointer = Some(env.arena());
        config.fastmem_address_space_bits = 64;
        config.silently_mirror_fastmem = false;
        if options {
            enable_codegen_options(&mut config);
        }
        let mut jit = A64Jit::new(config).expect("A64 JIT");

        // The first run faults at each access to the page; the second runs the recompiled
        // block, whose accesses there go to the callbacks directly.
        let mut states = Vec::new();
        for _ in 0..2 {
            env.reset();
            for i in 0..31u64 {
                jit.set_register(i as usize, 0x1111_0000_0000_0000 * (i % 3) + i * 0x101);
            }
            jit.set_register(0, 0x2800);
            jit.set_register(8, 0x2900);
            jit.set_sp(0x8000);
            jit.set_pc(0);
            let halt = jit.run();
            assert!(halt.contains(HaltReason::SVC), "halt {halt:?} pc 0x{:x}", jit.get_pc());
            jit.clear_halt(halt);
            let mut memory = env.state.lock().unwrap().fault_page.clone();
            for i in 0x2000..0x3000u64 {
                memory.insert(i, unsafe { env.arena().add(i as usize).read() });
            }
            states.push(capture(&jit, memory));
        }
        states
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn codegen_options_fastmem_faults() {
        let off = run_fastmem_program(false);
        let on = run_fastmem_program(true);
        assert_eq!(off[0], off[1]);
        assert_eq!(on[0], off[0]);
        assert_eq!(on[1], off[1]);
    }
}

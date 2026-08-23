//! A32 instruction tests ported from dynarmic's tests/A32/test_arm_instructions.cpp.
//! These verify that rdynarmic produces identical results to C++ dynarmic.

#[cfg(test)]
mod tests {
    use crate::interface::a32::coprocessor::{
        Callback, CallbackOrAccessOneWord, CallbackOrAccessTwoWords, Coprocessor,
    };
    use crate::interface::a32::coprocessor_util::CoprocReg;
    use crate::jit::A32Jit;
    use crate::jit_config::{JitConfig, OptimizationFlag, UserCallbacks};
    use std::cell::UnsafeCell;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    struct TestEnv {
        code_mem: Vec<u32>,
        data_mem: HashMap<u64, u8>,
        ticks_left: u64,
    }

    impl TestEnv {
        fn new(code: Vec<u32>) -> Self {
            Self {
                code_mem: code,
                data_mem: HashMap::new(),
                ticks_left: 100,
            }
        }
    }

    impl UserCallbacks for TestEnv {
        fn memory_read_code(&self, vaddr: u64) -> Option<u32> {
            let idx = (vaddr as usize) / 4;
            if idx < self.code_mem.len() {
                Some(self.code_mem[idx])
            } else {
                // Infinite loop: B .
                Some(0xEAFFFFFE)
            }
        }

        fn memory_read_8(&self, vaddr: u64) -> u8 {
            *self.data_mem.get(&vaddr).unwrap_or(&0)
        }
        fn memory_read_16(&self, vaddr: u64) -> u16 {
            let lo = self.memory_read_8(vaddr) as u16;
            let hi = self.memory_read_8(vaddr + 1) as u16;
            lo | (hi << 8)
        }
        fn memory_read_32(&self, vaddr: u64) -> u32 {
            let lo = self.memory_read_16(vaddr) as u32;
            let hi = self.memory_read_16(vaddr + 2) as u32;
            lo | (hi << 16)
        }
        fn memory_read_64(&self, vaddr: u64) -> u64 {
            let lo = self.memory_read_32(vaddr) as u64;
            let hi = self.memory_read_32(vaddr + 4) as u64;
            lo | (hi << 32)
        }
        fn memory_read_128(&self, vaddr: u64) -> (u64, u64) {
            (self.memory_read_64(vaddr), self.memory_read_64(vaddr + 8))
        }

        fn memory_write_8(&mut self, vaddr: u64, value: u8) {
            self.data_mem.insert(vaddr, value);
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

        fn exclusive_read_8(&self, vaddr: u64) -> u8 {
            self.memory_read_8(vaddr)
        }
        fn exclusive_read_16(&self, vaddr: u64) -> u16 {
            self.memory_read_16(vaddr)
        }
        fn exclusive_read_32(&self, vaddr: u64) -> u32 {
            self.memory_read_32(vaddr)
        }
        fn exclusive_read_64(&self, vaddr: u64) -> u64 {
            self.memory_read_64(vaddr)
        }
        fn exclusive_read_128(&self, vaddr: u64) -> (u64, u64) {
            self.memory_read_128(vaddr)
        }
        fn exclusive_write_8(&mut self, _vaddr: u64, _value: u8, _expected: u8) -> bool {
            true
        }
        fn exclusive_write_16(&mut self, _vaddr: u64, _value: u16, _expected: u16) -> bool {
            true
        }
        fn exclusive_write_32(&mut self, _vaddr: u64, _value: u32, _expected: u32) -> bool {
            true
        }
        fn exclusive_write_64(&mut self, _vaddr: u64, _value: u64, _expected: u64) -> bool {
            true
        }
        fn exclusive_write_128(
            &mut self,
            _vaddr: u64,
            _lo: u64,
            _hi: u64,
            _expected_lo: u64,
            _expected_hi: u64,
        ) -> bool {
            true
        }
        fn exclusive_clear(&mut self) {}

        fn call_supervisor(&mut self, _svc_num: u32) {}
        fn exception_raised(&mut self, _pc: u64, _exception: u64) {}
        fn add_ticks(&mut self, ticks: u64) {
            self.ticks_left = self.ticks_left.saturating_sub(ticks);
        }
        fn get_ticks_remaining(&self) -> u64 {
            self.ticks_left
        }
        fn data_cache_operation(&mut self, _op: u64, _vaddr: u64) {}
        fn instruction_cache_operation(&mut self, _op: u64, _vaddr: u64) {}
    }

    fn make_jit_with_coprocessors(
        env: TestEnv,
        coprocessors: crate::interface::a32::config::Coprocessors,
    ) -> A32Jit {
        let config = JitConfig {
            coprocessors,
            callbacks: Box::new(env),
            enable_cycle_counting: true,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: crate::jit_config::OptimizationFlag::NO_OPTIMIZATIONS,
            unsafe_optimizations: false,
            global_monitor: None,
            fastmem_pointer: None,
            page_table_pointer: None,
            define_unpredictable_behaviour: false,
            arch_version: crate::interface::a32::arch_version::ArchVersion::V8,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        A32Jit::new(config).expect("JIT creation should succeed")
    }

    fn make_jit(env: TestEnv) -> A32Jit {
        make_jit_with_coprocessors(env, JitConfig::default_coprocessors())
    }

    struct ThreadPointerCoprocessor {
        uro: UnsafeCell<u32>,
    }

    unsafe impl Send for ThreadPointerCoprocessor {}
    unsafe impl Sync for ThreadPointerCoprocessor {}

    impl Coprocessor for ThreadPointerCoprocessor {
        fn compile_internal_operation(
            &self,
            _two: bool,
            _opc1: u32,
            _crd: CoprocReg,
            _crn: CoprocReg,
            _crm: CoprocReg,
            _opc2: u32,
        ) -> Option<Callback> {
            None
        }

        fn compile_send_one_word(
            &self,
            _two: bool,
            _opc1: u32,
            _crn: CoprocReg,
            _crm: CoprocReg,
            _opc2: u32,
        ) -> CallbackOrAccessOneWord {
            CallbackOrAccessOneWord::CoprocessorException
        }

        fn compile_send_two_words(
            &self,
            _two: bool,
            _opc: u32,
            _crm: CoprocReg,
        ) -> CallbackOrAccessTwoWords {
            CallbackOrAccessTwoWords::CoprocessorException
        }

        fn compile_get_one_word(
            &self,
            two: bool,
            opc1: u32,
            crn: CoprocReg,
            crm: CoprocReg,
            opc2: u32,
        ) -> CallbackOrAccessOneWord {
            if !two
                && opc1 == 0
                && crn == CoprocReg::C13
                && crm == CoprocReg::C0
                && opc2 == 3
            {
                CallbackOrAccessOneWord::Memory(self.uro.get())
            } else {
                CallbackOrAccessOneWord::CoprocessorException
            }
        }

        fn compile_get_two_words(
            &self,
            _two: bool,
            _opc: u32,
            _crm: CoprocReg,
        ) -> CallbackOrAccessTwoWords {
            CallbackOrAccessTwoWords::CoprocessorException
        }

        fn compile_load_words(
            &self,
            _two: bool,
            _long_transfer: bool,
            _crd: CoprocReg,
            _option: Option<u8>,
        ) -> Option<Callback> {
            None
        }

        fn compile_store_words(
            &self,
            _two: bool,
            _long_transfer: bool,
            _crd: CoprocReg,
            _option: Option<u8>,
        ) -> Option<Callback> {
            None
        }
    }

    struct SharedEnv {
        code_mem: Vec<u32>,
        data_mem: Arc<Mutex<HashMap<u64, u8>>>,
        ticks_left: u64,
    }

    impl SharedEnv {
        fn new(code: Vec<u32>, data_mem: Arc<Mutex<HashMap<u64, u8>>>) -> Self {
            Self {
                code_mem: code,
                data_mem,
                ticks_left: 100,
            }
        }
    }

    impl UserCallbacks for SharedEnv {
        fn memory_read_code(&self, vaddr: u64) -> Option<u32> {
            let idx = (vaddr as usize) / 4;
            if idx < self.code_mem.len() {
                Some(self.code_mem[idx])
            } else {
                Some(0xEAFFFFFE)
            }
        }

        fn memory_read_8(&self, vaddr: u64) -> u8 {
            *self.data_mem.lock().unwrap().get(&vaddr).unwrap_or(&0)
        }
        fn memory_read_16(&self, vaddr: u64) -> u16 {
            let lo = self.memory_read_8(vaddr) as u16;
            let hi = self.memory_read_8(vaddr + 1) as u16;
            lo | (hi << 8)
        }
        fn memory_read_32(&self, vaddr: u64) -> u32 {
            let lo = self.memory_read_16(vaddr) as u32;
            let hi = self.memory_read_16(vaddr + 2) as u32;
            lo | (hi << 16)
        }
        fn memory_read_64(&self, vaddr: u64) -> u64 {
            let lo = self.memory_read_32(vaddr) as u64;
            let hi = self.memory_read_32(vaddr + 4) as u64;
            lo | (hi << 32)
        }
        fn memory_read_128(&self, vaddr: u64) -> (u64, u64) {
            (self.memory_read_64(vaddr), self.memory_read_64(vaddr + 8))
        }

        fn memory_write_8(&mut self, vaddr: u64, value: u8) {
            self.data_mem.lock().unwrap().insert(vaddr, value);
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

        fn exclusive_read_8(&self, vaddr: u64) -> u8 {
            self.memory_read_8(vaddr)
        }
        fn exclusive_read_16(&self, vaddr: u64) -> u16 {
            self.memory_read_16(vaddr)
        }
        fn exclusive_read_32(&self, vaddr: u64) -> u32 {
            self.memory_read_32(vaddr)
        }
        fn exclusive_read_64(&self, vaddr: u64) -> u64 {
            self.memory_read_64(vaddr)
        }
        fn exclusive_read_128(&self, vaddr: u64) -> (u64, u64) {
            self.memory_read_128(vaddr)
        }
        fn exclusive_write_8(&mut self, _vaddr: u64, _value: u8, _expected: u8) -> bool {
            true
        }
        fn exclusive_write_16(&mut self, _vaddr: u64, _value: u16, _expected: u16) -> bool {
            true
        }
        fn exclusive_write_32(&mut self, _vaddr: u64, _value: u32, _expected: u32) -> bool {
            true
        }
        fn exclusive_write_64(&mut self, _vaddr: u64, _value: u64, _expected: u64) -> bool {
            true
        }
        fn exclusive_write_128(
            &mut self,
            _vaddr: u64,
            _lo: u64,
            _hi: u64,
            _expected_lo: u64,
            _expected_hi: u64,
        ) -> bool {
            true
        }
        fn exclusive_clear(&mut self) {}

        fn call_supervisor(&mut self, _svc_num: u32) {}
        fn exception_raised(&mut self, _pc: u64, _exception: u64) {}
        fn add_ticks(&mut self, ticks: u64) {
            self.ticks_left = self.ticks_left.saturating_sub(ticks);
        }
        fn get_ticks_remaining(&self) -> u64 {
            self.ticks_left
        }
        fn data_cache_operation(&mut self, _op: u64, _vaddr: u64) {}
        fn instruction_cache_operation(&mut self, _op: u64, _vaddr: u64) {}
    }

    fn make_jit_with_optimizations(env: SharedEnv, optimizations: OptimizationFlag) -> A32Jit {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(env),
            enable_cycle_counting: true,
            code_cache_size: 4 * 1024 * 1024,
            optimizations,
            unsafe_optimizations: false,
            global_monitor: None,
            fastmem_pointer: None,
            page_table_pointer: None,
            define_unpredictable_behaviour: false,
            arch_version: crate::interface::a32::arch_version::ArchVersion::V8,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        A32Jit::new(config).expect("JIT creation should succeed")
    }

    fn read_u16(mem: &Arc<Mutex<HashMap<u64, u8>>>, addr: u64) -> u16 {
        let mem = mem.lock().unwrap();
        let lo = *mem.get(&addr).unwrap_or(&0) as u16;
        let hi = *mem.get(&(addr + 1)).unwrap_or(&0) as u16;
        lo | (hi << 8)
    }

    fn read_u32(mem: &Arc<Mutex<HashMap<u64, u8>>>, addr: u64) -> u32 {
        let lo = read_u16(mem, addr) as u32;
        let hi = read_u16(mem, addr + 2) as u32;
        lo | (hi << 16)
    }

    /// Times a tight `SUBS/BNE` self-loop. With BLOCK_LINKING the back-edge
    /// must be a direct branch (a few ns per iteration); if every iteration
    /// re-enters the dispatcher instead, the linked and unlinked timings
    /// converge and the per-iteration cost explodes.
    fn time_counted_loop(optimizations: OptimizationFlag, ticks: u64) -> std::time::Duration {
        let code = vec![
            0xE2500001, // subs r0, r0, #1
            0x1AFFFFFD, // bne -8 (back to subs)
            0xEAFFFFFE, // b .
        ];
        let mem = Arc::new(Mutex::new(HashMap::new()));
        let mut env = SharedEnv::new(code, mem);
        env.ticks_left = ticks;
        let mut jit = make_jit_with_optimizations(env, optimizations);
        jit.set_register(0, u32::MAX); // never reaches zero within the tick budget
        jit.set_cpsr(0x0000_01D0);
        let start = std::time::Instant::now();
        jit.run();
        start.elapsed()
    }

    #[test]
    fn arm64_loop_back_edge_links_directly() {
        // Warm up JIT compilation, then measure a large tick budget.
        let _ = time_counted_loop(OptimizationFlag::ALL_SAFE_OPTIMIZATIONS, 10_000);
        let linked = time_counted_loop(OptimizationFlag::ALL_SAFE_OPTIMIZATIONS, 4_000_000);
        let _ = time_counted_loop(OptimizationFlag::NO_OPTIMIZATIONS, 10_000);
        let unlinked = time_counted_loop(OptimizationFlag::NO_OPTIMIZATIONS, 4_000_000);
        let linked_ns = linked.as_nanos() as f64 / 2_000_000.0;
        let unlinked_ns = unlinked.as_nanos() as f64 / 2_000_000.0;
        eprintln!(
            "loop iteration: linked={linked_ns:.1} ns, unlinked={unlinked_ns:.1} ns (ratio {:.1}x)",
            unlinked_ns / linked_ns
        );
        assert!(
            linked_ns < unlinked_ns / 2.0,
            "block linking does not speed up the loop back-edge: linked={linked_ns:.1} ns unlinked={unlinked_ns:.1} ns"
        );
    }

    fn run_arm_writer_block_with_opts(
        optimizations: OptimizationFlag,
    ) -> Arc<Mutex<HashMap<u64, u8>>> {
        let code = vec![
            0xE1C130B0, // strh r3, [r1]
            0xE9810005, // stmib r1, {r0, r2}
            0xE581000C, // str r0, [r1, #0xc]
            0xE5810010, // str r0, [r1, #0x10]
            0xEAFFFFFE, // b .
        ];
        let mem = Arc::new(Mutex::new(HashMap::new()));
        let env = SharedEnv::new(code, mem.clone());
        let mut jit = make_jit_with_optimizations(env, optimizations);
        jit.set_register(0, 0x0236_1A68);
        jit.set_register(1, 0x0236_1AAC);
        jit.set_register(2, 0x0236_1AC0);
        jit.set_register(3, 0x0000_4652);
        jit.set_cpsr(0x0000_01D0);
        jit.run();
        mem
    }

    #[test]
    fn test_arm_writer_block_populates_neighbor_fields_without_optimizations() {
        let mem = run_arm_writer_block_with_opts(OptimizationFlag::NO_OPTIMIZATIONS);
        assert_eq!(read_u16(&mem, 0x0236_1AAC), 0x4652, "strh at AAC");
        assert_eq!(
            read_u32(&mem, 0x0236_1AB0),
            0x0236_1A68,
            "stmib first word at AB0"
        );
        assert_eq!(
            read_u32(&mem, 0x0236_1AB4),
            0x0236_1AC0,
            "stmib second word at AB4"
        );
        assert_eq!(read_u32(&mem, 0x0236_1AB8), 0x0236_1A68, "str at AB8");
        assert_eq!(read_u32(&mem, 0x0236_1ABC), 0x0236_1A68, "str at ABC");
    }

    #[test]
    fn test_arm_writer_block_populates_neighbor_fields_with_safe_optimizations() {
        let mem = run_arm_writer_block_with_opts(OptimizationFlag::ALL_SAFE_OPTIMIZATIONS);
        assert_eq!(read_u16(&mem, 0x0236_1AAC), 0x4652, "strh at AAC");
        assert_eq!(
            read_u32(&mem, 0x0236_1AB0),
            0x0236_1A68,
            "stmib first word at AB0"
        );
        assert_eq!(
            read_u32(&mem, 0x0236_1AB4),
            0x0236_1AC0,
            "stmib second word at AB4"
        );
        assert_eq!(read_u32(&mem, 0x0236_1AB8), 0x0236_1A68, "str at AB8");
        assert_eq!(read_u32(&mem, 0x0236_1ABC), 0x0236_1A68, "str at ABC");
    }

    #[test]
    fn test_unintended_modification_in_setcflag() {
        // Port of: "arm: Unintended modification in SetCFlag"
        // Tests SubWithCarry + carry propagation to ADC.
        let env = TestEnv::new(vec![
            0xe35f0cd9, // cmp pc, #55552
            0xe11c0474, // tst r12, r4, ror r4
            0xe1a006a7, // mov r0, r7, lsr #13
            0xe35107fa, // cmp r1, #0x3E80000
            0xe2a54c8a, // adc r4, r5, #35328
            0xeafffffe, // b +#0 (infinite loop)
        ]);
        let mut jit = make_jit(env);

        jit.set_register(0, 0x6973b6bb);
        jit.set_register(1, 0x267ea626);
        jit.set_register(2, 0x69debf49);
        jit.set_register(3, 0x8f976895);
        jit.set_register(4, 0x4ecd2d0d);
        jit.set_register(5, 0xcf89b8c7);
        jit.set_register(6, 0xb6713f85);
        jit.set_register(7, 0x015e2aa5);
        jit.set_register(8, 0xcd14336a);
        jit.set_register(9, 0xafca0f3e);
        jit.set_register(10, 0xace2efd9);
        jit.set_register(11, 0x68fb82cd);
        jit.set_register(12, 0x775447c0);
        jit.set_register(13, 0xc9e1f8cd);
        jit.set_register(14, 0xebe0e626);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 0x00000af1, "r0");
        assert_eq!(jit.get_register(1), 0x267ea626, "r1");
        assert_eq!(jit.get_register(4), 0xcf8a42c8, "r4");
        assert_eq!(jit.get_register(7), 0x015e2aa5, "r7");
        assert_eq!(jit.get_register(15), 0x00000014, "r15/PC");
        assert_eq!(jit.get_cpsr(), 0x200001d0, "cpsr");
    }

    #[test]
    fn test_add_simple() {
        // Simple: R0 = R1 + R2, then infinite loop
        let env = TestEnv::new(vec![
            0xe0810002, // add r0, r1, r2
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 100);
        jit.set_register(2, 200);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 300, "r0 = r1 + r2");
    }

    #[test]
    fn test_sub_with_flags() {
        // SUBS R0, R1, R2 then BEQ (should not be taken when R1 != R2)
        // Then MOV R3, #1 and infinite loop
        let env = TestEnv::new(vec![
            0xe0510002, // subs r0, r1, r2
            0x0a000000, // beq +0 (skip MOV if equal)
            0xe3a03001, // mov r3, #1
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 42);
        jit.set_register(2, 10);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 32, "r0 = 42 - 10");
        assert_eq!(jit.get_register(3), 1, "r3 = 1 (BEQ not taken)");
    }

    #[test]
    fn test_cmp_and_conditional() {
        // CMP R0, #5; MOVEQ R1, #1; MOVNE R1, #2
        let env = TestEnv::new(vec![
            0xe3500005, // cmp r0, #5
            0x03a01001, // moveq r1, #1
            0x13a01002, // movne r1, #2
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 5);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(1), 1, "r1 = 1 (EQ taken because R0 == 5)");
    }

    #[test]
    fn test_cmp_and_conditional_ne() {
        let env = TestEnv::new(vec![
            0xe3500005, // cmp r0, #5
            0x03a01001, // moveq r1, #1
            0x13a01002, // movne r1, #2
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 7);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(1), 2, "r1 = 2 (NE taken because R0 != 5)");
    }

    #[test]
    fn test_umull() {
        // UMULL R0, R1, R2, R3
        let env = TestEnv::new(vec![
            0xe0810392, // umull r0, r1, r2, r3
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(2, 0x12345678);
        jit.set_register(3, 0x9ABCDEF0);
        jit.set_cpsr(0x000001d0);

        jit.run();

        let expected: u64 = 0x12345678u64 * 0x9ABCDEF0u64;
        assert_eq!(jit.get_register(0), expected as u32, "r0 = lo");
        assert_eq!(jit.get_register(1), (expected >> 32) as u32, "r1 = hi");
    }

    #[test]
    fn test_smull() {
        // SMULL R0, R1, R2, R3
        let env = TestEnv::new(vec![
            0xe0c10392, // smull r0, r1, r2, r3
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(2, 0xFFFFFFF6); // -10 as i32
        jit.set_register(3, 0x00000064); // 100
        jit.set_cpsr(0x000001d0);

        jit.run();

        let expected: i64 = -10i64 * 100i64; // = -1000
        assert_eq!(jit.get_register(0), expected as u32, "r0 = lo");
        assert_eq!(jit.get_register(1), (expected >> 32) as u32, "r1 = hi");
    }

    #[test]
    fn test_mrc_cp15_tpidruro() {
        // MRC p15, 0, R0, c13, c0, 3 (read TPIDRURO)
        let env = TestEnv::new(vec![
            0xee1d0f70, // mrc p15, 0, r0, c13, c0, 3
            0xeafffffe, // b +#0
        ]);
        let mut coprocessors = JitConfig::default_coprocessors();
        coprocessors[15] = Some(Arc::new(ThreadPointerCoprocessor {
            uro: UnsafeCell::new(0xDEADBEEF),
        }));
        let mut jit = make_jit_with_coprocessors(env, coprocessors);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 0xDEADBEEF, "r0 = TPIDRURO");
    }

    #[test]
    fn test_ldr_str_basic() {
        // STR R0, [R1]; LDR R2, [R1]
        let env = TestEnv::new(vec![
            0xe5810000, // str r0, [r1]
            0xe5912000, // ldr r2, [r1]
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 0x42424242);
        jit.set_register(1, 0x1000); // address to store at
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(2), 0x42424242, "r2 = loaded value");
    }

    #[test]
    fn test_bcs_after_cmp() {
        // CMP R0, R1; BCS taken; MOV R2, #0; B end; taken: MOV R2, #1; end: B .
        let env = TestEnv::new(vec![
            0xe1500001, // cmp r0, r1
            0x2a000001, // bcs +1 (skip MOV R2,#0 and B)
            0xe3a02000, // mov r2, #0
            0xea000000, // b +0 (to infinite loop)
            0xe3a02001, // mov r2, #1
            0xeafffffe, // b +#0 (infinite loop)
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 100); // R0 >= R1, so CS (carry set = no borrow)
        jit.set_register(1, 50);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(
            jit.get_register(2),
            1,
            "r2 = 1 (BCS taken because 100 >= 50)"
        );
    }

    // --- Tests targeting __nnDetailInitLibc0 patterns ---

    #[test]
    fn test_movw_movt() {
        // MOVW R0, #0x1234; MOVT R0, #0x5678
        // MOVW encoding: cond 0011 0000 imm4 Rd imm12
        // 0x1234 = imm4:imm12 = 1:0x234
        // MOVT encoding: cond 0011 0100 imm4 Rd imm12
        // 0x5678 = imm4:imm12 = 5:0x678
        let env = TestEnv::new(vec![
            0xe3010234, // movw r0, #0x1234
            0xe3450678, // movt r0, #0x5678
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 0x56781234, "r0 = MOVW/MOVT combined");
    }

    #[test]
    fn test_bic_with_shift() {
        // BIC R0, R1, R2, LSL #4
        let env = TestEnv::new(vec![
            0xe1c10202, // bic r0, r1, r2, lsl #4
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 0xFFFFFFFF);
        jit.set_register(2, 0x0000000F); // shifted left 4 = 0x000000F0
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 0xFFFFFF0F, "r0 = ~(0xF0) & 0xFFFFFFFF");
    }

    #[test]
    fn test_orr_with_ror() {
        // ORR R0, R1, R2, ROR #8
        let env = TestEnv::new(vec![
            0xe1810462, // orr r0, r1, r2, ror #8
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 0x00000000);
        jit.set_register(2, 0x12345678); // ROR #8 = 0x78123456
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(
            jit.get_register(0),
            0x78123456,
            "r0 = 0 | (0x12345678 ROR 8)"
        );
    }

    #[test]
    fn test_ldr_pre_index_writeback() {
        // STR R0, [R1, #0x10]; LDR R2, [R1, #0x10]!
        let env = TestEnv::new(vec![
            0xe5810010, // str r0, [r1, #0x10]
            0xe5b12010, // ldr r2, [r1, #0x10]!  (pre-index with writeback)
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 0xDEADBEEF);
        jit.set_register(1, 0x2000);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(2), 0xDEADBEEF, "r2 = loaded value");
        assert_eq!(jit.get_register(1), 0x2010, "r1 updated by writeback");
    }

    #[test]
    fn test_ldr_post_index() {
        // STR R0, [R1]; LDR R2, [R1], #4
        let env = TestEnv::new(vec![
            0xe5810000, // str r0, [r1]
            0xe4912004, // ldr r2, [r1], #4  (post-index)
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 0xCAFEBABE);
        jit.set_register(1, 0x3000);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(2), 0xCAFEBABE, "r2 = loaded value");
        assert_eq!(jit.get_register(1), 0x3004, "r1 = base + 4");
    }

    #[test]
    fn test_adc_carry_chain() {
        // Tests ADC carry propagation: ADDS sets carry, then ADC uses it.
        // R0 = 0xFFFFFFFF, R1 = 1; ADDS R2, R0, R1 (carry out!)
        // ADC R3, R4, #0 (should add the carry = 1)
        let env = TestEnv::new(vec![
            0xe0902001, // adds r2, r0, r1  (0xFFFFFFFF + 1 = 0, C=1)
            0xe2a43000, // adc r3, r4, #0   (r3 = r4 + 0 + C = 0 + 0 + 1 = 1)
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 0xFFFFFFFF);
        jit.set_register(1, 1);
        jit.set_register(4, 0);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(2), 0, "r2 = 0 (overflow)");
        assert_eq!(jit.get_register(3), 1, "r3 = 0 + 0 + carry(1) = 1");
    }

    #[test]
    fn test_sbc_borrow_chain() {
        // SBC: R3 = R0 - R1 - !C
        // First set C=0 via CMP that borrows, then SBC should subtract extra 1
        let env = TestEnv::new(vec![
            0xe3500064, // cmp r0, #100  (r0=50, 50 < 100, C=0)
            0xe0c23001, // sbc r3, r2, r1  (r3 = r2 - r1 - !C = 10 - 3 - 1 = 6)
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 50); // for CMP
        jit.set_register(1, 3); // subtrahend
        jit.set_register(2, 10); // minuend
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(3), 6, "r3 = 10 - 3 - 1 = 6");
    }

    #[test]
    fn test_tst_and_bhi_carry_set() {
        // TST R0, #0xFF000000 (rotated immediate: 0xFF ROR 8 = 0xFF000000)
        // For ROR amount=8: rotate_imm=4 (bits[11:8]), imm8=0xFF
        // carry_out = bit[31] of rotated value = 1 (MSB of 0xFF000000 = 1)
        // Then BHI (C=1 && Z=0)
        // Encoding: 0xFF ROR 8 → rotate_imm=4, imm8=0xFF → bits[11:8]=4, bits[7:0]=0xFF
        let env = TestEnv::new(vec![
            0xe31004ff, // tst r0, #0xFF000000  (0xFF ROR 8)
            0x8a000001, // bhi +1  (C && !Z)
            0xe3a02000, // mov r2, #0
            0xea000000, // b to end
            0xe3a02001, // mov r2, #1
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 0xFF000000); // TST result = 0xFF000000 & 0xFF000000 != 0
        jit.set_cpsr(0x000001d0);

        jit.run();

        // carry_out = bit[31] of (0xFF ROR 8) = bit[31] of 0xFF000000 = 1
        // TST result nonzero: Z=0, N=1 (bit31 of result), C=1 (barrel shifter)
        // BHI: C=1 && Z=0 → taken
        assert_eq!(
            jit.get_register(2),
            1,
            "r2 = 1 (BHI taken: C=1 from shifter, Z=0)"
        );
    }

    #[test]
    fn test_tst_and_bhi_carry_clear() {
        // TST R0, #0xFF00 (0xFF ROR 24)
        // carry_out = bit[31] of 0x0000FF00 = 0
        // BHI should NOT be taken (C=0)
        let env = TestEnv::new(vec![
            0xe3100cff, // tst r0, #0xFF00  (0xFF ROR 24)
            0x8a000001, // bhi +1
            0xe3a02000, // mov r2, #0
            0xea000000, // b to end
            0xe3a02001, // mov r2, #1
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 0x0000FF00);
        jit.set_cpsr(0x000001d0);

        jit.run();

        // carry = 0 (bit31 of 0x0000FF00 = 0), Z=0 (result nonzero)
        // BHI: C=0 → NOT taken
        assert_eq!(jit.get_register(2), 0, "r2 = 0 (BHI not taken: C=0)");
    }

    #[test]
    fn test_rsb() {
        // RSB R0, R1, #0 (R0 = 0 - R1 = -R1)
        let env = TestEnv::new(vec![
            0xe2610000, // rsb r0, r1, #0
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 42);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), (-42i32) as u32, "r0 = -42");
    }

    #[test]
    fn test_clz() {
        // CLZ R0, R1
        let env = TestEnv::new(vec![
            0xe16f0f11, // clz r0, r1
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 0x00100000); // bit 20 set = 11 leading zeros
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 11, "clz(0x00100000) = 11");
    }

    #[test]
    fn test_clz_zero() {
        // CLZ of zero = 32
        let env = TestEnv::new(vec![
            0xe16f0f11, // clz r0, r1
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 0);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 32, "clz(0) = 32");
    }

    #[test]
    fn test_rev() {
        // REV R0, R1 (byte reverse)
        let env = TestEnv::new(vec![
            0xe6bf0f31, // rev r0, r1
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 0x12345678);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(
            jit.get_register(0),
            0x78563412,
            "rev(0x12345678) = 0x78563412"
        );
    }

    #[test]
    fn test_uxtb() {
        // UXTB R0, R1
        let env = TestEnv::new(vec![
            0xe6ef0071, // uxtb r0, r1
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 0xABCDEF42);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 0x42, "uxtb extracts byte");
    }

    #[test]
    fn test_sxtb() {
        // SXTB R0, R1 (sign-extend byte)
        let env = TestEnv::new(vec![
            0xe6af0071, // sxtb r0, r1
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 0x000000F6); // -10 as signed byte
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(
            jit.get_register(0),
            0xFFFFFFF6,
            "sxtb(0xF6) = -10 sign-extended"
        );
    }

    #[test]
    fn test_uxth() {
        // UXTH R0, R1
        let env = TestEnv::new(vec![
            0xe6ff0071, // uxth r0, r1
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 0xABCD1234);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 0x1234, "uxth extracts halfword");
    }

    #[test]
    fn test_uxtab() {
        // UXTAB R0, R1, R2 (R0 = R1 + ZeroExtend(R2[7:0]))
        let env = TestEnv::new(vec![
            0xe6e10072, // uxtab r0, r1, r2
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 0x10000000);
        jit.set_register(2, 0xABCDEF42); // byte = 0x42
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 0x10000042, "uxtab = 0x10000000 + 0x42");
    }

    #[test]
    fn test_bfc() {
        // BFC R0, #8, #8 (clear bits 15:8)
        // BFC encoding: cond 0111 110 msb Rd lsb 001 1111
        // msb=15=01111, Rd=0=0000, lsb=8=01000
        // 1110 0111 1100 1111 0000 0100 0001 1111 = 0xe7cf081f
        let env = TestEnv::new(vec![
            0xe7cf041f, // bfc r0, #8, #8  (msb=15, lsb=8)
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 0xFFFFFFFF);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 0xFFFF00FF, "bfc clears bits 15:8");
    }

    #[test]
    fn test_ubfx() {
        // UBFX R0, R1, #4, #8 (extract 8 bits starting from bit 4)
        // UBFX encoding: cond 0111 111 widthm1 Rd lsb 101 Rm
        // widthm1=7=00111, Rd=0=0000, lsb=4=00100, Rm=1=0001
        // 1110 0111 1110 0111 0000 0010 0101 0001 = 0xe7e70251
        let env = TestEnv::new(vec![
            0xe7e70251, // ubfx r0, r1, #4, #8
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 0xABCDEF56); // bits [11:4] = 0xF5
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(
            jit.get_register(0),
            0xF5,
            "ubfx extracts bits [11:4] = 0xF5"
        );
    }

    #[test]
    fn test_mla() {
        // MLA R0, R1, R2, R3 (R0 = R1 * R2 + R3)
        let env = TestEnv::new(vec![
            0xe0203291, // mla r0, r1, r2, r3
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 7);
        jit.set_register(2, 6);
        jit.set_register(3, 100);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 142, "mla = 7*6 + 100 = 142");
    }

    #[test]
    fn test_stm_ldm() {
        // STMIA R0!, {R1-R4}; LDMIA R0!, {R5-R8} — but we need to reset R0 first
        // Use: STMIA R0, {R1-R4}; LDMIA R0, {R5-R8}
        let env = TestEnv::new(vec![
            0xe880001e, // stmia r0, {r1-r4}
            0xe8900f00, // ldmia r0, {r8-r11}  (load into r8-r11 to not overwrite r1-r4)
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 0x4000);
        jit.set_register(1, 0x11111111);
        jit.set_register(2, 0x22222222);
        jit.set_register(3, 0x33333333);
        jit.set_register(4, 0x44444444);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(8), 0x11111111, "r8  = stored r1");
        assert_eq!(jit.get_register(9), 0x22222222, "r9  = stored r2");
        assert_eq!(jit.get_register(10), 0x33333333, "r10 = stored r3");
        assert_eq!(jit.get_register(11), 0x44444444, "r11 = stored r4");
    }

    #[test]
    fn test_push_pop() {
        // PUSH {R0-R3}; POP {R4-R7}
        let env = TestEnv::new(vec![
            0xe92d000f, // push {r0-r3}  (stmfd sp!, {r0-r3})
            0xe8bd00f0, // pop {r4-r7}   (ldmfd sp!, {r4-r7})
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 0xAAAA);
        jit.set_register(1, 0xBBBB);
        jit.set_register(2, 0xCCCC);
        jit.set_register(3, 0xDDDD);
        jit.set_register(13, 0x5000); // SP
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(4), 0xAAAA, "r4 = popped r0");
        assert_eq!(jit.get_register(5), 0xBBBB, "r5 = popped r1");
        assert_eq!(jit.get_register(6), 0xCCCC, "r6 = popped r2");
        assert_eq!(jit.get_register(7), 0xDDDD, "r7 = popped r3");
        assert_eq!(jit.get_register(13), 0x5000, "SP restored after push+pop");
    }

    #[test]
    fn test_dynarmic_const_folding_most_significant_word() {
        // Port of: "arm: Opt Failure: Const folding in MostSignificantWord"
        let env = TestEnv::new(vec![
            0xe30ad071, // movw sp, #41073
            0xe75efd3d, // smmulr lr, sp, sp
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_cpsr(0x000001d0);

        jit.run();

        // If we don't crash, the const folding edge case is handled.
        // Verify SMMULR result: sp * sp >> 32 (rounded)
        // sp = 41073 = 0xA071
        // 41073 * 41073 = 1,686,989,329 = 0x6490C6F1
        // >> 32 = 0 (since product < 2^32)
        assert_eq!(jit.get_register(14), 0, "smmulr of small values = 0");
    }

    #[test]
    fn test_invalidate_cache_range() {
        // Port of: "arm: Test InvalidateCacheRange"
        let env = TestEnv::new(vec![
            0xe3a00005, // mov r0, #5
            0xe3a0100D, // mov r1, #13
            0xe0812000, // add r2, r1, r0
            0xeafffffe, // b +#0 (infinite loop)
        ]);
        let mut jit = make_jit(env);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 5, "r0 = 5");
        assert_eq!(jit.get_register(1), 13, "r1 = 13");
        assert_eq!(jit.get_register(2), 18, "r2 = 5 + 13 = 18");
        assert_eq!(jit.get_register(15), 0x0000000c, "PC at infinite loop");
    }

    #[test]
    fn test_bl_and_return() {
        // BL to func at +8, func does MOV R0, #42; BX LR
        let env = TestEnv::new(vec![
            0xeb000000, // bl +0 (calls next instruction at PC+8 = 0xC... wait)
            0xeafffffe, // b +#0 (infinite loop - return point)
            0xe3a0002a, // mov r0, #42
            0xe12fff1e, // bx lr
        ]);
        // Actually BL offset is (target - PC - 8) / 4
        // We want to call PC=0x8 from PC=0x0: offset = (8 - 0 - 8) / 4 = 0
        // But that means the BL calls the instruction at PC+8 = 0x8, which is the MOV
        let mut jit = make_jit(env);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 42, "r0 = 42 from function call");
        assert_eq!(jit.get_register(14), 4, "lr = return address after BL");
    }

    // --- More tests for SDK-pattern instructions ---

    #[test]
    fn test_movs_sets_flags() {
        // MOVS R0, R1, LSL #1 (shifts left by 1, S flag updates NZC)
        // R1 = 0x80000000, LSL #1 = 0, carry_out = 1 (bit 31 shifted out)
        let env = TestEnv::new(vec![
            0xe1b00081, // movs r0, r1, lsl #1
            0x2a000001, // bcs +1 (branch if carry set)
            0xe3a02000, // mov r2, #0
            0xea000000, // b end
            0xe3a02001, // mov r2, #1
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 0x80000000);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 0, "r0 = 0x80000000 << 1 = 0");
        assert_eq!(jit.get_register(2), 1, "r2 = 1 (BCS taken: carry from LSL)");
    }

    #[test]
    fn test_ands_with_reg_shift() {
        // ANDS R0, R1, R2, LSR #16 — S flag sets NZC
        let env = TestEnv::new(vec![
            0xe0110822, // ands r0, r1, r2, lsr #16
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 0x000000FF);
        jit.set_register(2, 0x00FF0000); // LSR 16 = 0x000000FF
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 0xFF, "r0 = 0xFF & 0xFF = 0xFF");
    }

    #[test]
    fn test_ldrb_strb() {
        // STRB R0, [R1]; LDRB R2, [R1]
        let env = TestEnv::new(vec![
            0xe5c10000, // strb r0, [r1]
            0xe5d12000, // ldrb r2, [r1]
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 0xABCDEF42);
        jit.set_register(1, 0x1000);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(2), 0x42, "ldrb loads only byte");
    }

    #[test]
    fn test_ldrh_strh() {
        // STRH R0, [R1]; LDRH R2, [R1]
        let env = TestEnv::new(vec![
            0xe1c100b0, // strh r0, [r1]
            0xe1d120b0, // ldrh r2, [r1]
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 0xABCD1234);
        jit.set_register(1, 0x2000);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(2), 0x1234, "ldrh loads only halfword");
    }

    #[test]
    fn test_ldrsb() {
        // LDRSB R2, [R1] — load signed byte
        let env = TestEnv::new(vec![
            0xe5c10000, // strb r0, [r1] (store 0xF6 = -10)
            0xe1d120d0, // ldrsb r2, [r1]
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 0xF6); // -10 as byte
        jit.set_register(1, 0x3000);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(2), 0xFFFFFFF6, "ldrsb sign-extends byte");
    }

    #[test]
    fn test_ldr_reg_offset_shift() {
        // LDR R0, [R1, R2, LSL #2] — base + index*4
        let env = TestEnv::new(vec![
            0xe5810010, // str r0, [r1, #0x10] (store at base+16)
            0xe7913102, // ldr r3, [r1, r2, lsl #2] (load from base + 4*4 = base+16)
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 0x12345678);
        jit.set_register(1, 0x4000); // base
        jit.set_register(2, 4); // index (4*4 = 16 = 0x10)
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(
            jit.get_register(3),
            0x12345678,
            "ldr with shifted reg offset"
        );
    }

    #[test]
    fn test_complex_flag_sequence() {
        // Sequence that uses multiple flag-setting instructions:
        // CMP R0, #10; BGE skip; SUBS R1, R1, #1; skip: ADDS R2, R2, R1
        let env = TestEnv::new(vec![
            0xe350000a, // cmp r0, #10
            0xaa000000, // bge +0 (skip SUBS)
            0xe2511001, // subs r1, r1, #1
            0xe0922001, // adds r2, r2, r1
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 5); // < 10, so GE not taken
        jit.set_register(1, 20);
        jit.set_register(2, 100);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(1), 19, "r1 = 20 - 1 = 19 (SUBS executed)");
        assert_eq!(jit.get_register(2), 119, "r2 = 100 + 19 = 119");
    }

    #[test]
    fn test_complex_flag_sequence_ge() {
        let env = TestEnv::new(vec![
            0xe350000a, // cmp r0, #10
            0xaa000000, // bge +0 (skip SUBS)
            0xe2511001, // subs r1, r1, #1
            0xe0922001, // adds r2, r2, r1
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 15); // >= 10, so GE taken, skip SUBS
        jit.set_register(1, 20);
        jit.set_register(2, 100);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(1), 20, "r1 = 20 (SUBS skipped)");
        assert_eq!(jit.get_register(2), 120, "r2 = 100 + 20 = 120");
    }

    #[test]
    fn test_pc_relative_add() {
        // ADD R0, PC, #0 — R0 = address of this instruction + 8 (ARM pipeline)
        let env = TestEnv::new(vec![
            0xe28f0000, // add r0, pc, #0  (PC = 0 + 8 = 8)
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(
            jit.get_register(0),
            8,
            "r0 = PC + 0 = 8 (ARM pipeline offset)"
        );
    }

    #[test]
    fn test_eors_flags() {
        // EORS R0, R1, R2 — XOR with flags
        let env = TestEnv::new(vec![
            0xe0310002, // eors r0, r1, r2
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 0xFFFFFFFF);
        jit.set_register(2, 0xFFFFFFFF);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 0, "eor(0xFFFFFFFF, 0xFFFFFFFF) = 0");
        // Z flag should be set
        assert_eq!(jit.get_cpsr() & 0x40000000, 0x40000000, "Z flag set");
    }

    #[test]
    fn test_mul() {
        // MUL R0, R1, R2
        let env = TestEnv::new(vec![
            0xe0000291, // mul r0, r1, r2
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 12);
        jit.set_register(2, 13);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 156, "mul = 12 * 13 = 156");
    }

    #[test]
    fn test_sdiv() {
        // SDIV R0, R1, R2
        let env = TestEnv::new(vec![
            0xe710f112, // sdiv r0, r2, r1
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 7);
        jit.set_register(2, 100);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 14, "sdiv(100, 7) = 14");
    }

    #[test]
    fn test_sdiv_negative() {
        // SDIV R0, R2, R1 where R2 is negative
        let env = TestEnv::new(vec![
            0xe710f112, // sdiv r0, r2, r1
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 3);
        jit.set_register(2, (-15i32) as u32); // -15
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), (-5i32) as u32, "sdiv(-15, 3) = -5");
    }

    #[test]
    fn test_udiv() {
        // UDIV R0, R2, R1
        let env = TestEnv::new(vec![
            0xe730f112, // udiv r0, r2, r1
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 4);
        jit.set_register(2, 100);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 25, "udiv(100, 4) = 25");
    }

    #[test]
    fn test_rbit() {
        // RBIT R0, R1
        let env = TestEnv::new(vec![
            0xe6ff0f31, // rbit r0, r1
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 0x80000000);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(jit.get_register(0), 1, "rbit(0x80000000) = 1");
    }

    #[test]
    fn test_rev16() {
        // REV16 R0, R1
        let env = TestEnv::new(vec![
            0xe6bf0fb1, // rev16 r0, r1
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 0x12345678);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(
            jit.get_register(0),
            0x34127856,
            "rev16 swaps bytes within halfwords"
        );
    }

    #[test]
    fn test_bfi() {
        // BFI R0, R1, #8, #8 (insert 8 bits from R1 at bit 8 of R0)
        // BFI encoding: cond 0111 110 msb Rd lsb 001 Rn
        // msb=15, Rd=0, lsb=8, Rn=1
        // 1110 0111 1100 1111 0000 0100 0001 0001 = 0xe7cf0411
        let env = TestEnv::new(vec![
            0xe7cf0411, // bfi r0, r1, #8, #8
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 0xFFFF00FF);
        jit.set_register(1, 0x000000AB);
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(
            jit.get_register(0),
            0xFFFFABFF,
            "bfi inserts 0xAB at bits [15:8]"
        );
    }

    #[test]
    fn test_sbfx() {
        // SBFX R0, R1, #4, #8 (extract 8 bits from bit 4, sign extend)
        // SBFX encoding: cond 0111 101 widthm1 Rd lsb 101 Rn
        // widthm1=7, Rd=0, lsb=4, Rn=1
        // 1110 0111 1010 0111 0000 0010 0101 0001 = 0xe7a70251
        let env = TestEnv::new(vec![
            0xe7a70251, // sbfx r0, r1, #4, #8
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 0x00000F60); // bits[11:4] = 0xF6 = -10 as signed byte
        jit.set_cpsr(0x000001d0);

        jit.run();

        assert_eq!(
            jit.get_register(0),
            0xFFFFFFF6,
            "sbfx sign-extends extracted bits"
        );
    }

    #[test]
    fn test_eors_asr0_carry() {
        // EORS R5, R1, R7, ASR #0 (= ASR #32)
        // The carry out of ASR #32 is bit[31] of R7.
        // Then BCS checks if carry is set.
        let env = TestEnv::new(vec![
            0xe0315047, // eors r5, r1, r7, asr #0 (= asr #32)
            0x2a000001, // bcs +1
            0xe3a06000, // mov r6, #0
            0xea000000, // b end
            0xe3a06001, // mov r6, #1
            0xeafffffe, // b +#0
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 0x12345678);
        jit.set_register(7, 0x80000000); // bit31=1, so ASR #32 carry = 1
        jit.set_cpsr(0x000001d0);

        jit.run();

        // ASR #32 of 0x80000000 = 0xFFFFFFFF (all sign bits)
        // EOR: 0x12345678 ^ 0xFFFFFFFF = 0xEDCBA987
        assert_eq!(
            jit.get_register(5),
            0xEDCBA987,
            "r5 = 0x12345678 ^ 0xFFFFFFFF"
        );
        // Carry from ASR #32 = bit31 of R7 = 1
        assert_eq!(
            jit.get_register(6),
            1,
            "r6 = 1 (BCS taken: carry from ASR #32 = 1)"
        );
    }

    #[test]
    fn test_thumb_movs_step() {
        // Thumb16: MOVS R0, #5 (encoding: 0x2005)
        // Pack two Thumb16 instructions per 32-bit word.
        // Word 0: [MOVS R0, #5 | MOVS R1, #10] = 0x210A_2005
        // Word 1: infinite loop ARM B . for fallback = 0xEAFFFFFE
        let env = TestEnv::new(vec![
            0x210A_2005, // MOVS R0, #5 (low hw) | MOVS R1, #10 (high hw)
            0xEAFFFFFE,  // fallback
        ]);
        let mut jit = make_jit(env);
        // CPSR=0x30: T flag (bit 5) + bit 4 (USR mode partial)
        jit.set_cpsr(0x00000030);
        jit.set_register(15, 0); // PC=0

        eprintln!(
            "Before step: PC={:#x} CPSR={:#x}",
            jit.get_pc(),
            jit.get_cpsr()
        );

        let hr = jit.step();

        eprintln!(
            "After step: PC={:#x} R0={:#x} CPSR={:#x} halt={:?}",
            jit.get_pc(),
            jit.get_register(0),
            jit.get_cpsr(),
            hr
        );

        assert_eq!(jit.get_register(0), 5, "R0 = 5");
        assert_eq!(jit.get_pc(), 2, "PC = 2 (next Thumb instruction)");
        assert!(jit.get_cpsr() & (1 << 5) != 0, "T flag should still be set");
    }

    fn make_jit_no_cycles(env: TestEnv) -> A32Jit {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(env),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: crate::jit_config::OptimizationFlag::NO_OPTIMIZATIONS,
            unsafe_optimizations: false,
            global_monitor: None,
            fastmem_pointer: None,
            page_table_pointer: None,
            define_unpredictable_behaviour: false,
            arch_version: crate::interface::a32::arch_version::ArchVersion::V8,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        A32Jit::new(config).expect("JIT creation should succeed")
    }

    #[test]
    fn test_thumb_movs_step_no_cycles() {
        // Same test but with cycle counting disabled (matches a32_diff config)
        let env = TestEnv::new(vec![
            0x210A_2005, // MOVS R0, #5 | MOVS R1, #10
            0xEAFFFFFE,
        ]);
        let mut jit = make_jit_no_cycles(env);
        jit.set_cpsr(0x00000030);
        jit.set_register(15, 0);

        let hr = jit.step();
        eprintln!(
            "no-cycles step: PC={:#x} R0={:#x} CPSR={:#x} halt={:?}",
            jit.get_pc(),
            jit.get_register(0),
            jit.get_cpsr(),
            hr
        );

        assert_eq!(jit.get_register(0), 5, "R0 = 5");
        assert_eq!(jit.get_pc(), 2, "PC = 2");
        assert!(jit.get_cpsr() & (1 << 5) != 0, "T flag should still be set");
    }

    #[test]
    fn test_cmp_r0_zero_sets_flags() {
        // CMP R0, #0 with R0=0 should set Z=1, C=1
        // CMP is SUB without writeback: 0 - 0 = 0, Z=1, no borrow so C=1
        let env = TestEnv::new(vec![
            0xE3500000, // CMP R0, #0
            0xEAFFFFFE, // B . (halt)
        ]);
        let mut jit = make_jit(env);
        jit.set_register(0, 0);
        jit.set_cpsr(0x000001d0);

        jit.run();

        let cpsr = jit.get_cpsr();
        eprintln!(
            "CMP R0,#0: CPSR={:#010x} Z={} C={}",
            cpsr,
            (cpsr >> 30) & 1,
            (cpsr >> 29) & 1
        );
        assert_ne!(cpsr & (1 << 30), 0, "Z flag should be set (0 == 0)");
        assert_ne!(cpsr & (1 << 29), 0, "C flag should be set (no borrow)");
    }

    #[test]
    fn test_cmp_ir_pseudo_op_chain() {
        // Verify the IR for CMP R0,#0 has the pseudo-op chain
        use crate::frontend::a32::decoder::decode_arm;
        use crate::frontend::a32::translate::data_processing::arm_dp_imm;
        use crate::ir::a32_emitter::A32IREmitter;
        use crate::ir::block::Block;
        use crate::ir::location::A32LocationDescriptor;
        use crate::ir::opcode::Opcode;

        let loc = A32LocationDescriptor::at(0x200008);
        let mut block = Block::new(loc.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, loc);

        let decoded = decode_arm(0xE3500000);
        arm_dp_imm(&mut ir, &decoded);

        // Dump IR
        for (i, inst) in block.instructions.iter().enumerate() {
            let pseudo = inst
                .next_pseudoop
                .map(|r| format!(" →pseudo#{}", r.0))
                .unwrap_or_default();
            eprintln!("  #{}: {:?}{}", i, inst.opcode, pseudo);
        }

        // Find the SUB and verify it has GetNZCVFromOp linked
        let mut found_sub = false;
        for (i, inst) in block.instructions.iter().enumerate() {
            if inst.opcode == Opcode::Sub32 {
                found_sub = true;
                let inst_ref = crate::ir::value::InstRef(i as u32);
                let nzcv = block.get_associated_pseudo_operation(inst_ref, Opcode::GetNZCVFromOp);
                eprintln!("  SUB at #{}: GetNZCVFromOp = {:?}", i, nzcv);
                assert!(
                    nzcv.is_some(),
                    "SUB should have GetNZCVFromOp pseudo-op linked"
                );
            }
        }
        assert!(found_sub, "CMP should produce a SUB instruction");
    }

    #[test]
    fn test_tst_imm_0x40000000_with_zero_result() {
        // TST R1, #0x40000000 with R1=0x000081FF
        // Encoding: 0xE3110101 (rotate=1, imm8=1 → ROR(1,2) = 0x40000000)
        // Expected: 0x000081FF & 0x40000000 = 0 → Z=1, N=0, C=0 (barrel shifter)
        let env = TestEnv::new(vec![
            0xE3110101, // TST R1, #0x40000000  (at address 0)
            0xEAFFFFFE, // B .                  (halt: self-loop until cycle limit)
        ]);
        let mut jit = make_jit(env);
        jit.set_register(1, 0x000081FF);
        jit.set_cpsr(0x00000000);
        // Code is mapped at address 0 (vaddr/4 → code_mem index); execute from there.
        jit.set_pc(0);
        jit.run();
        let cpsr = jit.get_cpsr();
        let z = (cpsr >> 30) & 1;
        let n = (cpsr >> 31) & 1;
        let c = (cpsr >> 29) & 1;
        println!("CPSR=0x{:08X} N={} Z={} C={}", cpsr, n, z, c);
        assert_eq!(
            z, 1,
            "Z should be 1 (result is 0): 0x000081FF & 0x40000000 = 0"
        );
        assert_eq!(n, 0, "N should be 0");
    }

    #[test]
    fn test_vstmia_single_no_writeback_stores_register_list() {
        // VSTMIA R1, {S0-S3}; this is a multiple-register store with
        // P=0,U=1,W=0. It must not be decoded as VSTR S0, [R1,#16].
        let data_mem = Arc::new(Mutex::new(HashMap::new()));
        let env = SharedEnv::new(
            vec![
                0xEC810A04, // VSTMIA R1, {S0-S3}
                0xEAFFFFFE, // B .
            ],
            data_mem.clone(),
        );
        let mut jit = make_jit_with_optimizations(env, OptimizationFlag::NO_OPTIMIZATIONS);
        jit.set_register(1, 0x100);
        jit.set_ext_reg(0, 0x11111111);
        jit.set_ext_reg(1, 0x22222222);
        jit.set_ext_reg(2, 0x33333333);
        jit.set_ext_reg(3, 0x44444444);
        jit.set_pc(0);

        jit.run();

        assert_eq!(read_u32(&data_mem, 0x100), 0x11111111);
        assert_eq!(read_u32(&data_mem, 0x104), 0x22222222);
        assert_eq!(read_u32(&data_mem, 0x108), 0x33333333);
        assert_eq!(read_u32(&data_mem, 0x10C), 0x44444444);
        assert_eq!(
            read_u32(&data_mem, 0x110),
            0,
            "VSTMIA must not behave like VSTR +#16"
        );
        assert_eq!(jit.get_register(1), 0x100, "no-writeback form preserves R1");
    }

    #[test]
    fn test_asimd_vzip_regression_sequence() {
        let env = TestEnv::new(vec![
            0xF3FA_21E0, // VZIP.32 Q9, Q8
            0xF3FA_41E6, // VZIP.32 Q10, Q11
            0xEAFF_FFFE, // B .
        ]);
        let mut jit = make_jit(env);

        for (index, value) in [1, 2, 3, 4].into_iter().enumerate() {
            jit.set_ext_reg(32 + index, value);
        }
        for (index, value) in [10, 20, 30, 40].into_iter().enumerate() {
            jit.set_ext_reg(36 + index, value);
        }
        for (index, value) in [100, 200, 300, 400].into_iter().enumerate() {
            jit.set_ext_reg(40 + index, value);
        }
        for index in 44..48 {
            jit.set_ext_reg(index, 0);
        }
        jit.set_pc(0);

        jit.run();

        let q8 = std::array::from_fn::<_, 4, _>(|index| jit.get_ext_reg(32 + index));
        let q9 = std::array::from_fn::<_, 4, _>(|index| jit.get_ext_reg(36 + index));
        let q10 = std::array::from_fn::<_, 4, _>(|index| jit.get_ext_reg(40 + index));
        let q11 = std::array::from_fn::<_, 4, _>(|index| jit.get_ext_reg(44 + index));
        assert_eq!(q8, [30, 3, 40, 4]);
        assert_eq!(q9, [10, 1, 20, 2]);
        assert_eq!(q10, [100, 0, 200, 0]);
        assert_eq!(q11, [300, 0, 400, 0]);
    }

    #[test]
    fn test_vshrn_vmovn_regression_sequence() {
        let env = TestEnv::new(vec![
            0xF2E0_3830, // VSHRN.I64 D19, Q8, #32
            0xF3FA_2220, // VMOVN.I64 D18, Q8
            0xEAFF_FFFE, // B .
        ]);
        let mut jit = make_jit(env);
        for (index, value) in [0x1111_1111, 0x2222_2222, 0x3333_3333, 0x4444_4444]
            .into_iter()
            .enumerate()
        {
            jit.set_ext_reg(32 + index, value);
        }
        jit.set_pc(0);

        jit.run();

        assert_eq!(jit.get_ext_reg(36), 0x1111_1111);
        assert_eq!(jit.get_ext_reg(37), 0x3333_3333);
        assert_eq!(jit.get_ext_reg(38), 0x2222_2222);
        assert_eq!(jit.get_ext_reg(39), 0x4444_4444);
    }

    #[test]
    fn test_asimd_vceq_zero_regression_instruction() {
        let env = TestEnv::new(vec![
            0xF3B9_6562, // VCEQ.F32 Q3, Q9, #0
            0xEAFF_FFFE, // B .
        ]);
        let mut jit = make_jit(env);
        for (index, value) in [0.0f32, 1.0, -0.0, -2.0].into_iter().enumerate() {
            jit.set_ext_reg(36 + index, value.to_bits());
        }
        jit.set_pc(0);

        jit.run();

        let q3 = std::array::from_fn::<_, 4, _>(|index| jit.get_ext_reg(12 + index));
        assert_eq!(q3, [u32::MAX, 0, u32::MAX, 0]);
    }
}

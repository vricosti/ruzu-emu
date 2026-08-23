use crate::frontend::a64::decoder::{A64InstructionName, DecodedInst};
use crate::frontend::a64::types::{Exception, Reg};
use crate::ir::a64_emitter::A64IREmitter;
use crate::ir::block::Block;
use crate::ir::location::A64LocationDescriptor;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

/// Options controlling translation behavior.
#[derive(Debug, Clone, Default)]
pub struct TranslationOptions {
    /// Hook hint instructions (YIELD, WFE, WFI, SEV, SEVL) as exceptions.
    pub hook_hint_instructions: bool,
    /// Use wall clock for CNTPCT (instead of cycle-accurate).
    pub wall_clock_cntpct: bool,
}

/// Translator visitor: translates decoded ARM64 instructions into IR.
pub struct TranslatorVisitor<'a> {
    pub ir: A64IREmitter<'a>,
    pub options: TranslationOptions,
}

impl<'a> TranslatorVisitor<'a> {
    pub fn new(
        block: &'a mut Block,
        location: A64LocationDescriptor,
        options: TranslationOptions,
    ) -> Self {
        Self {
            ir: A64IREmitter::with_location(block, location),
            options,
        }
    }

    // --- Register access helpers ---

    /// Read a general-purpose register (32 or 64 bit).
    /// R31 reads as zero register (XZR/WZR).
    pub fn x(&mut self, datasize: usize, reg: Reg) -> Value {
        // Mirrors upstream `TranslatorVisitor::X(size_t bitsize, Reg)` in
        // `translate/impl/impl.cpp:83-95`. The 8/16-bit cases return the
        // low byte/half of the 32-bit register read — used by byte/half
        // store paths (STLR/STXR/STLXR/STLLR/STTR with size=00 or 01).
        match datasize {
            8 => {
                let w = if reg == Reg::ZR {
                    self.ir.ir().imm32(0)
                } else {
                    self.ir.get_w(reg)
                };
                self.ir.ir().least_significant_byte(w)
            }
            16 => {
                let w = if reg == Reg::ZR {
                    self.ir.ir().imm32(0)
                } else {
                    self.ir.get_w(reg)
                };
                self.ir.ir().least_significant_half(w)
            }
            32 => {
                if reg == Reg::ZR {
                    self.ir.ir().imm32(0)
                } else {
                    self.ir.get_w(reg)
                }
            }
            64 => {
                if reg == Reg::ZR {
                    self.ir.ir().imm64(0)
                } else {
                    self.ir.get_x(reg)
                }
            }
            _ => panic!("Invalid datasize {}", datasize),
        }
    }

    /// Write a general-purpose register (32 or 64 bit).
    /// R31 writes are discarded (XZR/WZR).
    pub fn set_x(&mut self, datasize: usize, reg: Reg, value: Value) {
        if reg == Reg::ZR {
            return; // discard
        }
        match datasize {
            32 => self.ir.set_w(reg, value),
            64 => self.ir.set_x(reg, value),
            _ => panic!("Invalid datasize {}", datasize),
        }
    }

    /// Read the stack pointer (R31 as SP, not ZR).
    pub fn sp(&mut self, datasize: usize) -> Value {
        match datasize {
            32 => {
                let sp64 = self.ir.get_sp();
                self.ir.ir().least_significant_word(sp64)
            }
            64 => self.ir.get_sp(),
            _ => panic!("Invalid datasize {}", datasize),
        }
    }

    /// Write the stack pointer.
    pub fn set_sp(&mut self, datasize: usize, value: Value) {
        match datasize {
            32 => {
                let ext = self.ir.ir().zero_extend_word_to_long(value);
                self.ir.set_sp(ext);
            }
            64 => self.ir.set_sp(value),
            _ => panic!("Invalid datasize {}", datasize),
        }
    }

    /// Create an immediate of the given datasize.
    pub fn i(&mut self, datasize: usize, imm: u64) -> Value {
        match datasize {
            8 => self.ir.ir().imm8(imm as u8),
            16 => self.ir.ir().imm16(imm as u16),
            32 => self.ir.ir().imm32(imm as u32),
            64 => self.ir.ir().imm64(imm),
            _ => panic!("Invalid datasize {}", datasize),
        }
    }

    // --- Address arithmetic helper ---

    /// Add two 64-bit values (for address calculation, carry_in=false).
    pub(crate) fn addr_add(&mut self, a: Value, b: Value) -> Value {
        let carry = self.ir.ir().imm1(false);
        self.ir.ir().add_64(a, b, carry)
    }

    // --- Load/Store shared helpers ---

    /// Read memory by size.
    pub(crate) fn mem_read(
        &mut self,
        address: Value,
        bytes: usize,
        acc_type: crate::ir::acc_type::AccType,
    ) -> Value {
        match bytes {
            1 => self.ir.read_memory_8(address, acc_type),
            2 => self.ir.read_memory_16(address, acc_type),
            4 => self.ir.read_memory_32(address, acc_type),
            8 => self.ir.read_memory_64(address, acc_type),
            16 => self.ir.read_memory_128(address, acc_type),
            _ => panic!("Invalid memory read size {}", bytes),
        }
    }

    /// Write memory by size.
    pub(crate) fn mem_write(
        &mut self,
        address: Value,
        value: Value,
        bytes: usize,
        acc_type: crate::ir::acc_type::AccType,
    ) {
        match bytes {
            1 => self.ir.write_memory_8(address, value, acc_type),
            2 => self.ir.write_memory_16(address, value, acc_type),
            4 => self.ir.write_memory_32(address, value, acc_type),
            8 => self.ir.write_memory_64(address, value, acc_type),
            16 => self.ir.write_memory_128(address, value, acc_type),
            _ => panic!("Invalid memory write size {}", bytes),
        }
    }

    /// Get base address (Rn == R31 uses SP in load/store context).
    pub(crate) fn base_address(&mut self, rn: Reg) -> Value {
        if rn == Reg::ZR {
            self.sp(64)
        } else {
            self.x(64, rn)
        }
    }

    /// Writeback base register.
    pub(crate) fn writeback_address(&mut self, rn: Reg, address: Value) {
        if rn == Reg::ZR {
            self.set_sp(64, address);
        } else {
            self.set_x(64, rn, address);
        }
    }

    /// Sign or zero extend a loaded value.
    pub(crate) fn sign_or_zero_extend(
        &mut self,
        value: Value,
        from_size: usize,
        to_size: usize,
        signed: bool,
    ) -> Value {
        if from_size >= to_size {
            return value;
        }
        if signed {
            match (from_size, to_size) {
                (8, 32) => self.ir.ir().sign_extend_byte_to_word(value),
                (8, 64) => self.ir.ir().sign_extend_byte_to_long(value),
                (16, 32) => self.ir.ir().sign_extend_half_to_word(value),
                (16, 64) => self.ir.ir().sign_extend_half_to_long(value),
                (32, 64) => self.ir.ir().sign_extend_word_to_long(value),
                _ => value,
            }
        } else {
            match (from_size, to_size) {
                (8, 32) => self.ir.ir().zero_extend_byte_to_word(value),
                (8, 64) => self.ir.ir().zero_extend_byte_to_long(value),
                (16, 32) => self.ir.ir().zero_extend_half_to_word(value),
                (16, 64) => self.ir.ir().zero_extend_half_to_long(value),
                (32, 64) => self.ir.ir().zero_extend_word_to_long(value),
                _ => value,
            }
        }
    }

    /// Read a 32-, 64-, or 128-bit vector register value.
    pub(crate) fn v_read(
        &mut self,
        datasize: usize,
        vec: crate::frontend::a64::types::Vec,
    ) -> Value {
        match datasize {
            32 => self.ir.get_s(vec),
            64 => self.ir.get_d(vec),
            128 => self.ir.get_q(vec),
            _ => panic!("Invalid FP/SIMD vector datasize {}", datasize),
        }
    }

    /// Write a 32-, 64-, or 128-bit vector register value.
    pub(crate) fn v_write(
        &mut self,
        datasize: usize,
        vec: crate::frontend::a64::types::Vec,
        value: Value,
    ) {
        match datasize {
            32 => self.ir.set_s(vec, value),
            64 => {
                let value = self.ir.ir().vector_zero_upper(value);
                self.ir.set_d(vec, value);
            }
            128 => self.ir.set_q(vec, value),
            _ => panic!("Invalid FP/SIMD vector datasize {}", datasize),
        }
    }

    /// Read the low scalar element of a vector register.
    pub(crate) fn v_scalar_read(
        &mut self,
        datasize: usize,
        vec: crate::frontend::a64::types::Vec,
    ) -> Value {
        if datasize == 128 {
            return self.ir.get_q(vec);
        }

        assert!(
            matches!(datasize, 8 | 16 | 32 | 64),
            "Invalid FP/SIMD datasize {}",
            datasize
        );
        let value = self.ir.get_q(vec);
        self.ir.ir().vector_get_element(datasize, value, 0)
    }

    /// Read one 64-bit half of a vector register.
    pub(crate) fn vpart_read_64(
        &mut self,
        vec: crate::frontend::a64::types::Vec,
        part: usize,
    ) -> Value {
        assert!(part <= 1);
        if part == 0 {
            return self.v_read(64, vec);
        }
        let vector = self.ir.get_q(vec);
        let high = self.ir.ir().vector_get_element(64, vector, 1);
        self.ir.ir().zero_extend_to_quad(high)
    }

    /// Write one 64-bit half of a vector register.
    pub(crate) fn vpart_write_64(
        &mut self,
        vec: crate::frontend::a64::types::Vec,
        part: usize,
        value: Value,
    ) {
        assert!(part <= 1);
        if part == 0 {
            let value = self.ir.ir().vector_zero_extend(64, value);
            self.v_write(128, vec, value);
            return;
        }
        let current = self.v_read(128, vec);
        let combined = self.ir.ir().vector_interleave_lower(64, current, value);
        self.v_write(128, vec, combined);
    }

    /// Write a scalar value and zero the remaining vector register bits.
    pub(crate) fn v_scalar_write(
        &mut self,
        datasize: usize,
        vec: crate::frontend::a64::types::Vec,
        value: Value,
    ) {
        if datasize == 128 {
            let value_type = match value {
                Value::Inst(inst_ref) => self.ir.base.block.inst_real_return_type(inst_ref),
                value => value.get_type(),
            };
            assert_eq!(
                value_type,
                crate::ir::types::Type::U128,
                "V(128) write requires a U128 value"
            );
            self.ir.set_q(vec, value);
            return;
        }

        assert!(
            matches!(datasize, 8 | 16 | 32 | 64),
            "Invalid FP/SIMD datasize {}",
            datasize
        );
        let value = self.ir.ir().zero_extend_to_quad(value);
        self.ir.set_q(vec, value);
    }

    // --- Error handlers ---

    /// Fallback: interpret this instruction.
    pub fn interpret_this_instruction(&mut self) -> bool {
        let loc = self.ir.current_location.expect("location not set");
        // RUZU_LOG_INTERPRET_PC=1 — log every PC where translation fell back
        // to the interpret terminal. Useful for finding interpret-fallback
        // instructions in tight loops (each occurrence costs a JIT
        // exit/re-enter at ~10µs/iter, easily wedging boot if hit millions
        // of times).
        if std::env::var_os("RUZU_LOG_INTERPRET_PC").is_some() {
            static COUNTS: std::sync::OnceLock<
                std::sync::Mutex<std::collections::HashMap<u64, u64>>,
            > = std::sync::OnceLock::new();
            let pc = loc.pc();
            let mut counts = COUNTS
                .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
                .lock()
                .unwrap();
            let n = counts.entry(pc).and_modify(|c| *c += 1).or_insert(1);
            // Log first occurrence + powers of 16 (1, 16, 256, 4k, 65k, 1M, 16M, ...)
            // so a hot fallback shows growth without spamming.
            if *n == 1 || n.is_power_of_two() && (*n).trailing_zeros() % 4 == 0 {
                eprintln!("[INTERPRET_FALLBACK] pc=0x{:016X} count={}", pc, *n);
            }
        }
        self.ir.set_term(Terminal::Interpret {
            next: loc.to_location(),
            num_instructions: 1,
        });
        false
    }

    /// Unpredictable instruction — treat as interpret.
    pub fn unpredictable_instruction(&mut self) -> bool {
        self.raise_exception(Exception::UnpredictableInstruction)
    }

    /// Decode error.
    pub fn decode_error(&mut self) -> bool {
        unreachable!("A64 decode_error() reached for a decoded instruction")
    }

    /// Reserved value in instruction encoding.
    pub fn reserved_value(&mut self) -> bool {
        self.raise_exception(Exception::ReservedValue)
    }

    /// Unallocated encoding.
    pub fn unallocated_encoding(&mut self) -> bool {
        self.raise_exception(Exception::UnallocatedEncoding)
    }

    /// Raise an exception.
    pub fn raise_exception(&mut self, exception: Exception) -> bool {
        let loc = self.ir.current_location.expect("location not set");
        let pc_val = self.ir.ir().imm64(loc.pc() + 4);
        self.ir.set_pc(pc_val);
        self.ir.exception_raised(exception);
        self.ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        false
    }

    // --- Instruction dispatch ---

    /// Dispatch a decoded instruction to the appropriate handler.
    /// Returns true to continue translation, false to terminate the block.
    pub fn dispatch(&mut self, inst: &DecodedInst) -> bool {
        use A64InstructionName::*;
        match inst.name {
            // Data processing - Add/Sub immediate
            ADD_imm => self.add_imm(inst),
            ADDS_imm => self.adds_imm(inst),
            SUB_imm => self.sub_imm(inst),
            SUBS_imm => self.subs_imm(inst),

            // Data processing - Add/Sub shifted register
            ADD_shift => self.add_shift(inst),
            ADDS_shift => self.adds_shift(inst),
            SUB_shift => self.sub_shift(inst),
            SUBS_shift => self.subs_shift(inst),

            // Data processing - Add/Sub extended register
            ADD_ext => self.add_ext(inst),
            ADDS_ext => self.adds_ext(inst),
            SUB_ext => self.sub_ext(inst),
            SUBS_ext => self.subs_ext(inst),

            // Data processing - Logical immediate
            AND_imm => self.and_imm(inst),
            ORR_imm => self.orr_imm(inst),
            EOR_imm => self.eor_imm(inst),
            ANDS_imm => self.ands_imm(inst),

            // Data processing - Logical shifted register
            AND_shift => self.and_shift(inst),
            BIC_shift => self.bic_shift(inst),
            ORR_shift => self.orr_shift(inst),
            ORN_shift => self.orn_shift(inst),
            EOR_shift => self.eor_shift(inst),
            EON => self.eon_shift(inst),
            ANDS_shift => self.ands_shift(inst),
            BICS => self.bics_shift(inst),

            // Data processing - Bitfield
            SBFM => self.sbfm(inst),
            BFM => self.bfm(inst),
            UBFM => self.ubfm(inst),
            EXTR => self.extr(inst),
            // SBFM aliases — separate handlers matching upstream
            // `data_processing_bitfield.cpp:87-134`. These have more
            // specific encoding patterns than SBFM, so the decoder picks
            // them first. Without explicit handlers the block fell into
            // `interpret_this_instruction()` (a no-op) and silently
            // skipped the shift/sign-extend — STK's NVN driver at
            // NRO+0xE43F4C uses `asr w1, w0, #2` and without the
            // dispatch the result was wrong → bad pointer reads → no
            // display.
            ASR_1 => self.asr_1(inst),
            ASR_2 => self.asr_2(inst),
            SXTB_1 => self.sxtb_1(inst),
            SXTB_2 => self.sxtb_2(inst),
            SXTH_1 => self.sxth_1(inst),
            SXTH_2 => self.sxth_2(inst),
            SXTW => self.sxtw(inst),

            // Data processing - Shift (register)
            LSLV => self.lslv(inst),
            LSRV => self.lsrv(inst),
            ASRV => self.asrv(inst),
            RORV => self.rorv(inst),

            // Data processing - Conditional select
            CSEL => self.csel(inst),
            CSINC => self.csinc(inst),
            CSINV => self.csinv(inst),
            CSNEG => self.csneg(inst),

            // Data processing - PC-relative
            ADR => self.adr(inst),
            ADRP => self.adrp(inst),

            // Data processing - Multiply
            MADD => self.madd(inst),
            MSUB => self.msub(inst),
            SMADDL => self.smaddl(inst),
            SMSUBL => self.smsubl(inst),
            SMULH => self.smulh(inst),
            UMADDL => self.umaddl(inst),
            UMSUBL => self.umsubl(inst),
            UMULH => self.umulh(inst),

            // Data processing - Register misc (a64.inc uses _int suffix)
            RBIT_int => self.rbit(inst),
            REV16_int => self.rev16(inst),
            REV => self.rev(inst),
            REV32_int => self.rev32(inst),
            CLZ_int => self.clz(inst),
            CLS_int => self.cls(inst),

            // Data processing - Conditional compare
            CCMN_imm => self.ccmn_imm(inst),
            CCMP_imm => self.ccmp_imm(inst),
            CCMN_reg => self.ccmn_reg(inst),
            CCMP_reg => self.ccmp_reg(inst),

            // Data processing - CRC32 (a64.inc has CRC32 and CRC32C as single entries;
            // the size is encoded in the instruction bits, dispatch to common handler)
            CRC32 => self.crc32_dispatch(inst),
            CRC32C => self.crc32c_dispatch(inst),

            // Data processing - Divide
            UDIV => self.udiv(inst),
            SDIV => self.sdiv(inst),

            // Move wide
            MOVZ => self.movz(inst),
            MOVN => self.movn(inst),
            MOVK => self.movk(inst),

            // Branches
            B_uncond => self.b_uncond(inst),
            BL => self.bl(inst),
            B_cond => self.b_cond(inst),
            BR => self.br(inst),
            BLR => self.blr(inst),
            RET => self.ret(inst),
            CBZ => self.cbz(inst),
            CBNZ => self.cbnz(inst),
            TBZ => self.tbz(inst),
            TBNZ => self.tbnz(inst),

            // Exception
            SVC => self.svc(inst),
            BRK => self.brk(inst),

            // System
            NOP => self.nop(inst),
            MSR_reg => self.msr_reg(inst),
            MRS => self.mrs(inst),
            HINT => self.hint(inst),
            CLREX => self.clrex(inst),
            DSB => self.dsb(inst),
            DMB => self.dmb(inst),
            ISB => self.isb(inst),
            YIELD => self.yield_inst(inst),
            WFE => self.wfe(inst),
            WFI => self.wfi(inst),
            SEV => self.sev(inst),
            SEVL => self.sevl(inst),

            // Load/Store - Register immediate
            STRx_LDRx_imm_1 => self.strx_ldrx_imm_1(inst),
            STRx_LDRx_imm_2 => self.strx_ldrx_imm_2(inst),
            STURx_LDURx => self.sturx_ldurx(inst),
            STR_imm_fpsimd_1 => self.str_imm_fpsimd_1(inst),
            STR_imm_fpsimd_2 => self.str_imm_fpsimd_2(inst),
            LDR_imm_fpsimd_1 => self.ldr_imm_fpsimd_1(inst),
            LDR_imm_fpsimd_2 => self.ldr_imm_fpsimd_2(inst),
            STUR_fpsimd => self.stur_fpsimd(inst),
            LDUR_fpsimd => self.ldur_fpsimd(inst),

            // Load/Store - Register offset
            STRx_reg => self.strx_reg(inst),
            LDRx_reg => self.ldrx_reg(inst),
            STR_reg_fpsimd => self.str_reg_fpsimd(inst),
            LDR_reg_fpsimd => self.ldr_reg_fpsimd(inst),

            // Load/Store - Register pair
            STP_LDP_gen => self.stp_ldp_gen(inst),
            STP_LDP_fpsimd => self.stp_ldp_fpsimd(inst),
            STNP_LDNP_gen => self.stnp_ldnp_gen(inst),
            STNP_LDNP_fpsimd => self.stnp_ldnp_fpsimd(inst),

            // Load/Store - Literal
            LDR_lit_gen => self.ldr_lit_gen(inst),
            LDRSW_lit => self.ldrsw_lit(inst),
            LDR_lit_fpsimd => self.ldr_lit_fpsimd(inst),

            // Load/Store - Exclusive
            STXR => self.stxr(inst),
            STLXR => self.stlxr(inst),
            LDXR => self.ldxr(inst),
            LDAXR => self.ldaxr(inst),
            STXP => self.stxp(inst),
            STLXP => self.stlxp(inst),
            LDXP => self.ldxp(inst),
            LDAXP => self.ldaxp(inst),
            STLR => self.stlr(inst),
            LDAR => self.ldar(inst),
            STLLR => self.stllr(inst),
            LDLAR => self.ldlar(inst),

            // Load/Store - Unprivileged
            STTRB => self.sttrb(inst),
            LDTRB => self.ldtrb(inst),
            LDTRSB => self.ldtrsb(inst),
            STTRH => self.sttrh(inst),
            LDTRH => self.ldtrh(inst),
            LDTRSH => self.ldtrsh(inst),
            STTR => self.sttr(inst),
            LDTR => self.ldtr(inst),
            LDTRSW => self.ldtrsw(inst),

            // Prefetch (NOP)
            PRFM_imm => self.prfm_imm(inst),
            PRFM_lit => self.prfm_lit(inst),
            PRFM_unscaled_imm => self.prfm_unscaled_imm(inst),

            // SIMD structure loads/stores
            STx_mult_1 => self.stx_mult_1(inst),
            STx_mult_2 => self.stx_mult_2(inst),
            LDx_mult_1 => self.ldx_mult_1(inst),
            LDx_mult_2 => self.ldx_mult_2(inst),
            ST1_sngl_1 => self.st1_sngl_1(inst),
            ST1_sngl_2 => self.st1_sngl_2(inst),
            ST2_sngl_1 => self.st2_sngl_1(inst),
            ST2_sngl_2 => self.st2_sngl_2(inst),
            ST3_sngl_1 => self.st3_sngl_1(inst),
            ST3_sngl_2 => self.st3_sngl_2(inst),
            ST4_sngl_1 => self.st4_sngl_1(inst),
            ST4_sngl_2 => self.st4_sngl_2(inst),
            LD1_sngl_1 => self.ld1_sngl_1(inst),
            LD1_sngl_2 => self.ld1_sngl_2(inst),
            LD2_sngl_1 => self.ld2_sngl_1(inst),
            LD2_sngl_2 => self.ld2_sngl_2(inst),
            LD3_sngl_1 => self.ld3_sngl_1(inst),
            LD3_sngl_2 => self.ld3_sngl_2(inst),
            LD4_sngl_1 => self.ld4_sngl_1(inst),
            LD4_sngl_2 => self.ld4_sngl_2(inst),
            LD1R_1 => self.ld1r_1(inst),
            LD1R_2 => self.ld1r_2(inst),
            LD2R_1 => self.ld2r_1(inst),
            LD2R_2 => self.ld2r_2(inst),
            LD3R_1 => self.ld3r_1(inst),
            LD3R_2 => self.ld3r_2(inst),
            LD4R_1 => self.ld4r_1(inst),
            LD4R_2 => self.ld4r_2(inst),

            // Floating-point scalar
            FMOV_float => self.fmov_float(inst),
            FABS_float => self.fabs_float(inst),
            FNEG_float => self.fneg_float(inst),
            FSQRT_float => self.fsqrt_float(inst),
            FMOV_float_imm => self.fmov_float_imm(inst),
            FADD_float => self.fadd_float(inst),
            FSUB_float => self.fsub_float(inst),
            FMUL_float => self.fmul_float(inst),
            FDIV_float => self.fdiv_float(inst),
            FCMP_float => self.fcmp_float(inst),
            FCMPE_float => self.fcmpe_float(inst),
            FCSEL_float => self.fcsel_float(inst),
            FCVT_float => self.fcvt_float(inst),
            FRINTN_float => self.frintn_float(inst),
            FRINTP_float => self.frintp_float(inst),
            FRINTM_float => self.frintm_float(inst),
            FRINTZ_float => self.frintz_float(inst),
            FRINTA_float => self.frinta_float(inst),
            FRINTX_float => self.frintx_float(inst),
            FRINTI_float => self.frinti_float(inst),
            SCVTF_float_fix => self.scvtf_float_fix(inst),
            UCVTF_float_fix => self.ucvtf_float_fix(inst),
            FCVTZS_float_fix => self.fcvtzs_float_fix(inst),
            FCVTZU_float_fix => self.fcvtzu_float_fix(inst),
            FCVTNS_float => self.fcvtns_float(inst),
            FCVTNU_float => self.fcvtnu_float(inst),
            SCVTF_float_int => self.scvtf_float_int(inst),
            UCVTF_float_int => self.ucvtf_float_int(inst),
            FCVTAS_float => self.fcvtas_float(inst),
            FCVTAU_float => self.fcvtau_float(inst),
            FMOV_float_gen => self.fmov_float_gen(inst),
            FCVTPS_float => self.fcvtps_float(inst),
            FCVTPU_float => self.fcvtpu_float(inst),
            FCVTMS_float => self.fcvtms_float(inst),
            FCVTMU_float => self.fcvtmu_float(inst),
            FCVTZS_float_int => self.fcvtzs_float_int(inst),
            FCVTZU_float_int => self.fcvtzu_float_int(inst),
            FMADD_float => self.fmadd_float(inst),
            FMSUB_float => self.fmsub_float(inst),
            FNMADD_float => self.fnmadd_float(inst),
            FNMSUB_float => self.fnmsub_float(inst),
            FNMUL_float => self.fnmul_float(inst),
            FMAX_float => self.fmax_float(inst),
            FMIN_float => self.fmin_float(inst),
            FMAXNM_float => self.fmaxnm_float(inst),
            FMINNM_float => self.fminnm_float(inst),
            FCCMP_float => self.fccmp_float(inst),
            FCCMPE_float => self.fccmpe_float(inst),

            // SIMD copy: DUP (general / element). Without these, libc's
            // `__memset_aarch64` cannot broadcast the fill byte across V0
            // and the memset writes whatever was in V0 to memory — which
            // in STK manifests as a refcount table full of sentinel
            // pointers (0x0000FF00FFFF0000) and a CAS spin/SIGSEGV.
            DUP_gen => self.dup_gen(inst),
            DUP_elt_1 => self.dup_elt_1(inst),
            DUP_elt_2 => self.dup_elt_2(inst),
            INS_gen => self.ins_gen(inst),
            INS_elt => self.ins_elt(inst),
            UMOV => self.umov(inst),
            SMOV => self.smov(inst),

            // SIMD permute.
            TRN1 => self.trn1(inst),
            TRN2 => self.trn2(inst),
            UZP1 => self.uzp1(inst),
            UZP2 => self.uzp2(inst),
            ZIP1 => self.zip1(inst),
            ZIP2 => self.zip2(inst),

            // SIMD table lookup.
            TBL => self.tbl(inst),
            TBX => self.tbx(inst),

            // SIMD three-same: paired min/max (UMINP/UMAXP/SMINP/SMAXP).
            // Used by libnx's vectorized strlen at NRO+0x80E3B720; without
            // these, the loop never observes a null byte and spins
            // forever scanning the heap into unmapped memory.
            UMINP => self.uminp(inst),
            UMAXP => self.umaxp(inst),
            SMINP => self.sminp(inst),
            SMAXP => self.smaxp(inst),

            // SIMD three-same — vector arithmetic / compare / logical.
            ADD_vector => self.add_vector(inst),
            SUB_2 => self.sub_2(inst),
            SMAX => self.smax(inst),
            SMIN => self.smin(inst),
            UMAX => self.umax(inst),
            UMIN => self.umin(inst),
            SABA => self.saba(inst),
            SABD => self.sabd(inst),
            UABA => self.uaba(inst),
            UABD => self.uabd(inst),
            SHADD => self.shadd(inst),
            SHSUB => self.shsub(inst),
            SQADD_2 => self.sqadd_2(inst),
            SQSUB_2 => self.sqsub_2(inst),
            SRHADD => self.srhadd(inst),
            UHADD => self.uhadd(inst),
            UHSUB => self.uhsub(inst),
            UQADD_2 => self.uqadd_2(inst),
            UQSUB_2 => self.uqsub_2(inst),
            URHADD => self.urhadd(inst),
            CMEQ_reg_2 => self.cmeq_reg_2(inst),
            CMGE_reg_2 => self.cmge_reg_2(inst),
            CMGT_reg_2 => self.cmgt_reg_2(inst),
            CMHS_2 => self.cmhs_2(inst),
            CMHI_2 => self.cmhi_2(inst),
            CMTST_2 => self.cmtst_2(inst),
            ADDP_vec => self.addp_vec(inst),
            ADDHN => self.addhn(inst),
            RADDHN => self.raddhn(inst),
            SUBHN => self.subhn(inst),
            RSUBHN => self.rsubhn(inst),
            MLA_vec => self.mla_vec(inst),
            MLS_vec => self.mls_vec(inst),
            MUL_vec => self.mul_vec(inst),
            FMAXNMP_vec_2 => self.fmaxnmp_vec_2(inst),
            FMAXP_vec_2 => self.fmaxp_vec_2(inst),
            FMINNMP_vec_2 => self.fminnmp_vec_2(inst),
            FMINP_vec_2 => self.fminp_vec_2(inst),
            SSHL_2 => self.sshl_2(inst),
            USHL_2 => self.ushl_2(inst),
            AND_asimd => self.and_asimd(inst),
            BIC_asimd_reg => self.bic_asimd_reg(inst),
            ORR_asimd_reg => self.orr_asimd_reg(inst),
            ORN_asimd => self.orn_asimd(inst),
            EOR_asimd => self.eor_asimd(inst),
            BSL => self.bsl(inst),
            BIT => self.bit(inst),
            BIF => self.bif(inst),
            FCMEQ_reg_3 => self.fcmeq_reg_3(inst),
            FCMEQ_reg_4 => self.fcmeq_reg_4(inst),
            FCMGE_reg_4 => self.fcmge_reg_4(inst),
            FCMGT_reg_4 => self.fcmgt_reg_4(inst),
            FADD_2 => self.fadd_2(inst),
            FSUB_2 => self.fsub_2(inst),

            // SIMD two-register misc: compare against zero (CMEQ #0).
            // Same strlen loop reduces UMINP results then compares to 0
            // to detect null bytes.
            CMEQ_zero_2 => self.cmeq_zero_2(inst),
            CMGE_zero_2 => self.cmge_zero_2(inst),
            CMGT_zero_2 => self.cmgt_zero_2(inst),
            CMLE_2 => self.cmle_2(inst),
            CMLT_2 => self.cmlt_2(inst),
            ABS_2 => self.abs_2(inst),
            NEG_2 => self.neg_2(inst),
            SQABS_2 => self.sqabs_2(inst),
            SQNEG_2 => self.sqneg_2(inst),
            SUQADD_2 => self.suqadd_2(inst),
            USQADD_2 => self.usqadd_2(inst),
            NOT => self.not(inst),
            RBIT_asimd => self.rbit_asimd(inst),
            CLS_asimd => self.cls_asimd(inst),
            CNT => self.cnt(inst),
            CLZ_asimd => self.clz_asimd(inst),
            REV16_asimd => self.rev16_asimd(inst),
            REV32_asimd => self.rev32_asimd(inst),
            REV64_asimd => self.rev64_asimd(inst),
            XTN => self.xtn(inst),
            SQXTUN_2 => self.sqxtun_2(inst),
            SQXTN_2 => self.sqxtn_2(inst),
            UQXTN_2 => self.uqxtn_2(inst),
            FABS_1 => self.fabs_1(inst),
            FCVTL => self.fcvtl(inst),
            FCVTN => self.fcvtn(inst),
            FCVTXN_2 => self.fcvtxn_2(inst),
            FCMEQ_zero_3 => self.fcmeq_zero_3(inst),
            FCMEQ_zero_4 => self.fcmeq_zero_4(inst),
            FCMGE_zero_4 => self.fcmge_zero_4(inst),
            FCMGT_zero_4 => self.fcmgt_zero_4(inst),
            FABS_2 => self.fabs_2(inst),
            FNEG_1 => self.fneg_1(inst),
            FNEG_2 => self.fneg_2(inst),
            FRINTN_1 => self.frintn_1(inst),
            FRINTN_2 => self.frintn_2(inst),
            FRINTM_1 => self.frintm_1(inst),
            FRINTM_2 => self.frintm_2(inst),
            FRINTP_1 => self.frintp_1(inst),
            FRINTP_2 => self.frintp_2(inst),
            FRINTZ_1 => self.frintz_1(inst),
            FRINTZ_2 => self.frintz_2(inst),
            FRINTA_1 => self.frinta_1(inst),
            FRINTA_2 => self.frinta_2(inst),
            FRINTX_1 => self.frintx_1(inst),
            FRINTX_2 => self.frintx_2(inst),
            FRINTI_1 => self.frinti_1(inst),
            FRINTI_2 => self.frinti_2(inst),
            FCVTZS_int_4 => self.fcvtzs_int_4(inst),
            FCVTZU_int_4 => self.fcvtzu_int_4(inst),
            FCVTNS_4 => self.fcvtns_4(inst),
            FCVTMS_4 => self.fcvtms_4(inst),
            FCVTAS_4 => self.fcvtas_4(inst),
            FCVTPS_4 => self.fcvtps_4(inst),
            FCVTNU_4 => self.fcvtnu_4(inst),
            FCVTMU_4 => self.fcvtmu_4(inst),
            FCVTAU_4 => self.fcvtau_4(inst),
            FCVTPU_4 => self.fcvtpu_4(inst),
            SCVTF_int_4 => self.scvtf_int_4(inst),
            UCVTF_int_4 => self.ucvtf_int_4(inst),
            SADALP => self.sadalp(inst),
            SADDLP => self.saddlp(inst),
            UADALP => self.uadalp(inst),
            UADDLP => self.uaddlp(inst),
            URECPE => self.urecpe(inst),
            URSQRTE => self.ursqrte(inst),
            SHLL => self.shll(inst),

            // SIMD scalar two-register misc: saturated narrow.
            SQXTN_1 => self.sqxtn_1(inst),
            SQXTUN_1 => self.sqxtun_1(inst),
            UQXTN_1 => self.uqxtn_1(inst),

            // SIMD extract.
            EXT => self.ext(inst),

            // SIMD shift by immediate (vector form). When `immh == 0`
            // the encoding actually belongs to the modified-immediate
            // (MOVI/FMOV) group, NOT a shift; the decoder is configured
            // to match those first via the `comes_first` partition in
            // `build.rs` (mirrors upstream `decoder/a64.h:48-57`). So we
            // can call the shift handlers directly and trust they only
            // see well-formed encodings.
            SHL_2 => self.shl_2(inst),
            SHRN => self.shrn(inst),
            RSHRN => self.rshrn(inst),
            SQSHRN_2 => self.sqshrn_2(inst),
            SQRSHRN_2 => self.sqrshrn_2(inst),
            SQSHRUN_2 => self.sqshrun_2(inst),
            SQRSHRUN_2 => self.sqrshrun_2(inst),
            UQSHRN_2 => self.uqshrn_2(inst),
            UQRSHRN_2 => self.uqrshrn_2(inst),
            USHR_2 => self.ushr_2(inst),
            USRA_2 => self.usra_2(inst),
            URSHR_2 => self.urshr_2(inst),
            URSRA_2 => self.ursra_2(inst),
            SSHLL => self.sshll(inst),
            USHLL => self.ushll(inst),

            // SIMD three-different — long add/sub.
            SADDL => self.saddl(inst),
            SADDW => self.saddw(inst),
            SSUBL => self.ssubl(inst),
            SSUBW => self.ssubw(inst),
            UADDL => self.uaddl(inst),
            UADDW => self.uaddw(inst),
            USUBL => self.usubl(inst),
            USUBW => self.usubw(inst),
            SABAL => self.sabal(inst),
            SABDL => self.sabdl(inst),
            UABAL => self.uabal(inst),
            UABDL => self.uabdl(inst),
            SMLAL_vec => self.smlal_vec(inst),
            SMLSL_vec => self.smlsl_vec(inst),
            SMULL_vec => self.smull_vec(inst),
            UMLAL_vec => self.umlal_vec(inst),
            UMLSL_vec => self.umlsl_vec(inst),
            UMULL_vec => self.umull_vec(inst),

            // SIMD scalar three-same / scalar zero-compare.
            SQADD_1 => self.sqadd_1(inst),
            SQSUB_1 => self.sqsub_1(inst),
            UQADD_1 => self.uqadd_1(inst),
            UQSUB_1 => self.uqsub_1(inst),
            ADD_1 => self.add_1(inst),
            SUB_1 => self.sub_1(inst),
            CMEQ_reg_1 => self.cmeq_reg_1(inst),
            CMGE_reg_1 => self.cmge_reg_1(inst),
            CMGT_reg_1 => self.cmgt_reg_1(inst),
            CMHI_1 => self.cmhi_1(inst),
            CMHS_1 => self.cmhs_1(inst),
            CMTST_1 => self.cmtst_1(inst),
            SSHL_1 => self.sshl_1(inst),
            USHL_1 => self.ushl_1(inst),
            SQSHL_reg_1 => self.sqshl_reg_1(inst),
            UQSHL_reg_1 => self.uqshl_reg_1(inst),
            SRSHL_1 => self.srshl_1(inst),
            URSHL_1 => self.urshl_1(inst),
            SQDMULH_vec_1 => self.sqdmulh_vec_1(inst),
            SQRDMULH_vec_1 => self.sqrdmulh_vec_1(inst),
            CMEQ_zero_1 => self.cmeq_zero_1(inst),
            CMGE_zero_1 => self.cmge_zero_1(inst),
            CMGT_zero_1 => self.cmgt_zero_1(inst),
            CMLE_1 => self.cmle_1(inst),
            CMLT_1 => self.cmlt_1(inst),
            FCMEQ_reg_1 => self.fcmeq_reg_1(inst),
            FCMEQ_reg_2 => self.fcmeq_reg_2(inst),
            FCMGE_reg_2 => self.fcmge_reg_2(inst),
            FCMGT_reg_2 => self.fcmgt_reg_2(inst),
            FABD_2 => self.fabd_2(inst),
            FMULX_vec_2 => self.fmulx_vec_2(inst),
            FACGE_2 => self.facge_2(inst),
            FACGT_2 => self.facgt_2(inst),
            FRECPS_1 => self.frecps_1(inst),
            FRECPS_2 => self.frecps_2(inst),
            FRSQRTS_1 => self.frsqrts_1(inst),
            FRSQRTS_2 => self.frsqrts_2(inst),

            // SIMD scalar two-register misc.
            ABS_1 => self.abs_1(inst),
            NEG_1 => self.neg_1(inst),
            FCMEQ_zero_1 => self.fcmeq_zero_1(inst),
            FCMEQ_zero_2 => self.fcmeq_zero_2(inst),
            FCMGE_zero_2 => self.fcmge_zero_2(inst),
            FCMGT_zero_2 => self.fcmgt_zero_2(inst),
            FCMLE_4 => self.fcmle_4(inst),
            FCMLT_4 => self.fcmlt_4(inst),
            FCVTAS_2 => self.fcvtas_2(inst),
            FCVTAU_2 => self.fcvtau_2(inst),
            FCVTMS_2 => self.fcvtms_2(inst),
            FCVTMU_2 => self.fcvtmu_2(inst),
            FCVTNS_2 => self.fcvtns_2(inst),
            FCVTNU_2 => self.fcvtnu_2(inst),
            FCVTPS_2 => self.fcvtps_2(inst),
            FCVTPU_2 => self.fcvtpu_2(inst),
            FCVTZS_int_2 => self.fcvtzs_int_2(inst),
            FCVTZU_int_2 => self.fcvtzu_int_2(inst),
            FRECPE_1 => self.frecpe_1(inst),
            FRECPE_2 => self.frecpe_2(inst),
            FRECPX_1 => self.frecpx_1(inst),
            FRECPX_2 => self.frecpx_2(inst),
            FRSQRTE_1 => self.frsqrte_1(inst),
            FRSQRTE_2 => self.frsqrte_2(inst),
            SCVTF_int_2 => self.scvtf_int_2(inst),
            UCVTF_int_2 => self.ucvtf_int_2(inst),

            // SIMD scalar pairwise.
            ADDP_pair => self.addp_pair(inst),
            FADDP_pair_2 => self.faddp_pair_2(inst),
            FMAXNMP_pair_2 => self.fmaxnmp_pair_2(inst),
            FMAXP_pair_2 => self.fmaxp_pair_2(inst),
            FMINNMP_pair_2 => self.fminnmp_pair_2(inst),
            FMINP_pair_2 => self.fminp_pair_2(inst),

            // SIMD across lanes.
            SADDLV => self.saddlv(inst),
            ADDV => self.addv(inst),
            UADDLV => self.uaddlv(inst),
            FMAXNMV_2 => self.fmaxnmv_2(inst),
            FMAXV_2 => self.fmaxv_2(inst),
            FMINNMV_2 => self.fminnmv_2(inst),
            FMINV_2 => self.fminv_2(inst),
            SMAXV => self.smaxv(inst),
            SMINV => self.sminv(inst),
            UMAXV => self.umaxv(inst),
            UMINV => self.uminv(inst),

            // SIMD scalar shift by immediate.
            USHR_1 => self.ushr_1(inst),
            SSHR_1 => self.sshr_1(inst),
            USRA_1 => self.usra_1(inst),
            SSRA_1 => self.ssra_1(inst),
            URSHR_1 => self.urshr_1(inst),
            SRSHR_1 => self.srshr_1(inst),
            URSRA_1 => self.ursra_1(inst),
            SRSRA_1 => self.srsra_1(inst),
            SRI_1 => self.sri_1(inst),
            SLI_1 => self.sli_1(inst),
            SHL_1 => self.shl_1(inst),
            SQSHL_imm_1 => self.sqshl_imm_1(inst),
            SQSHLU_1 => self.sqshlu_1(inst),
            UQSHL_imm_1 => self.uqshl_imm_1(inst),
            SQSHRN_1 => self.sqshrn_1(inst),
            SQSHRUN_1 => self.sqshrun_1(inst),
            UQSHRN_1 => self.uqshrn_1(inst),
            FCVTZS_fix_1 => self.fcvtzs_fix_1(inst),
            FCVTZU_fix_1 => self.fcvtzu_fix_1(inst),
            SCVTF_fix_1 => self.scvtf_fix_1(inst),
            UCVTF_fix_1 => self.ucvtf_fix_1(inst),

            // SIMD modified immediate (MOVI / MVNI / ORR-imm / BIC-imm).
            // Decoder is configured to match these BEFORE the shift-by-
            // immediate group when immh==0 (see comment near SHL_2 above
            // and `comes_first` in build.rs).
            MOVI => self.movi(inst),
            FMOV_2 => self.fmov_2(inst),
            FMOV_3 => self.fmov_3(inst),
            FRECPE_3 => self.frecpe_3(inst),
            FRECPE_4 => self.frecpe_4(inst),
            FRSQRTE_3 => self.frsqrte_3(inst),
            FRSQRTE_4 => self.frsqrte_4(inst),
            FSQRT_2 => self.fsqrt_2(inst),
            FRECPS_3 => self.frecps_3(inst),
            FRECPS_4 => self.frecps_4(inst),
            FRSQRTS_3 => self.frsqrts_3(inst),
            FRSQRTS_4 => self.frsqrts_4(inst),
            FMLA_elt_1 => self.fmla_elt_1(inst),
            FMLA_elt_2 => self.fmla_elt_2(inst),
            FMLA_elt_3 => self.fmla_elt_3(inst),
            FMLA_elt_4 => self.fmla_elt_4(inst),
            FMLS_elt_1 => self.fmls_elt_1(inst),
            FMLS_elt_2 => self.fmls_elt_2(inst),
            FMLS_elt_3 => self.fmls_elt_3(inst),
            FMLS_elt_4 => self.fmls_elt_4(inst),
            FMUL_elt_2 => self.fmul_elt_2(inst),
            FMUL_elt_4 => self.fmul_elt_4(inst),
            FMULX_elt_2 => self.fmulx_elt_2(inst),
            FMULX_elt_4 => self.fmulx_elt_4(inst),
            MLA_elt => self.mla_elt(inst),
            MLS_elt => self.mls_elt(inst),
            MUL_elt => self.mul_elt(inst),
            FCMLA_elt => self.fcmla_elt(inst),
            SMLAL_elt => self.smlal_elt(inst),
            SMLSL_elt => self.smlsl_elt(inst),
            SMULL_elt => self.smull_elt(inst),
            SQDMULL_elt_2 => self.sqdmull_elt_2(inst),
            UMLAL_elt => self.umlal_elt(inst),
            UMLSL_elt => self.umlsl_elt(inst),
            UMULL_elt => self.umull_elt(inst),
            SQDMULH_elt_2 => self.sqdmulh_elt_2(inst),
            SQRDMULH_elt_2 => self.sqrdmulh_elt_2(inst),
            SDOT_elt => self.sdot_elt(inst),
            UDOT_elt => self.udot_elt(inst),
            FMUL_vec_2 => self.fmul_vec_2(inst),
            FMULX_vec_4 => self.fmulx_vec_4(inst),
            FDIV_2 => self.fdiv_2(inst),
            FMLA_vec_2 => self.fmla_vec_2(inst),
            FMLS_vec_2 => self.fmls_vec_2(inst),
            FADDP_vec_2 => self.faddp_vec_2(inst),
            FMAX_2 => self.fmax_2(inst),
            FMIN_2 => self.fmin_2(inst),
            FMAXNM_2 => self.fmaxnm_2(inst),
            FMINNM_2 => self.fminnm_2(inst),
            FABD_4 => self.fabd_4(inst),
            FACGE_4 => self.facge_4(inst),
            FACGT_4 => self.facgt_4(inst),
            ADC => self.adc(inst),
            ADCS => self.adcs(inst),
            SBC => self.sbc(inst),
            SBCS => self.sbcs(inst),
            SSHR_2 => self.sshr_2(inst),
            SSRA_2 => self.ssra_2(inst),
            SRSHR_2 => self.srshr_2(inst),
            SRSRA_2 => self.srsra_2(inst),

            // Crypto
            AESE => self.aese(inst),
            AESD => self.aesd(inst),
            AESMC => self.aesmc(inst),
            AESIMC => self.aesimc(inst),
            SHA1C => self.sha1c(inst),
            SHA1P => self.sha1p(inst),
            SHA1M => self.sha1m(inst),
            SHA1SU0 => self.sha1su0(inst),
            SHA256H => self.sha256h(inst),
            SHA256H2 => self.sha256h2(inst),
            SHA256SU1 => self.sha256su1(inst),
            SHA1H => self.sha1h(inst),
            SHA1SU1 => self.sha1su1(inst),
            SHA256SU0 => self.sha256su0(inst),
            EOR3 => self.eor3(inst),
            BCAX => self.bcax(inst),
            SM3SS1 => self.sm3ss1(inst),
            SM3TT1A => self.sm3tt1a(inst),
            SM3TT1B => self.sm3tt1b(inst),
            SM3TT2A => self.sm3tt2a(inst),
            SM3TT2B => self.sm3tt2b(inst),

            // Cache maintenance (NOP in userspace)
            DC_IVAC => self.dc_ivac(inst),
            DC_ISW => self.dc_isw(inst),
            DC_CSW => self.dc_csw(inst),
            DC_CISW => self.dc_cisw(inst),
            DC_ZVA => self.dc_zva(inst),
            DC_CVAC => self.dc_cvac(inst),
            DC_CVAU => self.dc_cvau(inst),
            DC_CVAP => self.dc_cvap(inst),
            DC_CIVAC => self.dc_civac(inst),
            IC_IALLU => self.ic_iallu(inst),
            IC_IALLUIS => self.ic_ialluis(inst),
            IC_IVAU => self.ic_ivau(inst),
            UnallocatedEncoding => self.unallocated_encoding(),

            // Unimplemented — fallback to interpreter
            _ => self.interpret_this_instruction(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::opcode::Opcode;

    fn assert_exception_terminal(block: &Block) {
        assert!(matches!(
            &block.terminal,
            Terminal::CheckHalt { else_ } if matches!(else_.as_ref(), Terminal::ReturnToDispatch)
        ));
        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, Opcode::A64ExceptionRaised)));
    }

    #[test]
    fn unallocated_encoding_raises_exception_instead_of_interpret() {
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        let mut visitor = TranslatorVisitor::new(
            &mut block,
            A64LocationDescriptor::new(0x1000, 0, false),
            TranslationOptions::default(),
        );

        assert!(!visitor.unallocated_encoding());
        drop(visitor);

        assert_exception_terminal(&block);
    }

    #[test]
    fn reserved_value_raises_exception_instead_of_interpret() {
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        let mut visitor = TranslatorVisitor::new(
            &mut block,
            A64LocationDescriptor::new(0x1000, 0, false),
            TranslationOptions::default(),
        );

        assert!(!visitor.reserved_value());
        drop(visitor);

        assert_exception_terminal(&block);
    }

    #[test]
    fn unpredictable_instruction_raises_exception_instead_of_interpret() {
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        let mut visitor = TranslatorVisitor::new(
            &mut block,
            A64LocationDescriptor::new(0x1000, 0, false),
            TranslationOptions::default(),
        );

        assert!(!visitor.unpredictable_instruction());
        drop(visitor);

        assert_exception_terminal(&block);
    }

    #[test]
    fn interpret_this_instruction_uses_current_location_like_upstream() {
        let loc = A64LocationDescriptor::new(0x1000, 0, false);
        let mut block = Block::new(loc.to_location());
        let mut visitor = TranslatorVisitor::new(&mut block, loc, TranslationOptions::default());

        assert!(!visitor.interpret_this_instruction());
        drop(visitor);

        match &block.terminal {
            Terminal::Interpret {
                next,
                num_instructions,
            } => {
                assert_eq!(*next, loc.to_location());
                assert_eq!(*num_instructions, 1);
            }
            other => panic!("expected Interpret terminal, got {other:?}"),
        }
    }

    #[test]
    #[should_panic(expected = "decode_error() reached")]
    fn decode_error_is_unreachable() {
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        let mut visitor = TranslatorVisitor::new(
            &mut block,
            A64LocationDescriptor::new(0x1000, 0, false),
            TranslationOptions::default(),
        );

        let _ = visitor.decode_error();
    }
}

//! Differential fuzzer: generates random ARM32 programs, runs them in both
//! rdynarmic and upstream C++ dynarmic, and compares register results.
//! The upstream oracle binary must be at /home/vricosti/Dev/emulators/zuyu/build/a32_oracle.

#[cfg(test)]
mod tests {
    use crate::jit::A32Jit;
    use crate::jit_config::{JitConfig, OptimizationFlag, UserCallbacks};
    use std::collections::HashMap;
    use std::io::Write;
    use std::process::{Command, Stdio};

    struct FuzzEnv {
        code_mem: Vec<u32>,
        data_mem: HashMap<u64, u8>,
        ticks_left: u64,
    }

    impl FuzzEnv {
        fn new(code: Vec<u32>) -> Self {
            Self {
                code_mem: code,
                data_mem: HashMap::new(),
                ticks_left: 200,
            }
        }
    }

    impl UserCallbacks for FuzzEnv {
        fn memory_read_code(&self, vaddr: u64) -> Option<u32> {
            let idx = (vaddr as usize) / 4;
            if idx < self.code_mem.len() {
                Some(self.code_mem[idx])
            } else {
                Some(0xEAFFFFFE)
            }
        }
        fn memory_read_8(&self, vaddr: u64) -> u8 {
            *self.data_mem.get(&vaddr).unwrap_or(&0)
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
        fn exclusive_clear(&mut self) {}
        fn call_supervisor(&mut self, _: u32) {}
        fn exception_raised(&mut self, _: u64, _: u64) {}
        fn add_ticks(&mut self, ticks: u64) {
            self.ticks_left = self.ticks_left.saturating_sub(ticks);
        }
        fn get_ticks_remaining(&self) -> u64 {
            self.ticks_left
        }
        fn data_cache_operation(&mut self, _: u64, _: u64) {}
        fn instruction_cache_operation(&mut self, _: u64, _: u64) {}
    }

    const ORACLE: &str = "/home/vricosti/Dev/emulators/zuyu/build/a32_oracle";

    fn next_rand(rng: &mut u64) -> u32 {
        *rng ^= *rng << 13;
        *rng ^= *rng >> 7;
        *rng ^= *rng << 17;
        *rng as u32
    }

    /// Pick a random register 0-12 (avoids R13/SP, R14/LR, R15/PC).
    fn rand_reg(rng: &mut u64) -> u32 {
        next_rand(rng) % 13
    }

    /// Pick a random register 0-12 different from `exclude`.
    fn rand_reg_ne(rng: &mut u64, exclude: u32) -> u32 {
        loop {
            let r = rand_reg(rng);
            if r != exclude {
                return r;
            }
        }
    }

    /// Generate a random valid ARM32 instruction using a dictionary approach.
    /// Every generated instruction is architecturally valid (no UNPREDICTABLE).
    fn gen_instruction(rng: &mut u64) -> u32 {
        let r = next_rand(rng);

        match r % 30 {
            // --- Data processing immediate: AND/EOR/SUB/RSB/ADD/ADC/SBC/RSC ---
            0..=3 => {
                let opcodes = [0u32, 1, 2, 3, 4, 5, 6, 7]; // AND..RSC
                let op = opcodes[(r as usize >> 8) % 8];
                let s = (r >> 4) & 1;
                let rd = rand_reg(rng);
                let rn = rand_reg(rng);
                let imm8 = next_rand(rng) & 0xFF;
                let rotate = next_rand(rng) & 0xF;
                0xE2000000 | (op << 21) | (s << 20) | (rn << 16) | (rd << 12) | (rotate << 8) | imm8
            }
            // --- Data processing immediate: ORR/BIC ---
            4 => {
                let op = if (r >> 4) & 1 == 0 { 12u32 } else { 14 }; // ORR or BIC
                let s = (r >> 5) & 1;
                let rd = rand_reg(rng);
                let rn = rand_reg(rng);
                let imm8 = next_rand(rng) & 0xFF;
                let rotate = next_rand(rng) & 0xF;
                0xE2000000 | (op << 21) | (s << 20) | (rn << 16) | (rd << 12) | (rotate << 8) | imm8
            }
            // --- MOV/MVN immediate (Rn=0 SBZ) ---
            5 => {
                let op = if (r >> 4) & 1 == 0 { 13u32 } else { 15 }; // MOV or MVN
                let s = (r >> 5) & 1;
                let rd = rand_reg(rng);
                let imm8 = next_rand(rng) & 0xFF;
                let rotate = next_rand(rng) & 0xF;
                0xE2000000 | (op << 21) | (s << 20) | (rd << 12) | (rotate << 8) | imm8
            }
            // --- TST/TEQ/CMP/CMN immediate (Rd=0 SBZ, S=1) ---
            6 => {
                let ops = [8u32, 9, 10, 11];
                let op = ops[(r as usize >> 4) % 4];
                let rn = rand_reg(rng);
                let imm8 = next_rand(rng) & 0xFF;
                let rotate = next_rand(rng) & 0xF;
                0xE2000000 | (op << 21) | (1 << 20) | (rn << 16) | (rotate << 8) | imm8
            }
            // --- Data processing register: AND/EOR/SUB/RSB/ADD/ADC/SBC/RSC ---
            7..=10 => {
                let opcodes = [0u32, 1, 2, 3, 4, 5, 6, 7];
                let op = opcodes[(r as usize >> 8) % 8];
                let s = (r >> 4) & 1;
                let rd = rand_reg(rng);
                let rn = rand_reg(rng);
                let rm = rand_reg(rng);
                let shift_type = next_rand(rng) & 3;
                let imm5 = next_rand(rng) & 0x1F;
                0xE0000000
                    | (op << 21)
                    | (s << 20)
                    | (rn << 16)
                    | (rd << 12)
                    | (imm5 << 7)
                    | (shift_type << 5)
                    | rm
            }
            // --- ORR/BIC register ---
            11 => {
                let op = if (r >> 4) & 1 == 0 { 12u32 } else { 14 };
                let s = (r >> 5) & 1;
                let rd = rand_reg(rng);
                let rn = rand_reg(rng);
                let rm = rand_reg(rng);
                let shift_type = next_rand(rng) & 3;
                let imm5 = next_rand(rng) & 0x1F;
                0xE0000000
                    | (op << 21)
                    | (s << 20)
                    | (rn << 16)
                    | (rd << 12)
                    | (imm5 << 7)
                    | (shift_type << 5)
                    | rm
            }
            // --- MOV/MVN register (Rn=0 SBZ) ---
            12 => {
                let op = if (r >> 4) & 1 == 0 { 13u32 } else { 15 };
                let s = (r >> 5) & 1;
                let rd = rand_reg(rng);
                let rm = rand_reg(rng);
                let shift_type = next_rand(rng) & 3;
                let imm5 = next_rand(rng) & 0x1F;
                0xE0000000
                    | (op << 21)
                    | (s << 20)
                    | (rd << 12)
                    | (imm5 << 7)
                    | (shift_type << 5)
                    | rm
            }
            // --- TST/TEQ/CMP/CMN register (Rd=0, S=1) ---
            13 => {
                let ops = [8u32, 9, 10, 11];
                let op = ops[(r as usize >> 4) % 4];
                let rn = rand_reg(rng);
                let rm = rand_reg(rng);
                let shift_type = next_rand(rng) & 3;
                let imm5 = next_rand(rng) & 0x1F;
                0xE0000000
                    | (op << 21)
                    | (1 << 20)
                    | (rn << 16)
                    | (imm5 << 7)
                    | (shift_type << 5)
                    | rm
            }
            // --- MOVW ---
            14 => {
                let rd = rand_reg(rng);
                let imm16 = next_rand(rng) & 0xFFFF;
                let imm4 = (imm16 >> 12) & 0xF;
                let imm12 = imm16 & 0xFFF;
                0xE3000000 | (imm4 << 16) | (rd << 12) | imm12
            }
            // --- MOVT ---
            15 => {
                let rd = rand_reg(rng);
                let imm16 = next_rand(rng) & 0xFFFF;
                let imm4 = (imm16 >> 12) & 0xF;
                let imm12 = imm16 & 0xFFF;
                0xE3400000 | (imm4 << 16) | (rd << 12) | imm12
            }
            // --- MUL ---
            16 => {
                let rd = rand_reg(rng);
                let rm = rand_reg(rng);
                let rs = rand_reg(rng);
                0xE0000090 | (rd << 16) | (rs << 8) | rm
            }
            // --- MLA ---
            17 => {
                let rd = rand_reg(rng);
                let rm = rand_reg(rng);
                let rs = rand_reg(rng);
                let rn = rand_reg(rng);
                0xE0200090 | (rd << 16) | (rn << 12) | (rs << 8) | rm
            }
            // --- UMULL ---
            18 => {
                let rdhi = rand_reg(rng);
                let rdlo = rand_reg_ne(rng, rdhi);
                let rm = rand_reg_ne(rng, rdhi); // Rm != RdHi (ARM requirement)
                let rs = rand_reg(rng);
                0xE0800090 | (rdhi << 16) | (rdlo << 12) | (rs << 8) | rm
            }
            // --- SMULL ---
            19 => {
                let rdhi = rand_reg(rng);
                let rdlo = rand_reg_ne(rng, rdhi);
                let rm = rand_reg_ne(rng, rdhi);
                let rs = rand_reg(rng);
                0xE0C00090 | (rdhi << 16) | (rdlo << 12) | (rs << 8) | rm
            }
            // --- CLZ ---
            20 => {
                let rd = rand_reg(rng);
                let rm = rand_reg(rng);
                0xE16F0F10 | (rd << 12) | rm
            }
            // --- REV ---
            21 => {
                let rd = rand_reg(rng);
                let rm = rand_reg(rng);
                0xE6BF0F30 | (rd << 12) | rm
            }
            // --- RBIT ---
            22 => {
                let rd = rand_reg(rng);
                let rm = rand_reg(rng);
                0xE6FF0F30 | (rd << 12) | rm
            }
            // --- UXTB ---
            23 => {
                let rd = rand_reg(rng);
                let rm = rand_reg(rng);
                0xE6EF0070 | (rd << 12) | rm
            }
            // --- SXTB ---
            24 => {
                let rd = rand_reg(rng);
                let rm = rand_reg(rng);
                0xE6AF0070 | (rd << 12) | rm
            }
            // --- UXTH ---
            25 => {
                let rd = rand_reg(rng);
                let rm = rand_reg(rng);
                0xE6FF0070 | (rd << 12) | rm
            }
            // --- BFC (bits[6:4]=001, Rm=1111) ---
            26 => {
                let rd = rand_reg(rng);
                let lsb = next_rand(rng) & 0x1F;
                let width = (next_rand(rng) % 31) + 1;
                let msb = (lsb + width - 1).min(31);
                0xE7C0001F | (msb << 16) | (rd << 12) | (lsb << 7)
            }
            // --- UBFX ---
            27 => {
                let rd = rand_reg(rng);
                let rn = rand_reg(rng);
                let lsb = next_rand(rng) & 0x1F;
                let width = (next_rand(rng) % (32 - lsb).max(1)) + 1;
                let widthm1 = (width - 1).min(31);
                0xE7E00050 | (widthm1 << 16) | (rd << 12) | (lsb << 7) | rn
            }
            // --- RSB immediate (common in SDK) ---
            28 => {
                let rd = rand_reg(rng);
                let rn = rand_reg(rng);
                let imm8 = next_rand(rng) & 0xFF;
                0xE2600000 | (rn << 16) | (rd << 12) | imm8
            }
            // --- NOP ---
            _ => 0xE320F000,
        }
    }

    /// Safe data memory base for fuzzed memory ops. Must be:
    /// - aligned and clear of code (code starts at PC=0)
    /// - clear of stack (SP=0x8000), with enough headroom for negative offsets
    /// - encodable as an ARM immediate via MOVW (16-bit imm).
    /// We use 0x4000 (16 KB) which leaves room for ±256B offsets and predates SP.
    const FUZZ_DATA_BASE: u32 = 0x4000;

    /// Encode `MOVW Rd, #imm16` (ARM, cond=AL).
    fn movw_imm(rd: u32, imm16: u32) -> u32 {
        let imm4 = (imm16 >> 12) & 0xF;
        let imm12 = imm16 & 0xFFF;
        0xE3000000 | (imm4 << 16) | (rd << 12) | imm12
    }

    /// Encode `MOVT Rd, #imm16` (clears upper 16 bits would need MOVW first).
    #[allow(dead_code)]
    fn movt_imm(rd: u32, imm16: u32) -> u32 {
        let imm4 = (imm16 >> 12) & 0xF;
        let imm12 = imm16 & 0xFFF;
        0xE3400000 | (imm4 << 16) | (rd << 12) | imm12
    }

    /// Pre-load a safe base address into Rn so subsequent memory ops can't
    /// trap. Pushes a `MOVW Rn, #base` instruction.
    fn setup_base(code: &mut Vec<u32>, rn: u32, base: u32) {
        // MOVW handles 16-bit immediates — base must fit. Caller picks base.
        debug_assert!(base <= 0xFFFF, "base must fit in MOVW imm16");
        code.push(movw_imm(rn, base));
    }

    /// Generate a memory load/store and append to `code`. Pre-loads a safe
    /// base into Rn first. Avoids R15 (PC) for both Rt and Rn so we don't
    /// branch unexpectedly. For LDM/STM also avoids using PC in register list.
    fn gen_memory_instruction(rng: &mut u64, code: &mut Vec<u32>) {
        let r = next_rand(rng);
        match r % 20 {
            // --- LDRSB Rt, [Rn, #imm8] (offset, P=1, U=1, W=0) ---
            0 => {
                let rn = rand_reg(rng);
                let rt = rand_reg_ne(rng, rn);
                setup_base(code, rn, FUZZ_DATA_BASE);
                let imm8 = next_rand(rng) & 0x7F; // small positive offset
                let imm4h = (imm8 >> 4) & 0xF;
                let imm4l = imm8 & 0xF;
                // ARM LDRSB imm: cond=E, P=1 U=1 1 W=0 1, Rn, Rt, imm4H, 1 1 0 1, imm4L
                code.push(0xE1D000D0 | (rn << 16) | (rt << 12) | (imm4h << 8) | imm4l);
            }
            // --- LDRSH Rt, [Rn, #imm8] ---
            1 => {
                let rn = rand_reg(rng);
                let rt = rand_reg_ne(rng, rn);
                setup_base(code, rn, FUZZ_DATA_BASE);
                let imm8 = (next_rand(rng) & 0x7E) | 0; // even offset (halfword aligned)
                let imm4h = (imm8 >> 4) & 0xF;
                let imm4l = imm8 & 0xF;
                // ARM LDRSH imm: bits[7:4] = 1111
                code.push(0xE1D000F0 | (rn << 16) | (rt << 12) | (imm4h << 8) | imm4l);
            }
            // --- LDRB Rt, [Rn, #imm12] ---
            2 => {
                let rn = rand_reg(rng);
                let rt = rand_reg_ne(rng, rn);
                setup_base(code, rn, FUZZ_DATA_BASE);
                let imm12 = next_rand(rng) & 0x3F; // small positive offset
                code.push(0xE5D00000 | (rn << 16) | (rt << 12) | imm12);
            }
            // --- LDRH Rt, [Rn, #imm8] ---
            3 => {
                let rn = rand_reg(rng);
                let rt = rand_reg_ne(rng, rn);
                setup_base(code, rn, FUZZ_DATA_BASE);
                let imm8 = (next_rand(rng) & 0x7E) | 0; // even offset
                let imm4h = (imm8 >> 4) & 0xF;
                let imm4l = imm8 & 0xF;
                // ARM LDRH imm: bits[7:4] = 1011
                code.push(0xE1D000B0 | (rn << 16) | (rt << 12) | (imm4h << 8) | imm4l);
            }
            // --- STRB Rt, [Rn, #imm12] ---
            4 => {
                let rn = rand_reg(rng);
                let rt = rand_reg_ne(rng, rn);
                setup_base(code, rn, FUZZ_DATA_BASE);
                let imm12 = next_rand(rng) & 0x3F;
                code.push(0xE5C00000 | (rn << 16) | (rt << 12) | imm12);
            }
            // --- STRH Rt, [Rn, #imm8] ---
            5 => {
                let rn = rand_reg(rng);
                let rt = rand_reg_ne(rng, rn);
                setup_base(code, rn, FUZZ_DATA_BASE);
                let imm8 = (next_rand(rng) & 0x7E) | 0;
                let imm4h = (imm8 >> 4) & 0xF;
                let imm4l = imm8 & 0xF;
                code.push(0xE1C000B0 | (rn << 16) | (rt << 12) | (imm4h << 8) | imm4l);
            }
            // --- LDR Rt, [Rn, #imm12] ---
            6 => {
                let rn = rand_reg(rng);
                let rt = rand_reg_ne(rng, rn);
                setup_base(code, rn, FUZZ_DATA_BASE);
                let imm12 = (next_rand(rng) & 0x3C) | 0; // word-aligned offset
                code.push(0xE5900000 | (rn << 16) | (rt << 12) | imm12);
            }
            // --- STR Rt, [Rn, #imm12] ---
            7 => {
                let rn = rand_reg(rng);
                let rt = rand_reg_ne(rng, rn);
                setup_base(code, rn, FUZZ_DATA_BASE);
                let imm12 = (next_rand(rng) & 0x3C) | 0;
                code.push(0xE5800000 | (rn << 16) | (rt << 12) | imm12);
            }
            // --- LDR with writeback: LDR Rt, [Rn, #imm12]! (pre-indexed, W=1) ---
            8 => {
                let rn = rand_reg(rng);
                let rt = rand_reg_ne(rng, rn);
                setup_base(code, rn, FUZZ_DATA_BASE);
                let imm12 = (next_rand(rng) & 0x3C) | 0;
                // P=1, U=1, B=0, W=1, L=1
                code.push(0xE5B00000 | (rn << 16) | (rt << 12) | imm12);
            }
            // --- LDM Rn, {reg_list} (no writeback, ascending IA) ---
            9 => {
                let rn = rand_reg(rng);
                setup_base(code, rn, FUZZ_DATA_BASE);
                // Reg list: pick 1-4 registers from r0..r12 (avoid PC, also avoid Rn to prevent
                // base-register-in-list ambiguity)
                let mut reglist = 0u32;
                let count = (next_rand(rng) % 4) + 1;
                let mut picked = 0u32;
                let mut attempts = 0;
                while picked < count && attempts < 40 {
                    attempts += 1;
                    let r = rand_reg(rng);
                    if r == rn {
                        continue;
                    }
                    if reglist & (1 << r) != 0 {
                        continue;
                    }
                    reglist |= 1 << r;
                    picked += 1;
                }
                if reglist == 0 {
                    code.push(0xE320F000); // NOP fallback
                    return;
                }
                // LDMIA Rn, {reglist}: cond=E, 100 P=0 U=1 S=0 W=0 L=1, Rn, reglist
                code.push(0xE8900000 | (rn << 16) | reglist);
            }
            // --- LDMIA Rn!, {reg_list} (writeback variant) ---
            10 => {
                let rn = rand_reg(rng);
                setup_base(code, rn, FUZZ_DATA_BASE);
                let mut reglist = 0u32;
                let count = (next_rand(rng) % 4) + 1;
                let mut picked = 0u32;
                let mut attempts = 0;
                while picked < count && attempts < 40 {
                    attempts += 1;
                    let r = rand_reg(rng);
                    if r == rn {
                        continue;
                    }
                    if reglist & (1 << r) != 0 {
                        continue;
                    }
                    reglist |= 1 << r;
                    picked += 1;
                }
                if reglist == 0 {
                    code.push(0xE320F000);
                    return;
                }
                // LDMIA Rn!, {reglist}: W=1
                code.push(0xE8B00000 | (rn << 16) | reglist);
            }
            // --- STMIA Rn!, {reg_list} ---
            11 => {
                let rn = rand_reg(rng);
                setup_base(code, rn, FUZZ_DATA_BASE);
                let mut reglist = 0u32;
                let count = (next_rand(rng) % 4) + 1;
                let mut picked = 0u32;
                let mut attempts = 0;
                while picked < count && attempts < 40 {
                    attempts += 1;
                    let r = rand_reg(rng);
                    if r == rn {
                        continue;
                    }
                    if reglist & (1 << r) != 0 {
                        continue;
                    }
                    reglist |= 1 << r;
                    picked += 1;
                }
                if reglist == 0 {
                    code.push(0xE320F000);
                    return;
                }
                // STMIA Rn!, {reglist}: cond=E, 100 P=0 U=1 S=0 W=1 L=0
                code.push(0xE8A00000 | (rn << 16) | reglist);
            }
            // --- LDR Rt, [Rn], #imm12 (post-indexed, P=0 W=0, implicit writeback) ---
            // Tests writeback ordering: Rn must be updated with base+imm; Rt
            // reads from the original base (data=0, but Rn delta is observable).
            12 => {
                let rn = rand_reg(rng);
                let rt = rand_reg_ne(rng, rn);
                setup_base(code, rn, FUZZ_DATA_BASE);
                let imm12 = (next_rand(rng) & 0x3C) | 0;
                // cond=E, 0100 P=0 U=1 B=0 W=0 L=1 => 0xE4900000
                code.push(0xE4900000 | (rn << 16) | (rt << 12) | imm12);
            }
            // --- STR Rt, [Rn], #imm12 (post-indexed store) ---
            13 => {
                let rn = rand_reg(rng);
                let rt = rand_reg_ne(rng, rn);
                setup_base(code, rn, FUZZ_DATA_BASE);
                let imm12 = (next_rand(rng) & 0x3C) | 0;
                // cond=E, 0100 P=0 U=1 B=0 W=0 L=0 => 0xE4800000
                code.push(0xE4800000 | (rn << 16) | (rt << 12) | imm12);
            }
            // --- LDR Rt, [Rn, #-imm12]! (pre-indexed writeback, U=0) ---
            // Base is set to FUZZ_DATA_BASE+0x100 so the negative offset stays
            // within the safe data region.
            14 => {
                let rn = rand_reg(rng);
                let rt = rand_reg_ne(rng, rn);
                setup_base(code, rn, FUZZ_DATA_BASE + 0x100);
                let imm12 = (next_rand(rng) & 0x3C) | 0;
                // cond=E, 0101 P=1 U=0 B=0 W=1 L=1 => 0xE5300000
                code.push(0xE5300000 | (rn << 16) | (rt << 12) | imm12);
            }
            // --- LDR Rt, [Rn, Rm] (register offset, no writeback) ---
            // Emits MOVW Rm, #offset before the load so Rm has a known safe value.
            15 => {
                let rn = rand_reg(rng);
                let rt = rand_reg_ne(rng, rn);
                let rm = rand_reg_ne(rng, rn);
                setup_base(code, rn, FUZZ_DATA_BASE);
                let offset = (next_rand(rng) & 0x3C) | 0;
                code.push(movw_imm(rm, offset));
                // cond=E, 0111 P=1 U=1 B=0 W=0 L=1, imm5=0 type=00 bit4=0
                code.push(0xE7900000 | (rn << 16) | (rt << 12) | rm);
            }
            // --- STRB+LDRSB pair (sign extension correctness) ---
            // Store a non-zero byte (low byte of random reg) then sign-extend
            // load it back. Exercises the signed-byte→32-bit conversion path.
            16 => {
                let rn = rand_reg(rng);
                let rt_store = rand_reg_ne(rng, rn);
                let rt_load = rand_reg_ne(rng, rn);
                setup_base(code, rn, FUZZ_DATA_BASE);
                let imm12 = next_rand(rng) & 0x3F;
                // STRB Rt_store, [Rn, #imm12]
                code.push(0xE5C00000 | (rn << 16) | (rt_store << 12) | imm12);
                // LDRSB Rt_load, [Rn, #imm8] — split imm into imm4H:imm4L
                let imm4h = (imm12 >> 4) & 0xF;
                let imm4l = imm12 & 0xF;
                code.push(0xE1D000D0 | (rn << 16) | (rt_load << 12) | (imm4h << 8) | imm4l);
            }
            // --- STRH+LDRSH pair (sign extension on halfword) ---
            17 => {
                let rn = rand_reg(rng);
                let rt_store = rand_reg_ne(rng, rn);
                let rt_load = rand_reg_ne(rng, rn);
                setup_base(code, rn, FUZZ_DATA_BASE);
                let imm8 = (next_rand(rng) & 0x3E) | 0; // even (halfword aligned)
                let imm4h = (imm8 >> 4) & 0xF;
                let imm4l = imm8 & 0xF;
                // STRH Rt_store, [Rn, #imm8]: bits[7:4]=1011
                code.push(0xE1C000B0 | (rn << 16) | (rt_store << 12) | (imm4h << 8) | imm4l);
                // LDRSH Rt_load, [Rn, #imm8]: bits[7:4]=1111
                code.push(0xE1D000F0 | (rn << 16) | (rt_load << 12) | (imm4h << 8) | imm4l);
            }
            // --- LDMIA Rn, {list including Rn} (no writeback, base in list) ---
            // ARMv7 legal without writeback: the loaded value overwrites Rn.
            // Exercises the "base register is also a destination" path.
            18 => {
                let rn = rand_reg(rng);
                setup_base(code, rn, FUZZ_DATA_BASE);
                let mut reglist = 1u32 << rn;
                let extra = (next_rand(rng) % 3) + 1;
                let mut picked = 0u32;
                let mut attempts = 0;
                while picked < extra && attempts < 40 {
                    attempts += 1;
                    let r = rand_reg(rng);
                    if reglist & (1 << r) != 0 {
                        continue;
                    }
                    reglist |= 1 << r;
                    picked += 1;
                }
                code.push(0xE8900000 | (rn << 16) | reglist);
            }
            // --- LDMIA Rn!, {single reg} (W=1, single non-Rn reg → legal) ---
            _ => {
                let rn = rand_reg(rng);
                let reg = rand_reg_ne(rng, rn);
                setup_base(code, rn, FUZZ_DATA_BASE);
                let reglist = 1u32 << reg;
                code.push(0xE8B00000 | (rn << 16) | reglist);
            }
        }
    }

    fn run_rdynarmic_with_optimizations(
        code: &[u32],
        regs: &[u32; 15],
        cpsr: u32,
        optimizations: OptimizationFlag,
    ) -> ([u32; 16], u32) {
        let mut code_with_loop = code.to_vec();
        code_with_loop.push(0xEAFFFFFE); // infinite loop

        let env = FuzzEnv::new(code_with_loop);
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
        let mut jit = A32Jit::new(config).expect("JIT creation failed");

        for i in 0..15 {
            jit.set_register(i, regs[i]);
        }
        jit.set_cpsr(cpsr);

        jit.run();

        let mut out = [0u32; 16];
        for i in 0..16 {
            out[i] = jit.get_register(i);
        }
        (out, jit.get_cpsr())
    }

    fn run_rdynarmic(code: &[u32], regs: &[u32; 15], cpsr: u32) -> ([u32; 16], u32) {
        run_rdynarmic_with_optimizations(code, regs, cpsr, OptimizationFlag::NO_OPTIMIZATIONS)
    }

    fn run_oracle(code: &[u32], regs: &[u32; 15], cpsr: u32) -> Option<([u32; 16], u32)> {
        let mut input = format!("{:08x}", cpsr);
        for r in regs {
            input += &format!(" {:08x}", r);
        }
        input += &format!(" {:x}", code.len());
        for insn in code {
            input += &format!(" {:08x}", insn);
        }
        input += "\n";

        let mut child = Command::new(ORACLE)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        child.stdin.as_mut()?.write_all(input.as_bytes()).ok()?;
        drop(child.stdin.take());

        let output = child.wait_with_output().ok()?;
        let line = String::from_utf8_lossy(&output.stdout);
        let tokens: Vec<u32> = line
            .trim()
            .split_whitespace()
            .filter_map(|s| u32::from_str_radix(s, 16).ok())
            .collect();

        if tokens.len() < 17 {
            return None;
        }

        let mut out = [0u32; 16];
        for i in 0..16 {
            out[i] = tokens[i];
        }
        Some((out, tokens[16]))
    }

    #[test]
    fn fuzz_compare_with_upstream() {
        let mut rng: u64 = 0xDEADBEEF12345678;
        let mut pass = 0u32;
        let mut fail = 0u32;
        let num_tests = 5000;

        for test_idx in 0..num_tests {
            // Generate random registers (avoid using R13/SP as random to prevent stack issues,
            // and R15/PC is always 0)
            let mut regs = [0u32; 15];
            for i in 0..13 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                regs[i] = rng as u32;
            }
            regs[13] = 0x8000; // SP = safe stack area

            let cpsr = 0x000001d0; // User mode

            // Generate 1-5 random instructions. With ~40% probability mix in
            // a memory operation slice (which emits a MOVW base setup + memop).
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let num_insns = ((rng as u32) % 5) + 1;
            let mut code = Vec::new();
            for _ in 0..num_insns {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                if (rng as u32) % 5 < 2 {
                    gen_memory_instruction(&mut rng, &mut code);
                } else {
                    code.push(gen_instruction(&mut rng));
                }
            }

            let (rdyn_regs, rdyn_cpsr) = run_rdynarmic(&code, &regs, cpsr);
            let oracle_result = run_oracle(&code, &regs, cpsr);

            if let Some((oracle_regs, oracle_cpsr)) = oracle_result {
                let regs_match = rdyn_regs == oracle_regs;
                // Compare CPSR but only NZCV bits (top 4 bits) since
                // other bits may differ due to mode handling
                let cpsr_match = (rdyn_cpsr & 0xF0000000) == (oracle_cpsr & 0xF0000000);

                if regs_match && cpsr_match {
                    pass += 1;
                } else {
                    fail += 1;
                    eprintln!("=== FUZZ MISMATCH test #{} ===", test_idx);
                    eprint!("  Code:");
                    for insn in &code {
                        eprint!(" {:08x}", insn);
                    }
                    eprintln!();
                    eprintln!("  Input regs: {:08x?}", &regs[..13]);
                    eprintln!("  Input regs: {:08x?}", &regs[..]);
                    if !regs_match {
                        for i in 0..16 {
                            if rdyn_regs[i] != oracle_regs[i] {
                                eprintln!(
                                    "  r{}: rdynarmic={:08x} upstream={:08x}",
                                    i, rdyn_regs[i], oracle_regs[i]
                                );
                            }
                        }
                    }
                    if !cpsr_match {
                        eprintln!(
                            "  CPSR: rdynarmic={:08x} upstream={:08x}",
                            rdyn_cpsr, oracle_cpsr
                        );
                    }
                    if fail >= 5 {
                        eprintln!("Stopping after 5 failures");
                        break;
                    }
                }
            } else {
                // Oracle failed to run, skip this test
                continue;
            }
        }

        eprintln!(
            "Fuzz results: {} passed, {} failed out of {} tests",
            pass,
            fail,
            pass + fail
        );
        assert_eq!(fail, 0, "Fuzz test found {} mismatches", fail);
    }

    // ================== Thumb memory fuzz ==================

    /// Thumb16 B.N . (infinite loop) — used to halt execution.
    const THUMB_HALT: u16 = 0xE7FE;
    /// Thumb16 NOP (MOV r8, r8). Used as padding.
    const THUMB_NOP: u16 = 0xBF00;

    /// Thumb16 MOVS Rd, #imm8 (encoding: 00100 Rd imm8). Rd must be r0-r7.
    fn thumb_movs_imm8(rd: u32, imm8: u32) -> u16 {
        debug_assert!(rd < 8 && imm8 < 0x100);
        (0x2000 | (rd << 8) | imm8) as u16
    }

    /// Thumb16 LSLS Rd, Rm, #imm5 (encoding: 00000 imm5 Rm Rd). All low regs.
    fn thumb_lsls_imm(rd: u32, rm: u32, imm5: u32) -> u16 {
        debug_assert!(rd < 8 && rm < 8 && imm5 < 32);
        (0x0000 | (imm5 << 6) | (rm << 3) | rd) as u16
    }

    /// Set low register `rn` (r0-r7) to FUZZ_DATA_BASE via `MOVS rn, #0x40`
    /// followed by `LSLS rn, rn, #8` (result = 0x4000).
    fn thumb_setup_base(hws: &mut Vec<u16>, rn: u32) {
        debug_assert!(rn < 8);
        hws.push(thumb_movs_imm8(rn, 0x40));
        hws.push(thumb_lsls_imm(rn, rn, 8));
    }

    /// Pick a low register (r0-r7) different from `other`.
    fn rand_low_reg_ne(rng: &mut u64, other: u32) -> u32 {
        loop {
            let r = next_rand(rng) & 7;
            if r != other {
                return r;
            }
        }
    }

    /// Pick a low register (r0-r7) different from both `a` and `b`.
    fn rand_low_reg_ne2(rng: &mut u64, a: u32, b: u32) -> u32 {
        loop {
            let r = next_rand(rng) & 7;
            if r != a && r != b {
                return r;
            }
        }
    }

    /// Generate a Thumb16 memory operation sequence. Emits a base-setup
    /// (MOVS/LSLS → Rn=0x4000) followed by the memory instruction(s).
    fn gen_thumb_memory(rng: &mut u64, hws: &mut Vec<u16>) {
        // Pick a low base register and point it at FUZZ_DATA_BASE.
        let rn = next_rand(rng) & 7;
        thumb_setup_base(hws, rn);

        let r = next_rand(rng);
        match r % 13 {
            // --- LDR Rt, [Rn, #imm5*4] (T1): 01101 imm5 Rn Rt ---
            0 => {
                let rt = rand_low_reg_ne(rng, rn);
                let imm5 = next_rand(rng) & 0x0F; // offset ≤ 60 bytes
                hws.push((0x6800 | (imm5 << 6) | (rn << 3) | rt) as u16);
            }
            // --- STR Rt, [Rn, #imm5*4] (T1): 01100 imm5 Rn Rt ---
            1 => {
                let rt = rand_low_reg_ne(rng, rn);
                let imm5 = next_rand(rng) & 0x0F;
                hws.push((0x6000 | (imm5 << 6) | (rn << 3) | rt) as u16);
            }
            // --- LDRB Rt, [Rn, #imm5] (T1): 01111 imm5 Rn Rt ---
            2 => {
                let rt = rand_low_reg_ne(rng, rn);
                let imm5 = next_rand(rng) & 0x1F;
                hws.push((0x7800 | (imm5 << 6) | (rn << 3) | rt) as u16);
            }
            // --- STRB Rt, [Rn, #imm5] (T1): 01110 imm5 Rn Rt ---
            3 => {
                let rt = rand_low_reg_ne(rng, rn);
                let imm5 = next_rand(rng) & 0x1F;
                hws.push((0x7000 | (imm5 << 6) | (rn << 3) | rt) as u16);
            }
            // --- LDRH Rt, [Rn, #imm5*2] (T1): 10001 imm5 Rn Rt ---
            4 => {
                let rt = rand_low_reg_ne(rng, rn);
                let imm5 = next_rand(rng) & 0x0F;
                hws.push((0x8800 | (imm5 << 6) | (rn << 3) | rt) as u16);
            }
            // --- STRH Rt, [Rn, #imm5*2] (T1): 10000 imm5 Rn Rt ---
            5 => {
                let rt = rand_low_reg_ne(rng, rn);
                let imm5 = next_rand(rng) & 0x0F;
                hws.push((0x8000 | (imm5 << 6) | (rn << 3) | rt) as u16);
            }
            // --- LDR Rt, [Rn, Rm] (T1): 0101100 Rm Rn Rt ---
            // Rm preloaded with a small word-aligned offset.
            6 => {
                let rt = rand_low_reg_ne(rng, rn);
                let rm = rand_low_reg_ne2(rng, rn, rt);
                hws.push(thumb_movs_imm8(rm, next_rand(rng) & 0x3C));
                hws.push((0x5800 | (rm << 6) | (rn << 3) | rt) as u16);
            }
            // --- STR Rt, [Rn, Rm] (T1): 0101000 ---
            7 => {
                let rt = rand_low_reg_ne(rng, rn);
                let rm = rand_low_reg_ne2(rng, rn, rt);
                hws.push(thumb_movs_imm8(rm, next_rand(rng) & 0x3C));
                hws.push((0x5000 | (rm << 6) | (rn << 3) | rt) as u16);
            }
            // --- LDRSB Rt, [Rn, Rm]: 0101011 — sign-extend byte ---
            8 => {
                let rt = rand_low_reg_ne(rng, rn);
                let rm = rand_low_reg_ne2(rng, rn, rt);
                hws.push(thumb_movs_imm8(rm, next_rand(rng) & 0x3F));
                hws.push((0x5600 | (rm << 6) | (rn << 3) | rt) as u16);
            }
            // --- LDRSH Rt, [Rn, Rm]: 0101111 — sign-extend halfword ---
            9 => {
                let rt = rand_low_reg_ne(rng, rn);
                let rm = rand_low_reg_ne2(rng, rn, rt);
                hws.push(thumb_movs_imm8(rm, next_rand(rng) & 0x3E)); // halfword-aligned
                hws.push((0x5E00 | (rm << 6) | (rn << 3) | rt) as u16);
            }
            // --- LDMIA Rn!, {list} (T1): 11001 Rn list8 ---
            // Writeback variant — Rn excluded from list per ARMv7.
            10 => {
                let mut list = 0u32;
                let mut picked = 0u32;
                let mut attempts = 0;
                while picked < 2 && attempts < 40 {
                    attempts += 1;
                    let r = next_rand(rng) & 7;
                    if r == rn {
                        continue;
                    }
                    if list & (1 << r) != 0 {
                        continue;
                    }
                    list |= 1 << r;
                    picked += 1;
                }
                if list == 0 {
                    hws.push(THUMB_NOP);
                    return;
                }
                hws.push((0xC800 | (rn << 8) | list) as u16);
            }
            // --- STMIA Rn!, {list} (T1): 11000 Rn list8 ---
            11 => {
                let mut list = 0u32;
                let mut picked = 0u32;
                let mut attempts = 0;
                while picked < 2 && attempts < 40 {
                    attempts += 1;
                    let r = next_rand(rng) & 7;
                    if r == rn {
                        continue;
                    }
                    if list & (1 << r) != 0 {
                        continue;
                    }
                    list |= 1 << r;
                    picked += 1;
                }
                if list == 0 {
                    hws.push(THUMB_NOP);
                    return;
                }
                hws.push((0xC000 | (rn << 8) | list) as u16);
            }
            // --- LDR Rt, [PC, #imm8*4] (T1 literal): 01001 Rt imm8 ---
            // Target reads from our code buffer; both emulators see identical
            // code_mem so the loaded value must match.
            _ => {
                let rt = next_rand(rng) & 7;
                let imm8 = next_rand(rng) & 0x07; // stay close to PC
                hws.push((0x4800 | (rt << 8) | imm8) as u16);
            }
        }
    }

    /// Pack a stream of Thumb halfwords into u32 words, little-endian
    /// (low halfword = low 16 bits of the u32). Pads to even count with NOP.
    fn pack_thumb(hws: &[u16]) -> Vec<u32> {
        let mut code = Vec::new();
        let mut i = 0;
        while i < hws.len() {
            let lo = hws[i] as u32;
            let hi = if i + 1 < hws.len() {
                hws[i + 1] as u32
            } else {
                THUMB_NOP as u32
            };
            code.push(lo | (hi << 16));
            i += 2;
        }
        code
    }

    #[test]
    fn fuzz_compare_thumb_memory_with_upstream() {
        let mut rng: u64 = 0xA1B2_C3D4_E5F6_0789;
        let mut pass = 0u32;
        let mut fail = 0u32;
        let num_tests = 2000;

        for test_idx in 0..num_tests {
            let mut regs = [0u32; 15];
            for i in 0..13 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                regs[i] = rng as u32;
            }
            regs[13] = 0x8000;

            // User mode + T bit set: 0x1d0 | 0x20 = 0x1f0.
            let cpsr = 0x000001f0;

            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let num_ops = ((rng as u32) % 3) + 1;

            let mut hws: Vec<u16> = Vec::new();
            for _ in 0..num_ops {
                gen_thumb_memory(&mut rng, &mut hws);
            }
            // Terminate with a Thumb halt pair so execution spins inside our
            // code buffer rather than decoding the ARM-mode fallback word.
            hws.push(THUMB_HALT);
            hws.push(THUMB_HALT);

            let code = pack_thumb(&hws);

            let (rdyn_regs, rdyn_cpsr) = run_rdynarmic(&code, &regs, cpsr);
            let oracle_result = run_oracle(&code, &regs, cpsr);

            if let Some((oracle_regs, oracle_cpsr)) = oracle_result {
                let regs_match = rdyn_regs == oracle_regs;
                let cpsr_match = (rdyn_cpsr & 0xF0000000) == (oracle_cpsr & 0xF0000000);

                if regs_match && cpsr_match {
                    pass += 1;
                } else {
                    fail += 1;
                    eprintln!("=== THUMB FUZZ MISMATCH test #{} ===", test_idx);
                    eprint!("  Halfwords:");
                    for hw in &hws {
                        eprint!(" {:04x}", hw);
                    }
                    eprintln!();
                    eprint!("  Code words:");
                    for insn in &code {
                        eprint!(" {:08x}", insn);
                    }
                    eprintln!();
                    eprintln!("  Input regs: {:08x?}", &regs[..]);
                    if !regs_match {
                        for i in 0..16 {
                            if rdyn_regs[i] != oracle_regs[i] {
                                eprintln!(
                                    "  r{}: rdynarmic={:08x} upstream={:08x}",
                                    i, rdyn_regs[i], oracle_regs[i]
                                );
                            }
                        }
                    }
                    if !cpsr_match {
                        eprintln!(
                            "  CPSR: rdynarmic={:08x} upstream={:08x}",
                            rdyn_cpsr, oracle_cpsr
                        );
                    }
                    if fail >= 5 {
                        eprintln!("Stopping after 5 failures");
                        break;
                    }
                }
            } else {
                continue;
            }
        }

        eprintln!(
            "Thumb fuzz results: {} passed, {} failed out of {} tests",
            pass,
            fail,
            pass + fail
        );
        assert_eq!(fail, 0, "Thumb fuzz test found {} mismatches", fail);
    }

    // ================== Thumb32 memory fuzz ==================

    /// Thumb32 MOVW Rd, #imm16 (T3 encoding). Returns (hw0, hw1).
    /// hw0 = 11110 i 100100 imm4 / hw1 = 0 imm3 Rd imm8
    /// imm16 = imm4:i:imm3:imm8
    fn thumb32_movw(rd: u32, imm16: u32) -> (u16, u16) {
        debug_assert!(rd < 15 && imm16 <= 0xFFFF);
        let imm4 = (imm16 >> 12) & 0xF;
        let i = (imm16 >> 11) & 1;
        let imm3 = (imm16 >> 8) & 7;
        let imm8 = imm16 & 0xFF;
        let hw0 = 0xF240 | ((i as u16) << 10) | imm4 as u16;
        let hw1 = ((imm3 as u16) << 12) | ((rd as u16) << 8) | imm8 as u16;
        (hw0, hw1)
    }

    /// Point `rn` (any r0-r12) at FUZZ_DATA_BASE via a Thumb32 MOVW.T3.
    fn thumb32_setup_base(hws: &mut Vec<u16>, rn: u32, base: u32) {
        let (hw0, hw1) = thumb32_movw(rn, base);
        hws.push(hw0);
        hws.push(hw1);
    }

    /// Pick an r0-r12 register different from `other`.
    fn rand_reg_r12_ne(rng: &mut u64, other: u32) -> u32 {
        loop {
            let r = next_rand(rng) % 13;
            if r != other {
                return r;
            }
        }
    }

    /// Pick an r0-r12 register different from both `a` and `b`.
    fn rand_reg_r12_ne2(rng: &mut u64, a: u32, b: u32) -> u32 {
        loop {
            let r = next_rand(rng) % 13;
            if r != a && r != b {
                return r;
            }
        }
    }

    /// Generate a Thumb32 memory instruction sequence. Emits a Thumb32 MOVW
    /// base setup (2 halfwords) then the memory op (also 2 halfwords).
    fn gen_thumb32_memory(rng: &mut u64, hws: &mut Vec<u16>) {
        let r = next_rand(rng);
        match r % 14 {
            // --- LDRD Rt, Rt2, [Rn, #imm8*4] T1 (P=1, U=1, W=0) ---
            // hw0 = 0xE9D0 | Rn, hw1 = (Rt<<12) | (Rt2<<8) | imm8
            0 => {
                let rn = next_rand(rng) % 13;
                thumb32_setup_base(hws, rn, FUZZ_DATA_BASE);
                let rt = rand_reg_r12_ne(rng, rn);
                let rt2 = rand_reg_r12_ne2(rng, rn, rt);
                let imm8 = next_rand(rng) & 0x0F;
                hws.push((0xE9D0 | rn) as u16);
                hws.push(((rt << 12) | (rt2 << 8) | imm8) as u16);
            }
            // --- STRD T1 offset: hw0 = 0xE9C0 | Rn ---
            1 => {
                let rn = next_rand(rng) % 13;
                thumb32_setup_base(hws, rn, FUZZ_DATA_BASE);
                let rt = rand_reg_r12_ne(rng, rn);
                let rt2 = rand_reg_r12_ne2(rng, rn, rt);
                let imm8 = next_rand(rng) & 0x0F;
                hws.push((0xE9C0 | rn) as u16);
                hws.push(((rt << 12) | (rt2 << 8) | imm8) as u16);
            }
            // --- LDRD T1 pre-indexed writeback (P=1, U=1, W=1) hw0 bit[5]=1 ---
            // 1110_1001_11W1_Rn: W=1 sets bit[5] of hw0 → 0xE9F0 | Rn
            2 => {
                let rn = next_rand(rng) % 13;
                thumb32_setup_base(hws, rn, FUZZ_DATA_BASE);
                let rt = rand_reg_r12_ne(rng, rn);
                let rt2 = rand_reg_r12_ne2(rng, rn, rt);
                let imm8 = next_rand(rng) & 0x0F;
                hws.push((0xE9F0 | rn) as u16);
                hws.push(((rt << 12) | (rt2 << 8) | imm8) as u16);
            }
            // --- LDRD T1 post-indexed writeback (P=0, U=1, W=1) ---
            // hw0 = 1110_1000_11_1_1_Rn = 0xE8F0 | Rn (P=0 clears bit[8], W=1 sets bit[5])
            3 => {
                let rn = next_rand(rng) % 13;
                thumb32_setup_base(hws, rn, FUZZ_DATA_BASE);
                let rt = rand_reg_r12_ne(rng, rn);
                let rt2 = rand_reg_r12_ne2(rng, rn, rt);
                let imm8 = next_rand(rng) & 0x0F;
                hws.push((0xE8F0 | rn) as u16);
                hws.push(((rt << 12) | (rt2 << 8) | imm8) as u16);
            }
            // --- STRD T1 pre-indexed writeback: 0xE9E0 | Rn (bit[4]=0 for STRD, bit[5]=W=1) ---
            4 => {
                let rn = next_rand(rng) % 13;
                thumb32_setup_base(hws, rn, FUZZ_DATA_BASE);
                let rt = rand_reg_r12_ne(rng, rn);
                let rt2 = rand_reg_r12_ne2(rng, rn, rt);
                let imm8 = next_rand(rng) & 0x0F;
                hws.push((0xE9E0 | rn) as u16);
                hws.push(((rt << 12) | (rt2 << 8) | imm8) as u16);
            }
            // --- STRD T1 post-indexed writeback: 0xE8E0 | Rn ---
            5 => {
                let rn = next_rand(rng) % 13;
                thumb32_setup_base(hws, rn, FUZZ_DATA_BASE);
                let rt = rand_reg_r12_ne(rng, rn);
                let rt2 = rand_reg_r12_ne2(rng, rn, rt);
                let imm8 = next_rand(rng) & 0x0F;
                hws.push((0xE8E0 | rn) as u16);
                hws.push(((rt << 12) | (rt2 << 8) | imm8) as u16);
            }
            // --- LDR.W Rt, [Rn, #imm12] T3 (P=1 U=1 W=0) ---
            // hw0 = 0xF8D0 | Rn, hw1 = (Rt<<12) | imm12
            6 => {
                let rn = next_rand(rng) % 13;
                thumb32_setup_base(hws, rn, FUZZ_DATA_BASE);
                let rt = rand_reg_r12_ne(rng, rn);
                let imm12 = (next_rand(rng) & 0x3C) | 0;
                hws.push((0xF8D0 | rn) as u16);
                hws.push(((rt << 12) | imm12) as u16);
            }
            // --- STR.W Rt, [Rn, #imm12] T3: hw0 = 0xF8C0 | Rn ---
            7 => {
                let rn = next_rand(rng) % 13;
                thumb32_setup_base(hws, rn, FUZZ_DATA_BASE);
                let rt = rand_reg_r12_ne(rng, rn);
                let imm12 = (next_rand(rng) & 0x3C) | 0;
                hws.push((0xF8C0 | rn) as u16);
                hws.push(((rt << 12) | imm12) as u16);
            }
            // --- LDRSB.W Rt, [Rn, #imm12] T1: hw0 = 0xF990 | Rn ---
            8 => {
                let rn = next_rand(rng) % 13;
                thumb32_setup_base(hws, rn, FUZZ_DATA_BASE);
                let rt = rand_reg_r12_ne(rng, rn);
                let imm12 = next_rand(rng) & 0x3F;
                hws.push((0xF990 | rn) as u16);
                hws.push(((rt << 12) | imm12) as u16);
            }
            // --- LDRSH.W Rt, [Rn, #imm12] T1: hw0 = 0xF9B0 | Rn ---
            9 => {
                let rn = next_rand(rng) % 13;
                thumb32_setup_base(hws, rn, FUZZ_DATA_BASE);
                let rt = rand_reg_r12_ne(rng, rn);
                let imm12 = next_rand(rng) & 0x3E;
                hws.push((0xF9B0 | rn) as u16);
                hws.push(((rt << 12) | imm12) as u16);
            }
            // --- LDM.W Rn, {reglist} T2 (no writeback, W=0) ---
            // hw0 = 0xE890 | Rn, hw1 = reglist13 (bits[12:0] for r0-r12)
            10 => {
                let rn = next_rand(rng) % 13;
                thumb32_setup_base(hws, rn, FUZZ_DATA_BASE);
                let mut reglist = 0u32;
                let count = (next_rand(rng) % 4) + 2; // at least 2 regs for T2
                let mut picked = 0u32;
                let mut attempts = 0;
                while picked < count && attempts < 40 {
                    attempts += 1;
                    let r = next_rand(rng) % 13;
                    if r == rn {
                        continue;
                    }
                    if reglist & (1 << r) != 0 {
                        continue;
                    }
                    reglist |= 1 << r;
                    picked += 1;
                }
                if reglist.count_ones() < 2 {
                    hws.push(THUMB_NOP);
                    hws.push(THUMB_NOP);
                    return;
                }
                hws.push((0xE890 | rn) as u16);
                hws.push(reglist as u16);
            }
            // --- LDMIA.W Rn!, {reglist} T2 (writeback, W=1) hw0 = 0xE8B0 | Rn ---
            11 => {
                let rn = next_rand(rng) % 13;
                thumb32_setup_base(hws, rn, FUZZ_DATA_BASE);
                let mut reglist = 0u32;
                let count = (next_rand(rng) % 4) + 2;
                let mut picked = 0u32;
                let mut attempts = 0;
                while picked < count && attempts < 40 {
                    attempts += 1;
                    let r = next_rand(rng) % 13;
                    if r == rn {
                        continue;
                    }
                    if reglist & (1 << r) != 0 {
                        continue;
                    }
                    reglist |= 1 << r;
                    picked += 1;
                }
                if reglist.count_ones() < 2 {
                    hws.push(THUMB_NOP);
                    hws.push(THUMB_NOP);
                    return;
                }
                hws.push((0xE8B0 | rn) as u16);
                hws.push(reglist as u16);
            }
            // --- STMIA.W Rn!, {reglist} T2 (writeback): hw0 = 0xE8A0 | Rn ---
            12 => {
                let rn = next_rand(rng) % 13;
                thumb32_setup_base(hws, rn, FUZZ_DATA_BASE);
                let mut reglist = 0u32;
                let count = (next_rand(rng) % 4) + 2;
                let mut picked = 0u32;
                let mut attempts = 0;
                while picked < count && attempts < 40 {
                    attempts += 1;
                    let r = next_rand(rng) % 13;
                    if r == rn {
                        continue;
                    }
                    if reglist & (1 << r) != 0 {
                        continue;
                    }
                    reglist |= 1 << r;
                    picked += 1;
                }
                if reglist.count_ones() < 2 {
                    hws.push(THUMB_NOP);
                    hws.push(THUMB_NOP);
                    return;
                }
                hws.push((0xE8A0 | rn) as u16);
                hws.push(reglist as u16);
            }
            // --- LDREX + STREX pair on same address ---
            // LDREX Rt, [Rn, #imm8*4]: hw0 = 0xE850 | Rn, hw1 = (Rt<<12) | 0x0F00 | imm8
            // STREX Rd, Rt2, [Rn, #imm8*4]: hw0 = 0xE840 | Rn, hw1 = (Rt2<<12) | (Rd<<8) | imm8
            // The FuzzEnv exclusive_write_* callbacks always return true (success),
            // so the store should succeed symmetrically in both emulators.
            _ => {
                let rn = next_rand(rng) % 13;
                thumb32_setup_base(hws, rn, FUZZ_DATA_BASE);
                let rt = rand_reg_r12_ne(rng, rn);
                let rt2 = rand_reg_r12_ne2(rng, rn, rt);
                let rd = rand_reg_r12_ne2(rng, rn, rt); // Rd must differ from Rn/Rt/Rt2
                let imm8 = next_rand(rng) & 0x0F;
                // LDREX
                hws.push((0xE850 | rn) as u16);
                hws.push(((rt << 12) | 0x0F00 | imm8) as u16);
                // STREX Rd, Rt2, [Rn, #same_imm8*4]
                hws.push((0xE840 | rn) as u16);
                hws.push(((rt2 << 12) | (rd << 8) | imm8) as u16);
            }
        }
    }

    #[test]
    fn fuzz_compare_thumb32_memory_with_upstream() {
        let mut rng: u64 = 0x1357_9BDF_2468_ACE0;
        let mut pass = 0u32;
        let mut fail = 0u32;
        let num_tests = 1500;

        for test_idx in 0..num_tests {
            let mut regs = [0u32; 15];
            for i in 0..13 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                regs[i] = rng as u32;
            }
            regs[13] = 0x8000;

            let cpsr = 0x000001f0; // USR + T bit

            // Exercise multi-op blocks: up to 3 Thumb32 memory ops chained
            // inside the same JIT block. Earlier runs with num_ops>=2 exposed
            // the STR.W #0 → STR_reg decoder collapse and the scattered-field
            // imm12() bug in compute_thumb32_ls_address; both are fixed.
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let num_ops = ((rng as u32) % 3) + 1;
            let mut hws: Vec<u16> = Vec::new();
            for _ in 0..num_ops {
                gen_thumb32_memory(&mut rng, &mut hws);
            }
            hws.push(THUMB_HALT);
            hws.push(THUMB_HALT);

            let code = pack_thumb(&hws);

            let (rdyn_regs, rdyn_cpsr) = run_rdynarmic(&code, &regs, cpsr);
            let oracle_result = run_oracle(&code, &regs, cpsr);

            if let Some((oracle_regs, oracle_cpsr)) = oracle_result {
                let regs_match = rdyn_regs == oracle_regs;
                let cpsr_match = (rdyn_cpsr & 0xF0000000) == (oracle_cpsr & 0xF0000000);

                if regs_match && cpsr_match {
                    pass += 1;
                } else {
                    fail += 1;
                    eprintln!("=== THUMB32 FUZZ MISMATCH test #{} ===", test_idx);
                    eprint!("  Halfwords:");
                    for hw in &hws {
                        eprint!(" {:04x}", hw);
                    }
                    eprintln!();
                    eprint!("  Code words:");
                    for insn in &code {
                        eprint!(" {:08x}", insn);
                    }
                    eprintln!();
                    eprintln!("  Input regs: {:08x?}", &regs[..]);
                    if !regs_match {
                        for i in 0..16 {
                            if rdyn_regs[i] != oracle_regs[i] {
                                eprintln!(
                                    "  r{}: rdynarmic={:08x} upstream={:08x}",
                                    i, rdyn_regs[i], oracle_regs[i]
                                );
                            }
                        }
                    }
                    if !cpsr_match {
                        eprintln!(
                            "  CPSR: rdynarmic={:08x} upstream={:08x}",
                            rdyn_cpsr, oracle_cpsr
                        );
                    }
                    if fail >= 5 {
                        eprintln!("Stopping after 5 failures");
                        break;
                    }
                }
            } else {
                continue;
            }
        }

        eprintln!(
            "Thumb32 fuzz results: {} passed, {} failed out of {} tests",
            pass,
            fail,
            pass + fail
        );
        assert_eq!(fail, 0, "Thumb32 fuzz test found {} mismatches", fail);
    }

    // --- Focused reproducers for the same-block store→load bug ---
    //
    // Bug: when the fuzzer runs with num_ops>=2, a Thumb32 store followed
    // by a Thumb32 load inside the same JIT block sometimes reads back 0
    // instead of the stored value. Suspected in the AccType::Atomic path
    // (STM/LDM emit Atomic-typed memory ops; STRD emits a 64-bit ATOMIC).
    //
    // These reproducers isolate the smallest failing program per variant
    // so we can compare rdynarmic vs a32_oracle output and narrow down the
    // layer (translator/IR/backend) that drops the data.

    fn thumb32_str_w(rt: u32, rn: u32, imm12: u32) -> (u16, u16) {
        ((0xF8C0 | rn) as u16, ((rt << 12) | (imm12 & 0xFFF)) as u16)
    }

    fn thumb32_ldr_w(rt: u32, rn: u32, imm12: u32) -> (u16, u16) {
        ((0xF8D0 | rn) as u16, ((rt << 12) | (imm12 & 0xFFF)) as u16)
    }

    /// Build a Thumb program and run both engines. Returns `(rdyn, oracle)`
    /// register arrays so the caller can assert. Panics if the oracle is
    /// unreachable.
    fn run_thumb_pair(hws: &[u16], regs: &[u32; 15]) -> ([u32; 16], [u32; 16]) {
        let cpsr = 0x0000_01F0; // USR + T
        let code = pack_thumb(hws);
        let (rdyn, _) = run_rdynarmic(&code, regs, cpsr);
        let (oracle, _) = run_oracle(&code, regs, cpsr).expect("oracle available");
        (rdyn, oracle)
    }

    /// Dump a programs halfwords, input regs, and the two engine outputs.
    fn dump_diff(label: &str, hws: &[u16], regs: &[u32; 15], rdyn: &[u32; 16], oracle: &[u32; 16]) {
        eprintln!("=== {} ===", label);
        eprint!("  hws:");
        for hw in hws {
            eprint!(" {:04x}", hw);
        }
        eprintln!();
        eprintln!("  regs_in: {:08x?}", &regs[..]);
        for i in 0..16 {
            if rdyn[i] != oracle[i] {
                eprintln!(
                    "  r{:<2} rdyn={:08x} oracle={:08x}  DIFF",
                    i, rdyn[i], oracle[i]
                );
            }
        }
    }

    /// Pattern A: STR.W R1,[R0] ; LDR.W R3,[R2]  with R0==R2==FUZZ_DATA_BASE.
    /// Two distinct base registers pointing at the same address.
    #[test]
    fn repro_thumb32_str_then_ldr_aliased_bases() {
        let mut regs = [0u32; 15];
        regs[1] = 0xDEAD_BEEF;
        regs[13] = 0x8000;

        let mut hws = Vec::new();
        thumb32_setup_base(&mut hws, 0, FUZZ_DATA_BASE);
        thumb32_setup_base(&mut hws, 2, FUZZ_DATA_BASE);
        let (a, b) = thumb32_str_w(1, 0, 0);
        hws.push(a);
        hws.push(b);
        let (a, b) = thumb32_ldr_w(3, 2, 0);
        hws.push(a);
        hws.push(b);
        hws.push(THUMB_HALT);
        hws.push(THUMB_HALT);

        let (rdyn, oracle) = run_thumb_pair(&hws, &regs);
        if rdyn != oracle {
            dump_diff("aliased_bases", &hws, &regs, &rdyn, &oracle);
        }
        assert_eq!(rdyn[3], oracle[3], "R3 should match oracle");
    }

    /// Pattern B: STR.W R1,[R0] ; LDR.W R3,[R0]  (same base reg for both).
    #[test]
    fn repro_thumb32_str_then_ldr_same_base() {
        let mut regs = [0u32; 15];
        regs[1] = 0xCAFE_F00D;
        regs[13] = 0x8000;

        let mut hws = Vec::new();
        thumb32_setup_base(&mut hws, 0, FUZZ_DATA_BASE);
        let (a, b) = thumb32_str_w(1, 0, 0);
        hws.push(a);
        hws.push(b);
        let (a, b) = thumb32_ldr_w(3, 0, 0);
        hws.push(a);
        hws.push(b);
        hws.push(THUMB_HALT);
        hws.push(THUMB_HALT);

        let (rdyn, oracle) = run_thumb_pair(&hws, &regs);
        if rdyn != oracle {
            dump_diff("same_base", &hws, &regs, &rdyn, &oracle);
        }
        assert_eq!(rdyn[3], oracle[3], "R3 should match oracle");
    }

    /// Pattern C: STRD T1 ; LDRD T1  (suspected Atomic 64-bit store→load).
    #[test]
    fn repro_thumb32_strd_then_ldrd_same_base() {
        let mut regs = [0u32; 15];
        regs[1] = 0x1111_2222;
        regs[2] = 0x3333_4444;
        regs[13] = 0x8000;

        // R0 = base
        // STRD R1,R2,[R0,#0]  -> hw0=0xE9C0|0=0xE9C0, hw1=(1<<12)|(2<<8)|0
        // LDRD R3,R4,[R0,#0]  -> hw0=0xE9D0|0=0xE9D0, hw1=(3<<12)|(4<<8)|0
        let mut hws = Vec::new();
        thumb32_setup_base(&mut hws, 0, FUZZ_DATA_BASE);
        hws.push(0xE9C0);
        hws.push((1u16 << 12) | (2u16 << 8));
        hws.push(0xE9D0);
        hws.push((3u16 << 12) | (4u16 << 8));
        hws.push(THUMB_HALT);
        hws.push(THUMB_HALT);

        let (rdyn, oracle) = run_thumb_pair(&hws, &regs);
        if rdyn != oracle {
            dump_diff("strd_ldrd_same_base", &hws, &regs, &rdyn, &oracle);
        }
        assert_eq!(rdyn[3], oracle[3], "R3 should match oracle");
        assert_eq!(rdyn[4], oracle[4], "R4 should match oracle");
    }

    /// Pattern D: STMIA.W Rn!,{R1,R2} ; LDMIA.W Rm!,{R3,R4}
    /// Both Rn and Rm point at FUZZ_DATA_BASE. Store then read back.
    #[test]
    fn repro_thumb32_stm_then_ldm_aliased() {
        let mut regs = [0u32; 15];
        regs[1] = 0xAAAA_0001;
        regs[2] = 0xBBBB_0002;
        regs[13] = 0x8000;

        let mut hws = Vec::new();
        thumb32_setup_base(&mut hws, 0, FUZZ_DATA_BASE);
        thumb32_setup_base(&mut hws, 5, FUZZ_DATA_BASE);
        // STMIA.W R0!, {R1,R2}: hw0=0xE8A0, hw1=0b0110=0x0006
        hws.push(0xE8A0);
        hws.push(0x0006);
        // LDMIA.W R5, {R3,R4}: hw0=0xE890|5=0xE895, hw1=0b11000=0x0018
        hws.push(0xE895);
        hws.push(0x0018);
        hws.push(THUMB_HALT);
        hws.push(THUMB_HALT);

        let (rdyn, oracle) = run_thumb_pair(&hws, &regs);
        if rdyn != oracle {
            dump_diff("stm_ldm_aliased", &hws, &regs, &rdyn, &oracle);
        }
        assert_eq!(rdyn[3], oracle[3], "R3 should match oracle");
        assert_eq!(rdyn[4], oracle[4], "R4 should match oracle");
    }

    // ================== VFP scalar F32 fuzz ==================
    //
    // The integer fuzzer above never generates VFP/NEON floating-point, yet
    // The garbled cinematic was localized to NaN/wrong transform matrices
    // authored by the guest CPU, so this differential checks ruzu's AArch32
    // VFP scalar F32 emit against the upstream dynarmic oracle.
    //
    // The oracle protocol only exchanges GPRs+CPSR, so FP results are routed
    // through GPRs: `vmov sN, rN` (load inputs) -> FP op -> `vmov rN, sN`
    // (read result). The compared GPR therefore holds the raw F32 result bits.

    /// `vmov sN, rT` (ARM core register -> single FP register).
    fn enc_vmov_to_s(sn: u32, rt: u32) -> u32 {
        0xEE00_0A10 | (((sn >> 1) & 0xF) << 16) | ((sn & 1) << 7) | ((rt & 0xF) << 12)
    }
    /// `vmov rT, sN` (single FP register -> ARM core register).
    fn enc_vmov_from_s(rt: u32, sn: u32) -> u32 {
        0xEE10_0A10 | (((sn >> 1) & 0xF) << 16) | ((sn & 1) << 7) | ((rt & 0xF) << 12)
    }
    /// Encode a 3-operand VFP single-precision op: `op sd, sn, sm`.
    fn enc_vfp3(base: u32, sd: u32, sn: u32, sm: u32) -> u32 {
        base | (((sd >> 1) & 0xF) << 12)
            | ((sd & 1) << 22)
            | (((sn >> 1) & 0xF) << 16)
            | ((sn & 1) << 7)
            | ((sm >> 1) & 0xF)
            | ((sm & 1) << 5)
    }
    /// `vmov dN, rLo, rHi` (two ARM core registers -> double FP register).
    fn enc_vmov_to_d(dn: u32, rt_lo: u32, rt_hi: u32) -> u32 {
        0xEC40_0B10
            | ((rt_hi & 0xF) << 16)
            | ((rt_lo & 0xF) << 12)
            | (((dn >> 4) & 1) << 5)
            | (dn & 0xF)
    }
    /// `vmov rLo, rHi, dN` (double FP register -> two ARM core registers).
    fn enc_vmov_from_d(rt_lo: u32, rt_hi: u32, dn: u32) -> u32 {
        0xEC50_0B10
            | ((rt_hi & 0xF) << 16)
            | ((rt_lo & 0xF) << 12)
            | (((dn >> 4) & 1) << 5)
            | (dn & 0xF)
    }
    /// Encode a 3-operand VFP double-precision op: `op dd, dn, dm`.
    fn enc_vfp3_d(base: u32, dd: u32, dn: u32, dm: u32) -> u32 {
        base | ((dd & 0xF) << 12)
            | (((dd >> 4) & 1) << 22)
            | ((dn & 0xF) << 16)
            | (((dn >> 4) & 1) << 7)
            | (dm & 0xF)
            | (((dm >> 4) & 1) << 5)
    }

    // cond=AL (0xE) bases for F32 (sz=0).
    const VFP_VMLA: u32 = 0xEE00_0A00; // sd += sn * sm  (non-fused)
    const VFP_VMLS: u32 = 0xEE00_0A40; // sd += -(sn * sm)
    const VFP_VMUL: u32 = 0xEE20_0A00;
    const VFP_VADD: u32 = 0xEE30_0A00;
    const VFP_VSUB: u32 = 0xEE30_0A40;
    const VFP_VDIV: u32 = 0xEE80_0A00;
    const VFP_VFMA: u32 = 0xEEA0_0A00; // sd += sn * sm  (FUSED, VFPv4)
    const VFP_VFMS: u32 = 0xEEA0_0A40; // sd += -(sn * sm) (fused)
    const VFP_VFNMA: u32 = 0xEE90_0A40; // fused, negated accumulator
    const VFP_VFNMS: u32 = 0xEE90_0A00; // fused, negated accumulator
    const VFP_F64_BIT: u32 = 0x0000_0100;

    /// Run one VFP op `s0 = f(s0, s1, s2)` for inputs (a,b,c) given as f32 bit
    /// patterns and return the result GPR (rdynarmic, oracle).
    fn run_vfp_triple(base: u32, a: u32, b: u32, c: u32) -> ([u32; 16], Option<[u32; 16]>) {
        let mut regs = [0u32; 15];
        regs[0] = a; // -> s0 (acc / dest)
        regs[1] = b; // -> s1
        regs[2] = c; // -> s2
        regs[13] = 0x8000;
        let cpsr = 0x0000_01d0;
        let code = vec![
            enc_vmov_to_s(0, 0),
            enc_vmov_to_s(1, 1),
            enc_vmov_to_s(2, 2),
            enc_vfp3(base, 0, 1, 2),
            enc_vmov_from_s(0, 0),
        ];
        let (rdyn, _) = run_rdynarmic(&code, &regs, cpsr);
        let oracle = run_oracle(&code, &regs, cpsr).map(|(r, _)| r);
        (rdyn, oracle)
    }

    /// Run one F64 VFP op `d0 = f(d0, d1, d2)` and return result bits in R1:R0.
    fn run_vfp_triple_f64(base: u32, a: u64, b: u64, c: u64) -> ([u32; 16], Option<[u32; 16]>) {
        let mut regs = [0u32; 15];
        regs[0] = a as u32; // -> d0 low
        regs[1] = (a >> 32) as u32; // -> d0 high
        regs[2] = b as u32; // -> d1 low
        regs[3] = (b >> 32) as u32; // -> d1 high
        regs[4] = c as u32; // -> d2 low
        regs[5] = (c >> 32) as u32; // -> d2 high
        regs[13] = 0x8000;
        let cpsr = 0x0000_01d0;
        let code = vec![
            enc_vmov_to_d(0, 0, 1),
            enc_vmov_to_d(1, 2, 3),
            enc_vmov_to_d(2, 4, 5),
            enc_vfp3_d(base | VFP_F64_BIT, 0, 1, 2),
            enc_vmov_from_d(0, 1, 0),
        ];
        let (rdyn, _) = run_rdynarmic(&code, &regs, cpsr);
        let oracle = run_oracle(&code, &regs, cpsr).map(|(r, _)| r);
        (rdyn, oracle)
    }

    /// Compare two F32 results. Finite/inf/zero results must match EXACTLY.
    /// Only the known default-NaN sign delta is tolerated.
    fn f32_results_match(rdyn: u32, oracle: u32) -> bool {
        rdyn == oracle || ((rdyn ^ oracle) == 0x8000_0000 && (rdyn & 0x7FFF_FFFF) == 0x7FC0_0000)
    }

    /// Compare two F64 results. Finite/inf/zero results must match EXACTLY.
    /// Only the known default-NaN sign delta is tolerated.
    fn f64_results_match(rdyn: u64, oracle: u64) -> bool {
        rdyn == oracle
            || ((rdyn ^ oracle) == 0x8000_0000_0000_0000
                && (rdyn & 0x7FFF_FFFF_FFFF_FFFF) == 0x7FF8_0000_0000_0000)
    }

    /// Small F32 pool: finite fused math, signed zero, invalid default-NaN
    /// behavior, and qNaN propagation without making oracle tests too slow.
    const F32_POOL: [u32; 9] = [
        0x3F80_0000, // 1.0
        0x4000_0000, // 2.0
        0xBF80_0000, // -1.0
        0x3EAA_AAAB, // 1/3
        0x0000_0000, // +0.0
        0x8000_0000, // -0.0
        0x7F80_0000, // +inf
        0xFF80_0000, // -inf
        0x7FC0_0000, // qNaN
    ];

    /// Small F64 pool: enough to exercise finite fused math and invalid default-NaN
    /// behavior without making the external oracle run unreasonably long.
    const F64_POOL: [u64; 8] = [
        0x3FF0_0000_0000_0000, // 1.0
        0x4000_0000_0000_0000, // 2.0
        0xBFF0_0000_0000_0000, // -1.0
        0x3FD5_5555_5555_5555, // 1/3
        0x0000_0000_0000_0000, // +0.0
        0x8000_0000_0000_0000, // -0.0
        0x7FF0_0000_0000_0000, // +inf
        0xFFF0_0000_0000_0000, // -inf
    ];

    /// Differential check for the non-fused VFP ops that ARE decoded — these
    /// must already match the oracle (control / regression guard).
    #[test]
    fn fuzz_vfp_scalar_f32_decoded_ops() {
        let ops: [(&str, u32); 6] = [
            ("VADD", VFP_VADD),
            ("VSUB", VFP_VSUB),
            ("VMUL", VFP_VMUL),
            ("VDIV", VFP_VDIV),
            ("VMLA", VFP_VMLA),
            ("VMLS", VFP_VMLS),
        ];
        let mut fail = 0u32;
        let mut nan_sign = 0u32;
        let mut total = 0u32;
        for (name, base) in ops {
            for &a in &F32_POOL {
                for &b in &F32_POOL {
                    for &c in &F32_POOL {
                        let (rdyn, oracle) = run_vfp_triple(base, a, b, c);
                        let Some(oracle) = oracle else { continue };
                        total += 1;
                        if rdyn[0] != oracle[0] {
                            if f32_results_match(rdyn[0], oracle[0]) {
                                nan_sign += 1;
                            } else {
                                fail += 1;
                                if fail <= 12 {
                                    eprintln!(
                                        "{} mismatch a={:08x} b={:08x} c={:08x} rdyn={:08x} oracle={:08x}",
                                        name, a, b, c, rdyn[0], oracle[0]
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        eprintln!(
            "VFP decoded-op differential: {}/{} hard-mismatched ({} default-NaN-sign-only, tolerated)",
            fail, total, nan_sign
        );
        assert_eq!(fail, 0, "decoded VFP scalar F32 ops diverged from oracle");
    }

    /// Differential check for the FUSED multiply-add family (VFPv4). If these
    /// are not decoded/emitted by ruzu's AArch32 frontend, they will diverge
    /// from the oracle — which corrupts any matrix math the guest compiler
    /// lowered to scalar fused MACs.
    #[test]
    fn fuzz_vfp_scalar_f32_fused_mac() {
        let ops: [(&str, u32); 4] = [
            ("VFMA", VFP_VFMA),
            ("VFMS", VFP_VFMS),
            ("VFNMA", VFP_VFNMA),
            ("VFNMS", VFP_VFNMS),
        ];
        let mut fail = 0u32;
        let mut nan_sign = 0u32;
        let mut total = 0u32;
        for (name, base) in ops {
            for &a in &F32_POOL {
                for &b in &F32_POOL {
                    for &c in &F32_POOL {
                        let (rdyn, oracle) = run_vfp_triple(base, a, b, c);
                        let Some(oracle) = oracle else { continue };
                        total += 1;
                        if rdyn[0] != oracle[0] {
                            if f32_results_match(rdyn[0], oracle[0]) {
                                nan_sign += 1;
                            } else {
                                fail += 1;
                                if fail <= 12 {
                                    eprintln!(
                                        "{} mismatch a={:08x} b={:08x} c={:08x} rdyn={:08x} oracle={:08x}",
                                        name, a, b, c, rdyn[0], oracle[0]
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        eprintln!(
            "VFP fused-MAC differential: {}/{} hard-mismatched ({} default-NaN-sign-only, tolerated)",
            fail, total, nan_sign
        );
        assert_eq!(fail, 0, "fused VFP scalar F32 ops diverged from oracle");
    }

    #[test]
    fn fuzz_vfp_scalar_f64_fused_mac() {
        let ops: [(&str, u32); 4] = [
            ("VFMA.F64", VFP_VFMA),
            ("VFMS.F64", VFP_VFMS),
            ("VFNMA.F64", VFP_VFNMA),
            ("VFNMS.F64", VFP_VFNMS),
        ];
        let mut fail = 0u32;
        let mut nan_sign = 0u32;
        let mut total = 0u32;
        for (name, base) in ops {
            for &a in &F64_POOL {
                for &b in &F64_POOL {
                    for &c in &F64_POOL {
                        let (rdyn, oracle) = run_vfp_triple_f64(base, a, b, c);
                        let Some(oracle) = oracle else { continue };
                        let rdyn_bits = ((rdyn[1] as u64) << 32) | rdyn[0] as u64;
                        let oracle_bits = ((oracle[1] as u64) << 32) | oracle[0] as u64;
                        total += 1;
                        if rdyn_bits != oracle_bits {
                            if f64_results_match(rdyn_bits, oracle_bits) {
                                nan_sign += 1;
                            } else {
                                fail += 1;
                                if fail <= 12 {
                                    eprintln!(
                                        "{} mismatch a={:016x} b={:016x} c={:016x} rdyn={:016x} oracle={:016x}",
                                        name, a, b, c, rdyn_bits, oracle_bits
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        eprintln!(
            "VFP fused-MAC F64 differential: {}/{} hard-mismatched ({} default-NaN-sign-only, tolerated)",
            fail, total, nan_sign
        );
        assert_eq!(fail, 0, "fused VFP scalar F64 ops diverged from oracle");
    }

    #[test]
    fn fuzz_vfp_conversions_with_upstream() {
        const INT_INPUTS: [u32; 7] = [
            0,
            1,
            0x7FFF_FFFF,
            0x8000_0000,
            0xFFFF_FFFF,
            0x00FF_FFFF,
            0x0100_0001,
        ];
        const FP_INPUTS: [u32; 10] = [
            0x0000_0000,
            0x8000_0000,
            0x3F80_0000,
            0xBF80_0000,
            0x3F00_0000,
            0x4F00_0000,
            0xCF00_0000,
            0x7F80_0000,
            0xFF80_0000,
            0x7FC0_0000,
        ];

        // Keep source and destination S-registers distinct to cover
        // extension-register indexing as well as the conversion itself.
        for (name, op, src, dst, inputs) in [
            ("VCVT.F32.U32", 0xEEB8_1A40, 0, 2, &INT_INPUTS[..]),
            ("VCVT.F32.S32", 0xEEB8_0AC0, 0, 0, &INT_INPUTS[..]),
            ("VCVT.U32.F32", 0xEEBC_0AC0, 0, 0, &FP_INPUTS[..]),
            ("VCVT.S32.F32", 0xEEBD_0AC1, 2, 0, &FP_INPUTS[..]),
        ] {
            for &input in inputs {
                let mut regs = [0u32; 15];
                regs[0] = input;
                regs[13] = 0x8000;
                let code = [enc_vmov_to_s(src, 0), op, enc_vmov_from_s(0, dst)];
                let oracle = run_oracle(&code, &regs, 0x0000_01D0)
                    .unwrap_or_else(|| panic!("{name} oracle failed for {input:08X}"))
                    .0;
                for optimizations in [
                    OptimizationFlag::NO_OPTIMIZATIONS,
                    OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
                ] {
                    let rdyn =
                        run_rdynarmic_with_optimizations(&code, &regs, 0x0000_01D0, optimizations)
                            .0;
                    assert_eq!(
                        rdyn[0],
                        oracle[0],
                        "{name} input={input:08X} optimization mask=0x{:X}",
                        optimizations.bits()
                    );
                }
            }
        }
    }

    #[test]
    fn scalar_lane_conversion_multiply_chain_matches_upstream() {
        const VCVT_F32_U32_S12_S15: u32 = 0xEEB8_6A67;
        const VMUL_F32_S4_S12_S13: u32 = 0xEE26_2A26;
        const VMUL_F32_S0_S10_S4: u32 = 0xEE25_0A02;

        for input in [0u32, 1, 12, 45, 96, 160, 252, 255] {
            let mut regs = [0u32; 15];
            regs[0] = input;
            regs[1] = 0x3B80_8081; // 1.0 / 255.0.
            regs[2] = 0x3F80_0000;
            regs[13] = 0x8000;
            let code = [
                enc_vmov_to_s(15, 0),
                enc_vmov_to_s(13, 1),
                enc_vmov_to_s(10, 2),
                VCVT_F32_U32_S12_S15,
                VMUL_F32_S4_S12_S13,
                VMUL_F32_S0_S10_S4,
                enc_vmov_from_s(0, 0),
            ];
            let oracle = run_oracle(&code, &regs, 0x0000_01D0)
                .unwrap_or_else(|| panic!("scalar lane chain oracle failed for {input}"))
                .0;
            for optimizations in [
                OptimizationFlag::NO_OPTIMIZATIONS,
                OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
            ] {
                let rdyn =
                    run_rdynarmic_with_optimizations(&code, &regs, 0x0000_01D0, optimizations).0;
                assert_eq!(
                    rdyn[0],
                    oracle[0],
                    "scalar lane chain input={input} optimization mask=0x{:X}",
                    optimizations.bits()
                );
            }
        }
    }

    #[test]
    fn fuzz_vfp_explicit_rounding_with_upstream() {
        const INPUTS: [u32; 18] = [
            0x0000_0000, // +0.0
            0x8000_0000, // -0.0
            0x3E80_0000, // 0.25
            0x3F00_0000, // 0.5
            0x3FC0_0000, // 1.5
            0xBE80_0000, // -0.25
            0xBF00_0000, // -0.5
            0xBFC0_0000, // -1.5
            0x4EFF_FFFF, // largest f32 below 2^31
            0x4F00_0000, // +2^31
            0xCF00_0000, // -2^31
            0xCF00_0001, // below -2^31
            0x7F80_0000, // +inf
            0xFF80_0000, // -inf
            0x7FC0_0000, // qNaN
            0x0000_0001, // smallest subnormal
            0x3F7F_FFFF, // largest f32 below 1.0
            0xBF7F_FFFF, // largest-magnitude f32 above -1.0
        ];

        // Representative A32 VFPv4 explicit-rounding opcodes.
        for (name, op, integer_result) in [
            ("VRINTP.F32", 0xFEBA_0A40, false),
            ("VRINTM.F32", 0xFEBB_0A40, false),
            ("VCVTP.S32.F32", 0xFEBE_0AC0, true),
            ("VCVTM.S32.F32", 0xFEBF_0AC0, true),
        ] {
            for &input in &INPUTS {
                let mut regs = [0u32; 15];
                regs[0] = input;
                regs[13] = 0x8000;
                let code = [enc_vmov_to_s(0, 0), op, enc_vmov_from_s(0, 0)];
                let oracle = run_oracle(&code, &regs, 0x0000_01D0)
                    .unwrap_or_else(|| panic!("{name} oracle failed for {input:08X}"))
                    .0;
                for optimizations in [
                    OptimizationFlag::NO_OPTIMIZATIONS,
                    OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
                ] {
                    let rdyn =
                        run_rdynarmic_with_optimizations(&code, &regs, 0x0000_01D0, optimizations)
                            .0;
                    if integer_result || !f32_results_match(rdyn[0], oracle[0]) {
                        assert_eq!(
                            rdyn[0],
                            oracle[0],
                            "{name} input={input:08X} optimization mask=0x{:X}",
                            optimizations.bits()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn fuzz_vmov_i32_lanes_with_upstream() {
        let inputs = [
            [0x0123_4567, 0x89AB_CDEF, 0xA5A5_5A5A],
            [0x0000_0000, 0xFFFF_FFFF, 0x8000_0000],
            [0x3F80_0000, 0x4000_0000, 0x7FC0_0000],
        ];
        for (name, op) in [
            ("VMOV.32 d16[0], r2", 0xEE00_2B90),
            ("VMOV.32 d16[1], r2", 0xEE20_2B90),
            ("VMOV.32 r2, d16[0]", 0xEE10_2B90),
            ("VMOV.32 r2, d16[1]", 0xEE30_2B90),
        ] {
            for input in inputs {
                let mut regs = [0u32; 15];
                regs[..3].copy_from_slice(&input);
                regs[13] = 0x8000;
                let code = [enc_vmov_to_d(16, 0, 1), op, enc_vmov_from_d(0, 1, 16)];
                let oracle = run_oracle(&code, &regs, 0x0000_01D0)
                    .unwrap_or_else(|| panic!("{name} oracle failed"))
                    .0;
                for optimizations in [
                    OptimizationFlag::NO_OPTIMIZATIONS,
                    OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
                ] {
                    let rdyn =
                        run_rdynarmic_with_optimizations(&code, &regs, 0x0000_01D0, optimizations)
                            .0;
                    assert_eq!(
                        &rdyn[..3],
                        &oracle[..3],
                        "{name} input={input:08X?} optimization mask=0x{:X}",
                        optimizations.bits()
                    );
                }
            }
        }
    }

    #[test]
    fn fuzz_vfp_compare_and_select_with_upstream() {
        const VALUES: [u32; 8] = [
            0x0000_0000,
            0x8000_0000,
            0x3F80_0000,
            0xBF80_0000,
            0x7F80_0000,
            0xFF80_0000,
            0x7FC0_0000,
            0x3EAA_AAAB,
        ];
        const VCMPE_S0_S2: u32 = 0xEEB4_0AC1;
        const VMRS_APSR_NZCV: u32 = 0xEEF1_FA10;

        for &lhs in &VALUES {
            for &rhs in &VALUES {
                let mut regs = [0u32; 15];
                regs[0] = lhs;
                regs[1] = rhs;
                regs[13] = 0x8000;
                let code = [
                    enc_vmov_to_s(0, 0),
                    enc_vmov_to_s(2, 1),
                    VCMPE_S0_S2,
                    VMRS_APSR_NZCV,
                ];
                let oracle = run_oracle(&code, &regs, 0x0000_01D0)
                    .expect("VCMPE oracle failed")
                    .1;
                for optimizations in [
                    OptimizationFlag::NO_OPTIMIZATIONS,
                    OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
                ] {
                    let rdyn =
                        run_rdynarmic_with_optimizations(&code, &regs, 0x0000_01D0, optimizations)
                            .1;
                    assert_eq!(
                        rdyn & 0xF000_0000,
                        oracle & 0xF000_0000,
                        "VCMPE lhs={lhs:08X} rhs={rhs:08X} optimization mask=0x{:X}",
                        optimizations.bits()
                    );
                }
            }
        }

        for (name, op) in [
            ("VSELEQ", 0xFE00_0A81),
            ("VSELVS", 0xFE10_0A81),
            ("VSELGE", 0xFE20_0A81),
            ("VSELGT", 0xFE30_0A81),
        ] {
            for nzcv in 0..16u32 {
                let mut regs = [0u32; 15];
                regs[0] = 0x1122_3344;
                regs[1] = 0xAABB_CCDD;
                regs[13] = 0x8000;
                let cpsr = (nzcv << 28) | 0x0000_01D0;
                let code = [
                    enc_vmov_to_s(1, 0),
                    enc_vmov_to_s(2, 1),
                    op,
                    enc_vmov_from_s(0, 0),
                ];
                let oracle = run_oracle(&code, &regs, cpsr)
                    .unwrap_or_else(|| panic!("{name} oracle failed for NZCV={nzcv:X}"))
                    .0;
                for optimizations in [
                    OptimizationFlag::NO_OPTIMIZATIONS,
                    OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
                ] {
                    let rdyn =
                        run_rdynarmic_with_optimizations(&code, &regs, cpsr, optimizations).0;
                    assert_eq!(
                        rdyn[0],
                        oracle[0],
                        "{name} NZCV={nzcv:X} optimization mask=0x{:X}",
                        optimizations.bits()
                    );
                }
            }
        }
    }

    // ================== NEON / ASIMD F32 vector fuzz ==================
    //
    // (NEON F32). After the scalar VFMA fix the game reaches a load loop that
    // never completes; a NEON F32 emit divergence is the prime remaining
    // suspect (same op family as the scalar VFMA gap). These run D-register
    // (2-lane F32) ops and route both result lanes through GPRs via the
    // existing enc_vmov_to_d / enc_vmov_from_d helpers, comparing against the
    // upstream dynarmic oracle.

    // D-register F32 (Q=0, sz=0) encodings, cond=1111 (NEON is unconditional).
    // dst=d0, srcN=d1, srcM=d2.
    const NEON_VADD_F32: u32 = 0xF201_0D02; // d0 = d1 + d2
    const NEON_VSUB_F32: u32 = 0xF221_0D02; // d0 = d1 - d2
    const NEON_VMUL_F32: u32 = 0xF301_0D12; // d0 = d1 * d2
    const NEON_VMLA_F32: u32 = 0xF201_0D12; // d0 += d1 * d2  (non-fused)
    const NEON_VFMA_F32: u32 = 0xF201_0C12; // d0 += d1 * d2  (fused)
    const NEON_VRECPS_F32: u32 = 0xF201_0F12; // d0 = 2 - d1*d2   (fused step)
    const NEON_VRSQRTS_F32: u32 = 0xF221_0F12; // d0 = (3 - d1*d2)/2 (fused step)

    /// Run a 2-lane NEON F32 op `d0 = f(d0, d1, d2)` for the given lane bit
    /// patterns. d0 holds the accumulator (lanes a*), d1=(b*), d2=(c*).
    fn run_neon_f32(
        op: u32,
        a0: u32,
        a1: u32,
        b0: u32,
        b1: u32,
        c0: u32,
        c1: u32,
    ) -> ([u32; 16], Option<[u32; 16]>) {
        let mut regs = [0u32; 15];
        regs[0] = a0;
        regs[1] = a1;
        regs[2] = b0;
        regs[3] = b1;
        regs[4] = c0;
        regs[5] = c1;
        regs[13] = 0x8000;
        let cpsr = 0x0000_01d0;
        let code = vec![
            enc_vmov_to_d(0, 0, 1),
            enc_vmov_to_d(1, 2, 3),
            enc_vmov_to_d(2, 4, 5),
            op,
            enc_vmov_from_d(0, 1, 0),
        ];
        let (rdyn, _) = run_rdynarmic(&code, &regs, cpsr);
        let oracle = run_oracle(&code, &regs, cpsr).map(|(r, _)| r);
        (rdyn, oracle)
    }

    #[test]
    fn fuzz_neon_f32_vector() {
        let ops: [(&str, u32); 7] = [
            ("VADD.F32", NEON_VADD_F32),
            ("VSUB.F32", NEON_VSUB_F32),
            ("VMUL.F32", NEON_VMUL_F32),
            ("VMLA.F32", NEON_VMLA_F32),
            ("VFMA.F32", NEON_VFMA_F32),
            ("VRECPS.F32", NEON_VRECPS_F32),
            ("VRSQRTS.F32", NEON_VRSQRTS_F32),
        ];
        let mut fail = 0u32;
        let mut nan_sign = 0u32;
        let mut total = 0u32;
        // Use a smaller pool on the second lane to keep the run bounded while
        // still exercising lane independence.
        for (name, op) in ops {
            for &a in &F32_POOL {
                for &b in &F32_POOL {
                    for &c in &F32_POOL {
                        // lane0 = (a,b,c); lane1 = (b,c,a) so both lanes differ.
                        let (rdyn, oracle) = run_neon_f32(op, a, b, b, c, c, a);
                        let Some(oracle) = oracle else { continue };
                        for lane in 0..2 {
                            total += 1;
                            if rdyn[lane] != oracle[lane] {
                                if f32_results_match(rdyn[lane], oracle[lane]) {
                                    nan_sign += 1;
                                } else {
                                    fail += 1;
                                    if fail <= 16 {
                                        eprintln!(
                                            "{} lane{} mismatch a={:08x} b={:08x} c={:08x} rdyn={:08x} oracle={:08x}",
                                            name, lane, a, b, c, rdyn[lane], oracle[lane]
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        eprintln!(
            "NEON F32 vector differential: {}/{} hard-mismatched ({} default-NaN-sign-only, tolerated)",
            fail, total, nan_sign
        );
        assert_eq!(fail, 0, "NEON F32 vector ops diverged from oracle");
    }

    fn enc_asimd_two_reg(base: u32, sz: u32, q: bool, vd: u32, vm: u32) -> u32 {
        base | ((sz & 3) << 18)
            | ((vd & 0xF) << 12)
            | ((q as u32) << 6)
            | (((vm >> 4) & 1) << 5)
            | (vm & 0xF)
    }

    fn run_neon_q_pair(op: u32, inputs: [u32; 8], optimizations: OptimizationFlag) -> [u32; 16] {
        let mut regs = [0u32; 15];
        regs[..8].copy_from_slice(&inputs);
        regs[13] = 0x8000;
        let code = vec![
            enc_vmov_to_d(0, 0, 1),
            enc_vmov_to_d(1, 2, 3),
            enc_vmov_to_d(2, 4, 5),
            enc_vmov_to_d(3, 6, 7),
            op,
            enc_vmov_from_d(0, 1, 0),
            enc_vmov_from_d(2, 3, 1),
            enc_vmov_from_d(4, 5, 2),
            enc_vmov_from_d(6, 7, 3),
        ];
        run_rdynarmic_with_optimizations(&code, &regs, 0x0000_01d0, optimizations).0
    }

    fn run_neon_q_pair_oracle(op: u32, inputs: [u32; 8]) -> Option<[u32; 16]> {
        let mut regs = [0u32; 15];
        regs[..8].copy_from_slice(&inputs);
        regs[13] = 0x8000;
        let code = vec![
            enc_vmov_to_d(0, 0, 1),
            enc_vmov_to_d(1, 2, 3),
            enc_vmov_to_d(2, 4, 5),
            enc_vmov_to_d(3, 6, 7),
            op,
            enc_vmov_from_d(0, 1, 0),
            enc_vmov_from_d(2, 3, 1),
            enc_vmov_from_d(4, 5, 2),
            enc_vmov_from_d(6, 7, 3),
        ];
        run_oracle(&code, &regs, 0x0000_01d0).map(|(result, _)| result)
    }

    #[test]
    fn fuzz_neon_zip_unzip_transpose_with_upstream() {
        const VTRN: u32 = 0xF3B2_0080;
        const VUZP: u32 = 0xF3B2_0100;
        const VZIP: u32 = 0xF3B2_0180;
        let inputs = [
            [
                0x0000_0000,
                0xFFFF_FFFF,
                0x0123_4567,
                0x89AB_CDEF,
                0x1020_3040,
                0x5060_7080,
                0x90A0_B0C0,
                0xD0E0_F000,
            ],
            [
                0x3F80_0000,
                0x4000_0000,
                0x4040_0000,
                0x4080_0000,
                0xBF80_0000,
                0xC000_0000,
                0xC040_0000,
                0xC080_0000,
            ],
            [
                0x7654_3210,
                0xFEDC_BA98,
                0x1357_9BDF,
                0x2468_ACE0,
                0x55AA_55AA,
                0xAA55_AA55,
                0x0F0F_F0F0,
                0xF0F0_0F0F,
            ],
        ];

        for (name, base, sizes) in [
            ("VTRN", VTRN, &[0u32, 1, 2][..]),
            ("VUZP", VUZP, &[0u32, 1, 2][..]),
            ("VZIP", VZIP, &[0u32, 1, 2][..]),
        ] {
            for &sz in sizes {
                let op = enc_asimd_two_reg(base, sz, true, 0, 2);
                for input in inputs {
                    let oracle = run_neon_q_pair_oracle(op, input)
                        .unwrap_or_else(|| panic!("{name}.{sz} oracle failed"));
                    for optimizations in [
                        OptimizationFlag::NO_OPTIMIZATIONS,
                        OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
                    ] {
                        let rdyn = run_neon_q_pair(op, input, optimizations);
                        assert_eq!(
                            &rdyn[..8],
                            &oracle[..8],
                            "{name}.{sz} diverged with optimization mask 0x{:X}",
                            optimizations.bits()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn fuzz_neon_reciprocal_estimates_with_upstream() {
        const VRECPE: u32 = 0xF3B3_0400;
        const VRSQRTE: u32 = 0xF3B3_0480;
        let inputs = [
            [
                0x3F80_0000,
                0x4000_0000,
                0x3EAA_AAAB,
                0x4120_0000,
                0x3F00_0000,
                0x4080_0000,
                0x3DCC_CCCD,
                0x42C8_0000,
            ],
            [
                0x0000_0000,
                0x8000_0000,
                0x7F80_0000,
                0xFF80_0000,
                0x7FC0_0000,
                0x0080_0000,
                0x7F7F_FFFF,
                0x0000_0001,
            ],
            [
                0x8000_0000,
                0x8000_0001,
                0xBFFF_FFFF,
                0xC000_0000,
                0x4000_0000,
                0x7FFF_FFFF,
                0xFFFF_FFFF,
                0x3FFF_FFFF,
            ],
        ];

        for (name, base, fp) in [
            ("VRECPE.F32", VRECPE, true),
            ("VRSQRTE.F32", VRSQRTE, true),
            ("VRECPE.U32", VRECPE, false),
            ("VRSQRTE.U32", VRSQRTE, false),
        ] {
            let op = enc_asimd_two_reg(base | ((fp as u32) << 8), 2, true, 0, 2);
            for input in inputs {
                let oracle = run_neon_q_pair_oracle(op, input)
                    .unwrap_or_else(|| panic!("{name} oracle failed"));
                for optimizations in [
                    OptimizationFlag::NO_OPTIMIZATIONS,
                    OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
                ] {
                    let rdyn = run_neon_q_pair(op, input, optimizations);
                    assert_eq!(
                        &rdyn[..4],
                        &oracle[..4],
                        "{name} diverged with optimization mask 0x{:X}",
                        optimizations.bits()
                    );
                }
            }
        }
    }
}

// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Maxwell Macro Executor (MME) interpreter.
//!
//! The Switch GPU contains a programmable macro unit that games use to drive
//! multi-draw sequences, set up instancing parameters, and perform per-draw
//! register writes without CPU intervention. Macros are uploaded during GPU
//! initialization and invoked via method registers 0xE00–0xFFF.
//!
//! Reference: yuzu `src/video_core/macro/macro.h` and `macro_interpreter.cpp`.

// ── Instruction field enums ──────────────────────────────────────────────────

/// Primary operation encoded in bits[2:0] of the 32-bit opcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Operation {
    ALU = 0,
    AddImmediate = 1,
    ExtractInsert = 2,
    ExtractShiftLeftImmediate = 3,
    ExtractShiftLeftRegister = 4,
    Read = 5,
    Unused = 6,
    Branch = 7,
}

impl Operation {
    fn from_raw(v: u32) -> Self {
        match v & 0x7 {
            0 => Self::ALU,
            1 => Self::AddImmediate,
            2 => Self::ExtractInsert,
            3 => Self::ExtractShiftLeftImmediate,
            4 => Self::ExtractShiftLeftRegister,
            5 => Self::Read,
            6 => Self::Unused,
            7 => Self::Branch,
            _ => unreachable!(),
        }
    }
}

/// ALU sub-operation encoded in bits[21:17].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ALUOperation {
    Add = 0,
    AddWithCarry = 1,
    Subtract = 2,
    SubtractWithBorrow = 3,
    Xor = 8,
    Or = 9,
    And = 10,
    AndNot = 11,
    Nand = 12,
}

impl ALUOperation {
    fn from_raw(v: u32) -> Self {
        match v {
            0 => Self::Add,
            1 => Self::AddWithCarry,
            2 => Self::Subtract,
            3 => Self::SubtractWithBorrow,
            8 => Self::Xor,
            9 => Self::Or,
            10 => Self::And,
            11 => Self::AndNot,
            12 => Self::Nand,
            _ => {
                log::warn!("Unknown ALU operation {}, defaulting to Add", v);
                Self::Add
            }
        }
    }
}

/// Result operation encoded in bits[6:4].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ResultOperation {
    IgnoreAndFetch = 0,
    Move = 1,
    MoveAndSetMethod = 2,
    FetchAndSend = 3,
    MoveAndSend = 4,
    FetchAndSetMethod = 5,
    MoveAndSetMethodFetchAndSend = 6,
    MoveAndSetMethodSend = 7,
}

impl ResultOperation {
    fn from_raw(v: u32) -> Self {
        match v & 0x7 {
            0 => Self::IgnoreAndFetch,
            1 => Self::Move,
            2 => Self::MoveAndSetMethod,
            3 => Self::FetchAndSend,
            4 => Self::MoveAndSend,
            5 => Self::FetchAndSetMethod,
            6 => Self::MoveAndSetMethodFetchAndSend,
            7 => Self::MoveAndSetMethodSend,
            _ => unreachable!(),
        }
    }
}

/// Branch condition encoded in bit[4].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum BranchCondition {
    Zero = 0,
    NotZero = 1,
}

impl BranchCondition {
    fn from_raw(v: u32) -> Self {
        if v & 1 == 0 {
            Self::Zero
        } else {
            Self::NotZero
        }
    }
}

// ── Opcode ───────────────────────────────────────────────────────────────────

/// Decoded 32-bit macro opcode with accessor methods for each bitfield.
#[derive(Clone, Copy, Debug)]
pub struct Opcode(pub u32);

impl Opcode {
    /// Primary operation (bits[2:0]).
    pub fn operation(&self) -> Operation {
        Operation::from_raw(self.0 & 0x7)
    }

    /// Result operation (bits[6:4]).
    pub fn result_operation(&self) -> ResultOperation {
        ResultOperation::from_raw((self.0 >> 4) & 0x7)
    }

    /// Branch condition (bit[4]).
    pub fn branch_condition(&self) -> BranchCondition {
        BranchCondition::from_raw((self.0 >> 4) & 0x1)
    }

    /// Branch annul flag (bit[5]). If set, branch has no delay slot.
    pub fn branch_annul(&self) -> bool {
        (self.0 >> 5) & 1 != 0
    }

    /// Exit flag (bit[7]). Instruction terminates macro after delay slot.
    pub fn is_exit(&self) -> bool {
        (self.0 >> 7) & 1 != 0
    }

    /// Destination register (bits[10:8]).
    pub fn dst(&self) -> u32 {
        (self.0 >> 8) & 0x7
    }

    /// Source register A (bits[13:11]).
    pub fn src_a(&self) -> u32 {
        (self.0 >> 11) & 0x7
    }

    /// Source register B (bits[16:14]).
    pub fn src_b(&self) -> u32 {
        (self.0 >> 14) & 0x7
    }

    /// Signed 18-bit immediate (bits[31:14]).
    pub fn immediate(&self) -> i32 {
        let raw = (self.0 >> 14) as i32;
        // Sign-extend from 18 bits.
        (raw << 14) >> 14
    }

    /// Branch target = immediate * 4 (byte offset from current PC).
    pub fn branch_target(&self) -> i32 {
        self.immediate() * 4
    }

    /// ALU sub-operation (bits[21:17]).
    pub fn alu_operation(&self) -> ALUOperation {
        ALUOperation::from_raw((self.0 >> 17) & 0x1F)
    }

    /// Bitfield source bit position (bits[21:17]).
    pub fn bf_src_bit(&self) -> u32 {
        (self.0 >> 17) & 0x1F
    }

    /// Bitfield size (bits[26:22]).
    pub fn bf_size(&self) -> u32 {
        (self.0 >> 22) & 0x1F
    }

    /// Bitfield destination bit position (bits[31:27]).
    pub fn bf_dst_bit(&self) -> u32 {
        (self.0 >> 27) & 0x1F
    }

    /// Bitfield mask: (1 << bf_size) - 1.
    pub fn bitfield_mask(&self) -> u32 {
        (1u32 << self.bf_size()).wrapping_sub(1)
    }
}

// ── MethodAddress ────────────────────────────────────────────────────────────

/// Packed method address with auto-increment: bits[11:0] = address,
/// bits[17:12] = increment.
#[derive(Clone, Copy, Debug, Default)]
struct MethodAddress {
    raw: u32,
}

impl MethodAddress {
    fn address(&self) -> u32 {
        self.raw & 0xFFF
    }

    fn increment(&self) -> u32 {
        (self.raw >> 12) & 0x3F
    }

    fn set(&mut self, value: u32) {
        self.raw = value;
    }

    fn advance(&mut self) {
        let addr = self.address() + self.increment();
        // Keep increment, update address.
        self.raw = (self.raw & !0xFFF) | (addr & 0xFFF);
    }
}

// ── MacroProcessor trait ─────────────────────────────────────────────────────

/// Decouples the macro interpreter from Maxwell3D. The 3D engine implements
/// this trait so the interpreter can read/write GPU registers.
pub trait MacroProcessor {
    /// Read a GPU register at the given method address.
    fn macro_read(&self, method: u32) -> u32;

    /// Write a GPU register at the given method address.
    fn macro_write(&mut self, method: u32, value: u32);
}

// ── MacroInterpreter ─────────────────────────────────────────────────────────

/// Number of macro register slots (128 per the MME spec).
const NUM_MACRO_SLOTS: usize = 128;

/// Number of general-purpose registers ($r0..$r7).
const NUM_REGISTERS: usize = 8;

/// GPU macro interpreter implementing the Maxwell Macro Executor (MME)
/// instruction set.
pub struct MacroInterpreter {
    /// Uploaded macro code words.
    code: Vec<u32>,
    /// Macro start offsets (128 slots).
    positions: [u32; NUM_MACRO_SLOTS],
    /// General-purpose registers $r0..$r7 ($r0 hardwired to 0).
    registers: [u32; NUM_REGISTERS],
    /// Program counter (word index into code).
    pc: u32,
    /// Branch delay slot target.
    delayed_pc: Option<u32>,
    /// Current method address + auto-increment for send().
    method_address: MethodAddress,
    /// ALU carry flag.
    carry: bool,
    /// Current macro parameters.
    params: Vec<u32>,
    /// Next parameter index to fetch.
    next_param_index: usize,
}

impl Default for MacroInterpreter {
    fn default() -> Self {
        Self::new()
    }
}

impl MacroInterpreter {
    pub fn new() -> Self {
        Self {
            code: Vec::new(),
            positions: [0u32; NUM_MACRO_SLOTS],
            registers: [0u32; NUM_REGISTERS],
            pc: 0,
            delayed_pc: None,
            method_address: MethodAddress::default(),
            carry: false,
            params: Vec::new(),
            next_param_index: 0,
        }
    }

    /// Clear macro code starting from the given offset. Matches upstream
    /// `MacroEngine::ClearCode` — resets code storage from the pointer onward.
    pub fn code_size(&self) -> usize {
        self.code.len()
    }

    pub fn clear_code(&mut self, offset: u32) {
        let idx = offset as usize;
        if idx < self.code.len() {
            for i in idx..self.code.len() {
                self.code[i] = 0;
            }
        }
    }

    /// Upload a code word at the given offset (word index).
    pub fn upload_code(&mut self, offset: u32, word: u32) {
        let idx = offset as usize;
        if idx >= self.code.len() {
            self.code.resize(idx + 1, 0);
        }
        self.code[idx] = word;
    }

    /// Set the start position (word offset) for a macro slot.
    pub fn set_position(&mut self, slot: u32, start: u32) {
        let idx = slot as usize;
        if idx < NUM_MACRO_SLOTS {
            self.positions[idx] = start;
        }
    }

    /// Execute the macro at the given slot with the provided parameters.
    pub fn execute(&mut self, slot: u32, params: &[u32], processor: &mut dyn MacroProcessor) {
        // Reset state.
        self.registers = [0u32; NUM_REGISTERS];
        self.carry = false;
        self.delayed_pc = None;
        self.method_address = MethodAddress::default();
        self.next_param_index = 1;

        // $r1 receives the first parameter.
        if !params.is_empty() {
            self.registers[1] = params[0];
        }
        self.params = params.to_vec();

        // Look up start offset and set PC (word index).
        let slot_idx = slot as usize;
        let start = if slot_idx < NUM_MACRO_SLOTS {
            self.positions[slot_idx]
        } else {
            0
        };
        self.pc = start;

        // Execute until exit.
        let mut steps = 0u32;
        while self.step(false, processor) {
            steps += 1;
            if steps > 1_000_000 {
                log::warn!(
                    "Macro slot {}: exceeded 1M steps, breaking (PC={})",
                    slot,
                    self.pc
                );
                break;
            }
        }
        if steps > 100 {
            log::info!("Macro slot {} executed {} steps", slot, steps);
        }

        // Verify all parameters were consumed.
        if self.next_param_index != self.params.len() {
            log::warn!(
                "Macro slot {}: consumed {} of {} parameters",
                slot,
                self.next_param_index,
                self.params.len()
            );
        }
    }

    /// Execute a single instruction. Returns true if execution should continue.
    fn step(&mut self, is_delay_slot: bool, processor: &mut dyn MacroProcessor) -> bool {
        let base_pc = self.pc;

        let opcode = self.get_opcode();
        self.pc += 1; // Advance by one word.

        // Apply delayed branch target.
        if let Some(target) = self.delayed_pc.take() {
            debug_assert!(is_delay_slot);
            self.pc = target;
        }

        match opcode.operation() {
            Operation::ALU => {
                let result = self.alu(
                    opcode.alu_operation(),
                    self.get_register(opcode.src_a()),
                    self.get_register(opcode.src_b()),
                );
                self.process_result(opcode.result_operation(), opcode.dst(), result, processor);
            }
            Operation::AddImmediate => {
                let result = self
                    .get_register(opcode.src_a())
                    .wrapping_add(opcode.immediate() as u32);
                self.process_result(opcode.result_operation(), opcode.dst(), result, processor);
            }
            Operation::ExtractInsert => {
                let mut dst_val = self.get_register(opcode.src_a());
                let src_val = self.get_register(opcode.src_b());

                let mask = opcode.bitfield_mask();
                let extracted = (src_val >> opcode.bf_src_bit()) & mask;
                dst_val &= !(mask << opcode.bf_dst_bit());
                dst_val |= extracted << opcode.bf_dst_bit();

                self.process_result(opcode.result_operation(), opcode.dst(), dst_val, processor);
            }
            Operation::ExtractShiftLeftImmediate => {
                let dst_val = self.get_register(opcode.src_a());
                let src_val = self.get_register(opcode.src_b());

                let result = ((src_val >> dst_val) & opcode.bitfield_mask()) << opcode.bf_dst_bit();

                self.process_result(opcode.result_operation(), opcode.dst(), result, processor);
            }
            Operation::ExtractShiftLeftRegister => {
                let dst_val = self.get_register(opcode.src_a());
                let src_val = self.get_register(opcode.src_b());

                let result = ((src_val >> opcode.bf_src_bit()) & opcode.bitfield_mask()) << dst_val;

                self.process_result(opcode.result_operation(), opcode.dst(), result, processor);
            }
            Operation::Read => {
                let addr = self
                    .get_register(opcode.src_a())
                    .wrapping_add(opcode.immediate() as u32);
                let result = processor.macro_read(addr);
                self.process_result(opcode.result_operation(), opcode.dst(), result, processor);
            }
            Operation::Branch => {
                debug_assert!(!is_delay_slot, "Branch in delay slot is invalid");
                let value = self.get_register(opcode.src_a());
                let taken = match opcode.branch_condition() {
                    BranchCondition::Zero => value == 0,
                    BranchCondition::NotZero => value != 0,
                };
                if taken {
                    let target = (base_pc as i32 + opcode.branch_target() / 4) as u32;
                    if opcode.branch_annul() {
                        // Skip delay slot.
                        self.pc = target;
                        return true;
                    }
                    self.delayed_pc = Some(target);
                    return self.step(true, processor);
                }
            }
            Operation::Unused => {
                log::warn!("Macro: unused operation at PC {}", base_pc);
            }
        }

        // Exit flag: execute one more instruction (delay slot), then stop.
        if opcode.is_exit() && !is_delay_slot {
            self.step(true, processor);
            return false;
        }

        true
    }

    /// Process the result of an operation according to the ResultOperation.
    fn process_result(
        &mut self,
        op: ResultOperation,
        dst_reg: u32,
        result: u32,
        processor: &mut dyn MacroProcessor,
    ) {
        match op {
            ResultOperation::IgnoreAndFetch => {
                let param = self.fetch_param();
                self.set_register(dst_reg, param);
            }
            ResultOperation::Move => {
                self.set_register(dst_reg, result);
            }
            ResultOperation::MoveAndSetMethod => {
                self.set_register(dst_reg, result);
                self.method_address.set(result);
            }
            ResultOperation::FetchAndSend => {
                let param = self.fetch_param();
                self.set_register(dst_reg, param);
                self.send(result, processor);
            }
            ResultOperation::MoveAndSend => {
                self.set_register(dst_reg, result);
                self.send(result, processor);
            }
            ResultOperation::FetchAndSetMethod => {
                let param = self.fetch_param();
                self.set_register(dst_reg, param);
                self.method_address.set(result);
            }
            ResultOperation::MoveAndSetMethodFetchAndSend => {
                self.set_register(dst_reg, result);
                self.method_address.set(result);
                let param = self.fetch_param();
                self.send(param, processor);
            }
            ResultOperation::MoveAndSetMethodSend => {
                self.set_register(dst_reg, result);
                self.method_address.set(result);
                self.send((result >> 12) & 0x3F, processor);
            }
        }
    }

    /// Perform an ALU operation, updating the carry flag.
    fn alu(&mut self, op: ALUOperation, a: u32, b: u32) -> u32 {
        match op {
            ALUOperation::Add => {
                let result = a as u64 + b as u64;
                self.carry = result > 0xFFFF_FFFF;
                result as u32
            }
            ALUOperation::AddWithCarry => {
                let result = a as u64 + b as u64 + self.carry as u64;
                self.carry = result > 0xFFFF_FFFF;
                result as u32
            }
            ALUOperation::Subtract => {
                let result = (a as u64).wrapping_sub(b as u64);
                self.carry = result < 0x1_0000_0000;
                result as u32
            }
            ALUOperation::SubtractWithBorrow => {
                let result = (a as u64)
                    .wrapping_sub(b as u64)
                    .wrapping_sub((!self.carry) as u64);
                self.carry = result < 0x1_0000_0000;
                result as u32
            }
            ALUOperation::Xor => a ^ b,
            ALUOperation::Or => a | b,
            ALUOperation::And => a & b,
            ALUOperation::AndNot => a & !b,
            ALUOperation::Nand => !(a & b),
        }
    }

    /// Send a value to the current method address and auto-increment.
    fn send(&mut self, value: u32, processor: &mut dyn MacroProcessor) {
        processor.macro_write(self.method_address.address(), value);
        self.method_address.advance();
    }

    /// Read the opcode at the current PC.
    fn get_opcode(&self) -> Opcode {
        let idx = self.pc as usize;
        let word = if idx < self.code.len() {
            self.code[idx]
        } else {
            log::warn!(
                "Macro: PC {} out of bounds (code size {})",
                idx,
                self.code.len()
            );
            0
        };
        Opcode(word)
    }

    /// Get a register value. Register 0 is hardwired to 0.
    fn get_register(&self, id: u32) -> u32 {
        if id == 0 {
            0
        } else {
            self.registers[id as usize & 0x7]
        }
    }

    /// Set a register value. Writes to register 0 are silently ignored.
    fn set_register(&mut self, id: u32, value: u32) {
        if id != 0 {
            self.registers[id as usize & 0x7] = value;
        }
    }

    /// Fetch the next parameter from the parameter queue.
    fn fetch_param(&mut self) -> u32 {
        if self.next_param_index < self.params.len() {
            let val = self.params[self.next_param_index];
            self.next_param_index += 1;
            val
        } else {
            log::warn!(
                "Macro: parameter fetch out of bounds (index {})",
                self.next_param_index,
            );
            0
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    // ── Mock processor for testing ───────────────────────────────────────

    struct MockProcessor {
        regs: [u32; 0x2000],
        writes: RefCell<Vec<(u32, u32)>>,
    }

    impl MockProcessor {
        fn new() -> Self {
            Self {
                regs: [0u32; 0x2000],
                writes: RefCell::new(Vec::new()),
            }
        }

        fn set_reg(&mut self, method: u32, value: u32) {
            self.regs[method as usize] = value;
        }

        fn recorded_writes(&self) -> Vec<(u32, u32)> {
            self.writes.borrow().clone()
        }
    }

    impl MacroProcessor for MockProcessor {
        fn macro_read(&self, method: u32) -> u32 {
            self.regs.get(method as usize).copied().unwrap_or(0)
        }

        fn macro_write(&mut self, method: u32, value: u32) {
            if (method as usize) < self.regs.len() {
                self.regs[method as usize] = value;
            }
            self.writes.borrow_mut().push((method, value));
        }
    }

    /// Build an AddImmediate opcode (operation=1). Immediate is in bits[31:14].
    fn encode_add_imm(result_op: u32, is_exit: bool, dst: u32, src_a: u32, imm: i32) -> u32 {
        let base = (1u32) // operation = AddImmediate
            | ((result_op & 0x7) << 4)
            | ((is_exit as u32) << 7)
            | ((dst & 0x7) << 8)
            | ((src_a & 0x7) << 11);
        // Immediate in bits[31:14] (18-bit signed).
        let imm_bits = ((imm as u32) & 0x3FFFF) << 14;
        base | imm_bits
    }

    /// Build a Branch opcode (operation=7).
    fn encode_branch(
        condition: u32, // 0=Zero, 1=NotZero (bit[4])
        annul: bool,    // bit[5]
        is_exit: bool,
        src_a: u32,
        imm: i32, // branch target = imm * 4
    ) -> u32 {
        let base = 7u32 // operation = Branch
            | ((condition & 0x1) << 4)
            | ((annul as u32) << 5)
            | ((is_exit as u32) << 7)
            | ((src_a & 0x7) << 11);
        let imm_bits = ((imm as u32) & 0x3FFFF) << 14;
        base | imm_bits
    }

    /// Build a Read opcode (operation=5).
    fn encode_read(result_op: u32, is_exit: bool, dst: u32, src_a: u32, imm: i32) -> u32 {
        let base = 5u32
            | ((result_op & 0x7) << 4)
            | ((is_exit as u32) << 7)
            | ((dst & 0x7) << 8)
            | ((src_a & 0x7) << 11);
        let imm_bits = ((imm as u32) & 0x3FFFF) << 14;
        base | imm_bits
    }

    /// Build an ExtractInsert opcode (operation=2).
    fn encode_extract_insert(
        result_op: u32,
        is_exit: bool,
        dst: u32,
        src_a: u32,
        src_b: u32,
        bf_src_bit: u32,
        bf_size: u32,
        bf_dst_bit: u32,
    ) -> u32 {
        2u32 | ((result_op & 0x7) << 4)
            | ((is_exit as u32) << 7)
            | ((dst & 0x7) << 8)
            | ((src_a & 0x7) << 11)
            | ((src_b & 0x7) << 14)
            | ((bf_src_bit & 0x1F) << 17)
            | ((bf_size & 0x1F) << 22)
            | ((bf_dst_bit & 0x1F) << 27)
    }

    /// Build an ExtractShiftLeftImmediate opcode (operation=3).
    fn encode_extract_shift_left_imm(
        result_op: u32,
        is_exit: bool,
        dst: u32,
        src_a: u32,
        src_b: u32,
        bf_src_bit: u32,
        bf_size: u32,
        bf_dst_bit: u32,
    ) -> u32 {
        3u32 | ((result_op & 0x7) << 4)
            | ((is_exit as u32) << 7)
            | ((dst & 0x7) << 8)
            | ((src_a & 0x7) << 11)
            | ((src_b & 0x7) << 14)
            | ((bf_src_bit & 0x1F) << 17)
            | ((bf_size & 0x1F) << 22)
            | ((bf_dst_bit & 0x1F) << 27)
    }

    /// Build an ExtractShiftLeftRegister opcode (operation=4).
    fn encode_extract_shift_left_reg(
        result_op: u32,
        is_exit: bool,
        dst: u32,
        src_a: u32,
        src_b: u32,
        bf_src_bit: u32,
        bf_size: u32,
        bf_dst_bit: u32,
    ) -> u32 {
        4u32 | ((result_op & 0x7) << 4)
            | ((is_exit as u32) << 7)
            | ((dst & 0x7) << 8)
            | ((src_a & 0x7) << 11)
            | ((src_b & 0x7) << 14)
            | ((bf_src_bit & 0x1F) << 17)
            | ((bf_size & 0x1F) << 22)
            | ((bf_dst_bit & 0x1F) << 27)
    }

    // ── Opcode decoding tests ────────────────────────────────────────────

    #[test]
    fn test_opcode_fields() {
        // Build: operation=5(Read), result_op=3(FetchAndSend), exit=true,
        //        dst=2, src_a=4, immediate=0x100
        let raw = 5u32 | (3 << 4) | (1 << 7) | (2 << 8) | (4 << 11) | (0x100 << 14);
        let op = Opcode(raw);
        assert_eq!(op.operation(), Operation::Read);
        assert_eq!(op.result_operation(), ResultOperation::FetchAndSend);
        assert!(op.is_exit());
        assert_eq!(op.dst(), 2);
        assert_eq!(op.src_a(), 4);
        assert_eq!(op.immediate(), 0x100);
    }

    #[test]
    fn test_opcode_immediate_sign_extension() {
        // Positive: 0x1FFFF (17-bit, fits in 18-bit signed as positive)
        let raw = (0x1FFFFu32) << 14;
        let op = Opcode(raw);
        assert_eq!(op.immediate(), 0x1FFFF);

        // Negative: -1 in 18-bit = 0x3FFFF
        let raw_neg = (0x3FFFFu32) << 14;
        let op_neg = Opcode(raw_neg);
        assert_eq!(op_neg.immediate(), -1);

        // Negative: -2 in 18-bit = 0x3FFFE
        let raw_neg2 = (0x3FFFEu32) << 14;
        let op_neg2 = Opcode(raw_neg2);
        assert_eq!(op_neg2.immediate(), -2);
    }

    #[test]
    fn test_opcode_branch_target() {
        // immediate=3 → branch_target=12
        let raw = (3u32) << 14 | 7; // operation=Branch
        let op = Opcode(raw);
        assert_eq!(op.branch_target(), 12);

        // immediate=-1 → branch_target=-4
        let raw_neg = (0x3FFFFu32) << 14 | 7;
        let op_neg = Opcode(raw_neg);
        assert_eq!(op_neg.branch_target(), -4);
    }

    #[test]
    fn test_operation_from_raw() {
        assert_eq!(Operation::from_raw(0), Operation::ALU);
        assert_eq!(Operation::from_raw(1), Operation::AddImmediate);
        assert_eq!(Operation::from_raw(2), Operation::ExtractInsert);
        assert_eq!(Operation::from_raw(3), Operation::ExtractShiftLeftImmediate);
        assert_eq!(Operation::from_raw(4), Operation::ExtractShiftLeftRegister);
        assert_eq!(Operation::from_raw(5), Operation::Read);
        assert_eq!(Operation::from_raw(6), Operation::Unused);
        assert_eq!(Operation::from_raw(7), Operation::Branch);
    }

    #[test]
    fn test_alu_operation_from_raw() {
        assert_eq!(ALUOperation::from_raw(0), ALUOperation::Add);
        assert_eq!(ALUOperation::from_raw(1), ALUOperation::AddWithCarry);
        assert_eq!(ALUOperation::from_raw(2), ALUOperation::Subtract);
        assert_eq!(ALUOperation::from_raw(3), ALUOperation::SubtractWithBorrow);
        assert_eq!(ALUOperation::from_raw(8), ALUOperation::Xor);
        assert_eq!(ALUOperation::from_raw(9), ALUOperation::Or);
        assert_eq!(ALUOperation::from_raw(10), ALUOperation::And);
        assert_eq!(ALUOperation::from_raw(11), ALUOperation::AndNot);
        assert_eq!(ALUOperation::from_raw(12), ALUOperation::Nand);
        // Unknown → defaults to Add.
        assert_eq!(ALUOperation::from_raw(5), ALUOperation::Add);
    }

    // ── ALU tests ────────────────────────────────────────────────────────

    #[test]
    fn test_alu_add() {
        let mut interp = MacroInterpreter::new();

        // Normal add, no overflow.
        let result = interp.alu(ALUOperation::Add, 10, 20);
        assert_eq!(result, 30);
        assert!(!interp.carry);

        // Overflow.
        let result = interp.alu(ALUOperation::Add, 0xFFFF_FFFF, 1);
        assert_eq!(result, 0);
        assert!(interp.carry);
    }

    #[test]
    fn test_alu_subtract() {
        let mut interp = MacroInterpreter::new();

        // Normal subtract: 20 - 10 = 10, carry=true (no borrow).
        let result = interp.alu(ALUOperation::Subtract, 20, 10);
        assert_eq!(result, 10);
        assert!(interp.carry);

        // Underflow: 0 - 1 = 0xFFFF_FFFF, carry=false (borrow).
        let result = interp.alu(ALUOperation::Subtract, 0, 1);
        assert_eq!(result, 0xFFFF_FFFF);
        assert!(!interp.carry);
    }

    #[test]
    fn test_alu_bitwise() {
        let mut interp = MacroInterpreter::new();

        assert_eq!(interp.alu(ALUOperation::Xor, 0xFF00, 0x0FF0), 0xF0F0);
        assert_eq!(interp.alu(ALUOperation::Or, 0xFF00, 0x00FF), 0xFFFF);
        assert_eq!(interp.alu(ALUOperation::And, 0xFF00, 0x0FF0), 0x0F00);
        assert_eq!(interp.alu(ALUOperation::AndNot, 0xFF00, 0x0F00), 0xF000);
        assert_eq!(interp.alu(ALUOperation::Nand, 0xFF00, 0x0FF0), !0x0F00);
    }

    #[test]
    fn test_alu_add_with_carry() {
        let mut interp = MacroInterpreter::new();

        // First add with overflow to set carry.
        interp.alu(ALUOperation::Add, 0xFFFF_FFFF, 1);
        assert!(interp.carry);

        // AddWithCarry: 5 + 3 + carry(1) = 9.
        let result = interp.alu(ALUOperation::AddWithCarry, 5, 3);
        assert_eq!(result, 9);
        assert!(!interp.carry);
    }

    // ── Execution tests ──────────────────────────────────────────────────

    #[test]
    fn test_move_immediate() {
        // AddImmediate: $r2 = $r0 + 42 (Move result)
        // Then exit.
        let code = vec![
            encode_add_imm(1, true, 2, 0, 42), // Move, exit, dst=r2, src=r0, imm=42
        ];
        let mut interp = MacroInterpreter::new();
        for (i, &w) in code.iter().enumerate() {
            interp.upload_code(i as u32, w);
        }
        interp.set_position(0, 0);

        let mut proc = MockProcessor::new();
        interp.execute(0, &[0], &mut proc);

        assert_eq!(interp.registers[2], 42);
    }

    #[test]
    fn test_register_zero_hardwired() {
        // AddImmediate: $r0 = $r0 + 99 (Move result) — write to r0 should be ignored.
        // Then: AddImmediate: $r2 = $r0 + 1 (Move, exit) — r0 should still be 0.
        let code = vec![
            encode_add_imm(1, false, 0, 0, 99), // Move, dst=r0, src=r0, imm=99
            encode_add_imm(1, true, 2, 0, 1),   // Move, exit, dst=r2, src=r0, imm=1
        ];
        let mut interp = MacroInterpreter::new();
        for (i, &w) in code.iter().enumerate() {
            interp.upload_code(i as u32, w);
        }
        interp.set_position(0, 0);

        let mut proc = MockProcessor::new();
        interp.execute(0, &[0], &mut proc);

        assert_eq!(interp.registers[0], 0); // Hardwired.
        assert_eq!(interp.registers[2], 1); // r0(0) + 1.
    }

    #[test]
    fn test_fetch_parameter() {
        // IgnoreAndFetch: fetch next param into $r2.
        // Then exit.
        let code = vec![
            encode_add_imm(0, true, 2, 0, 0), // IgnoreAndFetch, exit, dst=r2
        ];
        let mut interp = MacroInterpreter::new();
        for (i, &w) in code.iter().enumerate() {
            interp.upload_code(i as u32, w);
        }
        interp.set_position(0, 0);

        let mut proc = MockProcessor::new();
        // params[0] goes to r1, params[1] fetched by IgnoreAndFetch.
        interp.execute(0, &[0xAA, 0xBB], &mut proc);

        assert_eq!(interp.registers[1], 0xAA);
        assert_eq!(interp.registers[2], 0xBB);
    }

    #[test]
    fn test_send_writes_method() {
        // MoveAndSetMethod: r2 = 0x100 (also sets method_address)
        // Then MoveAndSend: send the value 42
        let code = vec![
            // AddImmediate, MoveAndSetMethod, dst=r2, src=r0, imm=0x100
            encode_add_imm(2, false, 2, 0, 0x100),
            // AddImmediate, MoveAndSend(4), exit, dst=r3, src=r0, imm=42
            encode_add_imm(4, true, 3, 0, 42),
        ];
        let mut interp = MacroInterpreter::new();
        for (i, &w) in code.iter().enumerate() {
            interp.upload_code(i as u32, w);
        }
        interp.set_position(0, 0);

        let mut proc = MockProcessor::new();
        interp.execute(0, &[0], &mut proc);

        let writes = proc.recorded_writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0], (0x100, 42));
    }

    #[test]
    fn test_method_address_increment() {
        // MoveAndSetMethod: r2 = method_addr with address=0x100, increment=1.
        // Then two MoveAndSend calls. Each should write to 0x100, 0x101.
        // method_address raw = address(0x100) | increment(1 << 12) = 0x1100.
        let method_raw = 0x100 | (1 << 12); // address=0x100, increment=1
        let code = vec![
            // AddImmediate, MoveAndSetMethod, dst=r2, src=r0, imm=method_raw
            encode_add_imm(2, false, 2, 0, method_raw as i32),
            // AddImmediate, MoveAndSend(4), dst=r3, src=r0, imm=10
            encode_add_imm(4, false, 3, 0, 10),
            // AddImmediate, MoveAndSend(4), exit, dst=r3, src=r0, imm=20
            encode_add_imm(4, true, 3, 0, 20),
        ];
        let mut interp = MacroInterpreter::new();
        for (i, &w) in code.iter().enumerate() {
            interp.upload_code(i as u32, w);
        }
        interp.set_position(0, 0);

        let mut proc = MockProcessor::new();
        interp.execute(0, &[0], &mut proc);

        let writes = proc.recorded_writes();
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0], (0x100, 10));
        assert_eq!(writes[1], (0x101, 20));
    }

    #[test]
    fn test_read_gpu_register() {
        // Read operation: result = processor.macro_read(r1 + 0).
        // r1 = param[0] = 0x50. We set mock reg 0x50 = 0xCAFE.
        // Move result to r2, then exit.
        let code = vec![
            // Read, Move(1), exit, dst=r2, src_a=r1, imm=0
            encode_read(1, true, 2, 1, 0),
        ];
        let mut interp = MacroInterpreter::new();
        for (i, &w) in code.iter().enumerate() {
            interp.upload_code(i as u32, w);
        }
        interp.set_position(0, 0);

        let mut proc = MockProcessor::new();
        proc.set_reg(0x50, 0xCAFE);
        interp.execute(0, &[0x50], &mut proc);

        assert_eq!(interp.registers[2], 0xCAFE);
    }

    #[test]
    fn test_exit_with_delay_slot() {
        // Instruction 0: AddImm, Move, dst=r2, imm=10 (exit=true)
        // Instruction 1: AddImm, Move, dst=r3, imm=20 (delay slot, executed)
        // Instruction 2: AddImm, Move, dst=r4, imm=30 (should NOT execute)
        let code = vec![
            encode_add_imm(1, true, 2, 0, 10),  // exit=true
            encode_add_imm(1, false, 3, 0, 20), // delay slot
            encode_add_imm(1, false, 4, 0, 30), // unreachable
        ];
        let mut interp = MacroInterpreter::new();
        for (i, &w) in code.iter().enumerate() {
            interp.upload_code(i as u32, w);
        }
        interp.set_position(0, 0);

        let mut proc = MockProcessor::new();
        interp.execute(0, &[0], &mut proc);

        assert_eq!(interp.registers[2], 10);
        assert_eq!(interp.registers[3], 20); // Delay slot executed.
        assert_eq!(interp.registers[4], 0); // Not executed.
    }

    #[test]
    fn test_branch_zero_taken() {
        // r1 = param[0] = 0 → branch to +2 (skip instruction 1, land on 2).
        // Instruction 0: Branch Zero on r1, annul=true, target=+2
        // Instruction 1: AddImm r2 = 99 (skipped)
        // Instruction 2: AddImm r3 = 42, exit
        let code = vec![
            encode_branch(0, true, false, 1, 2), // Branch Zero, annul, src_a=r1, imm=+2
            encode_add_imm(1, true, 2, 0, 99),   // skipped
            encode_add_imm(1, true, 3, 0, 42),   // landed here
        ];
        let mut interp = MacroInterpreter::new();
        for (i, &w) in code.iter().enumerate() {
            interp.upload_code(i as u32, w);
        }
        interp.set_position(0, 0);

        let mut proc = MockProcessor::new();
        interp.execute(0, &[0], &mut proc); // param[0]=0 → r1=0

        assert_eq!(interp.registers[2], 0); // Skipped.
        assert_eq!(interp.registers[3], 42); // Executed.
    }

    // ── Bitfield tests ───────────────────────────────────────────────────

    #[test]
    fn test_extract_insert() {
        // ExtractInsert: extract bits[7:4] (4 bits starting at bit 4) from src_b,
        //                insert at bit 8 of src_a.
        // src_a(r2) = 0xFF00, src_b(r3) = 0xAB.
        // Extract: (0xAB >> 4) & 0xF = 0xA
        // Insert at bit 8: 0xFF00 & ~(0xF << 8) | (0xA << 8) = 0xFA00

        // First set up registers.
        let code = vec![
            encode_add_imm(1, false, 2, 0, 0xFF00u32 as i32), // r2 = 0xFF00
            encode_add_imm(1, false, 3, 0, 0xAB),             // r3 = 0xAB
            // ExtractInsert: result_op=Move(1), dst=r4, src_a=r2, src_b=r3,
            //   bf_src_bit=4, bf_size=4, bf_dst_bit=8
            encode_extract_insert(1, true, 4, 2, 3, 4, 4, 8),
        ];
        let mut interp = MacroInterpreter::new();
        for (i, &w) in code.iter().enumerate() {
            interp.upload_code(i as u32, w);
        }
        interp.set_position(0, 0);

        let mut proc = MockProcessor::new();
        interp.execute(0, &[0], &mut proc);

        assert_eq!(interp.registers[4], 0xFA00);
    }

    #[test]
    fn test_extract_shift_left_immediate() {
        // ExtractShiftLeftImmediate:
        //   result = ((src_b >> src_a_val) & mask) << bf_dst_bit
        // src_a(r2) = 4 (shift right amount), src_b(r3) = 0xABCD.
        // bf_src_bit unused for this op, bf_size=8, bf_dst_bit=4.
        // result = ((0xABCD >> 4) & 0xFF) << 4 = (0xABC & 0xFF) << 4 = 0xBC << 4 = 0xBC0
        let code = vec![
            encode_add_imm(1, false, 2, 0, 4),      // r2 = 4
            encode_add_imm(1, false, 3, 0, 0xABCD), // r3 = 0xABCD
            encode_extract_shift_left_imm(1, true, 4, 2, 3, 0, 8, 4),
        ];
        let mut interp = MacroInterpreter::new();
        for (i, &w) in code.iter().enumerate() {
            interp.upload_code(i as u32, w);
        }
        interp.set_position(0, 0);

        let mut proc = MockProcessor::new();
        interp.execute(0, &[0], &mut proc);

        assert_eq!(interp.registers[4], 0xBC0);
    }

    #[test]
    fn test_extract_shift_left_register() {
        // ExtractShiftLeftRegister:
        //   result = ((src_b >> bf_src_bit) & mask) << src_a_val
        // src_a(r2) = 8 (shift left amount), src_b(r3) = 0xABCD.
        // bf_src_bit=4, bf_size=8, bf_dst_bit unused.
        // result = ((0xABCD >> 4) & 0xFF) << 8 = 0xBC << 8 = 0xBC00
        let code = vec![
            encode_add_imm(1, false, 2, 0, 8),      // r2 = 8
            encode_add_imm(1, false, 3, 0, 0xABCD), // r3 = 0xABCD
            encode_extract_shift_left_reg(1, true, 4, 2, 3, 4, 8, 0),
        ];
        let mut interp = MacroInterpreter::new();
        for (i, &w) in code.iter().enumerate() {
            interp.upload_code(i as u32, w);
        }
        interp.set_position(0, 0);

        let mut proc = MockProcessor::new();
        interp.execute(0, &[0], &mut proc);

        assert_eq!(interp.registers[4], 0xBC00);
    }

    // ── Integration tests ────────────────────────────────────────────────

    #[test]
    fn test_upload_and_execute() {
        let mut interp = MacroInterpreter::new();

        // Upload a simple macro at offset 10.
        let code = vec![
            encode_add_imm(1, true, 2, 0, 77), // Move r2=77, exit
        ];
        for (i, &w) in code.iter().enumerate() {
            interp.upload_code(10 + i as u32, w);
        }
        interp.set_position(3, 10); // Slot 3 starts at word 10.

        let mut proc = MockProcessor::new();
        interp.execute(3, &[0], &mut proc);

        assert_eq!(interp.registers[2], 77);
    }

    #[test]
    fn test_multi_draw_macro() {
        // Macro that writes DRAW_BEGIN (0x1615) and DRAW_END (0x1614)
        // in a loop driven by r1 (param[0] = iteration count).
        //
        // Pseudo-code:
        //   r2 = r1  (count)
        //   r3 = 0x1615 | (1 << 12) = method_addr for DRAW_BEGIN + incr=1
        // loop:
        //   method = r3; send(0x10)   → writes DRAW_BEGIN, then addr becomes 0x1616
        //   send(0)                   → writes (0x1616 but we just need DRAW_END at 0x1614)
        //
        // Simpler approach: write DRAW_BEGIN then DRAW_END directly.
        // Let's do 2 sends per iteration.

        // Actually, let's keep it simple: a macro that sends 2 values to
        // consecutive methods. method=0x100 increment=1, send 0xAA then 0xBB.
        let method_raw = 0x100 | (1 << 12); // addr=0x100, incr=1
        let code = vec![
            // Set method address.
            encode_add_imm(2, false, 2, 0, method_raw as i32),
            // MoveAndSend(4): send r1 (param 0).
            encode_add_imm(4, false, 3, 1, 0),
            // FetchAndSend(3): fetch param, send result (which is imm 0).
            // Actually, let's use AddImm MoveAndSend to send a known value.
            encode_add_imm(4, true, 4, 0, 0xBB),
        ];
        let mut interp = MacroInterpreter::new();
        for (i, &w) in code.iter().enumerate() {
            interp.upload_code(i as u32, w);
        }
        interp.set_position(0, 0);

        let mut proc = MockProcessor::new();
        interp.execute(0, &[0xAA], &mut proc);

        let writes = proc.recorded_writes();
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0], (0x100, 0xAA)); // r1=0xAA
        assert_eq!(writes[1], (0x101, 0xBB));
    }

    #[test]
    fn test_branch_annul() {
        // Branch with annul=true skips the delay slot entirely.
        // r1 = 0 → branch Zero taken to instruction 2.
        let code = vec![
            encode_branch(0, true, false, 1, 2), // Branch Zero, annul=true, to +2
            encode_add_imm(1, true, 2, 0, 99),   // Would be delay slot, but annul → skip
            encode_add_imm(1, true, 3, 0, 42),   // Target
        ];
        let mut interp = MacroInterpreter::new();
        for (i, &w) in code.iter().enumerate() {
            interp.upload_code(i as u32, w);
        }
        interp.set_position(0, 0);

        let mut proc = MockProcessor::new();
        interp.execute(0, &[0], &mut proc);

        assert_eq!(interp.registers[2], 0); // Skipped (annulled).
        assert_eq!(interp.registers[3], 42); // Target executed.
    }

    #[test]
    fn test_set_method_fetch_and_send() {
        // ResultOp 6: MoveAndSetMethodFetchAndSend
        // Move result into dst, set method address = result, fetch param, send param.
        let method_raw = 0x200 | (1 << 12); // addr=0x200, incr=1
        let code = vec![
            // AddImmediate, result_op=6, dst=r2, src=r0, imm=method_raw
            encode_add_imm(6, true, 2, 0, method_raw as i32),
        ];
        let mut interp = MacroInterpreter::new();
        for (i, &w) in code.iter().enumerate() {
            interp.upload_code(i as u32, w);
        }
        interp.set_position(0, 0);

        let mut proc = MockProcessor::new();
        // params[0] → r1, params[1] → fetched and sent.
        interp.execute(0, &[0, 0xFF], &mut proc);

        assert_eq!(interp.registers[2], method_raw);
        let writes = proc.recorded_writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0], (0x200, 0xFF)); // Fetched param sent to method 0x200.
    }

    #[test]
    fn test_move_and_set_method_send() {
        // ResultOp 7: MoveAndSetMethodSend
        // Move result into dst, set method address = result,
        // send (result >> 12) & 0x3F.
        let method_raw = 0x300 | (5 << 12); // addr=0x300, incr=5
                                            // (method_raw >> 12) & 0x3F = 5
        let code = vec![encode_add_imm(7, true, 2, 0, method_raw as i32)];
        let mut interp = MacroInterpreter::new();
        for (i, &w) in code.iter().enumerate() {
            interp.upload_code(i as u32, w);
        }
        interp.set_position(0, 0);

        let mut proc = MockProcessor::new();
        interp.execute(0, &[0], &mut proc);

        assert_eq!(interp.registers[2], method_raw);
        let writes = proc.recorded_writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0], (0x300, 5)); // (method_raw >> 12) & 0x3F = 5
    }

    #[test]
    fn test_branch_not_zero_not_taken() {
        // Branch NotZero on r1=0 → not taken, falls through.
        // Instruction layout:
        //   0: Branch NotZero r1, annul, target=+3
        //   1: r2=42, exit → delay slot runs instruction 2 (harmless)
        //   2: r5=0 (delay slot, no side effects)
        //   3: r3=99, exit (branch target — NOT reached)
        let code = vec![
            encode_branch(1, true, false, 1, 3), // Branch NotZero, annul, src=r1, target=+3
            encode_add_imm(1, true, 2, 0, 42),   // Falls through: r2=42, exit.
            encode_add_imm(1, false, 5, 0, 0),   // Delay slot: r5=0 (harmless).
            encode_add_imm(1, true, 3, 0, 99),   // Branch target: r3=99 (not reached).
        ];
        let mut interp = MacroInterpreter::new();
        for (i, &w) in code.iter().enumerate() {
            interp.upload_code(i as u32, w);
        }
        interp.set_position(0, 0);

        let mut proc = MockProcessor::new();
        interp.execute(0, &[0], &mut proc); // r1=0, NotZero → not taken

        assert_eq!(interp.registers[2], 42); // Fallthrough executed.
        assert_eq!(interp.registers[3], 0); // Target not reached.
    }

    #[test]
    fn test_branch_with_delay_slot() {
        // Branch Zero taken (no annul) → delay slot instruction executes.
        // Instruction 0: Branch Zero on r0(=0), target=+2 (jump to instr 2), no annul
        // Instruction 1: AddImm r2=55 (delay slot, executed)
        // Instruction 2: AddImm r3=77, exit
        let code = vec![
            encode_branch(0, false, false, 0, 2), // Branch Zero, no annul, src=r0, target=+2
            encode_add_imm(1, false, 2, 0, 55),   // Delay slot: executed.
            encode_add_imm(1, true, 3, 0, 77),    // Branch target.
        ];
        let mut interp = MacroInterpreter::new();
        for (i, &w) in code.iter().enumerate() {
            interp.upload_code(i as u32, w);
        }
        interp.set_position(0, 0);

        let mut proc = MockProcessor::new();
        interp.execute(0, &[0], &mut proc);

        assert_eq!(interp.registers[2], 55); // Delay slot executed.
        assert_eq!(interp.registers[3], 77); // Target executed.
    }

    #[test]
    fn test_loop_with_branch() {
        // A simple loop: count down r2 from 3 to 0, each iteration sends r2
        // to method 0x50 (no auto-increment).
        //
        // Code:
        // 0: AddImm Move r2=r1 (r1=3 from param[0])        → r2 = 3
        // 1: AddImm MoveAndSetMethod r3=0x50                → set method = 0x50
        // 2: AddImm MoveAndSend r4=r2                        → send(r2) to 0x50
        // 3: AddImm Move r2=r2+(-1)                          → r2 -= 1
        // 4: Branch NotZero r2, target=-2 (back to instr 2), annul
        // 5: exit (AddImm r5=0, exit)

        // method_raw = addr=0x50, increment=0
        let method_raw = 0x50;

        let code = vec![
            encode_add_imm(1, false, 2, 1, 0),            // Move r2 = r1
            encode_add_imm(2, false, 3, 0, method_raw),   // MoveAndSetMethod r3 = 0x50
            encode_add_imm(4, false, 4, 2, 0),            // MoveAndSend r4 = r2, send r2
            encode_add_imm(1, false, 2, 2, -1i32 as i32), // Move r2 = r2 - 1
            encode_branch(1, true, false, 2, -2),         // Branch NotZero r2, target=-2, annul
            encode_add_imm(1, true, 5, 0, 0),             // exit
        ];
        let mut interp = MacroInterpreter::new();
        for (i, &w) in code.iter().enumerate() {
            interp.upload_code(i as u32, w);
        }
        interp.set_position(0, 0);

        let mut proc = MockProcessor::new();
        interp.execute(0, &[3], &mut proc);

        let writes = proc.recorded_writes();
        // Should have sent 3, 2, 1.
        assert_eq!(writes.len(), 3);
        assert_eq!(writes[0], (0x50, 3));
        assert_eq!(writes[1], (0x50, 2));
        assert_eq!(writes[2], (0x50, 1));
    }

    #[test]
    fn test_subtract_with_borrow() {
        let mut interp = MacroInterpreter::new();

        // Set carry=false first (borrow set).
        interp.carry = false;

        // SubtractWithBorrow: a - b - (!carry) = 10 - 3 - 1 = 6
        let result = interp.alu(ALUOperation::SubtractWithBorrow, 10, 3);
        assert_eq!(result, 6);
        assert!(interp.carry); // No borrow on this result.
    }

    #[test]
    fn test_method_address_struct() {
        let mut ma = MethodAddress::default();
        assert_eq!(ma.address(), 0);
        assert_eq!(ma.increment(), 0);

        ma.set(0x100 | (3 << 12));
        assert_eq!(ma.address(), 0x100);
        assert_eq!(ma.increment(), 3);

        ma.advance();
        assert_eq!(ma.address(), 0x103);
        assert_eq!(ma.increment(), 3);

        ma.advance();
        assert_eq!(ma.address(), 0x106);
    }

    #[test]
    fn test_fetch_and_set_method() {
        // ResultOp 5: FetchAndSetMethod — fetch param into dst, set method = result.
        let code = vec![
            // AddImm, FetchAndSetMethod(5), exit, dst=r2, src=r0, imm=0x300
            encode_add_imm(5, true, 2, 0, 0x300),
        ];
        let mut interp = MacroInterpreter::new();
        for (i, &w) in code.iter().enumerate() {
            interp.upload_code(i as u32, w);
        }
        interp.set_position(0, 0);

        let mut proc = MockProcessor::new();
        // params[0]=0xAA → r1, params[1]=0xBB → fetched into r2
        interp.execute(0, &[0xAA, 0xBB], &mut proc);

        assert_eq!(interp.registers[2], 0xBB); // Fetched param.
        assert_eq!(interp.method_address.address(), 0x300); // Set from result.
    }
}

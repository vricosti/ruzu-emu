// SPDX-FileCopyrightText: Copyright 2019 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/memory/dmnt_cheat_vm.h and dmnt_cheat_vm.cpp
//! Cheat VM interpreter for the dmnt cheat system.

use super::dmnt_cheat_types::{CheatEntry, CheatProcessMetadata};

// ---- Enums matching upstream ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum CheatVmOpcodeType {
    StoreStatic = 0,
    BeginConditionalBlock = 1,
    EndConditionalBlock = 2,
    ControlLoop = 3,
    LoadRegisterStatic = 4,
    LoadRegisterMemory = 5,
    StoreStaticToAddress = 6,
    PerformArithmeticStatic = 7,
    BeginKeypressConditionalBlock = 8,

    // These are not implemented by Gateway's VM.
    PerformArithmeticRegister = 9,
    StoreRegisterToAddress = 10,
    Reserved11 = 11,

    // Meta entry for multi-nybble instruction decoding.
    ExtendedWidth = 12,

    // Extended width opcodes.
    BeginRegisterConditionalBlock = 0xC0,
    SaveRestoreRegister = 0xC1,
    SaveRestoreRegisterMask = 0xC2,
    ReadWriteStaticRegister = 0xC3,

    // Meta entry for multi-nybble instruction decoding.
    DoubleExtendedWidth = 0xF0,

    // Double-extended width opcodes.
    PauseProcess = 0xFF0,
    ResumeProcess = 0xFF1,
    DebugLog = 0xFFF,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MemoryAccessType {
    MainNso = 0,
    Heap = 1,
    Alias = 2,
    Aslr = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ConditionalComparisonType {
    GT = 1,
    GE = 2,
    LT = 3,
    LE = 4,
    EQ = 5,
    NE = 6,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum RegisterArithmeticType {
    Addition = 0,
    Subtraction = 1,
    Multiplication = 2,
    LeftShift = 3,
    RightShift = 4,

    // Not supported by Gateway's VM.
    LogicalAnd = 5,
    LogicalOr = 6,
    LogicalNot = 7,
    LogicalXor = 8,

    None = 9,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum StoreRegisterOffsetType {
    None = 0,
    Reg = 1,
    Imm = 2,
    MemReg = 3,
    MemImm = 4,
    MemImmReg = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum CompareRegisterValueType {
    MemoryRelAddr = 0,
    MemoryOfsReg = 1,
    RegisterRelAddr = 2,
    RegisterOfsReg = 3,
    StaticValue = 4,
    OtherRegister = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SaveRestoreRegisterOpType {
    Restore = 0,
    Save = 1,
    ClearSaved = 2,
    ClearRegs = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum DebugLogValueType {
    MemoryRelAddr = 0,
    MemoryOfsReg = 1,
    RegisterRelAddr = 2,
    RegisterOfsReg = 3,
    RegisterValue = 4,
}

// ---- VM integer union ----

/// Union-like type for VM integer values of varying widths.
/// Upstream uses a C union { u8, u16, u32, u64 }. We store as u64 and
/// extract the relevant width.
#[derive(Debug, Clone, Copy, Default)]
pub struct VmInt {
    pub bit64: u64,
}

impl VmInt {
    pub fn bit8(&self) -> u8 {
        self.bit64 as u8
    }
    pub fn bit16(&self) -> u16 {
        self.bit64 as u16
    }
    pub fn bit32(&self) -> u32 {
        self.bit64 as u32
    }
}

// ---- Opcode structs ----

#[derive(Debug, Clone, Default)]
pub struct StoreStaticOpcode {
    pub bit_width: u32,
    pub mem_type: u32, // MemoryAccessType as u32
    pub offset_register: u32,
    pub rel_address: u64,
    pub value: VmInt,
}

#[derive(Debug, Clone, Default)]
pub struct BeginConditionalOpcode {
    pub bit_width: u32,
    pub mem_type: u32,
    pub cond_type: u32,
    pub rel_address: u64,
    pub value: VmInt,
}

#[derive(Debug, Clone, Default)]
pub struct EndConditionalOpcode {
    pub is_else: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ControlLoopOpcode {
    pub start_loop: bool,
    pub reg_index: u32,
    pub num_iters: u32,
}

#[derive(Debug, Clone, Default)]
pub struct LoadRegisterStaticOpcode {
    pub reg_index: u32,
    pub value: u64,
}

#[derive(Debug, Clone, Default)]
pub struct LoadRegisterMemoryOpcode {
    pub bit_width: u32,
    pub mem_type: u32,
    pub reg_index: u32,
    pub load_from_reg: bool,
    pub rel_address: u64,
}

#[derive(Debug, Clone, Default)]
pub struct StoreStaticToAddressOpcode {
    pub bit_width: u32,
    pub reg_index: u32,
    pub increment_reg: bool,
    pub add_offset_reg: bool,
    pub offset_reg_index: u32,
    pub value: u64,
}

#[derive(Debug, Clone, Default)]
pub struct PerformArithmeticStaticOpcode {
    pub bit_width: u32,
    pub reg_index: u32,
    pub math_type: u32,
    pub value: u32,
}

#[derive(Debug, Clone, Default)]
pub struct BeginKeypressConditionalOpcode {
    pub key_mask: u32,
}

#[derive(Debug, Clone, Default)]
pub struct PerformArithmeticRegisterOpcode {
    pub bit_width: u32,
    pub math_type: u32,
    pub dst_reg_index: u32,
    pub src_reg_1_index: u32,
    pub src_reg_2_index: u32,
    pub has_immediate: bool,
    pub value: VmInt,
}

#[derive(Debug, Clone, Default)]
pub struct StoreRegisterToAddressOpcode {
    pub bit_width: u32,
    pub str_reg_index: u32,
    pub addr_reg_index: u32,
    pub increment_reg: bool,
    pub ofs_type: u32,
    pub mem_type: u32,
    pub ofs_reg_index: u32,
    pub rel_address: u64,
}

#[derive(Debug, Clone, Default)]
pub struct BeginRegisterConditionalOpcode {
    pub bit_width: u32,
    pub cond_type: u32,
    pub val_reg_index: u32,
    pub comp_type: u32,
    pub mem_type: u32,
    pub addr_reg_index: u32,
    pub other_reg_index: u32,
    pub ofs_reg_index: u32,
    pub rel_address: u64,
    pub value: VmInt,
}

#[derive(Debug, Clone, Default)]
pub struct SaveRestoreRegisterOpcode {
    pub dst_index: u32,
    pub src_index: u32,
    pub op_type: u32,
}

#[derive(Debug, Clone)]
pub struct SaveRestoreRegisterMaskOpcode {
    pub op_type: u32,
    pub should_operate: [bool; 0x10],
}

impl Default for SaveRestoreRegisterMaskOpcode {
    fn default() -> Self {
        Self {
            op_type: 0,
            should_operate: [false; 0x10],
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ReadWriteStaticRegisterOpcode {
    pub static_idx: u32,
    pub idx: u32,
}

#[derive(Debug, Clone, Default)]
pub struct PauseProcessOpcode;

#[derive(Debug, Clone, Default)]
pub struct ResumeProcessOpcode;

#[derive(Debug, Clone, Default)]
pub struct DebugLogOpcode {
    pub bit_width: u32,
    pub log_id: u32,
    pub val_type: u32,
    pub mem_type: u32,
    pub addr_reg_index: u32,
    pub val_reg_index: u32,
    pub ofs_reg_index: u32,
    pub rel_address: u64,
}

#[derive(Debug, Clone, Default)]
pub struct UnrecognizedInstruction {
    pub opcode: u32,
}

/// Variant of all possible decoded opcodes.
#[derive(Debug, Clone)]
pub enum CheatVmOpcodeVariant {
    StoreStatic(StoreStaticOpcode),
    BeginConditional(BeginConditionalOpcode),
    EndConditional(EndConditionalOpcode),
    ControlLoop(ControlLoopOpcode),
    LoadRegisterStatic(LoadRegisterStaticOpcode),
    LoadRegisterMemory(LoadRegisterMemoryOpcode),
    StoreStaticToAddress(StoreStaticToAddressOpcode),
    PerformArithmeticStatic(PerformArithmeticStaticOpcode),
    BeginKeypressConditional(BeginKeypressConditionalOpcode),
    PerformArithmeticRegister(PerformArithmeticRegisterOpcode),
    StoreRegisterToAddress(StoreRegisterToAddressOpcode),
    BeginRegisterConditional(BeginRegisterConditionalOpcode),
    SaveRestoreRegister(SaveRestoreRegisterOpcode),
    SaveRestoreRegisterMask(SaveRestoreRegisterMaskOpcode),
    ReadWriteStaticRegister(ReadWriteStaticRegisterOpcode),
    PauseProcess(PauseProcessOpcode),
    ResumeProcess(ResumeProcessOpcode),
    DebugLog(DebugLogOpcode),
    Unrecognized(UnrecognizedInstruction),
}

#[derive(Debug, Clone)]
pub struct CheatVmOpcode {
    pub begin_conditional_block: bool,
    pub opcode: CheatVmOpcodeVariant,
}

// ---- Callbacks trait ----

/// Helper trait for DmntCheatVm <=> emulator interface.
/// Port of DmntCheatVm::Callbacks.
pub trait VmCallbacks: Send + Sync {
    fn memory_read_unsafe(&self, address: u64, data: &mut [u8]);
    fn memory_write_unsafe(&self, address: u64, data: &[u8]);
    fn hid_keys_down(&self) -> u64;
    fn pause_process(&self);
    fn resume_process(&self);
    fn debug_log(&self, id: u8, value: u64);
    fn command_log(&self, data: &str);
}

// ---- DmntCheatVm ----

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::cheat_engine::{CheatParser, TextCheatParser};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Memory(Arc<Mutex<BTreeMap<u64, u8>>>);

    impl VmCallbacks for Memory {
        fn memory_read_unsafe(&self, address: u64, data: &mut [u8]) {
            let bytes = self.0.lock().unwrap();
            for (offset, byte) in data.iter_mut().enumerate() {
                *byte = *bytes.get(&(address + offset as u64)).unwrap_or(&0);
            }
        }
        fn memory_write_unsafe(&self, address: u64, data: &[u8]) {
            let mut bytes = self.0.lock().unwrap();
            for (offset, byte) in data.iter().enumerate() {
                bytes.insert(address + offset as u64, *byte);
            }
        }
        fn hid_keys_down(&self) -> u64 { 0 }
        fn pause_process(&self) {}
        fn resume_process(&self) {}
        fn debug_log(&self, _: u8, _: u64) {}
        fn command_log(&self, _: &str) {}
    }

    fn run(memory: &Memory, text: &str, base: u64) -> DmntCheatVm {
        let mut vm = DmntCheatVm::new(Box::new(memory.clone()));
        let cheats = TextCheatParser.parse(text);
        assert!(cheats.iter().any(|entry| entry.enabled));
        assert!(vm.load_program(&cheats));
        let mut metadata = CheatProcessMetadata::default();
        metadata.main_nso_extents.base = base;
        vm.execute(&metadata);
        vm
    }

    #[test]
    fn wonder_99_lives_follows_pointer_offsets_and_writes_one_byte() {
        let memory = Memory::default();
        let base = 0x7100000000;
        for (address, pointer) in [
            (base + 0x03628398, 0x10000u64),
            (0x10030, 0x20000), (0x20008, 0x30000),
            (0x30030, 0x40000), (0x40168, 0x50000),
        ] {
            memory.memory_write_unsafe(address, &pointer.to_le_bytes());
        }
        memory.memory_write_unsafe(0x5001f, &[0xaa, 10, 0xbb]);
        let before = memory.0.lock().unwrap().clone();
        run(&memory, "[99 Lives]\n580F0000 03628398\n580F1000 00000030\n580F1000 00000008\n580F1000 00000030\n580F1000 00000168\n780F0000 00000020\n610F0000 00000000 00000063", base);
        let mut expected = before;
        expected.insert(0x50020, 99);
        assert_eq!(*memory.0.lock().unwrap(), expected);
    }

    #[test]
    fn static_writes_use_byte_widths_and_consume_the_exact_immediate() {
        for width in [1u32, 2, 4, 8] {
            let memory = Memory::default();
            let immediate = if width == 8 { "11223344 55667788" } else { "55667788" };
            run(&memory, &format!("[writes]\n{:08X} 00000020 {immediate}\n01000000 00000040 000000AB", width << 24), 0x1000);
            let bytes = memory.0.lock().unwrap();
            let value = 0x1122334455667788u64.to_le_bytes();
            for offset in 0..width as usize {
                assert_eq!(bytes[&(0x1020 + offset as u64)], value[offset]);
            }
            assert_eq!(bytes.len(), width as usize + 1);
            assert_eq!(bytes[&0x1040], 0xab);
        }
    }

    #[test]
    fn register_store_uses_offset_increment_and_full_immediate() {
        let memory = Memory::default();
        let vm = run(&memory, "[store]\n400F0000 00000000 00001000\n400E0000 00000000 00000020\n610F11E0 11223344 55667788", 0);
        assert_eq!(memory.0.lock().unwrap().get(&0x1020), Some(&0x88));
        assert_eq!(memory.0.lock().unwrap().len(), 1);
        assert_eq!(vm.registers[15], 0x1001);
    }

    #[test]
    fn narrow_register_read_preserves_upper_bytes() {
        let memory = Memory::default();
        memory.memory_write_unsafe(0x1020, &[0x99]);
        let vm = run(&memory, "[read]\n400F0000 11223344 55667788\n510F0000 00000020", 0x1000);
        assert_eq!(vm.registers[15], 0x1122334455667799);
    }

    #[test]
    fn static_arithmetic_decodes_operation_and_rejects_extensions() {
        let memory = Memory::default();
        let vm = run(&memory, "[math]\n400F0000 00000000 00000100\n740F1000 00000003\n740F5000 00000000", 0);
        assert_eq!(vm.registers[15], 0xfd);
    }

    #[test]
    fn register_arithmetic_selects_register_or_immediate() {
        let vm = run(&Memory::default(), "[math]\n400F0000 00000000 00000010\n400E0000 00000000 00000003\n940DF0E0\n940CD100 00000002", 0);
        assert_eq!(vm.registers[13], 0x13);
        assert_eq!(vm.registers[12], 0x15);
    }

    #[test]
    fn loop_end_does_not_consume_following_instruction() {
        let mut vm = DmntCheatVm::new(Box::new(Memory::default()));
        vm.load_program(&TextCheatParser.parse("[loop]\n30000000 00000002\n31000000\n01000000 00000020 00000063"));
        let start = vm.decode_next_opcode().unwrap();
        assert!(!start.begin_conditional_block);
        let end = vm.decode_next_opcode().unwrap();
        assert!(!end.begin_conditional_block);
        assert!(matches!(end.opcode, CheatVmOpcodeVariant::ControlLoop(ControlLoopOpcode { start_loop: false, .. })));
        assert!(matches!(vm.decode_next_opcode().unwrap().opcode, CheatVmOpcodeVariant::StoreStatic(_)));
    }
}

pub const MAXIMUM_PROGRAM_OPCODE_COUNT: usize = 0x400;
pub const NUM_REGISTERS: usize = 0x10;
pub const NUM_READABLE_STATIC_REGISTERS: usize = 0x80;
pub const NUM_WRITABLE_STATIC_REGISTERS: usize = 0x80;
pub const NUM_STATIC_REGISTERS: usize =
    NUM_READABLE_STATIC_REGISTERS + NUM_WRITABLE_STATIC_REGISTERS;

pub struct DmntCheatVm {
    callbacks: Box<dyn VmCallbacks>,

    num_opcodes: usize,
    instruction_ptr: usize,
    condition_depth: usize,
    decode_success: bool,
    program: [u32; MAXIMUM_PROGRAM_OPCODE_COUNT],
    registers: [u64; NUM_REGISTERS],
    saved_values: [u64; NUM_REGISTERS],
    static_registers: [u64; NUM_STATIC_REGISTERS],
    loop_tops: [usize; NUM_REGISTERS],
}

impl DmntCheatVm {
    pub fn new(callbacks: Box<dyn VmCallbacks>) -> Self {
        Self {
            callbacks,
            num_opcodes: 0,
            instruction_ptr: 0,
            condition_depth: 0,
            decode_success: false,
            program: [0u32; MAXIMUM_PROGRAM_OPCODE_COUNT],
            registers: [0u64; NUM_REGISTERS],
            saved_values: [0u64; NUM_REGISTERS],
            static_registers: [0u64; NUM_STATIC_REGISTERS],
            loop_tops: [0usize; NUM_REGISTERS],
        }
    }

    pub fn get_program_size(&self) -> usize {
        self.num_opcodes
    }

    pub fn load_program(&mut self, cheats: &[CheatEntry]) -> bool {
        // Reset state
        self.num_opcodes = 0;
        self.instruction_ptr = 0;
        self.decode_success = false;
        self.program = [0u32; MAXIMUM_PROGRAM_OPCODE_COUNT];

        for cheat in cheats {
            if !cheat.enabled {
                continue;
            }

            for i in 0..(cheat.definition.num_opcodes as usize) {
                if self.num_opcodes >= MAXIMUM_PROGRAM_OPCODE_COUNT {
                    self.num_opcodes = 0;
                    return false;
                }
                self.program[self.num_opcodes] = cheat.definition.opcodes[i];
                self.num_opcodes += 1;
            }
        }

        true
    }

    pub fn execute(&mut self, metadata: &CheatProcessMetadata) {
        self.reset_state();

        // Execute opcodes until we run out
        while self.instruction_ptr < self.num_opcodes {
            let opcode = match self.decode_next_opcode() {
                Some(op) => op,
                None => break,
            };

            // Log the opcode for debug
            self.log_opcode(&opcode);

            // Check if we need to skip conditional blocks
            if self.condition_depth > 0 {
                // Handle else/end
                if let CheatVmOpcodeVariant::EndConditional(ref end) = opcode.opcode {
                    if end.is_else {
                        // Don't consume the else marker at depth > 1
                        if self.condition_depth > 1 {
                            self.condition_depth -= 1;
                            continue;
                        }
                        // Else at depth 1 means continue executing
                        self.condition_depth = 0;
                    } else {
                        self.condition_depth -= 1;
                        continue;
                    }
                } else if opcode.begin_conditional_block {
                    self.condition_depth += 1;
                    continue;
                } else {
                    continue;
                }
            }

            // Execute the opcode
            self.execute_opcode(&opcode, metadata);
        }
    }

    fn reset_state(&mut self) {
        self.registers = [0u64; NUM_REGISTERS];
        self.saved_values = [0u64; NUM_REGISTERS];
        self.loop_tops = [0usize; NUM_REGISTERS];
        self.instruction_ptr = 0;
        self.condition_depth = 0;
    }

    fn get_vm_int(value: VmInt, bit_width: u32) -> u64 {
        match bit_width {
            1 => value.bit8() as u64,
            2 => value.bit16() as u64,
            4 => value.bit32() as u64,
            8 => value.bit64,
            _ => value.bit64,
        }
    }

    fn get_cheat_process_address(
        metadata: &CheatProcessMetadata,
        mem_type: u32,
        rel_address: u64,
    ) -> u64 {
        let base = match mem_type {
            0 => metadata.main_nso_extents.base, // MainNso
            1 => metadata.heap_extents.base,     // Heap
            2 => metadata.alias_extents.base,    // Alias
            3 => metadata.aslr_extents.base,     // Aslr
            _ => 0,
        };
        base + rel_address
    }

    fn debug_log_value(&self, log_id: u32, value: u64) {
        self.callbacks.debug_log(log_id as u8, value);
    }

    fn log_opcode(&self, opcode: &CheatVmOpcode) {
        match &opcode.opcode {
            CheatVmOpcodeVariant::StoreStatic(op) => {
                self.callbacks.command_log("Opcode: Store Static\n");
                self.callbacks
                    .command_log(&format!("Bit Width: {:X}\n", op.bit_width));
                self.callbacks
                    .command_log(&format!("Mem Type:  {:X}\n", op.mem_type));
                self.callbacks
                    .command_log(&format!("Reg Idx:   {:X}\n", op.offset_register));
                self.callbacks
                    .command_log(&format!("Rel Addr:  {:X}\n", op.rel_address));
                self.callbacks
                    .command_log(&format!("Value:     {:X}\n", op.value.bit64));
            }
            CheatVmOpcodeVariant::BeginConditional(op) => {
                self.callbacks.command_log("Opcode: Begin Conditional\n");
                self.callbacks
                    .command_log(&format!("Bit Width: {:X}\n", op.bit_width));
                self.callbacks
                    .command_log(&format!("Mem Type:  {:X}\n", op.mem_type));
                self.callbacks
                    .command_log(&format!("Cond Type: {:X}\n", op.cond_type));
                self.callbacks
                    .command_log(&format!("Rel Addr:  {:X}\n", op.rel_address));
                self.callbacks
                    .command_log(&format!("Value:     {:X}\n", op.value.bit64));
            }
            CheatVmOpcodeVariant::EndConditional(op) => {
                self.callbacks.command_log("Opcode: End Conditional\n");
                self.callbacks
                    .command_log(&format!("Is Else:   {}\n", op.is_else));
            }
            CheatVmOpcodeVariant::ControlLoop(op) => {
                self.callbacks.command_log("Opcode: Control Loop\n");
                self.callbacks
                    .command_log(&format!("Start:     {}\n", op.start_loop));
                self.callbacks
                    .command_log(&format!("Reg Idx:   {:X}\n", op.reg_index));
                self.callbacks
                    .command_log(&format!("Num Iters: {:X}\n", op.num_iters));
            }
            CheatVmOpcodeVariant::LoadRegisterStatic(op) => {
                self.callbacks.command_log("Opcode: Load Register Static\n");
                self.callbacks
                    .command_log(&format!("Reg Idx:   {:X}\n", op.reg_index));
                self.callbacks
                    .command_log(&format!("Value:     {:X}\n", op.value));
            }
            CheatVmOpcodeVariant::LoadRegisterMemory(op) => {
                self.callbacks.command_log("Opcode: Load Register Memory\n");
                self.callbacks
                    .command_log(&format!("Bit Width: {:X}\n", op.bit_width));
                self.callbacks
                    .command_log(&format!("Mem Type:  {:X}\n", op.mem_type));
                self.callbacks
                    .command_log(&format!("Reg Idx:   {:X}\n", op.reg_index));
                self.callbacks
                    .command_log(&format!("From Reg:  {}\n", op.load_from_reg));
                self.callbacks
                    .command_log(&format!("Rel Addr:  {:X}\n", op.rel_address));
            }
            _ => {
                // Other opcodes logged minimally
            }
        }
    }

    fn skip_conditional_block(&mut self, is_if: bool) {
        if is_if {
            self.condition_depth += 1;
        } else {
            // For else, we skip until the matching end
            self.condition_depth += 1;
        }
    }

    fn decode_next_opcode(&mut self) -> Option<CheatVmOpcode> {
        if self.instruction_ptr >= self.num_opcodes {
            return None;
        }

        // Helper to read the next u32 from the program
        let first = self.program[self.instruction_ptr];
        self.instruction_ptr += 1;

        let opcode_type = (first >> 28) & 0xF;
        let mut begin_conditional = false;

        let variant = match opcode_type {
            // StoreStatic (0)
            0 => {
                let bit_width = (first >> 24) & 0xF;
                let mem_type = (first >> 20) & 0xF;
                let offset_register = (first >> 16) & 0xF;
                let rel_address = ((first & 0xFF) as u64) << 32 | self.read_program_u32() as u64;
                let value = self.read_vm_int(bit_width);
                CheatVmOpcodeVariant::StoreStatic(StoreStaticOpcode {
                    bit_width,
                    mem_type,
                    offset_register,
                    rel_address,
                    value,
                })
            }
            // BeginConditionalBlock (1)
            1 => {
                begin_conditional = true;
                let bit_width = (first >> 24) & 0xF;
                let mem_type = (first >> 20) & 0xF;
                let cond_type = (first >> 16) & 0xF;
                let rel_address = ((first & 0xFF) as u64) << 32 | self.read_program_u32() as u64;
                let value = self.read_vm_int(bit_width);
                CheatVmOpcodeVariant::BeginConditional(BeginConditionalOpcode {
                    bit_width,
                    mem_type,
                    cond_type,
                    rel_address,
                    value,
                })
            }
            // EndConditionalBlock (2)
            2 => {
                let is_else = ((first >> 24) & 0xF) == 1;
                CheatVmOpcodeVariant::EndConditional(EndConditionalOpcode { is_else })
            }
            // ControlLoop (3)
            3 => {
                let start_loop = ((first >> 24) & 0xF) == 0;
                let reg_index = (first >> 20) & 0xF;
                let num_iters = if start_loop { self.read_program_u32() } else { 0 };
                CheatVmOpcodeVariant::ControlLoop(ControlLoopOpcode {
                    start_loop,
                    reg_index,
                    num_iters,
                })
            }
            // LoadRegisterStatic (4)
            4 => {
                let reg_index = (first >> 16) & 0xF;
                let value = (self.read_program_u32() as u64) << 32 | self.read_program_u32() as u64;
                CheatVmOpcodeVariant::LoadRegisterStatic(LoadRegisterStaticOpcode {
                    reg_index,
                    value,
                })
            }
            // LoadRegisterMemory (5)
            5 => {
                let bit_width = (first >> 24) & 0xF;
                let mem_type = (first >> 20) & 0xF;
                let reg_index = (first >> 16) & 0xF;
                let load_from_reg = ((first >> 12) & 0xF) != 0;
                let rel_address = ((first & 0xFF) as u64) << 32 | self.read_program_u32() as u64;
                CheatVmOpcodeVariant::LoadRegisterMemory(LoadRegisterMemoryOpcode {
                    bit_width,
                    mem_type,
                    reg_index,
                    load_from_reg,
                    rel_address,
                })
            }
            // StoreStaticToAddress (6)
            6 => {
                let bit_width = (first >> 24) & 0xF;
                let reg_index = (first >> 16) & 0xF;
                let increment_reg = ((first >> 12) & 0xF) != 0;
                let add_offset_reg = ((first >> 8) & 0xF) != 0;
                let offset_reg_index = (first >> 4) & 0xF;
                let value = (self.read_program_u32() as u64) << 32 | self.read_program_u32() as u64;
                CheatVmOpcodeVariant::StoreStaticToAddress(StoreStaticToAddressOpcode {
                    bit_width,
                    reg_index,
                    increment_reg,
                    add_offset_reg,
                    offset_reg_index,
                    value,
                })
            }
            // PerformArithmeticStatic (7)
            7 => {
                let bit_width = (first >> 24) & 0xF;
                let reg_index = (first >> 16) & 0xF;
                let math_type = (first >> 12) & 0xF;
                let value = self.read_program_u32();
                CheatVmOpcodeVariant::PerformArithmeticStatic(PerformArithmeticStaticOpcode {
                    bit_width,
                    reg_index,
                    math_type,
                    value,
                })
            }
            // BeginKeypressConditionalBlock (8)
            8 => {
                begin_conditional = true;
                let key_mask = first & 0x0FFFFFFF;
                CheatVmOpcodeVariant::BeginKeypressConditional(BeginKeypressConditionalOpcode {
                    key_mask,
                })
            }
            // PerformArithmeticRegister (9)
            9 => {
                let bit_width = (first >> 24) & 0xF;
                let math_type = (first >> 20) & 0xF;
                let dst_reg_index = (first >> 16) & 0xF;
                let src_reg_1_index = (first >> 12) & 0xF;
                let has_immediate = ((first >> 8) & 0xF) != 0;
                let src_reg_2_index = if has_immediate { 0 } else { (first >> 4) & 0xF };
                let value = if has_immediate {
                    self.read_vm_int(bit_width)
                } else {
                    VmInt::default()
                };
                CheatVmOpcodeVariant::PerformArithmeticRegister(PerformArithmeticRegisterOpcode {
                    bit_width,
                    math_type,
                    dst_reg_index,
                    src_reg_1_index,
                    src_reg_2_index,
                    has_immediate,
                    value,
                })
            }
            // StoreRegisterToAddress (10)
            10 => {
                let bit_width = (first >> 24) & 0xF;
                let str_reg_index = (first >> 20) & 0xF;
                let addr_reg_index = (first >> 16) & 0xF;
                let increment_reg = ((first >> 12) & 0xF) != 0;
                let ofs_type = (first >> 8) & 0xF;
                let mem_type = (first >> 4) & 0xF;
                let ofs_reg_index = first & 0xF;
                let rel_address = if ofs_type == 2 || ofs_type == 4 || ofs_type == 5 {
                    // Imm, MemImm, MemImmReg
                    let hi = self.read_program_u32() as u64;
                    let lo = self.read_program_u32() as u64;
                    (hi << 32) | lo
                } else {
                    0
                };
                CheatVmOpcodeVariant::StoreRegisterToAddress(StoreRegisterToAddressOpcode {
                    bit_width,
                    str_reg_index,
                    addr_reg_index,
                    increment_reg,
                    ofs_type,
                    mem_type,
                    ofs_reg_index,
                    rel_address,
                })
            }
            // ExtendedWidth (12)
            12 => {
                let sub_type = ((first >> 24) & 0xF) << 4 | ((first >> 20) & 0xF);
                let full_type = 0xC0 | (sub_type & 0x0F);
                match full_type {
                    // BeginRegisterConditionalBlock (0xC0)
                    0xC0 => {
                        begin_conditional = true;
                        let bit_width = 1u32 << ((first >> 16) & 0xF);
                        let cond_type = (first >> 12) & 0xF;
                        let val_reg_index = (first >> 8) & 0xF;
                        let comp_type = (first >> 4) & 0xF;

                        let second = self.read_program_u32();
                        let mem_type = (second >> 28) & 0xF;
                        let addr_reg_index = (second >> 24) & 0xF;
                        let other_reg_index = (second >> 20) & 0xF;
                        let ofs_reg_index = (second >> 16) & 0xF;

                        let (rel_address, value) = match comp_type {
                            0 | 2 => {
                                // MemoryRelAddr, RegisterRelAddr
                                let hi = (second & 0xFFFF) as u64;
                                let lo = self.read_program_u32() as u64;
                                ((hi << 32) | lo, VmInt::default())
                            }
                            4 => {
                                // StaticValue
                                let v = self.read_vm_int(bit_width);
                                (0, v)
                            }
                            _ => (0, VmInt::default()),
                        };

                        CheatVmOpcodeVariant::BeginRegisterConditional(
                            BeginRegisterConditionalOpcode {
                                bit_width,
                                cond_type,
                                val_reg_index,
                                comp_type,
                                mem_type,
                                addr_reg_index,
                                other_reg_index,
                                ofs_reg_index,
                                rel_address,
                                value,
                            },
                        )
                    }
                    // SaveRestoreRegister (0xC1)
                    0xC1 => {
                        let dst_index = (first >> 16) & 0xF;
                        let src_index = (first >> 12) & 0xF;
                        let op_type = (first >> 8) & 0xF;
                        CheatVmOpcodeVariant::SaveRestoreRegister(SaveRestoreRegisterOpcode {
                            dst_index,
                            src_index,
                            op_type,
                        })
                    }
                    // SaveRestoreRegisterMask (0xC2)
                    0xC2 => {
                        let op_type = (first >> 16) & 0xF;
                        let mut should_operate = [false; 0x10];
                        for i in 0..0x10 {
                            should_operate[i] = ((first >> (15 - i)) & 1) != 0;
                        }
                        // Re-read: the mask is in the lower 16 bits
                        let mask = first & 0xFFFF;
                        for i in 0..0x10 {
                            should_operate[i] = ((mask >> (15 - i)) & 1) != 0;
                        }
                        CheatVmOpcodeVariant::SaveRestoreRegisterMask(
                            SaveRestoreRegisterMaskOpcode {
                                op_type,
                                should_operate,
                            },
                        )
                    }
                    // ReadWriteStaticRegister (0xC3)
                    0xC3 => {
                        let static_idx = (first >> 16) & 0xFF;
                        let idx = (first >> 8) & 0xF;
                        CheatVmOpcodeVariant::ReadWriteStaticRegister(
                            ReadWriteStaticRegisterOpcode { static_idx, idx },
                        )
                    }
                    _ => CheatVmOpcodeVariant::Unrecognized(UnrecognizedInstruction {
                        opcode: full_type,
                    }),
                }
            }
            // DoubleExtendedWidth (0xF)
            0xF => {
                let sub_type =
                    ((first >> 24) & 0xF) << 8 | ((first >> 20) & 0xF) << 4 | ((first >> 16) & 0xF);
                let full_type = 0xF00 | (sub_type & 0xFF);
                match full_type {
                    0xFF0 => CheatVmOpcodeVariant::PauseProcess(PauseProcessOpcode),
                    0xFF1 => CheatVmOpcodeVariant::ResumeProcess(ResumeProcessOpcode),
                    0xFFF => {
                        let bit_width = 1u32 << ((first >> 12) & 0xF);
                        let log_id = (first >> 8) & 0xF;
                        let val_type = (first >> 4) & 0xF;

                        let second = self.read_program_u32();
                        let mem_type = (second >> 28) & 0xF;
                        let addr_reg_index = (second >> 24) & 0xF;
                        let val_reg_index = (second >> 20) & 0xF;
                        let ofs_reg_index = (second >> 16) & 0xF;
                        let rel_address = match val_type {
                            0 | 2 => {
                                let hi = (second & 0xFFFF) as u64;
                                let lo = self.read_program_u32() as u64;
                                (hi << 32) | lo
                            }
                            _ => 0,
                        };

                        CheatVmOpcodeVariant::DebugLog(DebugLogOpcode {
                            bit_width,
                            log_id,
                            val_type,
                            mem_type,
                            addr_reg_index,
                            val_reg_index,
                            ofs_reg_index,
                            rel_address,
                        })
                    }
                    _ => CheatVmOpcodeVariant::Unrecognized(UnrecognizedInstruction {
                        opcode: full_type,
                    }),
                }
            }
            _ => CheatVmOpcodeVariant::Unrecognized(UnrecognizedInstruction {
                opcode: opcode_type,
            }),
        };

        Some(CheatVmOpcode {
            begin_conditional_block: begin_conditional,
            opcode: variant,
        })
    }

    fn read_program_u32(&mut self) -> u32 {
        if self.instruction_ptr < self.num_opcodes {
            let val = self.program[self.instruction_ptr];
            self.instruction_ptr += 1;
            val
        } else {
            0
        }
    }

    fn read_vm_int(&mut self, bit_width: u32) -> VmInt {
        match bit_width {
            1 | 2 => {
                let word = self.read_program_u32();
                VmInt { bit64: word as u64 }
            }
            4 => {
                let word = self.read_program_u32();
                VmInt { bit64: word as u64 }
            }
            8 => {
                let hi = self.read_program_u32() as u64;
                let lo = self.read_program_u32() as u64;
                VmInt {
                    bit64: (hi << 32) | lo,
                }
            }
            _ => VmInt { bit64: 0 },
        }
    }

    fn execute_opcode(&mut self, opcode: &CheatVmOpcode, metadata: &CheatProcessMetadata) {
        match &opcode.opcode {
            CheatVmOpcodeVariant::StoreStatic(op) => {
                let address =
                    Self::get_cheat_process_address(metadata, op.mem_type, op.rel_address)
                        + self.registers[op.offset_register as usize];
                let value = Self::get_vm_int(op.value, op.bit_width);
                self.write_value_to_address(address, op.bit_width, value);
            }
            CheatVmOpcodeVariant::BeginConditional(op) => {
                let address =
                    Self::get_cheat_process_address(metadata, op.mem_type, op.rel_address);
                let mem_value = self.read_value_from_address(address, op.bit_width);
                let cond_value = Self::get_vm_int(op.value, op.bit_width);
                if !Self::evaluate_condition(op.cond_type, op.bit_width, mem_value, cond_value) {
                    self.skip_conditional_block(true);
                }
            }
            CheatVmOpcodeVariant::EndConditional(_) => {
                // Handled in the main loop
            }
            CheatVmOpcodeVariant::ControlLoop(op) => {
                if op.start_loop {
                    self.registers[op.reg_index as usize] = op.num_iters as u64;
                    self.loop_tops[op.reg_index as usize] = self.instruction_ptr;
                } else {
                    let reg = &mut self.registers[op.reg_index as usize];
                    *reg = reg.wrapping_sub(1);
                    if *reg != 0 {
                        self.instruction_ptr = self.loop_tops[op.reg_index as usize];
                    }
                }
            }
            CheatVmOpcodeVariant::LoadRegisterStatic(op) => {
                self.registers[op.reg_index as usize] = op.value;
            }
            CheatVmOpcodeVariant::LoadRegisterMemory(op) => {
                let address = if op.load_from_reg {
                    self.registers[op.reg_index as usize].wrapping_add(op.rel_address)
                } else {
                    Self::get_cheat_process_address(metadata, op.mem_type, op.rel_address)
                };
                // Eden reads directly into the register: a narrow read preserves
                // its upper bytes, and invalid widths do not access memory.
                if matches!(op.bit_width, 1 | 2 | 4 | 8) {
                    let mut bytes = self.registers[op.reg_index as usize].to_le_bytes();
                    self.callbacks.memory_read_unsafe(address, &mut bytes[..op.bit_width as usize]);
                    self.registers[op.reg_index as usize] = u64::from_le_bytes(bytes);
                }
            }
            CheatVmOpcodeVariant::StoreStaticToAddress(op) => {
                let mut address = self.registers[op.reg_index as usize];
                if op.add_offset_reg {
                    address = address.wrapping_add(self.registers[op.offset_reg_index as usize]);
                }
                self.write_value_to_address(address, op.bit_width, op.value);
                if op.increment_reg {
                    self.registers[op.reg_index as usize] =
                        self.registers[op.reg_index as usize].wrapping_add(op.bit_width as u64);
                }
            }
            CheatVmOpcodeVariant::PerformArithmeticStatic(op) => {
                let lhs = self.registers[op.reg_index as usize];
                let rhs = op.value as u64;
                self.registers[op.reg_index as usize] =
                    Self::perform_arithmetic(if op.math_type <= 4 { op.math_type } else { 9 }, op.bit_width, lhs, rhs);
            }
            CheatVmOpcodeVariant::BeginKeypressConditional(op) => {
                let keys = self.callbacks.hid_keys_down();
                if (keys & op.key_mask as u64) != op.key_mask as u64 {
                    self.skip_conditional_block(true);
                }
            }
            CheatVmOpcodeVariant::PerformArithmeticRegister(op) => {
                let lhs = self.registers[op.src_reg_1_index as usize];
                let rhs = if op.has_immediate {
                    Self::get_vm_int(op.value, op.bit_width)
                } else {
                    self.registers[op.src_reg_2_index as usize]
                };
                self.registers[op.dst_reg_index as usize] =
                    Self::perform_arithmetic(op.math_type, op.bit_width, lhs, rhs);
            }
            CheatVmOpcodeVariant::StoreRegisterToAddress(op) => {
                let value = self.registers[op.str_reg_index as usize];
                let mut address = self.registers[op.addr_reg_index as usize];

                match op.ofs_type {
                    0 => {} // None
                    1 => {
                        // Reg
                        address = address.wrapping_add(self.registers[op.ofs_reg_index as usize]);
                    }
                    2 => {
                        // Imm
                        address = address.wrapping_add(op.rel_address);
                    }
                    3 => {
                        // MemReg
                        let base = Self::get_cheat_process_address(metadata, op.mem_type, 0);
                        address = base.wrapping_add(self.registers[op.ofs_reg_index as usize]);
                    }
                    4 => {
                        // MemImm
                        let base =
                            Self::get_cheat_process_address(metadata, op.mem_type, op.rel_address);
                        address = base;
                    }
                    5 => {
                        // MemImmReg
                        let base =
                            Self::get_cheat_process_address(metadata, op.mem_type, op.rel_address);
                        address = base.wrapping_add(self.registers[op.ofs_reg_index as usize]);
                    }
                    _ => {}
                }

                self.write_value_to_address(address, op.bit_width, value);

                if op.increment_reg {
                    self.registers[op.addr_reg_index as usize] = self.registers
                        [op.addr_reg_index as usize]
                        .wrapping_add(op.bit_width as u64);
                }
            }
            CheatVmOpcodeVariant::BeginRegisterConditional(op) => {
                let lhs = self.registers[op.val_reg_index as usize];
                let rhs = match op.comp_type {
                    0 => {
                        // MemoryRelAddr
                        let addr =
                            Self::get_cheat_process_address(metadata, op.mem_type, op.rel_address);
                        self.read_value_from_address(addr, op.bit_width)
                    }
                    1 => {
                        // MemoryOfsReg
                        let base = Self::get_cheat_process_address(metadata, op.mem_type, 0);
                        let addr = base.wrapping_add(self.registers[op.ofs_reg_index as usize]);
                        self.read_value_from_address(addr, op.bit_width)
                    }
                    2 => {
                        // RegisterRelAddr
                        let addr =
                            self.registers[op.addr_reg_index as usize].wrapping_add(op.rel_address);
                        self.read_value_from_address(addr, op.bit_width)
                    }
                    3 => {
                        // RegisterOfsReg
                        let addr = self.registers[op.addr_reg_index as usize]
                            .wrapping_add(self.registers[op.ofs_reg_index as usize]);
                        self.read_value_from_address(addr, op.bit_width)
                    }
                    4 => {
                        // StaticValue
                        Self::get_vm_int(op.value, op.bit_width)
                    }
                    5 => {
                        // OtherRegister
                        self.registers[op.other_reg_index as usize]
                    }
                    _ => 0,
                };
                if !Self::evaluate_condition(op.cond_type, op.bit_width, lhs, rhs) {
                    self.skip_conditional_block(true);
                }
            }
            CheatVmOpcodeVariant::SaveRestoreRegister(op) => {
                match op.op_type {
                    0 => {
                        // Restore
                        self.registers[op.dst_index as usize] =
                            self.saved_values[op.src_index as usize];
                    }
                    1 => {
                        // Save
                        self.saved_values[op.dst_index as usize] =
                            self.registers[op.src_index as usize];
                    }
                    2 => {
                        // ClearSaved
                        self.saved_values[op.dst_index as usize] = 0;
                    }
                    3 => {
                        // ClearRegs
                        self.registers[op.dst_index as usize] = 0;
                    }
                    _ => {}
                }
            }
            CheatVmOpcodeVariant::SaveRestoreRegisterMask(op) => {
                for i in 0..NUM_REGISTERS {
                    if op.should_operate[i] {
                        match op.op_type {
                            0 => {
                                // Restore
                                self.registers[i] = self.saved_values[i];
                            }
                            1 => {
                                // Save
                                self.saved_values[i] = self.registers[i];
                            }
                            2 => {
                                // ClearSaved
                                self.saved_values[i] = 0;
                            }
                            3 => {
                                // ClearRegs
                                self.registers[i] = 0;
                            }
                            _ => {}
                        }
                    }
                }
            }
            CheatVmOpcodeVariant::ReadWriteStaticRegister(op) => {
                let static_idx = op.static_idx as usize;
                let idx = op.idx as usize;
                if static_idx < NUM_READABLE_STATIC_REGISTERS {
                    // Read from static register
                    if idx < NUM_REGISTERS {
                        self.registers[idx] = self.static_registers[static_idx];
                    }
                } else if static_idx < NUM_STATIC_REGISTERS {
                    // Write to static register
                    if idx < NUM_REGISTERS {
                        self.static_registers[static_idx] = self.registers[idx];
                    }
                }
            }
            CheatVmOpcodeVariant::PauseProcess(_) => {
                self.callbacks.pause_process();
            }
            CheatVmOpcodeVariant::ResumeProcess(_) => {
                self.callbacks.resume_process();
            }
            CheatVmOpcodeVariant::DebugLog(op) => {
                let value = match op.val_type {
                    0 => {
                        // MemoryRelAddr
                        let addr =
                            Self::get_cheat_process_address(metadata, op.mem_type, op.rel_address);
                        self.read_value_from_address(addr, op.bit_width)
                    }
                    1 => {
                        // MemoryOfsReg
                        let base = Self::get_cheat_process_address(metadata, op.mem_type, 0);
                        let addr = base.wrapping_add(self.registers[op.ofs_reg_index as usize]);
                        self.read_value_from_address(addr, op.bit_width)
                    }
                    2 => {
                        // RegisterRelAddr
                        let addr =
                            self.registers[op.addr_reg_index as usize].wrapping_add(op.rel_address);
                        self.read_value_from_address(addr, op.bit_width)
                    }
                    3 => {
                        // RegisterOfsReg
                        let addr = self.registers[op.addr_reg_index as usize]
                            .wrapping_add(self.registers[op.ofs_reg_index as usize]);
                        self.read_value_from_address(addr, op.bit_width)
                    }
                    4 => {
                        // RegisterValue
                        self.registers[op.val_reg_index as usize]
                    }
                    _ => 0,
                };
                self.debug_log_value(op.log_id, value);
            }
            CheatVmOpcodeVariant::Unrecognized(_) => {
                // Skip unrecognized instructions
            }
        }
    }

    fn read_value_from_address(&self, address: u64, bit_width: u32) -> u64 {
        let mut buf = [0u8; 8];
        let size = bit_width.min(8) as usize;
        self.callbacks.memory_read_unsafe(address, &mut buf[..size]);
        match size {
            1 => buf[0] as u64,
            2 => u16::from_le_bytes([buf[0], buf[1]]) as u64,
            4 => u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as u64,
            8 => u64::from_le_bytes(buf),
            _ => 0,
        }
    }

    fn write_value_to_address(&self, address: u64, bit_width: u32, value: u64) {
        match bit_width {
            1 => {
                let bytes = (value as u8).to_le_bytes();
                self.callbacks.memory_write_unsafe(address, &bytes);
            }
            2 => {
                let bytes = (value as u16).to_le_bytes();
                self.callbacks.memory_write_unsafe(address, &bytes);
            }
            4 => {
                let bytes = (value as u32).to_le_bytes();
                self.callbacks.memory_write_unsafe(address, &bytes);
            }
            8 => {
                let bytes = value.to_le_bytes();
                self.callbacks.memory_write_unsafe(address, &bytes);
            }
            _ => {}
        }
    }

    fn evaluate_condition(cond_type: u32, bit_width: u32, lhs: u64, rhs: u64) -> bool {
        // Mask to appropriate width
        let mask: u64 = match bit_width {
            1 => 0xFF,
            2 => 0xFFFF,
            4 => 0xFFFF_FFFF,
            _ => u64::MAX,
        };
        let lhs = lhs & mask;
        let rhs = rhs & mask;

        match cond_type {
            1 => lhs > rhs,  // GT
            2 => lhs >= rhs, // GE
            3 => lhs < rhs,  // LT
            4 => lhs <= rhs, // LE
            5 => lhs == rhs, // EQ
            6 => lhs != rhs, // NE
            _ => false,
        }
    }

    fn perform_arithmetic(math_type: u32, bit_width: u32, lhs: u64, rhs: u64) -> u64 {
        let mask: u64 = match bit_width {
            1 => 0xFF,
            2 => 0xFFFF,
            4 => 0xFFFF_FFFF,
            _ => u64::MAX,
        };

        let result = match math_type {
            0 => lhs.wrapping_add(rhs), // Addition
            1 => lhs.wrapping_sub(rhs), // Subtraction
            2 => lhs.wrapping_mul(rhs), // Multiplication
            3 => lhs << (rhs & 0x3F),   // LeftShift
            4 => lhs >> (rhs & 0x3F),   // RightShift
            5 => lhs & rhs,             // LogicalAnd
            6 => lhs | rhs,             // LogicalOr
            7 => !lhs,                  // LogicalNot
            8 => lhs ^ rhs,             // LogicalXor
            9 => lhs,                   // None
            _ => lhs,
        };

        result & mask
    }
}

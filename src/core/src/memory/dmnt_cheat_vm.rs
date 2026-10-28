// SPDX-FileCopyrightText: Copyright 2019 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of Eden's src/core/memory/dmnt_cheat_vm.h and dmnt_cheat_vm.cpp.
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
    struct Memory(Arc<Mutex<BTreeMap<u64, u8>>>, Arc<Mutex<Trace>>);

    #[derive(Default)]
    struct Trace {
        reads: Vec<(u64, usize)>,
        writes: Vec<(u64, Vec<u8>)>,
        keys: u64,
        key_samples: usize,
        events: Vec<&'static str>,
        debug: Vec<(u8, u64)>,
        commands: Vec<String>,
    }

    impl VmCallbacks for Memory {
        fn memory_read_unsafe(&self, address: u64, data: &mut [u8]) {
            self.1.lock().unwrap().reads.push((address, data.len()));
            let bytes = self.0.lock().unwrap();
            for (offset, byte) in data.iter_mut().enumerate() {
                *byte = *bytes.get(&(address + offset as u64)).unwrap_or(&0);
            }
        }
        fn memory_write_unsafe(&self, address: u64, data: &[u8]) {
            self.1.lock().unwrap().writes.push((address, data.to_vec()));
            let mut bytes = self.0.lock().unwrap();
            for (offset, byte) in data.iter().enumerate() {
                bytes.insert(address + offset as u64, *byte);
            }
        }
        fn hid_keys_down(&self) -> u64 {
            let mut trace = self.1.lock().unwrap();
            trace.key_samples += 1;
            let keys = trace.keys;
            trace.keys = 0; // GetAndResetPressState semantics.
            keys
        }
        fn pause_process(&self) {
            self.1.lock().unwrap().events.push("pause");
        }
        fn resume_process(&self) {
            self.1.lock().unwrap().events.push("resume");
        }
        fn debug_log(&self, id: u8, value: u64) {
            self.1.lock().unwrap().debug.push((id, value));
        }
        fn command_log(&self, text: &str) {
            self.1.lock().unwrap().commands.push(text.to_string());
        }
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
            (0x10030, 0x20000),
            (0x20008, 0x30000),
            (0x30030, 0x40000),
            (0x40168, 0x50000),
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
            let immediate = if width == 8 {
                "11223344 55667788"
            } else {
                "55667788"
            };
            run(
                &memory,
                &format!(
                    "[writes]\n{:08X} 00000020 {immediate}\n01000000 00000040 000000AB",
                    width << 24
                ),
                0x1000,
            );
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
        let vm = run(
            &memory,
            "[read]\n400F0000 11223344 55667788\n510F0000 00000020",
            0x1000,
        );
        assert_eq!(vm.registers[15], 0x1122334455667799);
    }

    #[test]
    fn static_arithmetic_decodes_operation_and_rejects_extensions() {
        let memory = Memory::default();
        let vm = run(
            &memory,
            "[math]\n400F0000 00000000 00000100\n740F1000 00000003\n740F5000 00000000",
            0,
        );
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
        vm.load_program(
            &TextCheatParser
                .parse("[loop]\n30000000 00000002\n31000000\n01000000 00000020 00000063"),
        );
        vm.reset_state();
        let start = vm.decode_next_opcode().unwrap();
        assert!(!start.begin_conditional_block);
        let end = vm.decode_next_opcode().unwrap();
        assert!(!end.begin_conditional_block);
        assert!(matches!(
            end.opcode,
            CheatVmOpcodeVariant::ControlLoop(ControlLoopOpcode {
                start_loop: false,
                ..
            })
        ));
        assert!(matches!(
            vm.decode_next_opcode().unwrap().opcode,
            CheatVmOpcodeVariant::StoreStatic(_)
        ));
    }

    fn words(memory: &Memory, program: &[u32], metadata: &CheatProcessMetadata) -> DmntCheatVm {
        let mut cheat = CheatEntry::default();
        cheat.enabled = true;
        cheat.definition.num_opcodes = program.len() as u32;
        cheat.definition.opcodes[..program.len()].copy_from_slice(program);
        let mut vm = DmntCheatVm::new(Box::new(memory.clone()));
        assert!(vm.load_program(&[cheat]));
        vm.execute(metadata);
        vm
    }

    #[test]
    fn nested_conditions_decode_immediates_and_select_exactly_one_branch() {
        for outer in [false, true] {
            for inner in [false, true] {
                let memory = Memory::default();
                memory.memory_write_unsafe(0x10, &[u8::from(outer)]);
                memory.memory_write_unsafe(0x11, &[u8::from(inner)]);
                let vm = run(&memory, "[nested]\n11050000 00000010 00000001\n11050000 00000011 00000001\n04000000 00000020 20000000\n21000000\n04000000 00000020 80000001\n20000000\n21000000\n04000000 00000020 21000000\n20000000\n01000000 00000030 000000AA", 0);
                let expected: u32 = if !outer {
                    0x21000000
                } else if inner {
                    0x20000000
                } else {
                    0x80000001
                };
                let mut value = [0; 4];
                memory.memory_read_unsafe(0x20, &mut value);
                assert_eq!(u32::from_le_bytes(value), expected);
                assert_eq!(memory.0.lock().unwrap()[&0x30], 0xaa);
                assert_eq!(vm.condition_depth, 0);
            }
        }
    }

    #[test]
    fn keypress_is_sampled_once_for_all_conditions_and_again_next_tick() {
        let memory = Memory::default();
        memory.1.lock().unwrap().keys = 3;
        let mut vm = run(
            &memory,
            "[keys]\n80000001\nFF000000\n20000000\n80000003\nFF100000\n20000000",
            0,
        );
        assert_eq!(memory.1.lock().unwrap().events, ["pause", "resume"]);
        assert_eq!(memory.1.lock().unwrap().key_samples, 1);
        vm.execute(&CheatProcessMetadata::default());
        assert_eq!(memory.1.lock().unwrap().events, ["pause", "resume"]);
        assert_eq!(memory.1.lock().unwrap().key_samples, 2);
    }

    #[test]
    fn counted_loop_executes_body_and_resumes_after_end() {
        let memory = Memory::default();
        // Eden uses bits 20..23 for the loop counter (including its encoding quirk).
        let vm = run(&memory, "[loop]\n400F0000 00000000 00000020\n30200000 00000003\n610F1000 00000000 000000AA\n31200000\n01000000 00000023 000000BB", 0);
        assert_eq!(vm.registers[2], 0);
        assert_eq!(vm.registers[15], 0x23);
        assert_eq!(
            *memory.0.lock().unwrap(),
            BTreeMap::from([(0x20, 0xaa), (0x21, 0xaa), (0x22, 0xaa), (0x23, 0xbb)])
        );
    }

    #[test]
    fn all_register_store_address_forms_and_widths() {
        let mut metadata = CheatProcessMetadata::default();
        metadata.heap_extents.base = 0x1000;
        for width in [1u32, 2, 4, 8] {
            for form in 0u32..=6 {
                let memory = Memory::default();
                let mut code = vec![
                    0x40010000, 0x11223344, 0x55667788, 0x40020000, 0, 0x20, 0x40030000, 0, 0x30,
                ];
                let low = if form == 1 {
                    0x30
                } else if form >= 3 {
                    0x10
                } else {
                    0
                };
                code.push(0xa0121000 | (width << 24) | (form << 8) | low);
                if matches!(form, 2 | 4 | 5) {
                    code.push(0x40);
                }
                let vm = words(&memory, &code, &metadata);
                let address = match form {
                    0 | 6 => 0x20,
                    1 => 0x50,
                    2 => 0x60,
                    3 => 0x1020,
                    4 => 0x1040,
                    5 => 0x1060,
                    _ => unreachable!(),
                };
                let trace = memory.1.lock().unwrap();
                assert_eq!(
                    trace.writes,
                    vec![(
                        address,
                        0x1122334455667788u64.to_le_bytes()[..width as usize].to_vec()
                    )],
                    "form {form}, width {width}"
                );
                assert_eq!(vm.registers[2], 0x20 + width as u64);
            }
        }
    }

    #[test]
    fn all_register_condition_operand_forms_and_comparisons() {
        for width in [1u32, 2, 4, 8] {
            for form in 0u32..=5 {
                for comparison in 1u32..=6 {
                    let memory = Memory::default();
                    let value = 0x0102030405060708u64;
                    memory.memory_write_unsafe(0x40, &value.to_le_bytes());
                    let mut code = vec![
                        0x40010000, 0x01020304, 0x05060708, 0x40020000, 0, 0x40, 0x40030000, 0, 0,
                        0x40040000, 0x01020304, 0x05060708,
                    ];
                    let low = match form {
                        0 | 4 => 0,
                        1 | 2 => 0x02,
                        3 => 0x23,
                        5 => 0x40,
                        _ => unreachable!(),
                    };
                    // Register-relative form stores the address-register nibble at bits 4..7.
                    let low = if form == 2 { 0x20 } else { low };
                    code.push(0xc0001000 | (width << 20) | (comparison << 16) | (form << 8) | low);
                    if form == 0 {
                        code.push(0x40);
                    }
                    if form == 2 {
                        code.push(0);
                    }
                    if form == 4 {
                        if width == 8 {
                            code.push(0x01020304);
                        }
                        code.push(0x05060708);
                    }
                    code.extend([
                        0x01000000, 0x80, 1, 0x21000000, 0x01000000, 0x80, 2, 0x20000000,
                    ]);
                    words(&memory, &code, &CheatProcessMetadata::default());
                    let expected = if matches!(comparison, 2 | 4 | 5) {
                        1
                    } else {
                        2
                    };
                    assert_eq!(
                        memory.0.lock().unwrap()[&0x80],
                        expected,
                        "form {form}, width {width}, comparison {comparison}"
                    );
                }
            }
        }
    }

    #[test]
    fn save_restore_single_and_masks_clear_and_restore_correct_slots() {
        let memory = Memory::default();
        let vm = run(&memory, "[save]\n40010000 00000000 00000011\n40020000 00000000 00000022\nC1030110\nC1040210\nC1050300\nC1060400\nC2000060\nC2100006\nC2300006\nC2000006\nC1030020\nC1070300\nC10804F0", 0);
        assert_eq!(vm.registers[1], 0x11);
        assert_eq!(vm.registers[2], 0x22);
        assert_eq!(vm.registers[5], 0);
        assert_eq!(vm.registers[6], 0);
        assert_eq!(vm.registers[7], 0);
        assert_eq!(vm.registers[8], 0x22); // invalid operation defaults to restore, as Eden.
        assert_eq!(vm.saved_values[1], 0x11);
        assert_eq!(vm.saved_values[2], 0x22);
    }

    #[test]
    fn static_registers_persist_but_working_and_saved_registers_reset() {
        let memory = Memory::default();
        let mut vm = run(
            &memory,
            "[static]\n400F0000 00000000 12345678\nC300080F\nC1010F10",
            0,
        );
        assert_eq!(vm.static_registers[0x80], 0x12345678);
        vm.static_registers[0x7f] = 0xabcdef;
        vm.load_program(&TextCheatParser.parse("[read]\nC30007FE\nC1020100"));
        vm.execute(&CheatProcessMetadata::default());
        assert_eq!(vm.static_registers[0x80], 0x12345678);
        assert_eq!(vm.registers[14], 0xabcdef);
        assert_eq!(vm.registers[15], 0);
        assert_eq!(vm.registers[2], 0);
    }

    #[test]
    fn debug_log_all_operand_forms_and_widths() {
        for width in [1u32, 2, 4, 8] {
            for form in 0u32..=4 {
                let memory = Memory::default();
                memory.memory_write_unsafe(0x40, &0x1122334455667788u64.to_le_bytes());
                let mut code = vec![
                    0x40010000, 0x11223344, 0x55667788, 0x40020000, 0, 0x40, 0x40030000, 0, 0,
                ];
                let low = match form {
                    0 => 0,
                    1 => 2,
                    2 => 0x20,
                    3 => 0x23,
                    4 => 0x10,
                    _ => unreachable!(),
                };
                code.push(0xfff00000 | (width << 16) | (0xa << 12) | (form << 8) | low);
                if form == 0 {
                    code.push(0x40);
                }
                if form == 2 {
                    code.push(0);
                }
                words(&memory, &code, &CheatProcessMetadata::default());
                let expected = match width {
                    1 => 0x88,
                    2 => 0x7788,
                    4 => 0x55667788,
                    8 => 0x1122334455667788,
                    _ => unreachable!(),
                };
                assert_eq!(
                    memory.1.lock().unwrap().debug,
                    vec![(10, expected)],
                    "form {form}, width {width}"
                );
            }
        }
    }

    #[test]
    fn truncated_and_unknown_instructions_stop_before_side_effects() {
        let instructions: &[&[u32]] = &[
            &[0x08000000, 0x40, 1, 2],
            &[0x18050000, 0x40, 1, 2],
            &[0x30000000, 3],
            &[0x40010000, 1, 2],
            &[0x58010000, 0x40],
            &[0x68010000, 1, 2],
            &[0x78010000, 1],
            &[0x98012100, 1, 2],
            &[0xa8120200, 0x40],
            &[0xc0851400, 1, 2],
            &[0xc0851000, 0x40],
            &[0xfff80000, 0x40],
        ];
        for instruction in instructions {
            for length in 1..instruction.len() {
                let memory = Memory::default();
                let mut vm = words(
                    &memory,
                    &instruction[..length],
                    &CheatProcessMetadata::default(),
                );
                assert!(!vm.decode_success);
                assert!(vm.decode_next_opcode().is_none());
                let trace = memory.1.lock().unwrap();
                assert!(
                    trace.reads.is_empty()
                        && trace.writes.is_empty()
                        && trace.events.is_empty()
                        && trace.debug.is_empty()
                );
            }
        }
        for unknown in [0xb0000000, 0xc4000000, 0xd0000000, 0xff200000] {
            let memory = Memory::default();
            words(
                &memory,
                &[unknown, 0x01000000, 0x40, 1],
                &CheatProcessMetadata::default(),
            );
            assert!(memory.0.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn register_arithmetic_all_operations_widths_and_unsigned_overflow() {
        for width in [1u32, 2, 4, 8] {
            for (lhs, rhs) in [(0xfedcba9876543210u64, 3u64), (u64::MAX, 1), (0, 1)] {
                let expected = [
                    lhs.wrapping_add(rhs),
                    lhs.wrapping_sub(rhs),
                    lhs.wrapping_mul(rhs),
                    lhs << rhs,
                    lhs >> rhs,
                    lhs & rhs,
                    lhs | rhs,
                    !lhs,
                    lhs ^ rhs,
                    lhs,
                    0,
                ];
                let mask = match width {
                    1 => 0xff,
                    2 => 0xffff,
                    4 => 0xffffffff,
                    _ => u64::MAX,
                };
                for (operation, result) in expected.iter().enumerate() {
                    let vm = words(
                        &Memory::default(),
                        &[
                            0x40010000,
                            (lhs >> 32) as u32,
                            lhs as u32,
                            0x40020000,
                            0,
                            rhs as u32,
                            0x90031020 | (width << 24) | ((operation as u32) << 20),
                        ],
                        &CheatProcessMetadata::default(),
                    );
                    assert_eq!(
                        vm.registers[3],
                        result & mask,
                        "operation {operation}, width {width}"
                    );
                }
            }
        }
    }

    #[test]
    fn invalid_width_does_not_access_memory_and_logs_zero() {
        let memory = Memory::default();
        let vm = words(
            &memory,
            &[
                0x40010000, 0x11223344, 0x55667788, 0x53010000,
                0x40, // invalid-width load preserves register
                0x63011000, 0, 1,          // invalid-width store still increments address register
                0xfff30410, // invalid-width register debug logs zero
                0xfff30000, 0x40, // invalid-width memory debug logs zero, no read
            ],
            &CheatProcessMetadata::default(),
        );
        assert_eq!(vm.registers[1], 0x112233445566778b);
        let trace = memory.1.lock().unwrap();
        assert!(trace.reads.is_empty() && trace.writes.is_empty());
        assert_eq!(trace.debug, [(0, 0), (0, 0)]);
    }

    #[test]
    fn mask_clear_saved_and_unknown_restore_preserve_unselected_slots() {
        let vm = run(&Memory::default(), "[mask]\n40000000 00000000 00000011\n400F0000 00000000 00000022\nC2108001\nC2200001\nC2308001\nC2F08001", 0);
        assert_eq!(vm.saved_values[0], 0);
        assert_eq!(vm.saved_values[15], 0x22);
        assert_eq!(vm.registers[0], 0);
        assert_eq!(vm.registers[15], 0x22);
    }

    #[test]
    fn program_capacity_disabled_entries_and_decode_failure_recovery() {
        let memory = Memory::default();
        let mut vm = DmntCheatVm::new(Box::new(memory.clone()));
        let mut entry = CheatEntry::default();
        entry.enabled = true;
        entry.definition.num_opcodes = 256;
        entry.definition.opcodes.fill(0x20000000); // unmatched end is a no-op in Eden
        assert!(vm.load_program(&vec![entry.clone(); 4]));
        assert_eq!(vm.get_program_size(), 1024);
        assert!(!vm.load_program(&vec![entry.clone(); 5]));
        assert_eq!(vm.get_program_size(), 0);
        entry.enabled = false;
        assert!(vm.load_program(&vec![entry; 5]));
        assert_eq!(vm.get_program_size(), 0);
        vm.load_program(&TextCheatParser.parse("[bad]\nB0000000"));
        vm.execute(&CheatProcessMetadata::default());
        vm.load_program(&TextCheatParser.parse("[good]\n01000000 00000040 00000063"));
        vm.execute(&CheatProcessMetadata::default());
        assert_eq!(memory.0.lock().unwrap()[&0x40], 99);
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
        self.num_opcodes = 0;
        for cheat in cheats.iter().filter(|entry| entry.enabled) {
            let count = cheat.definition.num_opcodes as usize;
            // Reject malformed public CheatEntry values instead of indexing out of bounds.
            if count > cheat.definition.opcodes.len()
                || count + self.num_opcodes > MAXIMUM_PROGRAM_OPCODE_COUNT
            {
                self.num_opcodes = 0;
                return false;
            }
            self.program[self.num_opcodes..self.num_opcodes + count]
                .copy_from_slice(&cheat.definition.opcodes[..count]);
            self.num_opcodes += count;
        }
        true
    }

    pub fn execute(&mut self, metadata: &CheatProcessMetadata) {
        let keys_down = self.callbacks.hid_keys_down();
        self.callbacks.command_log("Started VM execution.");
        self.callbacks.command_log(&format!(
            "Main NSO:  {:012X}",
            metadata.main_nso_extents.base
        ));
        // Preserve upstream's diagnostic text, including its use of main for Heap.
        self.callbacks.command_log(&format!(
            "Heap:      {:012X}",
            metadata.main_nso_extents.base
        ));
        self.callbacks
            .command_log(&format!("Keys Down: {:08X}", keys_down & 0x0fffffff));
        self.reset_state();
        while let Some(opcode) = self.decode_next_opcode() {
            self.callbacks
                .command_log(&format!("Instruction Ptr: {:04X}", self.instruction_ptr));
            for (i, value) in self.registers.iter().enumerate() {
                self.callbacks
                    .command_log(&format!("Registers[{:02X}]: {:016X}", i, value));
            }
            for (i, value) in self.saved_values.iter().enumerate() {
                self.callbacks
                    .command_log(&format!("SavedRegs[{:02X}]: {:016X}", i, value));
            }
            self.log_opcode(&opcode);
            if opcode.begin_conditional_block {
                self.condition_depth += 1;
            }
            self.execute_opcode(&opcode, metadata, keys_down);
        }
    }

    fn reset_state(&mut self) {
        self.registers = [0u64; NUM_REGISTERS];
        self.saved_values = [0u64; NUM_REGISTERS];
        self.loop_tops = [0usize; NUM_REGISTERS];
        self.instruction_ptr = 0;
        self.condition_depth = 0;
        self.decode_success = true;
    }

    fn get_vm_int(value: VmInt, bit_width: u32) -> u64 {
        match bit_width {
            1 => value.bit8() as u64,
            2 => value.bit16() as u64,
            4 => value.bit32() as u64,
            8 => value.bit64,
            _ => 0,
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
            _ => metadata.main_nso_extents.base,
        };
        base.wrapping_add(rel_address)
    }

    fn debug_log(&self, log_id: u32, value: u64) {
        self.callbacks.debug_log(log_id as u8, value);
    }

    fn log_opcode(&self, opcode: &CheatVmOpcode) {
        // Mechanical equivalent of Eden's formatted CommandLog calls.
        macro_rules! log { ($($arg:tt)*) => { self.callbacks.command_log(&format!($($arg)*)) }; }
        match &opcode.opcode {
            CheatVmOpcodeVariant::StoreStatic(op) => {
                log!("Opcode: Store Static");
                log!("Bit Width: {:X}", op.bit_width);
                log!("Mem Type:  {:X}", op.mem_type);
                log!("Reg Idx:   {:X}", op.offset_register);
                log!("Rel Addr:  {:X}", op.rel_address);
                log!("Value:     {:X}", op.value.bit64);
            }
            CheatVmOpcodeVariant::BeginConditional(op) => {
                log!("Opcode: Begin Conditional");
                log!("Bit Width: {:X}", op.bit_width);
                log!("Mem Type:  {:X}", op.mem_type);
                log!("Cond Type: {:X}", op.cond_type);
                log!("Rel Addr:  {:X}", op.rel_address);
                log!("Value:     {:X}", op.value.bit64);
            }
            CheatVmOpcodeVariant::EndConditional(_) => log!("Opcode: End Conditional"),
            CheatVmOpcodeVariant::ControlLoop(op) => {
                if op.start_loop {
                    log!("Opcode: Start Loop");
                    log!("Reg Idx:   {:X}", op.reg_index);
                    log!("Num Iters: {:X}", op.num_iters);
                } else {
                    log!("Opcode: End Loop");
                    log!("Reg Idx:   {:X}", op.reg_index);
                }
            }
            CheatVmOpcodeVariant::LoadRegisterStatic(op) => {
                log!("Opcode: Load Register Static");
                log!("Reg Idx:   {:X}", op.reg_index);
                log!("Value:     {:X}", op.value);
            }
            CheatVmOpcodeVariant::LoadRegisterMemory(op) => {
                log!("Opcode: Load Register Memory");
                log!("Bit Width: {:X}", op.bit_width);
                log!("Reg Idx:   {:X}", op.reg_index);
                log!("Mem Type:  {:X}", op.mem_type);
                log!("From Reg:  {}", u8::from(op.load_from_reg));
                log!("Rel Addr:  {:X}", op.rel_address);
            }
            CheatVmOpcodeVariant::StoreStaticToAddress(op) => {
                log!("Opcode: Store Static to Address");
                log!("Bit Width: {:X}", op.bit_width);
                log!("Reg Idx:   {:X}", op.reg_index);
                if op.add_offset_reg {
                    log!("O Reg Idx: {:X}", op.offset_reg_index);
                }
                log!("Incr Reg:  {}", u8::from(op.increment_reg));
                log!("Value:     {:X}", op.value);
            }
            CheatVmOpcodeVariant::PerformArithmeticStatic(op) => {
                log!("Opcode: Perform Static Arithmetic");
                log!("Bit Width: {:X}", op.bit_width);
                log!("Reg Idx:   {:X}", op.reg_index);
                log!("Math Type: {:X}", op.math_type);
                log!("Value:     {:X}", op.value);
            }
            CheatVmOpcodeVariant::BeginKeypressConditional(op) => {
                log!("Opcode: Begin Keypress Conditional");
                log!("Key Mask:  {:X}", op.key_mask);
            }
            CheatVmOpcodeVariant::PerformArithmeticRegister(op) => {
                log!("Opcode: Perform Register Arithmetic");
                log!("Bit Width: {:X}", op.bit_width);
                log!("Dst Idx:   {:X}", op.dst_reg_index);
                log!("Src1 Idx:  {:X}", op.src_reg_1_index);
                if op.has_immediate {
                    log!("Value:     {:X}", op.value.bit64);
                } else {
                    log!("Src2 Idx:  {:X}", op.src_reg_2_index);
                }
            }
            CheatVmOpcodeVariant::StoreRegisterToAddress(op) => {
                log!("Opcode: Store Register to Address");
                log!("Bit Width: {:X}", op.bit_width);
                log!("S Reg Idx: {:X}", op.str_reg_index);
                log!("A Reg Idx: {:X}", op.addr_reg_index);
                log!("Incr Reg:  {}", u8::from(op.increment_reg));
                match op.ofs_type {
                    1 => log!("O Reg Idx: {:X}", op.ofs_reg_index),
                    2 => log!("Rel Addr:  {:X}", op.rel_address),
                    3 => log!("Mem Type:  {:X}", op.mem_type),
                    4 | 5 => {
                        log!("Mem Type:  {:X}", op.mem_type);
                        log!("Rel Addr:  {:X}", op.rel_address);
                    }
                    _ => {}
                }
            }
            CheatVmOpcodeVariant::BeginRegisterConditional(op) => {
                log!("Opcode: Begin Register Conditional");
                log!("Bit Width: {:X}", op.bit_width);
                log!("Cond Type: {:X}", op.cond_type);
                log!("V Reg Idx: {:X}", op.val_reg_index);
                match op.comp_type {
                    4 => {
                        log!("Comp Type: Static Value");
                        log!("Value:     {:X}", op.value.bit64);
                    }
                    5 => {
                        log!("Comp Type: Other Register");
                        log!("X Reg Idx: {:X}", op.other_reg_index);
                    }
                    0 => {
                        log!("Comp Type: Memory Relative Address");
                        log!("Mem Type:  {:X}", op.mem_type);
                        log!("Rel Addr:  {:X}", op.rel_address);
                    }
                    1 => {
                        log!("Comp Type: Memory Offset Register");
                        log!("Mem Type:  {:X}", op.mem_type);
                        log!("O Reg Idx: {:X}", op.ofs_reg_index);
                    }
                    2 => {
                        log!("Comp Type: Register Relative Address");
                        log!("A Reg Idx: {:X}", op.addr_reg_index);
                        log!("Rel Addr:  {:X}", op.rel_address);
                    }
                    3 => {
                        log!("Comp Type: Register Offset Register");
                        log!("A Reg Idx: {:X}", op.addr_reg_index);
                        log!("O Reg Idx: {:X}", op.ofs_reg_index);
                    }
                    _ => {}
                }
            }
            CheatVmOpcodeVariant::SaveRestoreRegister(op) => {
                log!("Opcode: Save or Restore Register");
                log!("Dst Idx:   {:X}", op.dst_index);
                log!("Src Idx:   {:X}", op.src_index);
                log!("Op Type:   {}", op.op_type);
            }
            CheatVmOpcodeVariant::SaveRestoreRegisterMask(op) => {
                log!("Opcode: Save or Restore Register Mask");
                log!("Op Type:   {}", op.op_type);
                for (i, active) in op.should_operate.iter().enumerate() {
                    log!("Act[{:02X}]:   {}", i, u8::from(*active));
                }
            }
            CheatVmOpcodeVariant::ReadWriteStaticRegister(op) => {
                log!("Opcode: Read/Write Static Register");
                if (op.static_idx as usize) < NUM_READABLE_STATIC_REGISTERS {
                    log!("Op Type: ReadStaticRegister");
                } else {
                    log!("Op Type: WriteStaticRegister");
                }
                log!("Reg Idx   {:X}", op.idx);
                log!("Stc Idx   {:X}", op.static_idx);
            }
            CheatVmOpcodeVariant::DebugLog(op) => {
                log!("Opcode: Debug Log");
                log!("Bit Width: {:X}", op.bit_width);
                log!("Log ID:    {:X}", op.log_id);
                log!("Val Type:  {:X}", op.val_type);
                match op.val_type {
                    4 => {
                        log!("Val Type:  Register Value");
                        log!("X Reg Idx: {:X}", op.val_reg_index);
                    }
                    0 => {
                        log!("Val Type:  Memory Relative Address");
                        log!("Mem Type:  {:X}", op.mem_type);
                        log!("Rel Addr:  {:X}", op.rel_address);
                    }
                    1 => {
                        log!("Val Type:  Memory Offset Register");
                        log!("Mem Type:  {:X}", op.mem_type);
                        log!("O Reg Idx: {:X}", op.ofs_reg_index);
                    }
                    2 => {
                        log!("Val Type:  Register Relative Address");
                        log!("A Reg Idx: {:X}", op.addr_reg_index);
                        log!("Rel Addr:  {:X}", op.rel_address);
                    }
                    3 => {
                        log!("Val Type:  Register Offset Register");
                        log!("A Reg Idx: {:X}", op.addr_reg_index);
                        log!("O Reg Idx: {:X}", op.ofs_reg_index);
                    }
                    _ => {}
                }
            }
            CheatVmOpcodeVariant::Unrecognized(op) => log!("Unknown opcode: {:X}", op.opcode),
            CheatVmOpcodeVariant::PauseProcess(_) | CheatVmOpcodeVariant::ResumeProcess(_) => {}
        }
    }

    fn skip_conditional_block(&mut self, is_if: bool) {
        assert!(
            self.condition_depth > 0,
            "Invalid condition depth in DMNT Cheat VM"
        );
        let desired_depth = self.condition_depth - 1;
        while self.condition_depth > desired_depth {
            let Some(opcode) = self.decode_next_opcode() else {
                break;
            };
            if opcode.begin_conditional_block {
                self.condition_depth += 1;
            } else if let CheatVmOpcodeVariant::EndConditional(end) = opcode.opcode {
                if !end.is_else {
                    self.condition_depth -= 1;
                } else if is_if && self.condition_depth - 1 == desired_depth {
                    break;
                }
            }
        }
    }

    fn decode_next_opcode(&mut self) -> Option<CheatVmOpcode> {
        if !self.decode_success {
            return None;
        }
        let first = self.read_program_u32();
        if !self.decode_success {
            return None;
        }
        let mut opcode_type = (first >> 28) & 0xf;
        if opcode_type >= 0xc {
            opcode_type = (opcode_type << 4) | ((first >> 24) & 0xf);
        }
        if opcode_type >= 0xf0 {
            opcode_type = (opcode_type << 4) | ((first >> 20) & 0xf);
        }
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
                let num_iters = if start_loop {
                    self.read_program_u32()
                } else {
                    0
                };
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
            // ATSRIOxa (aaaaaaaa)
            10 => {
                let mut op = StoreRegisterToAddressOpcode {
                    bit_width: (first >> 24) & 0xf,
                    str_reg_index: (first >> 20) & 0xf,
                    addr_reg_index: (first >> 16) & 0xf,
                    increment_reg: ((first >> 12) & 0xf) != 0,
                    ofs_type: (first >> 8) & 0xf,
                    mem_type: 0,
                    ofs_reg_index: (first >> 4) & 0xf,
                    rel_address: 0,
                };
                match op.ofs_type {
                    0 | 1 => {}
                    2 => {
                        op.rel_address =
                            ((first & 0xf) as u64) << 32 | self.read_program_u32() as u64
                    }
                    3 => op.mem_type = (first >> 4) & 0xf,
                    4 | 5 => {
                        op.mem_type = (first >> 4) & 0xf;
                        op.rel_address =
                            ((first & 0xf) as u64) << 32 | self.read_program_u32() as u64;
                    }
                    _ => op.ofs_type = 0,
                }
                CheatVmOpcodeVariant::StoreRegisterToAddress(op)
            }
            0xc0 => {
                begin_conditional = true;
                let mut op = BeginRegisterConditionalOpcode {
                    bit_width: (first >> 20) & 0xf,
                    cond_type: (first >> 16) & 0xf,
                    val_reg_index: (first >> 12) & 0xf,
                    comp_type: (first >> 8) & 0xf,
                    mem_type: 0,
                    addr_reg_index: 0,
                    other_reg_index: 0,
                    ofs_reg_index: 0,
                    rel_address: 0,
                    value: VmInt::default(),
                };
                match op.comp_type {
                    4 => op.value = self.read_vm_int(op.bit_width),
                    5 => op.other_reg_index = (first >> 4) & 0xf,
                    0 => {
                        op.mem_type = (first >> 4) & 0xf;
                        op.rel_address =
                            ((first & 0xf) as u64) << 32 | self.read_program_u32() as u64;
                    }
                    1 => {
                        op.mem_type = (first >> 4) & 0xf;
                        op.ofs_reg_index = first & 0xf;
                    }
                    2 => {
                        op.addr_reg_index = (first >> 4) & 0xf;
                        op.rel_address =
                            ((first & 0xf) as u64) << 32 | self.read_program_u32() as u64;
                    }
                    3 => {
                        op.addr_reg_index = (first >> 4) & 0xf;
                        op.ofs_reg_index = first & 0xf;
                    }
                    _ => {}
                }
                CheatVmOpcodeVariant::BeginRegisterConditional(op)
            }
            0xc1 => CheatVmOpcodeVariant::SaveRestoreRegister(SaveRestoreRegisterOpcode {
                dst_index: (first >> 16) & 0xf,
                src_index: (first >> 8) & 0xf,
                op_type: (first >> 4) & 0xf,
            }),
            0xc2 => CheatVmOpcodeVariant::SaveRestoreRegisterMask(SaveRestoreRegisterMaskOpcode {
                op_type: (first >> 20) & 0xf,
                should_operate: std::array::from_fn(|i| (first & (1u32 << i)) != 0),
            }),
            0xc3 => CheatVmOpcodeVariant::ReadWriteStaticRegister(ReadWriteStaticRegisterOpcode {
                static_idx: (first >> 4) & 0xff,
                idx: first & 0xf,
            }),
            0xff0 => CheatVmOpcodeVariant::PauseProcess(PauseProcessOpcode),
            0xff1 => CheatVmOpcodeVariant::ResumeProcess(ResumeProcessOpcode),
            0xfff => {
                let mut op = DebugLogOpcode {
                    bit_width: (first >> 16) & 0xf,
                    log_id: (first >> 12) & 0xf,
                    val_type: (first >> 8) & 0xf,
                    mem_type: 0,
                    addr_reg_index: 0,
                    val_reg_index: 0,
                    ofs_reg_index: 0,
                    rel_address: 0,
                };
                match op.val_type {
                    4 => op.val_reg_index = (first >> 4) & 0xf,
                    0 => {
                        op.mem_type = (first >> 4) & 0xf;
                        op.rel_address =
                            ((first & 0xf) as u64) << 32 | self.read_program_u32() as u64;
                    }
                    1 => {
                        op.mem_type = (first >> 4) & 0xf;
                        op.ofs_reg_index = first & 0xf;
                    }
                    2 => {
                        op.addr_reg_index = (first >> 4) & 0xf;
                        op.rel_address =
                            ((first & 0xf) as u64) << 32 | self.read_program_u32() as u64;
                    }
                    3 => {
                        op.addr_reg_index = (first >> 4) & 0xf;
                        op.ofs_reg_index = first & 0xf;
                    }
                    _ => {}
                }
                CheatVmOpcodeVariant::DebugLog(op)
            }
            _ => {
                self.decode_success = false;
                return None;
            }
        };
        if !self.decode_success {
            return None;
        }

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
            self.decode_success = false;
            0
        }
    }

    fn read_vm_int(&mut self, bit_width: u32) -> VmInt {
        let first = self.read_program_u32();
        let bit64 = match bit_width {
            1 => first as u8 as u64,
            2 => first as u16 as u64,
            4 => first as u64,
            8 => (first as u64) << 32 | self.read_program_u32() as u64,
            _ => 0,
        };
        VmInt { bit64 }
    }

    fn execute_opcode(
        &mut self,
        opcode: &CheatVmOpcode,
        metadata: &CheatProcessMetadata,
        keys_down: u64,
    ) {
        match &opcode.opcode {
            CheatVmOpcodeVariant::StoreStatic(op) => {
                let address =
                    Self::get_cheat_process_address(metadata, op.mem_type, op.rel_address)
                        .wrapping_add(self.registers[op.offset_register as usize]);
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
            CheatVmOpcodeVariant::EndConditional(op) => {
                if op.is_else {
                    self.skip_conditional_block(false);
                } else if self.condition_depth > 0 {
                    self.condition_depth -= 1;
                }
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
                    self.callbacks
                        .memory_read_unsafe(address, &mut bytes[..op.bit_width as usize]);
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
                self.registers[op.reg_index as usize] = Self::perform_arithmetic(
                    if op.math_type <= 4 { op.math_type } else { 9 },
                    op.bit_width,
                    lhs,
                    rhs,
                );
            }
            CheatVmOpcodeVariant::BeginKeypressConditional(op) => {
                if (keys_down & op.key_mask as u64) != op.key_mask as u64 {
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
                        address = base.wrapping_add(self.registers[op.addr_reg_index as usize]);
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
                        address = base.wrapping_add(self.registers[op.addr_reg_index as usize]);
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
                    _ => self.read_value_from_address(0, op.bit_width),
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
                    _ => {
                        self.registers[op.dst_index as usize] =
                            self.saved_values[op.src_index as usize]
                    }
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
                            _ => self.registers[i] = self.saved_values[i],
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
                        Self::get_vm_int(
                            VmInt {
                                bit64: self.registers[op.val_reg_index as usize],
                            },
                            op.bit_width,
                        )
                    }
                    _ => self.read_value_from_address(0, op.bit_width),
                };
                self.debug_log(op.log_id, value);
            }
            CheatVmOpcodeVariant::Unrecognized(_) => {
                // Skip unrecognized instructions
            }
        }
    }

    fn read_value_from_address(&self, address: u64, bit_width: u32) -> u64 {
        let mut buf = [0u8; 8];
        if !matches!(bit_width, 1 | 2 | 4 | 8) {
            return 0;
        }
        let size = bit_width as usize;
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
            8 => u64::MAX,
            _ => 0,
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

        // C++ shifts by >= 64 are undefined. Mask the count explicitly in Rust
        // rather than panicking; defined upstream shift counts remain identical.
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
            _ => 0,
        };

        result & mask
    }
}

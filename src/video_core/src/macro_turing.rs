// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later
// Instruction semantics adapted from Mesa mme_tu104_sim.c and mme_tu104.c:
// Copyright ? 2022 Collabora Ltd. SPDX-License-Identifier: MIT

//! Port of the header-only `video_core/macro_turing.h`.

pub type Word = u32;
pub type DataRam = [Word; 0xc00];
pub type DataRamAccess = (Word, Word);

#[derive(Debug, PartialEq, Eq)]
pub struct Result {
    pub success: bool,
    pub pc: Word,
    pub consumed: usize,
    pub error: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub struct Decoded {
    pub raw: [Word; 3],
    pub imm: [Word; 2],
    pub op: [Word; 2],
    pub dst: [Word; 2],
    pub src_a: [Word; 2],
    pub src_b: [Word; 2],
    pub out_method: [Word; 2],
    pub out_emit: [Word; 2],
    pub pred_reg: Word,
    pub pred_mode: Word,
    pub mask_true: Word,
    pub mask_false: Word,
    pub end: bool,
    pub load: [bool; 2],
}

impl Default for Decoded {
    fn default() -> Self {
        Self {
            raw: [Word::MAX; 3],
            imm: [0; 2],
            op: [0; 2],
            dst: [0; 2],
            src_a: [0; 2],
            src_b: [0; 2],
            out_method: [0; 2],
            out_emit: [0; 2],
            pred_reg: 0,
            pred_mode: 0,
            mask_true: 0,
            mask_false: 0,
            end: false,
            load: [false; 2],
        }
    }
}

pub struct TuringMacroInterpreter {
    pub dram: DataRam,
    /// Upstream persistent active_dram pointer; None selects the interpreter-owned RAM.
    pub active_dram: Option<std::sync::Arc<parking_lot::Mutex<DataRam>>>,
    pub decoded: Vec<Decoded>,
    pub read_registers: bool,
    pub reread_data_ram: bool,
    pub data_ram_reads: Vec<DataRamAccess>,
    pub data_ram_writes: Vec<DataRamAccess>,
}

impl Default for TuringMacroInterpreter {
    fn default() -> Self {
        Self {
            dram: [0; 0xc00],
            active_dram: None,
            decoded: Vec::new(),
            read_registers: false,
            reread_data_ram: false,
            data_ram_reads: Vec::new(),
            data_ram_writes: Vec::new(),
        }
    }
}

impl TuringMacroInterpreter {
    pub fn decode(bits: [Word; 3]) -> Decoded {
        let field = |low: usize, count: usize| {
            let mut value = bits[low / 32] as u64;
            if low / 32 < 2 {
                value |= (bits[low / 32 + 1] as u64) << 32;
            }
            ((value >> (low % 32)) & ((1u64 << count) - 1)) as Word
        };
        let mut d = Decoded {
            raw: bits,
            imm: [field(30, 16), field(66, 16)],
            op: [field(10, 5), field(46, 5)],
            dst: [field(15, 5), field(51, 5)],
            src_a: [field(20, 5), field(56, 5)],
            src_b: [field(25, 5), field(61, 5)],
            out_method: [field(82, 3), field(89, 3)],
            out_emit: [field(85, 4), field(92, 4)],
            pred_reg: field(5, 5),
            pred_mode: field(1, 4),
            end: field(0, 1) != 0,
            mask_true: 15,
            mask_false: 15,
            load: [false; 2],
        };
        const PREDICATES: [&[u8; 4]; 16] = [
            b"UUUU", b"TTTT", b"FFFF", b"TTUU", b"FFUU", b"TFUU", b"TUUU", b"FUUU", b"UUTT",
            b"UUTF", b"UUTU", b"UUFT", b"UUFF", b"UUFU", b"UUUT", b"UUUF",
        ];
        if d.pred_mode != 0 {
            d.mask_true = 0;
            d.mask_false = 0;
            for i in 0..4 {
                if PREDICATES[d.pred_mode as usize][i] != b'T' {
                    d.mask_true |= 1 << i;
                }
                if PREDICATES[d.pred_mode as usize][i] != b'F' {
                    d.mask_false |= 1 << i;
                }
            }
        }
        for i in 0..2 {
            let reg = 28 + i as u32;
            let out = 3 + i as u32;
            d.load[i] = d.pred_reg == reg
                || d.src_a.contains(&reg)
                || d.src_b.contains(&reg)
                || d.out_method.contains(&out)
                || d.out_emit.contains(&out);
        }
        d
    }

    /// `active_dram` borrows the upstream optional channel RAM for this call;
    /// None uses the persistent selected RAM, or our own dram when no channel RAM is selected.
    /// Emit borrows the same FIFO and active RAM explicitly so DMA methods may
    /// append FIFO words or write RAM without creating aliased mutable references.
    pub fn execute(
        &mut self,
        code: &[Word],
        start: Word,
        parameters: &mut Vec<Word>,
        read: impl FnMut(Word) -> Word,
        emit: impl FnMut(Word, Word, &mut Vec<Word>, &mut DataRam) -> bool,
        active_dram: Option<&mut DataRam>,
    ) -> Result {
        self.execute_with_code_reader(
            code.len(),
            |base| [code[base], code[base + 1], code[base + 2]],
            start,
            parameters,
            read,
            emit,
            active_dram,
        )
    }

    /// Mechanical adaptation of upstream Execute's live code span. The reader
    /// returns one instruction in upload order, with no code borrow retained
    /// across read/emit callbacks. This allows emitted uploads to replace later
    /// instructions, including entries already present in the decoded cache.
    /// `code_len` is the fixed word count of the upstream span; the reader is
    /// called only for complete instructions within that extent.
    pub fn execute_with_code_reader(
        &mut self,
        code_len: usize,
        mut read_code: impl FnMut(usize) -> [Word; 3],
        start: Word,
        parameters: &mut Vec<Word>,
        mut read: impl FnMut(Word) -> Word,
        mut emit: impl FnMut(Word, Word, &mut Vec<Word>, &mut DataRam) -> bool,
        active_dram: Option<&mut DataRam>,
    ) -> Result {
        let mut regs = [0u32; 24];
        let (mut initialized, mut carry, mut method, mut increment) = (0u32, 0u32, 0u32, 0u32);
        let (mut pc, mut loop_count, mut loop_start, mut loop_end) = (start, 0u32, 0u32, 0u32);
        let mut consumed = 0usize;
        let (mut end_next, mut stopped) = (false, false);
        let mut error = "";
        // Source's fail lambda records only the first error.
        macro_rules! fail {
            ($message:expr) => {
                if error.is_empty() {
                    error = $message;
                }
            };
        }
        if self.decoded.len() < code_len / 3 {
            self.decoded.resize(code_len / 3, Decoded::default());
        }
        self.read_registers = false;
        self.reread_data_ram = false;
        self.data_ram_reads.clear();
        self.data_ram_writes.clear();
        let selected_owner = self.active_dram.clone();
        let mut selected_guard = if active_dram.is_none() {
            selected_owner.as_ref().map(|ram| ram.lock())
        } else {
            None
        };
        let active_dram = active_dram
            .or_else(|| selected_guard.as_deref_mut())
            .unwrap_or(&mut self.dram);
        for _steps in 0..1_000_000 {
            if pc as usize * 3 + 2 >= code_len {
                fail!("instruction outside uploaded RAM");
                break;
            }
            let base = pc as usize * 3;
            let words = read_code(base);
            let bits = [words[2], words[1], words[0]];
            if self.decoded[pc as usize].raw != bits {
                self.decoded[pc as usize] = Self::decode(bits);
            }
            let d = self.decoded[pc as usize];
            let (mut load, mut alu) = ([0u32; 2], [0u32; 2]);
            for i in 0..2 {
                if d.load[i] {
                    if consumed >= parameters.len() {
                        fail!("parameter FIFO underflow");
                        break;
                    }
                    load[i] = parameters[consumed];
                    consumed += 1;
                }
            }
            let mut get = |reg: Word, lane: usize| -> Word {
                if reg < 24 {
                    if initialized & (1 << reg) == 0 {
                        fail!("uninitialized register read");
                    }
                    return regs[reg as usize];
                }
                match reg {
                    24 => 0,
                    25 => d.imm[lane] as i16 as i32 as u32,
                    26 => d.imm[1 - lane] as i16 as i32 as u32,
                    27 => (d.imm[0] << 16) | d.imm[1],
                    28 => load[0],
                    29 => load[1],
                    _ => {
                        fail!("invalid source register");
                        0
                    }
                }
            };
            let mask = if d.pred_mode != 0 {
                if get(d.pred_reg, 0) != 0 {
                    d.mask_true
                } else {
                    d.mask_false
                }
            } else {
                15
            };
            let (mut xs, mut ys) = ([0u32; 2], [0u32; 2]);
            for lane in 0..2 {
                if mask & (1 << lane) != 0 {
                    xs[lane] = get(d.src_a[lane], lane);
                    ys[lane] = get(d.src_b[lane], lane);
                }
            }
            let mut next_pc = pc.wrapping_add(1);
            let mut branch_cancels_end = false;
            for lane in 0..2 {
                if !error.is_empty() {
                    break;
                }
                if mask & (1 << lane) == 0 {
                    continue;
                }
                let (x, y, imm) = (xs[lane], ys[lane], d.imm[lane]);
                let mut value = 0;
                let condition = |comparison| match comparison {
                    17 | 25 => (x as i32) < y as i32,
                    18 | 26 => x < y,
                    19 | 27 => (x as i32) <= y as i32,
                    20 | 28 => x <= y,
                    _ => x == y,
                };
                match d.op[lane] {
                    0 => {
                        value = x.wrapping_add(y);
                        carry = u32::from(value < x);
                    }
                    1 => value = x.wrapping_add(y).wrapping_add(carry),
                    2 => {
                        value = x.wrapping_sub(y);
                        carry = u32::from(value > x);
                    }
                    3 => value = x.wrapping_sub(y).wrapping_sub(carry),
                    4 | 6 => {
                        let a = if d.op[lane] == 4 {
                            x as i32 as i64 as u64
                        } else {
                            x as u64
                        };
                        let b = if d.op[lane] == 4 {
                            y as i32 as i64 as u64
                        } else {
                            y as u64
                        };
                        let product = a.wrapping_mul(b);
                        value = product as u32;
                        carry = (product >> 32) as u32;
                    }
                    5 => value = carry,
                    7 => {
                        if x != 0x1000 {
                            fail!("unsupported extended operation");
                        }
                    }
                    8 => value = x.leading_zeros(),
                    9 => value = x << (y & 31),
                    10 => value = x >> (y & 31),
                    11 => value = ((x as i32) >> (y & 31)) as u32,
                    12 => value = x & y,
                    13 => value = !(x & y),
                    14 => value = x | y,
                    15 => value = x ^ y,
                    16 => {
                        let (dest, count, source) = ((imm >> 10) & 63, (imm >> 5) & 31, imm & 31);
                        if dest >= 32 {
                            fail!("invalid merge destination");
                        } else {
                            let bits_mask = (1u32 << count) - 1;
                            value =
                                (x & !(bits_mask << dest)) | (((y >> source) & bits_mask) << dest);
                        }
                    }
                    17..=21 => value = if condition(d.op[lane]) { u32::MAX } else { 0 },
                    22 => {
                        value = read(x.wrapping_add(y) & 0xffff);
                        self.read_registers = true;
                    }
                    23 => {
                        if loop_count != 0 || imm < 2 {
                            fail!("invalid loop");
                        } else if x == 0 {
                            next_pc = pc.wrapping_add(imm);
                        } else {
                            loop_count = x - 1;
                            loop_start = pc;
                            loop_end = pc.wrapping_add(imm).wrapping_sub(1);
                        }
                    }
                    24 => next_pc = pc.wrapping_add(imm & 0x7fff),
                    25..=29 => {
                        if condition(d.op[lane]) == (imm & 0x8000 != 0) {
                            let offset = ((imm << 19) as i32) >> 19;
                            if offset == -4096 {
                                stopped = true;
                            } else {
                                next_pc = pc.wrapping_add(offset as u32);
                                branch_cancels_end = imm & 0x4000 != 0;
                            }
                        }
                    }
                    30 => {
                        if x as usize >= active_dram.len() {
                            fail!("data RAM read out of bounds");
                        } else {
                            value = active_dram[x as usize];
                            if self
                                .data_ram_writes
                                .iter()
                                .any(|&(address, _)| address == x)
                            {
                                self.reread_data_ram = true;
                            }
                            self.data_ram_reads.push((x, value));
                        }
                    }
                    31 => {
                        if x as usize >= active_dram.len() {
                            fail!("data RAM write out of bounds");
                        } else {
                            active_dram[x as usize] = y;
                            self.data_ram_writes.push((x, y));
                        }
                    }
                    _ => unreachable!(),
                }
                alu[lane] = value;
                if d.dst[lane] < 24 {
                    regs[d.dst[lane] as usize] = value;
                    initialized |= 1 << d.dst[lane];
                } else if d.dst[lane] != 24 {
                    fail!("invalid destination register");
                }
            }
            // Local helper mirrors the source output lambda; error is passed
            // explicitly to allow emission to record its own first failure.
            let output = |selector, error: &mut &'static str| -> Word {
                match selector {
                    1 => alu[0],
                    2 => alu[1],
                    3 => load[0],
                    4 => load[1],
                    5 => d.imm[0],
                    6 => d.imm[1],
                    8 => d.imm[0] >> 12,
                    9 => d.imm[1] >> 12,
                    10 => (d.imm[0] << 16) | d.imm[1],
                    _ => {
                        if error.is_empty() {
                            *error = "invalid output selector";
                        }
                        0
                    }
                }
            };
            for lane in 0..2 {
                if !error.is_empty() {
                    break;
                }
                if mask & (4 << lane) == 0 {
                    continue;
                }
                if d.out_method[lane] != 0 {
                    let address = output(d.out_method[lane], &mut error);
                    method = address & 0xfff;
                    increment = (address >> 12) & 15;
                }
                if d.out_emit[lane] != 0 {
                    let value = output(d.out_emit[lane], &mut error);
                    if error.is_empty() && !emit(method, value, parameters, active_dram) {
                        fail!("unsupported emitted method");
                    }
                    method = method.wrapping_add(increment);
                }
            }
            if !error.is_empty() {
                break;
            }
            if end_next || stopped {
                return Result {
                    success: true,
                    pc,
                    consumed,
                    error: "",
                };
            }
            end_next = d.end && !branch_cancels_end;
            if loop_count != 0 && pc == loop_end {
                loop_count -= 1;
                next_pc = loop_start.wrapping_add(1);
            }
            pc = next_pc;
        }
        if error.is_empty() {
            error = "instruction limit exceeded";
        }
        Result {
            success: false,
            pc,
            consumed,
            error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Encode independently into the uploaded word order (high word first).
    fn instruction(fields: &[(usize, usize, u32)]) -> [u32; 3] {
        let mut bits = 0u128;
        // Both lanes default to ADD zero,zero with discarded destination.
        for low in [15, 20, 25, 51, 56, 61] {
            bits |= 24u128 << low;
        }
        for &(low, count, value) in fields {
            let mask = ((1u128 << count) - 1) << low;
            bits = (bits & !mask) | (((value as u128) << low) & mask);
        }
        [(bits >> 64) as u32, (bits >> 32) as u32, bits as u32]
    }

    fn run(code: &[[u32; 3]], params: Vec<u32>) -> (Result, Vec<(u32, u32)>) {
        let mut interpreter = TuringMacroInterpreter::default();
        let mut writes = Vec::new();
        let result = interpreter.execute(
            &code.concat(),
            0,
            &mut params.clone(),
            |_| 0,
            |method, value, _, _ram| {
                writes.push((method, value));
                true
            },
            None,
        );
        (result, writes)
    }

    #[test]
    fn emitted_upload_replaces_live_code_and_invalidates_decoded_instruction() {
        use std::cell::RefCell;

        let first = instruction(&[(0, 1, 1), (30, 16, 7), (85, 4, 5)]);
        let old = instruction(&[(30, 16, 11), (85, 4, 5)]);
        let replacement = instruction(&[(30, 16, 42), (85, 4, 5)]);
        let code = RefCell::new([first, old].concat());
        let mut interpreter = TuringMacroInterpreter::default();
        let mut writes = Vec::new();
        // Warm the decode cache with the instruction that the callback replaces.
        let result = interpreter.execute(
            &code.borrow(),
            0,
            &mut vec![],
            |_| 0,
            |_, value, _, _| {
                writes.push(value);
                true
            },
            None,
        );
        assert!(result.success);
        assert_eq!(writes, [7, 11]);
        writes.clear();
        let result = interpreter.execute_with_code_reader(
            6,
            |base| {
                let words = code.borrow();
                [words[base], words[base + 1], words[base + 2]]
            },
            0,
            &mut vec![],
            |_| 0,
            |_, value, _, _| {
                writes.push(value);
                code.borrow_mut()[3..6].copy_from_slice(&replacement);
                true
            },
            None,
        );
        assert!(result.success);
        assert_eq!((result.pc, result.consumed), (1, 0));
        assert_eq!(writes, [7, 42]);
        assert_eq!(interpreter.decoded[1].imm[0], 42);
    }

    #[test]
    fn live_code_reader_is_not_called_for_incomplete_or_out_of_bounds_instructions() {
        for (code_len, start) in [(0, 0), (2, 0), (5, 1), (6, 2), (6, u32::MAX)] {
            let result = TuringMacroInterpreter::default().execute_with_code_reader(
                code_len,
                |_| panic!("out-of-bounds instruction fetch"),
                start,
                &mut vec![],
                |_| 0,
                |_, _, _, _| true,
                None,
            );
            assert!(!result.success);
            assert_eq!(result.error, "instruction outside uploaded RAM");
            assert_eq!((result.pc, result.consumed), (start, 0));
        }
    }

    #[test]
    fn decode_cross_word_fields_and_all_predicate_masks() {
        let expected = [
            (15, 15),
            (0, 15),
            (15, 0),
            (12, 15),
            (15, 12),
            (14, 13),
            (14, 15),
            (15, 14),
            (3, 15),
            (11, 7),
            (11, 15),
            (7, 11),
            (15, 3),
            (15, 11),
            (7, 15),
            (15, 7),
        ];
        for (mode, (when_true, when_false)) in expected.into_iter().enumerate() {
            let words = instruction(&[
                (1, 4, mode as u32),
                (30, 16, 0xdead),
                (66, 16, 0xbeef),
                (25, 5, 27),
                (51, 5, 23),
                (89, 3, 6),
                (92, 4, 10),
            ]);
            let d = TuringMacroInterpreter::decode([words[2], words[1], words[0]]);
            assert_eq!((d.mask_true, d.mask_false), (when_true, when_false));
            assert_eq!(d.imm, [0xdead, 0xbeef]);
            assert_eq!(d.dst[1], 23);
            assert_eq!(d.src_b[0], 27);
            assert_eq!(d.out_method[1], 6);
            assert_eq!(d.out_emit[1], 10);
        }
    }

    #[test]
    fn branch_back_edge_cancels_end_and_immediates_preserve_bits() {
        let (result, _) = run(
            &[
                instruction(&[(0, 1, 1), (10, 5, 29), (20, 5, 28), (30, 16, 0x4000)]),
                instruction(&[]),
            ],
            vec![1, 0],
        );
        assert!(result.success);
        assert_eq!((result.pc, result.consumed), (1, 2));
        for (selector, expected) in [
            (5, 0xface),
            (6, 0xbeef),
            (8, 0xf),
            (9, 0xb),
            (10, 0xfacebeef),
        ] {
            let (result, writes) = run(
                &[
                    instruction(&[
                        (0, 1, 1),
                        (30, 16, 0xface),
                        (66, 16, 0xbeef),
                        (85, 4, selector),
                    ]),
                    instruction(&[]),
                ],
                vec![],
            );
            assert!(result.success);
            assert_eq!(writes, [(0, expected)]);
        }
        let (result, writes) = run(
            &[
                instruction(&[
                    (0, 1, 1),
                    (10, 5, 16),
                    (20, 5, 28),
                    (25, 5, 29),
                    (30, 16, (8 << 10) | (4 << 5) | 4),
                    (85, 4, 1),
                ]),
                instruction(&[]),
            ],
            vec![0xffffffff, 0x20],
        );
        assert!(result.success);
        assert_eq!(writes, [(0, 0xfffff2ff)]);
    }

    #[test]
    fn alu_operations_and_signed_comparisons_match_word_semantics() {
        let cases = [
            (0, u32::MAX, 2, 1),
            (2, 0, 1, u32::MAX),
            (4, u32::MAX, 3, 0xffff_fffd),
            (6, 0xffff_ffff, 2, 0xffff_fffe),
            (7, 0x1000, 0, 0),
            (8, 0, 0, 32),
            (9, 1, 33, 2),
            (10, 0x8000_0000, 31, 1),
            (11, 0x8000_0000, 31, u32::MAX),
            (12, 0x33, 0x55, 0x11),
            (13, 0x33, 0x55, !0x11),
            (14, 0x33, 0x55, 0x77),
            (15, 0x33, 0x55, 0x66),
            (17, u32::MAX, 0, u32::MAX),
            (18, u32::MAX, 0, 0),
            (19, 7, 7, u32::MAX),
            (20, 8, 7, 0),
            (21, 9, 9, u32::MAX),
        ];
        for (op, x, y, expected) in cases {
            let code = [
                instruction(&[(0, 1, 1), (10, 5, op), (20, 5, 28), (25, 5, 29), (85, 4, 1)]),
                instruction(&[]),
            ];
            let (result, writes) = run(&code, vec![x, y]);
            assert!(result.success, "op {op}: {result:?}");
            assert_eq!(writes, [(0, expected)], "op {op}");
            assert_eq!((result.pc, result.consumed), (1, 2));
        }
    }

    #[test]
    fn carry_chains_but_register_reads_precede_both_lane_writes() {
        for (first_op, second_op, x, y, expected) in [
            (0, 1, u32::MAX, 1, 1),
            (2, 3, 0, 1, u32::MAX),
            (4, 5, u32::MAX, 2, u32::MAX),
            (6, 5, u32::MAX, 2, 1),
        ] {
            let (result, writes) = run(
                &[
                    instruction(&[
                        (0, 1, 1),
                        (10, 5, first_op),
                        (20, 5, 28),
                        (25, 5, 29),
                        (46, 5, second_op),
                        (85, 4, 2),
                    ]),
                    instruction(&[]),
                ],
                vec![x, y],
            );
            assert!(result.success);
            assert_eq!(writes, [(0, expected)]);
        }
        // r0=7; next instruction writes r0=9 while ALU1 still reads 7.
        let (result, writes) = run(
            &[
                instruction(&[(15, 5, 0), (20, 5, 25), (30, 16, 7)]),
                instruction(&[
                    (0, 1, 1),
                    (15, 5, 0),
                    (20, 5, 25),
                    (30, 16, 9),
                    (56, 5, 0),
                    (85, 4, 2),
                ]),
                instruction(&[]),
            ],
            vec![],
        );
        assert!(result.success);
        assert_eq!(writes, [(0, 7)]);
    }

    #[test]
    fn fifo_loads_before_predication_and_emit_can_append() {
        let code = [
            instruction(&[(30, 16, 0x1005), (66, 16, 123), (82, 3, 5), (85, 4, 6)]),
            instruction(&[(0, 1, 1), (20, 5, 28), (85, 4, 1)]),
            instruction(&[]),
        ]
        .concat();
        let mut interp = TuringMacroInterpreter::default();
        let mut fifo = Vec::new();
        let mut writes = Vec::new();
        let result = interp.execute(
            &code,
            0,
            &mut fifo,
            |_| 0,
            |method, value, fifo, _ram| {
                writes.push((method, value));
                if method == 5 {
                    fifo.push(value + 1);
                }
                true
            },
            None,
        );
        assert!(result.success);
        assert_eq!(result.consumed, 1);
        assert_eq!(writes, [(5, 123), (6, 124)]);
        let (result, _) = run(
            &[instruction(&[
                (1, 4, 1),
                (5, 5, 25),
                (30, 16, 1),
                (20, 5, 28),
            ])],
            vec![],
        );
        assert_eq!(result.error, "parameter FIFO underflow");
    }

    #[test]
    fn persistent_active_ram_survives_calls_without_a_replacement() {
        let ram = std::sync::Arc::new(parking_lot::Mutex::new([0; 0xc00]));
        ram.lock()[0x27] = 77;
        let mut interpreter = TuringMacroInterpreter::default();
        interpreter.active_dram = Some(ram.clone());
        let code = [
            instruction(&[
                (0, 1, 1),
                (10, 5, 30),
                (20, 5, 25),
                (30, 16, 0x27),
                (85, 4, 1),
            ]),
            instruction(&[]),
        ]
        .concat();
        for expected in [77, 88] {
            ram.lock()[0x27] = expected;
            let mut writes = Vec::new();
            let result = interpreter.execute(
                &code,
                0,
                &mut vec![],
                |_| 0,
                |_, value, _, _| {
                    writes.push(value);
                    true
                },
                None,
            );
            assert!(result.success);
            assert_eq!(writes, [expected]);
            assert_eq!(interpreter.dram[0x27], 0);
        }
        let mut borrowed = [0; 0xc00];
        borrowed[0x27] = 99;
        let mut writes = Vec::new();
        assert!(
            interpreter
                .execute(
                    &code,
                    0,
                    &mut vec![],
                    |_| 0,
                    |_, value, _, _| {
                        writes.push(value);
                        true
                    },
                    Some(&mut borrowed)
                )
                .success
        );
        assert_eq!(writes, [99]);
        assert!(std::sync::Arc::ptr_eq(
            interpreter.active_dram.as_ref().unwrap(),
            &ram
        ));
    }

    #[test]
    fn data_ram_is_channel_owned_and_tracks_read_after_write() {
        let code = [
            instruction(&[
                (10, 5, 31),
                (20, 5, 25),
                (30, 16, 0x27),
                (25, 5, 26),
                (66, 16, 42),
            ]),
            instruction(&[
                (0, 1, 1),
                (10, 5, 30),
                (20, 5, 25),
                (30, 16, 0x27),
                (85, 4, 1),
            ]),
            instruction(&[]),
        ]
        .concat();
        let mut interp = TuringMacroInterpreter::default();
        let mut ram = [0; 0xc00];
        let result = interp.execute(
            &code,
            0,
            &mut vec![],
            |_| 0,
            |_, value, _, _ram| {
                assert_eq!(value, 42);
                true
            },
            Some(&mut ram),
        );
        assert!(result.success);
        assert_eq!(ram[0x27], 42);
        assert_eq!(interp.dram[0x27], 0);
        assert_eq!(interp.data_ram_reads, [(0x27, 42)]);
        assert_eq!(interp.data_ram_writes, [(0x27, 42)]);
        assert!(interp.reread_data_ram);
        assert!(!interp.read_registers);
        let code = [
            instruction(&[(0, 1, 1), (10, 5, 22), (20, 5, 25), (30, 16, 0xffff)]),
            instruction(&[]),
        ]
        .concat();
        let result = interp.execute(
            &code,
            0,
            &mut vec![],
            |address| {
                assert_eq!(address, 0xffff);
                99
            },
            |_, _, _, _ram| true,
            None,
        );
        assert!(result.success);
        assert!(interp.read_registers);
        assert!(!interp.reread_data_ram);
        assert!(interp.data_ram_reads.is_empty());
        assert!(interp.data_ram_writes.is_empty());
    }

    #[test]
    fn zero_count_loop_skips_fifo_and_nonzero_loop_repeats_body() {
        for count in [0, 1, 3] {
            let (result, writes) = run(
                &[
                    instruction(&[(10, 5, 23), (20, 5, 28), (30, 16, 2)]),
                    instruction(&[(20, 5, 28), (85, 4, 1)]),
                    instruction(&[(0, 1, 1)]),
                    instruction(&[]),
                ],
                std::iter::once(count).chain(10..10 + count).collect(),
            );
            assert!(result.success, "{result:?}");
            assert_eq!(result.consumed, 1 + count as usize);
            assert_eq!(writes, (10..10 + count).map(|v| (0, v)).collect::<Vec<_>>());
        }
    }

    #[test]
    fn termination_delay_stop_branch_and_code_redecode() {
        let (result, writes) = run(
            &[
                instruction(&[(0, 1, 1)]),
                instruction(&[(30, 16, 7), (85, 4, 5)]),
                instruction(&[(30, 16, 8), (85, 4, 5)]),
            ],
            vec![],
        );
        assert!(result.success);
        assert_eq!(result.pc, 1);
        assert_eq!(writes, [(0, 7)]);
        let (result, _) = run(&[instruction(&[(10, 5, 29), (30, 16, 0x9000)])], vec![]);
        assert!(result.success);
        assert_eq!(result.pc, 0);
        let mut interp = TuringMacroInterpreter::default();
        for value in [4, 9] {
            let code = [
                instruction(&[(0, 1, 1), (30, 16, value), (85, 4, 5)]),
                instruction(&[]),
            ]
            .concat();
            assert!(
                interp
                    .execute(
                        &code,
                        0,
                        &mut vec![],
                        |_| 0,
                        |_, v, _, _ram| {
                            assert_eq!(v, value);
                            true
                        },
                        None
                    )
                    .success
            );
        }
    }

    #[test]
    fn failures_preserve_first_error_and_report_pc_and_consumption() {
        for (fields, error) in [
            (vec![(20, 5, 0)], "uninitialized register read"),
            (vec![(20, 5, 30)], "invalid source register"),
            (vec![(15, 5, 25)], "invalid destination register"),
            (vec![(10, 5, 7)], "unsupported extended operation"),
            (
                vec![(10, 5, 16), (30, 16, 0x8000)],
                "invalid merge destination",
            ),
            (vec![(10, 5, 23), (30, 16, 1)], "invalid loop"),
            (
                vec![(10, 5, 30), (20, 5, 25), (30, 16, 0xc00)],
                "data RAM read out of bounds",
            ),
            (
                vec![(10, 5, 31), (20, 5, 25), (30, 16, 0xc00)],
                "data RAM write out of bounds",
            ),
            (vec![(85, 4, 7)], "invalid output selector"),
        ] {
            let (result, _) = run(&[instruction(&fields)], vec![]);
            assert_eq!(result.error, error);
            assert_eq!((result.pc, result.consumed), (0, 0));
        }
        assert_eq!(run(&[], vec![]).0.error, "instruction outside uploaded RAM");
        let (result, _) = run(&[instruction(&[(10, 5, 24)])], vec![]);
        assert_eq!(result.error, "instruction limit exceeded");
    }
}

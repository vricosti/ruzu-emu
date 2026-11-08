// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of header-only `video_core/macro_turing_jit.h` using rxbyak.

use crate::macro_turing::{
    DataRam, Decoded, Result as ExecutionResult, TuringMacroInterpreter, Word,
};
use rxbyak::*;
use std::ffi::c_void;
use std::mem::offset_of;

#[repr(C)]
pub struct State {
    pub regs: [Word; 24],
    pub carry: Word,
    pub method: Word,
    pub increment: Word,
    pub loop_count: Word,
    pub loop_start: Word,
    pub loop_end: Word,
    pub next_pc: Word,
    pub steps: Word,
    pub start: Word,
    pub consumed: Word,
    pub params_size: Word,
    pub fail_pc: Word,
    pub end_next: u8,
    pub stopped: u8,
    pub cancel_end: u8,
    pub pad: u8,
    pub scratch_x: Word,
    pub scratch_y: Word,
    pub _pointer_alignment: Word,
    pub params: *const Word,
    pub dram: *mut Word,
    pub user: *mut c_void,
    pub emit: unsafe extern "C" fn(*mut State, Word, Word) -> bool,
    pub read: unsafe extern "C" fn(*mut State, Word) -> Word,
    // Opaque vector pointer is only dereferenced by Rust callback thunks.
    pub fifo: *mut Vec<Word>,
}

#[repr(u32)]
#[derive(Clone, Copy)]
pub enum Error {
    None = 0,
    FifoUnderflow,
    InvalidSource,
    InvalidDestination,
    UnsupportedExtended,
    InvalidMerge,
    InvalidLoop,
    DataRamReadOutOfBounds,
    DataRamWriteOutOfBounds,
    InvalidOutput,
    UnsupportedEmit,
    OutsideCode,
    StepLimit,
}

pub struct TuringMacroJit {
    program: Option<Program>,
    compiled_generation: u64,
}
impl Default for TuringMacroJit {
    fn default() -> Self {
        Self {
            program: None,
            compiled_generation: u64::MAX,
        }
    }
}
// The compiled mapping belongs to a serialized GPU macro context, as upstream.
unsafe impl Send for TuringMacroJit {}
impl TuringMacroJit {
    pub fn error_text(error: Word) -> &'static str {
        const TEXTS: [&str; 13] = [
            "",
            "parameter FIFO underflow",
            "invalid source register",
            "invalid destination register",
            "unsupported extended operation",
            "invalid merge destination",
            "invalid loop",
            "data RAM read out of bounds",
            "data RAM write out of bounds",
            "invalid output selector",
            "unsupported emitted method",
            "instruction outside uploaded RAM",
            "instruction limit exceeded",
        ];
        TEXTS.get(error as usize).copied().unwrap_or("unknown")
    }
    pub fn prepare(&mut self, code: &[Word], generation: u64) -> rxbyak::Result<()> {
        if self.program.is_some() && generation == self.compiled_generation {
            return Ok(());
        }
        self.program = Some(Program::new(code)?);
        self.compiled_generation = generation;
        Ok(())
    }
    pub fn execute<
        R: FnMut(Word) -> Word,
        E: FnMut(Word, Word, &mut Vec<Word>, &mut DataRam) -> bool,
    >(
        &mut self,
        start: Word,
        parameters: &mut Vec<Word>,
        data_ram: &mut DataRam,
        mut read: R,
        mut emit: E,
    ) -> ExecutionResult {
        let mut thunks = Thunks {
            read: &mut read,
            emit: &mut emit,
        };
        // All fields (including upstream unspecified scratch/padding) deterministic.
        let mut state = State {
            regs: [0; 24],
            carry: 0,
            method: 0,
            increment: 0,
            loop_count: 0,
            loop_start: 0,
            loop_end: 0,
            next_pc: 0,
            steps: 1_000_000,
            start,
            consumed: 0,
            params_size: parameters.len() as u32,
            fail_pc: 0,
            end_next: 0,
            stopped: 0,
            cancel_end: 0,
            pad: 0,
            scratch_x: 0,
            scratch_y: 0,
            _pointer_alignment: 0,
            params: parameters.as_ptr(),
            dram: data_ram.as_mut_ptr(),
            user: (&mut thunks as *mut Thunks<R, E>).cast(),
            emit: Thunks::<R, E>::call_emit,
            read: Thunks::<R, E>::call_read,
            fifo: parameters,
        };
        let entry = self
            .program
            .as_ref()
            .expect("Prepare must precede Execute")
            .entry;
        let error = unsafe { entry.expect("Program must be finalized")(&mut state) };
        ExecutionResult {
            success: error == 0,
            pc: state.fail_pc,
            consumed: state.consumed as usize,
            error: Self::error_text(error),
        }
    }
}
struct Thunks<'a, R, E> {
    read: &'a mut R,
    emit: &'a mut E,
}
impl<R: FnMut(Word) -> Word, E: FnMut(Word, Word, &mut Vec<Word>, &mut DataRam) -> bool>
    Thunks<'_, R, E>
{
    unsafe extern "C" fn call_emit(state: *mut State, method: Word, value: Word) -> bool {
        let state = unsafe { &mut *state };
        let thunks = unsafe { &mut *state.user.cast::<Self>() };
        let fifo = unsafe { &mut *state.fifo };
        let ok = (thunks.emit)(method, value, fifo, unsafe {
            &mut *state.dram.cast::<DataRam>()
        });
        state.params = fifo.as_ptr();
        state.params_size = fifo.len() as u32;
        ok
    }
    unsafe extern "C" fn call_read(state: *mut State, address: Word) -> Word {
        let thunks = unsafe { &mut *(*state).user.cast::<Self>() };
        (thunks.read)(address)
    }
}
#[cfg(target_os = "windows")]
const ARG0: Reg = RCX;
#[cfg(not(target_os = "windows"))]
const ARG0: Reg = RDI;
#[cfg(target_os = "windows")]
const ARG1D: Reg = EDX;
#[cfg(target_os = "windows")]
const ARG2D: Reg = R8D;
#[cfg(not(target_os = "windows"))]
const ARG1D: Reg = ESI;
#[cfg(not(target_os = "windows"))]
const ARG2D: Reg = EDX;
macro_rules! d {
    ($field:ident) => {
        dword_ptr(RBX + offset_of!(State, $field) as i32)
    };
}
macro_rules! b {
    ($field:ident) => {
        byte_ptr(RBX + offset_of!(State, $field) as i32)
    };
}
macro_rules! q {
    ($field:ident) => {
        qword_ptr(RBX + offset_of!(State, $field) as i32)
    };
}

struct Program {
    asm: CodeAssembler,
    entry: Option<unsafe extern "C" fn(*mut State) -> Word>,
    labels: Vec<Label>,
    loop_end: Vec<bool>,
    after_end: Vec<bool>,
    table: Label,
    dispatch: Label,
    success: Label,
    leave: Label,
    fail_underflow: Label,
    fail_source: Label,
    fail_destination: Label,
    fail_extended: Label,
    fail_merge: Label,
    fail_loop: Label,
    fail_dread: Label,
    fail_dwrite: Label,
    fail_output: Label,
    fail_emit: Label,
    fail_outside: Label,
    fail_steps: Label,
}
impl Program {
    fn new(code: &[Word]) -> rxbyak::Result<Self> {
        let mut asm = CodeAssembler::new_auto_grow(1 << 20)?;
        let labels = (0..code.len() / 3).map(|_| asm.create_label()).collect();
        let table = asm.create_label();
        let dispatch = asm.create_label();
        let success = asm.create_label();
        let leave = asm.create_label();
        let fail_underflow = asm.create_label();
        let fail_source = asm.create_label();
        let fail_destination = asm.create_label();
        let fail_extended = asm.create_label();
        let fail_merge = asm.create_label();
        let fail_loop = asm.create_label();
        let fail_dread = asm.create_label();
        let fail_dwrite = asm.create_label();
        let fail_output = asm.create_label();
        let fail_emit = asm.create_label();
        let fail_outside = asm.create_label();
        let fail_steps = asm.create_label();
        // Entry is assigned only after the executable mapping has been finalized.
        let mut p = Self {
            asm,
            entry: None,
            labels,
            loop_end: Vec::new(),
            after_end: Vec::new(),
            table,
            dispatch,
            success,
            leave,
            fail_underflow,
            fail_source,
            fail_destination,
            fail_extended,
            fail_merge,
            fail_loop,
            fail_dread,
            fail_dwrite,
            fail_output,
            fail_emit,
            fail_outside,
            fail_steps,
        };
        p.generate(code)?;
        p.asm.ready()?;
        p.entry = Some(unsafe { p.asm.as_fn() });
        Ok(p)
    }
    fn reg_offset(reg: Word) -> i32 {
        (offset_of!(State, regs) + reg as usize * 4) as i32
    }
    fn is_flow(op: Word) -> bool {
        (23..=29).contains(&op)
    }
    fn analyze(&mut self, decoded: &[Decoded]) {
        let count = decoded.len();
        self.loop_end = vec![false; count];
        self.after_end = vec![false; count];
        let mark = |set: &mut Vec<bool>, pc: i64| {
            if pc >= 0 && (pc as usize) < count {
                set[pc as usize] = true;
            }
        };
        for (pc, d) in decoded.iter().enumerate() {
            for lane in 0..2 {
                if d.op[lane] == 23 && d.imm[lane] >= 2 && pc + d.imm[lane] as usize - 1 < count {
                    self.loop_end[pc + d.imm[lane] as usize - 1] = true;
                }
            }
        }
        for (pc, d) in decoded.iter().enumerate() {
            if !d.end {
                continue;
            }
            mark(&mut self.after_end, pc as i64 + 1);
            for lane in 0..2 {
                let imm = d.imm[lane];
                match d.op[lane] {
                    23 => mark(&mut self.after_end, pc as i64 + imm as i64),
                    24 => mark(&mut self.after_end, pc as i64 + (imm & 0x7fff) as i64),
                    25..=29 => mark(
                        &mut self.after_end,
                        pc as i64 + (((imm << 19) as i32) >> 19) as i64,
                    ),
                    _ => {}
                }
            }
        }
        for (pc, d) in decoded.iter().enumerate() {
            for lane in 0..2 {
                if d.op[lane] == 23 && d.imm[lane] >= 2 {
                    let end = pc + d.imm[lane] as usize - 1;
                    if end < count && decoded[end].end {
                        mark(&mut self.after_end, pc as i64 + 1);
                    }
                }
            }
        }
    }
    fn generate(&mut self, code: &[Word]) -> rxbyak::Result<()> {
        let count = (code.len() / 3) as u32;
        let decoded: Vec<_> = code
            .chunks_exact(3)
            .map(|w| TuringMacroInterpreter::decode([w[2], w[1], w[0]]))
            .collect();
        self.analyze(&decoded);
        for reg in [RBX, RBP, R12, R13, R14, R15] {
            self.asm.push(reg)?;
        }
        self.asm.sub(RSP, 40)?;
        self.asm.mov(RBX, ARG0)?;
        self.asm.mov(EAX, d!(start))?;
        self.asm.cmp(EAX, count)?;
        self.asm.jae(&self.fail_outside, JmpType::Near)?;
        self.asm.lea_label(RCX, &self.table)?;
        self.asm.jmp_reg(qword_ptr(RCX + RAX * 8))?;
        for (pc, d) in decoded.iter().enumerate() {
            self.asm.bind(&self.labels[pc])?;
            self.instruction(pc as u32, count, d)?;
        }
        self.asm.bind(&self.dispatch)?;
        self.asm.sub(d!(steps), 1)?;
        self.asm.jz(&self.fail_steps, JmpType::Near)?;
        self.asm.cmp(EAX, count)?;
        self.asm.jae(&self.fail_outside, JmpType::Near)?;
        self.asm.lea_label(RCX, &self.table)?;
        self.asm.jmp_reg(qword_ptr(RCX + RAX * 8))?;
        self.asm.bind(&self.success)?;
        self.asm.xor_(EAX, EAX)?;
        self.asm.bind(&self.leave)?;
        self.asm.add(RSP, 40)?;
        for reg in [R15, R14, R13, R12, RBP, RBX] {
            self.asm.pop(reg)?;
        }
        self.asm.ret()?;
        for (i, label) in [
            self.fail_underflow,
            self.fail_source,
            self.fail_destination,
            self.fail_extended,
            self.fail_merge,
            self.fail_loop,
            self.fail_dread,
            self.fail_dwrite,
            self.fail_output,
            self.fail_emit,
            self.fail_outside,
            self.fail_steps,
        ]
        .iter()
        .enumerate()
        {
            self.asm.bind(label)?;
            self.asm.mov(EAX, (i + 1) as u32)?;
            self.asm.jmp(&self.leave, JmpType::Near)?;
        }
        self.asm.align(8)?;
        self.asm.bind(&self.table)?;
        for label in &self.labels {
            self.asm.put_l(label)?;
        }
        Ok(())
    }
    fn get(&mut self, dst: Reg, d: &Decoded, reg: Word, lane: usize) -> rxbyak::Result<()> {
        if reg < 24 {
            return self.asm.mov(dst, dword_ptr(RBX + Self::reg_offset(reg)));
        }
        match reg {
            24 => self.asm.xor_(dst, dst)?,
            25 => self.asm.mov(dst, d.imm[lane] as i16 as i32 as u32)?,
            26 => self.asm.mov(dst, d.imm[1 - lane] as i16 as i32 as u32)?,
            27 => self.asm.mov(dst, (d.imm[0] << 16) | d.imm[1])?,
            28 => self.asm.mov(dst, R12D)?,
            29 => self.asm.mov(dst, R13D)?,
            _ => self.asm.jmp(&self.fail_source, JmpType::Near)?,
        }
        Ok(())
    }
    fn instruction(&mut self, pc: Word, count: Word, d: &Decoded) -> rxbyak::Result<()> {
        for i in 0..2 {
            let load = if i == 0 { R12D } else { R13D };
            if !d.load[i] {
                self.asm.xor_(load, load)?;
                continue;
            }
            self.asm.mov(EAX, d!(consumed))?;
            self.asm.cmp(EAX, d!(params_size))?;
            self.asm.jae(&self.fail_underflow, JmpType::Near)?;
            self.asm.mov(RCX, q!(params))?;
            self.asm.mov(load, dword_ptr(RCX + RAX * 4))?;
            self.asm.inc(EAX)?;
            self.asm.mov(d!(consumed), EAX)?;
        }
        if d.pred_mode == 0 {
            return self.body(pc, count, d, 15);
        }
        let when_false = self.asm.create_label();
        self.get(EAX, d, d.pred_reg, 0)?;
        self.asm.test(EAX, EAX)?;
        self.asm.jz(&when_false, JmpType::Near)?;
        self.body(pc, count, d, d.mask_true)?;
        self.asm.bind(&when_false)?;
        self.body(pc, count, d, d.mask_false)
    }
    fn body(&mut self, pc: Word, count: Word, d: &Decoded, mask: Word) -> rxbyak::Result<()> {
        for lane in 0..2 {
            if mask & (1 << lane) != 0 {
                continue;
            }
            let selected = (0..2).any(|out| {
                mask & (4 << out) != 0
                    && (d.out_method[out] == lane + 1 || d.out_emit[out] == lane + 1)
            });
            if selected {
                let reg = if lane == 0 { R14D } else { R15D };
                self.asm.xor_(reg, reg)?;
            }
        }
        let flow =
            (mask & 1 != 0 && Self::is_flow(d.op[0])) || (mask & 2 != 0 && Self::is_flow(d.op[1]));
        if flow {
            self.asm.mov(d!(next_pc), pc + 1)?;
            self.asm.mov(b!(cancel_end), 0)?;
        }
        let early = mask & 2 != 0
            && mask & 1 != 0
            && (d.op[0] == 22
                || (d.dst[0] < 24 && (d.dst[0] == d.src_a[1] || d.dst[0] == d.src_b[1])));
        if early {
            self.get(EAX, d, d.src_a[1], 1)?;
            self.asm.mov(d!(scratch_x), EAX)?;
            self.get(EAX, d, d.src_b[1], 1)?;
            self.asm.mov(d!(scratch_y), EAX)?;
        }
        for lane in 0..2 {
            if mask & (1 << lane) == 0 {
                continue;
            }
            if lane == 1 && early {
                self.asm.mov(ECX, d!(scratch_x))?;
                self.asm.mov(EDX, d!(scratch_y))?;
            } else {
                self.get(ECX, d, d.src_a[lane], lane)?;
                self.get(EDX, d, d.src_b[lane], lane)?;
            }
            self.lane(pc, d, lane)?;
            self.asm.mov(if lane == 0 { R14D } else { R15D }, EAX)?;
            if d.dst[lane] < 24 {
                self.asm
                    .mov(dword_ptr(RBX + Self::reg_offset(d.dst[lane])), EAX)?;
            } else if d.dst[lane] != 24 {
                self.asm.jmp(&self.fail_destination, JmpType::Near)?;
            }
        }
        for lane in 0..2 {
            if mask & (4 << lane) == 0 {
                continue;
            }
            if d.out_method[lane] != 0 {
                self.output(EAX, d, d.out_method[lane])?;
                self.asm.mov(ECX, EAX)?;
                self.asm.and_(ECX, 0xfff)?;
                self.asm.mov(d!(method), ECX)?;
                self.asm.shr(EAX, 12)?;
                self.asm.and_(EAX, 15)?;
                self.asm.mov(d!(increment), EAX)?;
            }
            if d.out_emit[lane] != 0 {
                self.output(EAX, d, d.out_emit[lane])?;
                self.asm.mov(ARG2D, EAX)?;
                self.asm.mov(ARG1D, d!(method))?;
                self.asm.mov(ARG0, RBX)?;
                self.asm.call_reg(q!(emit))?;
                self.asm.test(AL, AL)?;
                self.asm.jz(&self.fail_emit, JmpType::Near)?;
                self.asm.mov(EAX, d!(increment))?;
                self.asm.add(d!(method), EAX)?;
            }
        }
        if self.after_end[pc as usize] {
            self.asm.cmp(b!(end_next), 0)?;
            self.asm.jne(&self.success, JmpType::Near)?;
        }
        if flow {
            self.asm.cmp(b!(stopped), 0)?;
            self.asm.jne(&self.success, JmpType::Near)?;
        }
        if d.end {
            if flow {
                self.asm.mov(AL, b!(cancel_end))?;
                self.asm.xor_(AL, 1)?;
                self.asm.mov(b!(end_next), AL)?;
            } else {
                self.asm.mov(b!(end_next), 1)?;
            }
        }
        if self.loop_end[pc as usize] {
            let no_loop = self.asm.create_label();
            self.asm.mov(EAX, d!(loop_count))?;
            self.asm.test(EAX, EAX)?;
            self.asm.jz(&no_loop, JmpType::Near)?;
            self.asm.cmp(d!(loop_end), pc)?;
            self.asm.jne(&no_loop, JmpType::Near)?;
            self.asm.dec(d!(loop_count))?;
            self.asm.mov(EAX, d!(loop_start))?;
            self.asm.inc(EAX)?;
            self.asm.jmp(&self.dispatch, JmpType::Near)?;
            self.asm.bind(&no_loop)?;
        }
        if flow {
            self.asm.mov(EAX, d!(next_pc))?;
            if pc + 1 < count {
                self.asm.cmp(EAX, pc + 1)?;
                self.asm.jne(&self.dispatch, JmpType::Near)?;
                self.asm.jmp(&self.labels[pc as usize + 1], JmpType::Near)?;
            } else {
                self.asm.jmp(&self.dispatch, JmpType::Near)?;
            }
        } else if pc + 1 < count {
            self.asm.jmp(&self.labels[pc as usize + 1], JmpType::Near)?;
        } else {
            self.asm.jmp(&self.fail_outside, JmpType::Near)?;
        }
        Ok(())
    }
    fn output(&mut self, dst: Reg, d: &Decoded, selector: Word) -> rxbyak::Result<()> {
        match selector {
            1 => self.asm.mov(dst, R14D)?,
            2 => self.asm.mov(dst, R15D)?,
            3 => self.asm.mov(dst, R12D)?,
            4 => self.asm.mov(dst, R13D)?,
            5 => self.asm.mov(dst, d.imm[0])?,
            6 => self.asm.mov(dst, d.imm[1])?,
            8 => self.asm.mov(dst, d.imm[0] >> 12)?,
            9 => self.asm.mov(dst, d.imm[1] >> 12)?,
            10 => self.asm.mov(dst, (d.imm[0] << 16) | d.imm[1])?,
            _ => self.asm.jmp(&self.fail_output, JmpType::Near)?,
        }
        Ok(())
    }
    fn compare(&mut self, op: Word) -> rxbyak::Result<()> {
        self.asm.cmp(ECX, EDX)?;
        match op {
            17 | 25 => self.asm.setl(AL)?,
            18 | 26 => self.asm.setb(AL)?,
            19 | 27 => self.asm.setle(AL)?,
            20 | 28 => self.asm.setbe(AL)?,
            _ => self.asm.sete(AL)?,
        }
        self.asm.movzx(EAX, AL)
    }
    fn lane(&mut self, pc: Word, d: &Decoded, lane: usize) -> rxbyak::Result<()> {
        let imm = d.imm[lane];
        let op = d.op[lane];
        match op {
            0 | 2 => {
                self.asm.mov(EAX, ECX)?;
                if op == 0 {
                    self.asm.add(EAX, EDX)?;
                } else {
                    self.asm.sub(EAX, EDX)?;
                }
                self.asm.setc(R8B)?;
                self.asm.movzx(R8D, R8B)?;
                self.asm.mov(d!(carry), R8D)?;
            }
            1 => {
                self.asm.lea(EAX, ptr(RCX + RDX))?;
                self.asm.add(EAX, d!(carry))?;
            }
            3 => {
                self.asm.mov(EAX, ECX)?;
                self.asm.sub(EAX, EDX)?;
                self.asm.sub(EAX, d!(carry))?;
            }
            4 | 6 => {
                if op == 4 {
                    self.asm.movsxd(RAX, ECX)?;
                    self.asm.movsxd(RDX, EDX)?;
                } else {
                    self.asm.mov(EAX, ECX)?;
                    self.asm.mov(EDX, EDX)?;
                }
                self.asm.imul(RAX, RDX)?;
                self.asm.mov(RCX, RAX)?;
                self.asm.shr(RCX, 32)?;
                self.asm.mov(d!(carry), ECX)?;
            }
            5 => self.asm.mov(EAX, d!(carry))?,
            7 => {
                self.asm.cmp(ECX, 0x1000)?;
                self.asm.jne(&self.fail_extended, JmpType::Near)?;
                self.asm.xor_(EAX, EAX)?;
            }
            8 => {
                let zero = self.asm.create_label();
                let done = self.asm.create_label();
                self.asm.test(ECX, ECX)?;
                self.asm.jz(&zero, JmpType::Near)?;
                self.asm.bsr(EAX, ECX)?;
                self.asm.xor_(EAX, 31)?;
                self.asm.jmp(&done, JmpType::Near)?;
                self.asm.bind(&zero)?;
                self.asm.mov(EAX, 32)?;
                self.asm.bind(&done)?;
            }
            9..=11 => {
                self.asm.mov(EAX, ECX)?;
                self.asm.mov(ECX, EDX)?;
                self.asm.and_(ECX, 31)?;
                match op {
                    9 => self.asm.shl_cl(EAX)?,
                    10 => self.asm.shr_cl(EAX)?,
                    _ => self.asm.sar_cl(EAX)?,
                }
            }
            12 | 13 => {
                self.asm.mov(EAX, ECX)?;
                self.asm.and_(EAX, EDX)?;
                if op == 13 {
                    self.asm.not_(EAX)?;
                }
            }
            14 => {
                self.asm.mov(EAX, ECX)?;
                self.asm.or_(EAX, EDX)?;
            }
            15 => {
                self.asm.mov(EAX, ECX)?;
                self.asm.xor_(EAX, EDX)?;
            }
            16 => {
                let dest = (imm >> 10) & 63;
                let bit_count = (imm >> 5) & 31;
                let source = imm & 31;
                if dest >= 32 {
                    self.asm.jmp(&self.fail_merge, JmpType::Near)?;
                    return Ok(());
                }
                let bits_mask = (1u32 << bit_count) - 1;
                self.asm.mov(EAX, ECX)?;
                self.asm.and_(EAX, !(bits_mask << dest))?;
                self.asm.shr(EDX, source as u8)?;
                self.asm.and_(EDX, bits_mask)?;
                self.asm.shl(EDX, dest as u8)?;
                self.asm.or_(EAX, EDX)?;
            }
            17..=21 => {
                self.compare(op)?;
                self.asm.neg(EAX)?;
            }
            22 => {
                self.asm.lea(EAX, ptr(RCX + RDX))?;
                self.asm.and_(EAX, 0xffff)?;
                self.asm.mov(ARG1D, EAX)?;
                self.asm.mov(ARG0, RBX)?;
                self.asm.call_reg(q!(read))?;
            }
            23 => {
                if imm < 2 {
                    self.asm.jmp(&self.fail_loop, JmpType::Near)?;
                    return Ok(());
                }
                let skip = self.asm.create_label();
                let done = self.asm.create_label();
                self.asm.cmp(d!(loop_count), 0)?;
                self.asm.jne(&self.fail_loop, JmpType::Near)?;
                self.asm.test(ECX, ECX)?;
                self.asm.jz(&skip, JmpType::Near)?;
                self.asm.dec(ECX)?;
                self.asm.mov(d!(loop_count), ECX)?;
                self.asm.mov(d!(loop_start), pc)?;
                self.asm
                    .mov(d!(loop_end), pc.wrapping_add(imm).wrapping_sub(1))?;
                self.asm.jmp(&done, JmpType::Near)?;
                self.asm.bind(&skip)?;
                self.asm.mov(d!(next_pc), pc.wrapping_add(imm))?;
                self.asm.bind(&done)?;
                self.asm.xor_(EAX, EAX)?;
            }
            24 => {
                self.asm.mov(d!(next_pc), pc.wrapping_add(imm & 0x7fff))?;
                self.asm.xor_(EAX, EAX)?;
            }
            25..=29 => {
                let not_taken = self.asm.create_label();
                self.compare(op)?;
                self.asm.cmp(EAX, u32::from(imm & 0x8000 != 0))?;
                self.asm.jne(&not_taken, JmpType::Near)?;
                let offset = ((imm << 19) as i32) >> 19;
                if offset == -4096 {
                    self.asm.mov(b!(stopped), 1)?;
                } else {
                    self.asm.mov(d!(next_pc), pc.wrapping_add(offset as u32))?;
                    self.asm.mov(b!(cancel_end), u32::from(imm & 0x4000 != 0))?;
                }
                self.asm.bind(&not_taken)?;
                self.asm.xor_(EAX, EAX)?;
            }
            30 => {
                self.asm.cmp(ECX, 0xc00)?;
                self.asm.jae(&self.fail_dread, JmpType::Near)?;
                self.asm.mov(RAX, q!(dram))?;
                self.asm.mov(EAX, dword_ptr(RAX + RCX * 4))?;
            }
            31 => {
                self.asm.cmp(ECX, 0xc00)?;
                self.asm.jae(&self.fail_dwrite, JmpType::Near)?;
                self.asm.mov(RAX, q!(dram))?;
                self.asm.mov(dword_ptr(RAX + RCX * 4), EDX)?;
                self.asm.xor_(EAX, EAX)?;
            }
            _ => self.asm.xor_(EAX, EAX)?,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn instruction(fields: &[(usize, usize, u32)]) -> [u32; 3] {
        let mut bits = 0u128;
        for low in [15, 20, 25, 51, 56, 61] {
            bits |= 24u128 << low;
        }
        for &(low, count, value) in fields {
            let mask = ((1u128 << count) - 1) << low;
            bits = (bits & !mask) | (((value as u128) << low) & mask);
        }
        [(bits >> 64) as u32, (bits >> 32) as u32, bits as u32]
    }
    fn compare_execution(code: &[[u32; 3]], parameters: &[u32], append_fifo: bool) {
        let code = code.concat();
        let mut jit = TuringMacroJit::default();
        jit.prepare(&code, 1).unwrap();
        let mut interpreter = TuringMacroInterpreter::default();
        let mut jit_ram = [0u32; 0xc00];
        let mut expected_ram = [0u32; 0xc00];
        let mut actual = Vec::new();
        let mut expected = Vec::new();
        let mut actual_fifo = parameters.to_vec();
        let mut expected_fifo = parameters.to_vec();
        let native = jit.execute(
            0,
            &mut actual_fifo,
            &mut jit_ram,
            |address| address ^ 0x1234,
            |method, value, fifo, _ram| {
                actual.push((method, value));
                if append_fifo {
                    fifo.push(value.wrapping_add(1));
                }
                true
            },
        );
        let interpreted = interpreter.execute(
            &code,
            0,
            &mut expected_fifo,
            |address| address ^ 0x1234,
            |method, value, fifo, _ram| {
                expected.push((method, value));
                if append_fifo {
                    fifo.push(value.wrapping_add(1));
                }
                true
            },
            Some(&mut expected_ram),
        );
        assert_eq!(
            (native.success, native.error, native.consumed),
            (interpreted.success, interpreted.error, interpreted.consumed)
        );
        assert_eq!(native.pc, 0, "upstream JIT never updates fail_pc");
        assert_eq!(actual, expected);
        assert_eq!(jit_ram, expected_ram);
        assert_eq!(actual_fifo, expected_fifo);
    }
    #[test]
    fn dma_callback_writes_are_immediately_visible_to_dread_and_preserve_channel_ram() {
        // Emit DMA_READ, then DREAD the RAM word populated by the callback.
        let code = [
            instruction(&[(30, 16, 0x157), (66, 16, 77), (82, 3, 5), (85, 4, 6)]),
            instruction(&[
                (0, 1, 1),
                (10, 5, 30),
                (20, 5, 25),
                (30, 16, 0x27),
                (66, 16, 0x100),
                (82, 3, 6),
                (85, 4, 1),
            ]),
            instruction(&[]),
        ]
        .concat();
        for native in [false, true] {
            let mut ram = [0u32; 0xc00];
            ram[0x28] = 0xabcdef01;
            let mut fifo = Vec::new();
            let mut writes = Vec::new();
            let emit = |method, value, _fifo: &mut Vec<u32>, ram: &mut DataRam| {
                if method == 0x157 {
                    ram[0x27] = value;
                } else {
                    writes.push((method, value));
                }
                true
            };
            let result = if native {
                let mut jit = TuringMacroJit::default();
                jit.prepare(&code, 0).unwrap();
                jit.execute(0, &mut fifo, &mut ram, |_| 0, emit)
            } else {
                let mut interpreter = TuringMacroInterpreter::default();
                let result = interpreter.execute(&code, 0, &mut fifo, |_| 0, emit, Some(&mut ram));
                assert_eq!(interpreter.data_ram_reads, [(0x27, 77)]);
                assert!(interpreter.data_ram_writes.is_empty());
                assert!(!interpreter.reread_data_ram);
                result
            };
            assert!(result.success);
            assert_eq!(writes, [(0x100, 77)]);
            assert_eq!(ram[0x27], 77);
            assert_eq!(ram[0x28], 0xabcdef01);
        }
    }

    #[test]
    fn native_arithmetic_carry_and_callback_reads_match_interpreter() {
        for op in 0..=22 {
            let x = if op == 7 { 0x1000 } else { 0xffff_fffe };
            let fields = [
                (0, 1, 1),
                (10, 5, op),
                (20, 5, 28),
                (25, 5, 29),
                (30, 16, 0x1084),
                (46, 5, 5),
                (85, 4, 1),
                (92, 4, 2),
            ];
            compare_execution(&[instruction(&fields), instruction(&[])], &[x, 3], false);
        }
        // Lane1 reads the old register value while lane0 commits a new value.
        compare_execution(
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
            &[],
            false,
        );
    }
    #[test]
    fn fifo_reallocation_and_data_ram_are_visible_to_native_code() {
        compare_execution(
            &[
                instruction(&[(30, 16, 0x1005), (66, 16, 123), (82, 3, 5), (85, 4, 6)]),
                instruction(&[(0, 1, 1), (20, 5, 28), (85, 4, 1)]),
                instruction(&[]),
            ],
            &[],
            true,
        );
        compare_execution(
            &[
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
            ],
            &[],
            false,
        );
    }
    #[test]
    fn native_loops_predicates_and_end_cancellation_match_interpreter() {
        for count in [0, 1, 3] {
            compare_execution(
                &[
                    instruction(&[(10, 5, 23), (20, 5, 28), (30, 16, 2)]),
                    instruction(&[(20, 5, 28), (85, 4, 1)]),
                    instruction(&[(0, 1, 1)]),
                    instruction(&[]),
                ],
                &std::iter::once(count)
                    .chain(10..10 + count)
                    .collect::<Vec<_>>(),
                false,
            );
        }
        for mode in 0..16 {
            for predicate in [0, 1] {
                compare_execution(
                    &[
                        instruction(&[
                            (0, 1, 1),
                            (1, 4, mode),
                            (5, 5, 28),
                            (20, 5, 25),
                            (30, 16, 7),
                            (85, 4, 1),
                            (92, 4, 2),
                        ]),
                        instruction(&[]),
                    ],
                    &[predicate],
                    false,
                );
            }
        }
        compare_execution(
            &[
                instruction(&[(0, 1, 1), (10, 5, 29), (20, 5, 28), (30, 16, 0x4000)]),
                instruction(&[]),
            ],
            &[1, 0],
            false,
        );
        compare_execution(&[instruction(&[(10, 5, 29), (30, 16, 0x9000)])], &[], false);
    }
    #[test]
    fn native_error_paths_report_upstream_errors() {
        for fields in [
            vec![(20, 5, 28)],
            vec![(20, 5, 30)],
            vec![(15, 5, 25)],
            vec![(10, 5, 7)],
            vec![(10, 5, 16), (30, 16, 0x8000)],
            vec![(10, 5, 23), (30, 16, 1)],
            vec![(10, 5, 30), (20, 5, 25), (30, 16, 0xc00)],
            vec![(10, 5, 31), (20, 5, 25), (30, 16, 0xc00)],
            vec![(85, 4, 7)],
        ] {
            compare_execution(&[instruction(&fields)], &[], false);
        }
        compare_execution(&[], &[], false);
        let mut jit = TuringMacroJit::default();
        jit.prepare(&instruction(&[(85, 4, 5)]), 0).unwrap();
        assert_eq!(
            jit.execute(
                0,
                &mut vec![],
                &mut [0; 0xc00],
                |_| 0,
                |_, _, _, _ram| false
            )
            .error,
            "unsupported emitted method"
        );
        assert_eq!(TuringMacroJit::error_text(13), "unknown");
    }
    #[test]
    fn native_branch_conditions_limits_and_zero_register_initialization() {
        for op in 25..=29 {
            for (x, y) in [(u32::MAX, 0), (0, 1), (1, 1), (2, 1)] {
                for polarity in [0, 0x8000] {
                    compare_execution(
                        &[
                            instruction(&[
                                (0, 1, 1),
                                (10, 5, op),
                                (20, 5, 28),
                                (25, 5, 29),
                                (30, 16, polarity | 1),
                            ]),
                            instruction(&[]),
                        ],
                        &[x, y],
                        false,
                    );
                }
            }
        }
        let mut jit = TuringMacroJit::default();
        jit.prepare(&instruction(&[(10, 5, 24)]), 0).unwrap();
        assert_eq!(
            jit.execute(0, &mut vec![], &mut [0; 0xc00], |_| 0, |_, _, _, _ram| true)
                .error,
            "instruction limit exceeded"
        );
        assert_eq!(
            jit.execute(1, &mut vec![], &mut [0; 0xc00], |_| 0, |_, _, _, _ram| true)
                .error,
            "instruction outside uploaded RAM"
        );
        // Unlike the upstream interpreter, its JIT deliberately lacks an
        // initialized-register bitmask and reads initially zeroed registers.
        let code = [
            instruction(&[(0, 1, 1), (20, 5, 0), (85, 4, 1)]),
            instruction(&[]),
        ]
        .concat();
        jit.prepare(&code, 1).unwrap();
        let mut writes = Vec::new();
        assert!(
            jit.execute(
                0,
                &mut vec![],
                &mut [0; 0xc00],
                |_| 0,
                |m, v, _, _ram| {
                    writes.push((m, v));
                    true
                }
            )
            .success
        );
        assert_eq!(writes, [(0, 0)]);
    }

    #[test]
    fn prepare_uses_generation_and_native_state_layout_matches_upstream() {
        assert_eq!(offset_of!(State, carry), 96);
        assert_eq!(offset_of!(State, end_next), 144);
        assert_eq!(offset_of!(State, scratch_x), 148);
        assert_eq!(offset_of!(State, params), 160);
        assert_eq!(offset_of!(State, dram), 168);
        assert_eq!(offset_of!(State, user), 176);
        assert_eq!(offset_of!(State, emit), 184);
        assert_eq!(offset_of!(State, read), 192);
        assert_eq!(offset_of!(State, fifo), 200);
        assert_eq!(std::mem::size_of::<State>(), 208);
        let mut jit = TuringMacroJit::default();
        let a = [
            instruction(&[(0, 1, 1), (30, 16, 4), (85, 4, 5)]),
            instruction(&[]),
        ]
        .concat();
        let b = [
            instruction(&[(0, 1, 1), (30, 16, 9), (85, 4, 5)]),
            instruction(&[]),
        ]
        .concat();
        jit.prepare(&a, 0).unwrap();
        jit.prepare(&b, 0).unwrap();
        let mut value = 0;
        jit.execute(
            0,
            &mut vec![],
            &mut [0; 0xc00],
            |_| 0,
            |_, v, _, _ram| {
                value = v;
                true
            },
        );
        assert_eq!(value, 4);
        jit.prepare(&b, 1).unwrap();
        jit.execute(
            0,
            &mut vec![],
            &mut [0; 0xc00],
            |_| 0,
            |_, v, _, _ram| {
                value = v;
                true
            },
        );
        assert_eq!(value, 9);
    }
}
